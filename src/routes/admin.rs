//! Admin: teacher accounts and database backups.

use axum::{
    extract::{Path, State},
    http::header,
    response::{IntoResponse, Response},
};
use rusqlite::params;

use super::{F, flash_to, render};
use crate::{
    AppState, audit,
    auth::{self, Admin},
    backup,
    error::{AppError, AppResult},
    util::{content_disposition, random_token},
};

pub async fn users(State(state): State<AppState>, Admin(auth): Admin) -> AppResult<Response> {
    let users = state
        .db
        .call(|c| {
            let mut st = c.prepare(
                "SELECT u.id, u.username, u.full_name, u.email, u.school, u.is_admin, u.totp_secret IS NOT NULL, u.created_at,
                        (SELECT count(*) FROM classes WHERE user_id = u.id),
                        (SELECT max(last_seen) FROM sessions WHERE user_id = u.id), u.password_hash LIKE '!%'
                 FROM users u ORDER BY u.username",
            )?;
            let rows = st
                .query_map([], |r| {
                    Ok(ctx! {
                        id => r.get::<_, i64>(0)?, username => r.get::<_, String>(1)?, full_name => r.get::<_, String>(2)?,
                        email => r.get::<_, String>(3)?, school => r.get::<_, String>(4)?, is_admin => r.get::<_, i64>(5)? != 0,
                        totp => r.get::<_, i64>(6)? != 0, created => r.get::<_, String>(7)?, classes => r.get::<_, i64>(8)?,
                        last_seen => r.get::<_, Option<i64>>(9)?.and_then(|t| chrono::DateTime::from_timestamp(t, 0)).map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string()),
                        external => r.get::<_, i64>(10)? != 0,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await?;
    let users: Vec<_> = users
        .into_iter()
        .map(|u| {
            let id = u.get_attr("id").ok().and_then(|v| i64::try_from(v).ok()).unwrap_or_default();
            let open = state.workspaces.peek(id).map(|ws| ws.unsaved());
            minijinja::context! { ..u, ..ctx! { open_workspace => open.is_some(), open_unsaved => open.unwrap_or(false) } }
        })
        .collect();
    render(&state, &auth, "admin_users.html", ctx! { users, open_workspaces => state.workspaces.len(), sso => state.config.oidc.is_some() })
        .await
}

pub async fn user_add(State(state): State<AppState>, Admin(auth): Admin, f: F) -> AppResult<Response> {
    if !state.config.password_login() {
        return flash_to(
            &state,
            &auth.session,
            "warning",
            "Teachers sign in with single sign-on; their accounts are created the first time they do.",
            "/admin/users",
        )
        .await;
    }
    let (username, password) = (f.get("username"), f.raw("password"));
    if let Err(e) = auth::validate_username(&username).and_then(|_| auth::validate_password(&password)) {
        return flash_to(&state, &auth.session, "danger", e, "/admin/users").await;
    }
    let (full_name, email, school, is_admin) = (f.get("full_name"), f.get("email"), f.get("school"), f.has("is_admin"));
    let hash = tokio::task::spawn_blocking(move || auth::hash_password(&password)).await.map_err(|e| AppError::Internal(e.into()))?;
    let name = username.clone();
    let created = state
        .db
        .call(move |c| {
            let exists: bool = c.query_row("SELECT count(*) FROM users WHERE username = ?1", [&username], |r| r.get::<_, i64>(0))? > 0;
            if exists {
                return Ok(false);
            }
            c.execute(
                "INSERT INTO users (username, password_hash, full_name, email, school, is_admin) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![username, hash, full_name, email, school, is_admin],
            )?;
            Ok(true)
        })
        .await?;
    if created {
        audit::log(&state, Some(auth.user.id), &auth.user.username, "", "admin.user_add", &name).await;
        flash_to(
            &state,
            &auth.session,
            "success",
            format!("Account \"{name}\" created. They'll be walked through setup on first login."),
            "/admin/users",
        )
        .await
    } else {
        flash_to(&state, &auth.session, "danger", "That username is taken.", "/admin/users").await
    }
}

pub async fn user_delete(State(state): State<AppState>, Admin(auth): Admin, Path(id): Path<i64>, f: F) -> AppResult<Response> {
    if id == auth.user.id {
        return flash_to(&state, &auth.session, "danger", "You can't delete your own account.", "/admin/users").await;
    }
    let typed = f.get("confirm_username");
    let ok = state
        .db
        .call(move |c| {
            let name: Option<String> =
                rusqlite::OptionalExtension::optional(c.query_row("SELECT username FROM users WHERE id = ?1", [id], |r| r.get(0)))?;
            match name {
                Some(n) if n.eq_ignore_ascii_case(&typed) => {
                    c.execute("DELETE FROM users WHERE id = ?1", [id])?;
                    Ok(true)
                }
                _ => Ok(false),
            }
        })
        .await?;
    if ok {
        state.workspaces.discard(id);
        audit::log(&state, Some(auth.user.id), &auth.user.username, "", "admin.user_delete", format!("user {id}")).await;
        flash_to(&state, &auth.session, "success", "Account and all of its classes, students and marks deleted.", "/admin/users").await
    } else {
        flash_to(&state, &auth.session, "danger", "Type the username exactly to delete the account.", "/admin/users").await
    }
}

pub async fn user_password(State(state): State<AppState>, Admin(auth): Admin, Path(id): Path<i64>, f: F) -> AppResult<Response> {
    let password = f.raw("password");
    if let Err(e) = auth::validate_password(&password) {
        return flash_to(&state, &auth.session, "danger", e, "/admin/users").await;
    }
    let reset_totp = f.has("reset_totp");
    let hash = tokio::task::spawn_blocking(move || auth::hash_password(&password)).await.map_err(|e| AppError::Internal(e.into()))?;
    let n = state
        .db
        .call(move |c| {
            let n = c.execute("UPDATE users SET password_hash = ?1 WHERE id = ?2", params![hash, id])?;
            if reset_totp {
                c.execute("UPDATE users SET totp_secret = NULL WHERE id = ?1", [id])?;
            }
            Ok(n)
        })
        .await?;
    if n == 0 {
        return Err(AppError::NotFound);
    }
    if id != auth.user.id {
        auth::delete_sessions_for_user(&state, id).await?;
    }
    audit::log(&state, Some(auth.user.id), &auth.user.username, "", "admin.password_reset", format!("user {id}")).await;
    flash_to(&state, &auth.session, "success", "Password reset. That teacher has been signed out everywhere.", "/admin/users").await
}

pub async fn user_toggle_admin(State(state): State<AppState>, Admin(auth): Admin, Path(id): Path<i64>) -> AppResult<Response> {
    if id == auth.user.id {
        return flash_to(&state, &auth.session, "danger", "You can't remove your own admin rights.", "/admin/users").await;
    }
    state
        .db
        .call(move |c| {
            c.execute("UPDATE users SET is_admin = 1 - is_admin WHERE id = ?1", [id])?;
            Ok(())
        })
        .await?;
    flash_to(&state, &auth.session, "success", "Admin rights updated.", "/admin/users").await
}

pub async fn backups(State(state): State<AppState>, Admin(auth): Admin) -> AppResult<Response> {
    let dir = state.config.backup_dir.clone();
    let list: Vec<_> = dir
        .as_deref()
        .map(backup::list)
        .unwrap_or_default()
        .into_iter()
        .map(|(name, size)| ctx! { name, size => format!("{:.1} MB", size as f64 / 1_048_576.0) })
        .collect();
    render(
        &state,
        &auth,
        "admin_backups.html",
        ctx! { backups => list, dir => dir.map(|d| d.display().to_string()), keep => state.config.backup_keep },
    )
    .await
}

pub async fn backup_now(State(state): State<AppState>, Admin(auth): Admin) -> AppResult<Response> {
    if state.config.backup_dir.is_none() {
        return flash_to(&state, &auth.session, "warning", "Backups are turned off (BACKUP_DIR=off).", "/admin/backups").await;
    }
    backup::nightly(&state).await?;
    flash_to(&state, &auth.session, "success", "Today's backup is in place.", "/admin/backups").await
}

/// The audit log: sign-ins, saves, report downloads, admin actions.
pub async fn audit(State(state): State<AppState>, Admin(auth): Admin) -> AppResult<Response> {
    let entries = state
        .db
        .call(|c| {
            let mut st = c.prepare("SELECT at, username, ip, action, detail FROM audit_log ORDER BY id DESC LIMIT 500")?;
            let rows = st
                .query_map([], |r| {
                    Ok(ctx! {
                        at => r.get::<_, String>(0)?, username => r.get::<_, String>(1)?, ip => r.get::<_, String>(2)?,
                        action => r.get::<_, String>(3)?, detail => r.get::<_, String>(4)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await?;
    render(&state, &auth, "admin_audit.html", ctx! { entries, keep_days => state.config.audit_keep_days }).await
}

/// Stream a fresh consistent snapshot of the whole database.
pub async fn backup_download(State(state): State<AppState>, Admin(auth): Admin) -> AppResult<Response> {
    audit::log(&state, Some(auth.user.id), &auth.user.username, "", "admin.backup_download", "").await;
    let db_path = state.db.path().to_path_buf();
    let bytes = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        let tmp = std::env::temp_dir().join(format!("gradebook-export-{}.db", random_token(8)));
        let res = backup::snapshot(&db_path, &tmp).and_then(|_| Ok(std::fs::read(&tmp)?));
        let _ = std::fs::remove_file(&tmp);
        res
    })
    .await
    .map_err(|e| AppError::Internal(e.into()))??;
    let name = format!("gradebook-{}.db", chrono::Local::now().format("%Y-%m-%d-%H%M"));
    Ok((
        [
            (header::CONTENT_TYPE, "application/vnd.sqlite3".to_string()),
            (header::CONTENT_DISPOSITION, content_disposition("attachment", &name)),
        ],
        bytes,
    )
        .into_response())
}
