//! Passwords, sessions, CSRF, login rate limiting and TOTP.

use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, SocketAddr},
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::{
    body::{Body, to_bytes},
    extract::{ConnectInfo, FromRequestParts, Request, State},
    http::{HeaderMap, Method, StatusCode, header, request::Parts},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, SameSite};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    AppState,
    error::{AppError, AppResult},
    util::{now_unix, random_token, sha256_hex},
};

pub const COOKIE: &str = "gb_session";
const ABSOLUTE_LIFETIME: i64 = 12 * 3600;

// ── Passwords ────────────────────────────────────────────────────────────────

pub fn hash_password(password: &str) -> String {
    use argon2::password_hash::PasswordHasher;
    argon2::Argon2::default().hash_password(password.as_bytes()).expect("argon2 hashing with default params cannot fail").to_string()
}

/// Verify against argon2 (current) or werkzeug scrypt/pbkdf2 (the old Python app).
pub fn verify_password(password: &str, stored: &str) -> bool {
    if stored.starts_with("$argon2") {
        use argon2::password_hash::{PasswordVerifier, phc::PasswordHash};
        return PasswordHash::new(stored)
            .map(|h| argon2::Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
            .unwrap_or(false);
    }
    verify_werkzeug(password, stored).unwrap_or(false)
}

pub fn needs_rehash(stored: &str) -> bool {
    !stored.starts_with("$argon2")
}

fn verify_werkzeug(password: &str, stored: &str) -> Option<bool> {
    let mut parts = stored.splitn(3, '$');
    let method = parts.next()?;
    let salt = parts.next()?;
    let expected = hex::decode(parts.next()?).ok()?;
    let mut m = method.split(':');
    match m.next()? {
        "scrypt" => {
            let n: u64 = m.next().unwrap_or("32768").parse().ok()?;
            let r: u32 = m.next().unwrap_or("8").parse().ok()?;
            let p: u32 = m.next().unwrap_or("1").parse().ok()?;
            if !n.is_power_of_two() {
                return None;
            }
            let params = scrypt::Params::new(n.trailing_zeros() as u8, r, p).ok()?;
            let mut out = vec![0u8; expected.len()];
            scrypt::scrypt(password.as_bytes(), salt.as_bytes(), &params, &mut out).ok()?;
            Some(constant_time_eq(&out, &expected))
        }
        "pbkdf2" => {
            let digest = m.next().unwrap_or("sha256");
            let rounds: u32 = m.next().unwrap_or("600000").parse().ok()?;
            let mut out = vec![0u8; expected.len()];
            match digest {
                "sha256" => pbkdf2::pbkdf2_hmac::<sha2::Sha256>(password.as_bytes(), salt.as_bytes(), rounds, &mut out),
                "sha512" => pbkdf2::pbkdf2_hmac::<sha2::Sha512>(password.as_bytes(), salt.as_bytes(), rounds, &mut out),
                _ => return None,
            }
            Some(constant_time_eq(&out, &expected))
        }
        _ => None,
    }
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// At least 10 characters with a letter and a digit.
pub fn validate_password(p: &str) -> Result<(), String> {
    if p.chars().count() < 10 || !p.chars().any(|c| c.is_alphabetic()) || !p.chars().any(|c| c.is_ascii_digit()) {
        return Err("Passwords need at least 10 characters, including a letter and a number.".into());
    }
    Ok(())
}

pub fn validate_username(u: &str) -> Result<(), String> {
    if u.is_empty() || u.len() > 64 || !u.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
        return Err("Usernames may only contain letters, numbers, dots, hyphens and underscores.".into());
    }
    Ok(())
}

// ── TOTP ─────────────────────────────────────────────────────────────────────

pub fn new_totp_secret() -> String {
    totp_rs::Secret::generate().to_base32()
}

pub fn totp_for(secret_b32: &str, account: &str) -> Option<totp_rs::Totp> {
    let secret = totp_rs::Secret::try_from_base32(secret_b32).ok()?;
    let account: String = account.chars().filter(|c| *c != ':').collect();
    totp_rs::Builder::new().with_secret(secret).with_account_name(account).with_issuer(Some("Gradebook")).build().ok()
}

pub fn check_totp(secret_b32: &str, account: &str, code: &str) -> bool {
    let code: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
    totp_for(secret_b32, account).is_some_and(|t| t.check_current(&code).is_some())
}

// ── Sessions ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Flash {
    pub kind: String, // success | danger | info | warning
    pub msg: String,
}

#[derive(Clone, Debug)]
pub struct Session {
    pub token_hash: String,
    pub user_id: Option<i64>,
    pub pending_user: Option<i64>,
    pub csrf: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub full_name: String,
    pub email: String,
    pub school: String,
    pub is_admin: bool,
    pub totp_enabled: bool,
    /// Signs in without a password here (board single sign-on, or local mode).
    pub external: bool,
}

pub fn load_user(conn: &rusqlite::Connection, id: i64) -> rusqlite::Result<Option<User>> {
    conn.query_row(
        "SELECT id, username, full_name, email, school, is_admin, totp_secret IS NOT NULL, password_hash LIKE '!%'
         FROM users WHERE id = ?1",
        [id],
        |r| {
            Ok(User {
                id: r.get(0)?,
                username: r.get(1)?,
                full_name: r.get(2)?,
                email: r.get(3)?,
                school: r.get(4)?,
                is_admin: r.get::<_, i64>(5)? != 0,
                totp_enabled: r.get::<_, i64>(6)? != 0,
                external: r.get::<_, i64>(7)? != 0,
            })
        },
    )
    .optional()
}

fn session_cookie(token: &str, secure: bool) -> String {
    Cookie::build((COOKIE, token.to_string())).path("/").http_only(true).secure(secure).same_site(SameSite::Lax).build().to_string()
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| Cookie::parse(c.trim().to_string()).ok())
        .find(|c| c.name() == COOKIE)
        .map(|c| c.value().to_string())
}

/// Attach a [`Session`] to every request, creating one if needed, and enforce
/// CSRF on state-changing methods.
pub async fn session_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    match session_inner(state, req, next).await {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}

async fn session_inner(state: AppState, mut req: Request, next: Next) -> AppResult<Response> {
    let now = now_unix();
    let idle = state.config.idle_timeout_secs;
    let token = cookie_token(req.headers());

    let existing = match token.clone() {
        Some(t) => {
            let hash = sha256_hex(&t);
            state
                .db
                .call(move |c| {
                    let row = c
                        .query_row(
                            "SELECT user_id, pending_user, csrf, created_at, last_seen FROM sessions WHERE token_hash = ?1",
                            [&hash],
                            |r| {
                                Ok((
                                    r.get::<_, Option<i64>>(0)?,
                                    r.get::<_, Option<i64>>(1)?,
                                    r.get::<_, String>(2)?,
                                    r.get::<_, i64>(3)?,
                                    r.get::<_, i64>(4)?,
                                ))
                            },
                        )
                        .optional()?;
                    let Some((user_id, pending, csrf, created, last_seen)) = row else { return Ok(None) };
                    if now - last_seen > idle || now - created > ABSOLUTE_LIFETIME {
                        c.execute("DELETE FROM sessions WHERE token_hash = ?1", [&hash])?;
                        return Ok(None);
                    }
                    if now - last_seen > 60 {
                        c.execute("UPDATE sessions SET last_seen = ?1 WHERE token_hash = ?2", params![now, hash])?;
                    }
                    Ok(Some(Session { token_hash: hash, user_id, pending_user: pending, csrf }))
                })
                .await?
        }
        None => None,
    };

    let (session, new_token) = match existing {
        Some(s) => (s, None),
        None => {
            let (s, t) = create_session(&state, None).await?;
            (s, Some(t))
        }
    };

    if matches!(*req.method(), Method::POST | Method::PUT | Method::PATCH | Method::DELETE) {
        req = check_csrf(req, &session.csrf).await?;
    }

    req.extensions_mut().insert(session);
    let mut resp = next.run(req).await;
    if let Some(t) = new_token {
        // A handler that logged in or out sets its own cookie; don't clobber it.
        if !resp.headers().contains_key(header::SET_COOKIE)
            && let Ok(v) = session_cookie(&t, state.config.cookie_secure).parse()
        {
            resp.headers_mut().append(header::SET_COOKIE, v);
        }
    }
    Ok(resp)
}

async fn create_session(state: &AppState, user_id: Option<i64>) -> AppResult<(Session, String)> {
    let token = random_token(32);
    let hash = sha256_hex(&token);
    let csrf = random_token(24);
    let now = now_unix();
    let s = Session { token_hash: hash.clone(), user_id, pending_user: None, csrf: csrf.clone() };
    state
        .db
        .call(move |c| {
            c.execute(
                "INSERT INTO sessions (token_hash, user_id, csrf, created_at, last_seen) VALUES (?1, ?2, ?3, ?4, ?4)",
                params![hash, user_id, csrf, now],
            )?;
            Ok(())
        })
        .await?;
    Ok((s, token))
}

/// Replace the session with a fresh one (prevents session fixation) and return
/// the `Set-Cookie` header value.
pub async fn rotate_session(state: &AppState, old: &Session, user_id: Option<i64>, pending_user: Option<i64>) -> AppResult<String> {
    let old_hash = old.token_hash.clone();
    state
        .db
        .call(move |c| {
            c.execute("DELETE FROM sessions WHERE token_hash = ?1", [old_hash])?;
            Ok(())
        })
        .await?;
    let (s, token) = create_session(state, user_id).await?;
    if pending_user.is_some() {
        let hash = s.token_hash.clone();
        state
            .db
            .call(move |c| {
                c.execute("UPDATE sessions SET pending_user = ?1 WHERE token_hash = ?2", params![pending_user, hash])?;
                Ok(())
            })
            .await?;
    }
    Ok(session_cookie(&token, state.config.cookie_secure))
}

pub async fn delete_sessions_for_user(state: &AppState, user_id: i64) -> AppResult<()> {
    state
        .db
        .call(move |c| {
            c.execute("DELETE FROM sessions WHERE user_id = ?1 OR pending_user = ?1", [user_id])?;
            Ok(())
        })
        .await?;
    state.workspaces.discard(user_id);
    Ok(())
}

/// After a sign-out: forget the teacher's memory workspace unless they are
/// still signed in somewhere else.
pub async fn discard_workspace_if_signed_out(state: &AppState, user_id: i64) -> AppResult<()> {
    let still_in: bool = state
        .db
        .call(move |c| Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM sessions WHERE user_id = ?1)", [user_id], |r| r.get(0))?))
        .await?;
    if !still_in {
        state.workspaces.discard(user_id);
    }
    Ok(())
}

pub async fn prune_sessions(state: &AppState) -> AppResult<()> {
    let cutoff_idle = now_unix() - state.config.idle_timeout_secs;
    let cutoff_abs = now_unix() - ABSOLUTE_LIFETIME;
    state
        .db
        .call(move |c| {
            c.execute("DELETE FROM sessions WHERE last_seen < ?1 OR created_at < ?2", params![cutoff_idle, cutoff_abs])?;
            c.execute("DELETE FROM import_staging WHERE created_at < ?1", [now_unix() - 24 * 3600])?;
            Ok(())
        })
        .await
}

pub async fn add_flash(state: &AppState, session: &Session, kind: &str, msg: impl Into<String>) -> AppResult<()> {
    let hash = session.token_hash.clone();
    let f = Flash { kind: kind.into(), msg: msg.into() };
    state
        .db
        .call(move |c| {
            let cur: String = c
                .query_row("SELECT flash FROM sessions WHERE token_hash = ?1", [&hash], |r| r.get(0))
                .optional()?
                .unwrap_or_else(|| "[]".into());
            let mut list: Vec<Flash> = serde_json::from_str(&cur).unwrap_or_default();
            list.push(f);
            c.execute(
                "UPDATE sessions SET flash = ?1 WHERE token_hash = ?2",
                params![serde_json::to_string(&list).unwrap_or_else(|_| "[]".into()), hash],
            )?;
            Ok(())
        })
        .await
}

pub async fn take_flashes(state: &AppState, session: &Session) -> AppResult<Vec<Flash>> {
    let hash = session.token_hash.clone();
    state
        .db
        .call(move |c| {
            let cur: Option<String> = c.query_row("SELECT flash FROM sessions WHERE token_hash = ?1", [&hash], |r| r.get(0)).optional()?;
            match cur {
                Some(s) if s != "[]" => {
                    c.execute("UPDATE sessions SET flash = '[]' WHERE token_hash = ?1", [&hash])?;
                    Ok(serde_json::from_str(&s).unwrap_or_default())
                }
                _ => Ok(vec![]),
            }
        })
        .await
}

// ── CSRF ─────────────────────────────────────────────────────────────────────

/// Accept the token from the `X-CSRF-Token` header, the `csrf_token` query
/// parameter (multipart forms) or a urlencoded form field.
async fn check_csrf(req: Request, expected: &str) -> AppResult<Request> {
    let ok = |v: &str| constant_time_eq(v.as_bytes(), expected.as_bytes());
    if req.headers().get("x-csrf-token").and_then(|v| v.to_str().ok()).is_some_and(ok) {
        return Ok(req);
    }
    if let Some(q) = req.uri().query()
        && form_value(q.as_bytes(), "csrf_token").is_some_and(|v| ok(&v))
    {
        return Ok(req);
    }
    let is_form = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/x-www-form-urlencoded"));
    if is_form {
        let (parts, body) = req.into_parts();
        let bytes = to_bytes(body, 2 * 1024 * 1024).await.map_err(|_| crate::error::bad("Form too large."))?;
        if form_value(&bytes, "csrf_token").is_some_and(|v| ok(&v)) {
            return Ok(Request::from_parts(parts, Body::from(bytes)));
        }
    }
    Err(AppError::BadRequest("Your session expired or the form was out of date. Go back, refresh the page and try again.".into()))
}

fn form_value(body: &[u8], key: &str) -> Option<String> {
    serde_urlencoded_pairs(body).into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn serde_urlencoded_pairs(body: &[u8]) -> Vec<(String, String)> {
    let s = String::from_utf8_lossy(body);
    s.split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            let dec = |x: &str| percent_encoding::percent_decode_str(&x.replace('+', " ")).decode_utf8_lossy().into_owned();
            Some((dec(k), dec(v)))
        })
        .collect()
}

// ── Extractors ───────────────────────────────────────────────────────────────

/// A logged-in teacher. Redirects to the login page otherwise.
pub struct Auth {
    pub user: User,
    pub session: Session,
    /// Where this teacher's student data lives: their in-memory workspace
    /// (memory storage) or the database file (persistent storage).
    pub db: crate::db::Db,
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let session = parts.extensions.get::<Session>().cloned().ok_or_else(|| StatusCode::UNAUTHORIZED.into_response())?;
        let to_login = || {
            let next = parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
            if parts.uri.path().starts_with("/api/") {
                return (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(serde_json::json!({"error": "Your session expired. Reload the page to log in again."})),
                )
                    .into_response();
            }
            let enc = percent_encoding::utf8_percent_encode(next, percent_encoding::NON_ALPHANUMERIC);
            Redirect::to(&format!("/login?next={enc}")).into_response()
        };
        let Some(uid) = session.user_id else { return Err(to_login()) };
        let user = state.db.call(move |c| Ok(load_user(c, uid)?)).await.map_err(IntoResponse::into_response)?;
        match user {
            Some(user) => {
                let db = state.user_db(user.id).map_err(IntoResponse::into_response)?;
                Ok(Auth { user, session, db })
            }
            None => Err(to_login()),
        }
    }
}

/// A logged-in admin.
pub struct Admin(pub Auth);

impl FromRequestParts<AppState> for Admin {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let auth = Auth::from_request_parts(parts, state).await?;
        if !auth.user.is_admin {
            return Err(AppError::Forbidden.into_response());
        }
        Ok(Admin(auth))
    }
}

/// The session, for pages that don't need a login (login, TOTP).
pub struct AnySession(pub Session);

impl FromRequestParts<AppState> for AnySession {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _: &AppState) -> Result<Self, Self::Rejection> {
        parts.extensions.get::<Session>().cloned().map(AnySession).ok_or_else(|| StatusCode::UNAUTHORIZED.into_response())
    }
}

// ── Client IP + rate limiting ────────────────────────────────────────────────

pub struct ClientIp(pub IpAddr);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        if state.config.trust_proxy {
            // Like werkzeug's ProxyFix(x_for=1): the address our proxy saw.
            if let Some(ip) = parts
                .headers
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit(',').next())
                .and_then(|v| v.trim().parse().ok())
            {
                return Ok(ClientIp(ip));
            }
        }
        let ip = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip()).unwrap_or(IpAddr::from([127, 0, 0, 1]));
        Ok(ClientIp(ip))
    }
}

pub struct RateLimiter {
    max: usize,
    window: Duration,
    hits: Mutex<HashMap<IpAddr, VecDeque<Instant>>>,
}

impl RateLimiter {
    pub fn new(max: usize, window: Duration) -> Self {
        RateLimiter { max, window, hits: Mutex::new(HashMap::new()) }
    }

    /// Record a hit; false if the IP is over the limit.
    pub fn check(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut map = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() > 10_000 {
            map.retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t) < self.window));
        }
        let q = map.entry(ip).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) >= self.window) {
            q.pop_front();
        }
        if q.len() >= self.max {
            return false;
        }
        q.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argon2_roundtrip() {
        let h = hash_password("correct horse 1");
        assert!(verify_password("correct horse 1", &h));
        assert!(!verify_password("wrong", &h));
        assert!(!needs_rehash(&h));
    }

    #[test]
    fn werkzeug_hashes_verify() {
        // Same construction werkzeug uses: "<method>:<params>$<salt>$<hex digest>".
        let params = scrypt::Params::new(15, 8, 1).unwrap();
        let mut out = [0u8; 64];
        scrypt::scrypt(b"Password123", b"saltsalt", &params, &mut out).unwrap();
        let stored = format!("scrypt:32768:8:1$saltsalt${}", hex::encode(out));
        assert!(verify_password("Password123", &stored));
        assert!(!verify_password("Password124", &stored));
        assert!(needs_rehash(&stored));

        let mut out = [0u8; 32];
        pbkdf2::pbkdf2_hmac::<sha2::Sha256>(b"Password123", b"abc", 1000, &mut out);
        let stored = format!("pbkdf2:sha256:1000$abc${}", hex::encode(out));
        assert!(verify_password("Password123", &stored));
        assert!(!verify_password("nope", &stored));
    }

    #[test]
    fn password_policy() {
        assert!(validate_password("short1").is_err());
        assert!(validate_password("longenoughbutnodigit").is_err());
        assert!(validate_password("longenough1").is_ok());
    }

    #[test]
    fn rate_limiter_blocks_after_max() {
        let rl = RateLimiter::new(3, Duration::from_secs(60));
        let ip = IpAddr::from([10, 0, 0, 1]);
        assert!(rl.check(ip) && rl.check(ip) && rl.check(ip));
        assert!(!rl.check(ip));
        assert!(rl.check(IpAddr::from([10, 0, 0, 2])));
    }

    #[test]
    fn form_parsing() {
        assert_eq!(form_value(b"a=1&csrf_token=x%2By&b=", "csrf_token").as_deref(), Some("x+y"));
    }
}
