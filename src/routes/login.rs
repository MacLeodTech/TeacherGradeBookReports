use axum::{
    extract::{Query, State},
    http::header,
    response::{IntoResponse, Response},
};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;

use super::{F, flash_to, render_anon, to};
use crate::{
    AppState, audit,
    auth::{self, AnySession, ClientIp},
    config::AuthMode,
    error::{AppError, AppResult},
    oidc::Pending,
};

#[derive(Deserialize)]
pub struct Next {
    #[serde(default)]
    next: String,
}

/// Only same-site relative paths (no scheme, host, or `//`).
fn safe_next(next: &str) -> String {
    let n = next.replace('\\', "");
    if n.starts_with('/') && !n.starts_with("//") && !n.contains("://") { n } else { "/".into() }
}

fn with_cookie(mut resp: Response, cookie: String) -> Response {
    if let Ok(v) = cookie.parse() {
        resp.headers_mut().append(header::SET_COOKIE, v);
    }
    resp
}

pub async fn login_page(State(state): State<AppState>, AnySession(s): AnySession, Query(q): Query<Next>) -> AppResult<Response> {
    if s.user_id.is_some() {
        return Ok(to(&safe_next(&q.next)));
    }
    render_anon(
        &state,
        &s,
        "login.html",
        ctx! {
            next => safe_next(&q.next),
            password_login => state.config.password_login(),
            sso_name => state.oidc.as_ref().map(|o| o.cfg.display_name.clone()),
            local => state.config.auth == AuthMode::Local,
        },
    )
    .await
}

pub async fn login_submit(
    State(state): State<AppState>,
    AnySession(s): AnySession,
    ClientIp(ip): ClientIp,
    Query(q): Query<Next>,
    form: F,
) -> AppResult<Response> {
    if !state.config.password_login() {
        return Err(AppError::NotFound);
    }
    let next = safe_next(&if q.next.is_empty() { form.get("next") } else { q.next });
    let back = format!("/login?next={}", percent_encoding::utf8_percent_encode(&next, percent_encoding::NON_ALPHANUMERIC));
    if !state.login_limiter.check(ip) {
        return flash_to(&state, &s, "danger", "Too many attempts. Wait a minute and try again.", &back).await;
    }
    let username = form.get("username");
    let password = form.raw("password");
    let found = state
        .db
        .call(move |c| {
            Ok(c.query_row("SELECT id, password_hash, totp_secret IS NOT NULL FROM users WHERE username = ?1", [&username], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)? != 0))
            })
            .optional()?)
        })
        .await?;

    let Some((uid, hash, has_totp)) = found.filter(|(_, h, _)| auth::verify_password(&password, h)) else {
        // Spend comparable time on unknown usernames.
        let _ = auth::verify_password(&password, dummy_hash());
        audit::log(&state, None, &form.get("username"), &ip.to_string(), "login.failed", "").await;
        return flash_to(&state, &s, "danger", "Wrong username or password.", &back).await;
    };

    if auth::needs_rehash(&hash) {
        let new_hash = auth::hash_password(&password);
        state
            .db
            .call(move |c| {
                c.execute("UPDATE users SET password_hash = ?1 WHERE id = ?2", params![new_hash, uid])?;
                Ok(())
            })
            .await?;
    }

    if has_totp {
        let cookie = auth::rotate_session(&state, &s, None, Some(uid)).await?;
        let path = format!("/login/verify?next={}", percent_encoding::utf8_percent_encode(&next, percent_encoding::NON_ALPHANUMERIC));
        return Ok(with_cookie(to(&path), cookie));
    }
    let cookie = auth::rotate_session(&state, &s, Some(uid), None).await?;
    audit::log(&state, Some(uid), &form.get("username"), &ip.to_string(), "login", "password").await;
    Ok(with_cookie(to(&next), cookie))
}

fn dummy_hash() -> &'static str {
    static H: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    H.get_or_init(|| auth::hash_password("not-a-real-password-1"))
}

pub async fn totp_page(State(state): State<AppState>, AnySession(s): AnySession, Query(q): Query<Next>) -> AppResult<Response> {
    if s.pending_user.is_none() {
        return Ok(to("/login"));
    }
    render_anon(&state, &s, "login_totp.html", ctx! { next => safe_next(&q.next) }).await
}

pub async fn totp_submit(
    State(state): State<AppState>,
    AnySession(s): AnySession,
    ClientIp(ip): ClientIp,
    Query(q): Query<Next>,
    form: F,
) -> AppResult<Response> {
    let next = safe_next(&q.next);
    let Some(uid) = s.pending_user else { return Ok(to("/login")) };
    if !state.login_limiter.check(ip) {
        return flash_to(&state, &s, "danger", "Too many attempts. Wait a minute and try again.", "/login").await;
    }
    let secret = state
        .db
        .call(move |c| {
            Ok(c.query_row("SELECT totp_secret, username FROM users WHERE id = ?1", [uid], |r| {
                Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?))
            })
            .optional()?)
        })
        .await?;
    let ok = matches!(&secret, Some((Some(sec), name)) if auth::check_totp(sec, name, &form.get("code")));
    if !ok {
        let path = format!("/login/verify?next={}", percent_encoding::utf8_percent_encode(&next, percent_encoding::NON_ALPHANUMERIC));
        return flash_to(&state, &s, "danger", "That code didn't match. Check your authenticator app and try again.", &path).await;
    }
    let cookie = auth::rotate_session(&state, &s, Some(uid), None).await?;
    let name = secret.map(|(_, n)| n).unwrap_or_default();
    audit::log(&state, Some(uid), &name, &ip.to_string(), "login", "password + 2-step").await;
    Ok(with_cookie(to(&next), cookie))
}

pub async fn logout(State(state): State<AppState>, AnySession(s): AnySession, ClientIp(ip): ClientIp) -> AppResult<Response> {
    let cookie = auth::rotate_session(&state, &s, None, None).await?;
    if let Some(uid) = s.user_id {
        // With memory storage, signing out clears the teacher's student data.
        auth::discard_workspace_if_signed_out(&state, uid).await?;
        audit::log(&state, Some(uid), "", &ip.to_string(), "logout", "").await;
    }
    Ok(with_cookie(to("/login"), cookie).into_response())
}

// ── Board single sign-on ─────────────────────────────────────────────────────

pub async fn sso_start(State(state): State<AppState>, AnySession(s): AnySession, Query(q): Query<Next>) -> AppResult<Response> {
    let Some(provider) = state.oidc.clone() else { return Err(AppError::NotFound) };
    let (url, pending) = provider.start(safe_next(&q.next)).await.map_err(AppError::Internal)?;
    let json = serde_json::to_string(&pending).map_err(|e| AppError::Internal(e.into()))?;
    let hash = s.token_hash.clone();
    state
        .db
        .call(move |c| {
            c.execute("UPDATE sessions SET oidc_pending = ?1 WHERE token_hash = ?2", params![json, hash])?;
            Ok(())
        })
        .await?;
    Ok(to(&url))
}

#[derive(Deserialize)]
pub struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

pub async fn sso_callback(
    State(state): State<AppState>,
    AnySession(s): AnySession,
    ClientIp(ip): ClientIp,
    Query(cb): Query<Callback>,
) -> AppResult<Response> {
    let Some(provider) = state.oidc.clone() else { return Err(AppError::NotFound) };
    let hash = s.token_hash.clone();
    let pending: Option<String> = state
        .db
        .call(move |c| {
            let p = c.query_row("SELECT oidc_pending FROM sessions WHERE token_hash = ?1", [&hash], |r| r.get(0)).optional()?.flatten();
            c.execute("UPDATE sessions SET oidc_pending = NULL WHERE token_hash = ?1", [&hash])?;
            Ok(p)
        })
        .await?;
    let fail = |msg: &str| flash_to(&state, &s, "danger", msg.to_string(), "/login");
    let Some(pending) = pending.and_then(|p| serde_json::from_str::<Pending>(&p).ok()) else {
        return fail("That sign-in link has expired. Please try again.").await;
    };
    if cb.error.is_some() {
        return fail("Sign-in was cancelled.").await;
    }
    let (Some(code), Some(st)) = (cb.code, cb.state) else { return fail("The sign-in response was incomplete. Please try again.").await };
    if !auth::constant_time_eq(st.as_bytes(), pending.state.as_bytes()) {
        return fail("That sign-in response didn't match. Please try again.").await;
    }
    if !state.login_limiter.check(ip) {
        return fail("Too many attempts. Wait a minute and try again.").await;
    }
    let id = match provider.finish(code, &pending).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!("single sign-on failed: {e:#}");
            audit::log(&state, None, "", &ip.to_string(), "login.failed", "single sign-on").await;
            return fail("Single sign-on didn't work. Please try again, or ask your administrator.").await;
        }
    };
    if let Err(msg) = provider.allowed(&id) {
        audit::log(&state, None, id.email.as_deref().unwrap_or(""), &ip.to_string(), "login.refused", "domain not allowed").await;
        return fail(&msg).await;
    }
    let make_admin = provider.is_admin(&id);
    let (uid, username) = state
        .db
        .call(move |c| {
            let found: Option<(i64, String)> = c
                .query_row(
                    "SELECT id, username FROM users WHERE oidc_issuer = ?1 AND oidc_subject = ?2",
                    params![id.issuer, id.subject],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let (uid, username) = match found {
                Some(f) => f,
                None => {
                    // A new teacher: the username is their email (made unique if needed).
                    let base = id.email.clone().unwrap_or_else(|| format!("user-{}", &crate::util::sha256_hex(&id.subject)[..10]));
                    let mut name = base.clone();
                    let mut n = 2;
                    while c.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE username = ?1)", [&name], |r| r.get::<_, bool>(0))? {
                        name = format!("{base}-{n}");
                        n += 1;
                    }
                    c.execute(
                        "INSERT INTO users (username, password_hash, full_name, email, oidc_issuer, oidc_subject)
                         VALUES (?1, '!oidc', ?2, ?3, ?4, ?5)",
                        params![name, id.name.clone().unwrap_or_default(), id.email.clone().unwrap_or_default(), id.issuer, id.subject],
                    )?;
                    (c.last_insert_rowid(), name)
                }
            };
            if make_admin {
                c.execute("UPDATE users SET is_admin = 1 WHERE id = ?1", [uid])?;
            }
            Ok((uid, username))
        })
        .await?;
    let cookie = auth::rotate_session(&state, &s, Some(uid), None).await?;
    audit::log(&state, Some(uid), &username, &ip.to_string(), "login", "single sign-on").await;
    Ok(with_cookie(to(&pending.next), cookie))
}

// ── Local mode ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct Launch {
    #[serde(default)]
    token: String,
}

/// The link the local app opens in the browser signs the teacher in.
pub async fn launch(State(state): State<AppState>, AnySession(s): AnySession, Query(q): Query<Launch>) -> AppResult<Response> {
    let ok = state.config.auth == AuthMode::Local
        && state.config.launch_token.as_deref().is_some_and(|t| auth::constant_time_eq(t.as_bytes(), q.token.as_bytes()));
    if !ok {
        return Err(AppError::BadRequest(
            "This link is out of date. Close this tab and open Gradebook again from its window or shortcut.".into(),
        ));
    }
    let uid: i64 =
        state.db.call(|c| Ok(c.query_row("SELECT id FROM users WHERE username = ?1", [crate::local::LOCAL_USER], |r| r.get(0))?)).await?;
    let cookie = auth::rotate_session(&state, &s, Some(uid), None).await?;
    Ok(with_cookie(to("/"), cookie))
}

/// Local mode: stop the app.
pub async fn quit(State(state): State<AppState>, AnySession(s): AnySession) -> AppResult<Response> {
    if state.config.auth != AuthMode::Local || s.user_id.is_none() {
        return Err(AppError::NotFound);
    }
    state.shutdown.notify_one();
    render_anon(&state, &s, "closed.html", ctx! {}).await
}

#[cfg(test)]
mod tests {
    #[test]
    fn next_is_sanitised() {
        assert_eq!(super::safe_next("/classes/1"), "/classes/1");
        assert_eq!(super::safe_next("//evil.com"), "/");
        assert_eq!(super::safe_next("https://evil.com"), "/");
        assert_eq!(super::safe_next("/\\evil.com"), "/evil.com");
        assert_eq!(super::safe_next(""), "/");
    }
}
