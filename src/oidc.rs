//! Single sign-on with OpenID Connect (Microsoft Entra ID, Google Workspace,
//! or any standards-compliant provider), for board deployments.
//!
//! Authorization-code flow with PKCE and a nonce; the ID token's signature,
//! issuer, audience, expiry and nonce are all checked by `openidconnect`.

use std::time::{Duration, Instant};

use openidconnect::{
    AuthenticationFlow, AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier,
    RedirectUrl, Scope, TokenResponse,
    core::{CoreClient, CoreProviderMetadata, CoreResponseType},
    reqwest,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::config::Oidc;

/// Provider keys rotate; re-read the discovery document this often.
const REFRESH: Duration = Duration::from_secs(3600);

/// A client for the current provider metadata (the typestate type is unwieldy to name).
macro_rules! client {
    ($p:expr, $meta:expr) => {
        CoreClient::from_provider_metadata(
            $meta,
            ClientId::new($p.cfg.client_id.clone()),
            $p.cfg.client_secret.clone().map(ClientSecret::new),
        )
        .set_redirect_uri(RedirectUrl::new($p.redirect_url())?)
    };
}

pub struct Provider {
    pub cfg: Oidc,
    http: reqwest::Client,
    meta: RwLock<(CoreProviderMetadata, Instant)>,
}

/// Kept in the session between leaving for the provider and coming back.
#[derive(Serialize, Deserialize)]
pub struct Pending {
    pub state: String,
    pub nonce: String,
    pub verifier: String,
    pub next: String,
}

/// Who the provider says signed in.
#[derive(Debug)]
pub struct Identity {
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub name: Option<String>,
}

impl Provider {
    pub async fn discover(cfg: &Oidc) -> anyhow::Result<Self> {
        let http = reqwest::ClientBuilder::new()
            // Following redirects here would allow SSRF; providers don't need it.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .build()?;
        let meta = fetch(cfg, &http).await?;
        Ok(Provider { cfg: cfg.clone(), http, meta: RwLock::new((meta, Instant::now())) })
    }

    async fn metadata(&self) -> anyhow::Result<CoreProviderMetadata> {
        {
            let m = self.meta.read().await;
            if m.1.elapsed() < REFRESH {
                return Ok(m.0.clone());
            }
        }
        let fresh = fetch(&self.cfg, &self.http).await?;
        *self.meta.write().await = (fresh.clone(), Instant::now());
        Ok(fresh)
    }

    fn redirect_url(&self) -> String {
        format!("{}/login/oidc/callback", self.cfg.public_url)
    }

    /// The provider URL to send the browser to, and what to remember.
    pub async fn start(&self, next: String) -> anyhow::Result<(String, Pending)> {
        let client = client!(self, self.metadata().await?);
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, state, nonce) = client
            .authorize_url(AuthenticationFlow::<CoreResponseType>::AuthorizationCode, CsrfToken::new_random, Nonce::new_random)
            .add_scope(Scope::new("email".into()))
            .add_scope(Scope::new("profile".into()))
            .set_pkce_challenge(challenge)
            .url();
        let pending = Pending { state: state.secret().clone(), nonce: nonce.secret().clone(), verifier: verifier.secret().clone(), next };
        Ok((url.to_string(), pending))
    }

    /// Trade the code for tokens and verify the ID token.
    pub async fn finish(&self, code: String, pending: &Pending) -> anyhow::Result<Identity> {
        let client = client!(self, self.metadata().await?);
        let tokens = client
            .exchange_code(AuthorizationCode::new(code))?
            .set_pkce_verifier(PkceCodeVerifier::new(pending.verifier.clone()))
            .request_async(&self.http)
            .await
            .map_err(|e| anyhow::anyhow!("token request failed: {e}"))?;
        let id_token = tokens.id_token().ok_or_else(|| anyhow::anyhow!("the provider sent no ID token"))?;
        let nonce = Nonce::new(pending.nonce.clone());
        if let Ok(claims) = id_token.claims(&client.id_token_verifier(), &nonce) {
            return Ok(identity(claims));
        }
        // The provider may have rotated its keys since we last looked: refresh
        // once and verify again.
        let fresh = fetch(&self.cfg, &self.http).await?;
        *self.meta.write().await = (fresh.clone(), Instant::now());
        let client = client!(self, fresh);
        let claims = id_token.claims(&client.id_token_verifier(), &nonce).map_err(|e| anyhow::anyhow!("ID token rejected: {e}"))?;
        Ok(identity(claims))
    }

    /// Whether this identity may use the app (domain allow-list, verified email).
    pub fn allowed(&self, id: &Identity) -> Result<(), String> {
        if self.cfg.allowed_domains.is_empty() {
            return Ok(());
        }
        let domain = id.email.as_deref().and_then(|e| e.rsplit_once('@')).map(|(_, d)| d.to_ascii_lowercase());
        match domain {
            Some(d) if self.cfg.allowed_domains.contains(&d) => Ok(()),
            _ => Err("This account isn't from a school board that uses this Gradebook.".into()),
        }
    }

    pub fn is_admin(&self, id: &Identity) -> bool {
        id.email.as_deref().is_some_and(|e| self.cfg.admin_emails.iter().any(|a| a.eq_ignore_ascii_case(e)))
    }
}

async fn fetch(cfg: &Oidc, http: &reqwest::Client) -> anyhow::Result<CoreProviderMetadata> {
    CoreProviderMetadata::discover_async(IssuerUrl::new(cfg.issuer.clone())?, http)
        .await
        .map_err(|e| anyhow::anyhow!("couldn't read the sign-in provider's settings from {}: {e}", cfg.issuer))
}

fn identity(claims: &openidconnect::core::CoreIdTokenClaims) -> Identity {
    // Providers that say an email is unverified don't get to use it as an identity.
    let verified = claims.email_verified() != Some(false);
    let email = claims
        .email()
        .filter(|_| verified)
        .map(|e| e.as_str().to_ascii_lowercase())
        // Microsoft Entra often omits `email`; its `preferred_username` is the UPN.
        .or_else(|| claims.preferred_username().map(|u| u.as_str().to_ascii_lowercase()).filter(|u| u.contains('@')));
    Identity {
        issuer: claims.issuer().as_str().to_string(),
        subject: claims.subject().as_str().to_string(),
        email,
        name: claims.name().and_then(|n| n.get(None)).map(|n| n.as_str().to_string()),
    }
}
