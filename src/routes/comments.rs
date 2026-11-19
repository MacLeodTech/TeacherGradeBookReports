//! Personal comment bank. Placeholders such as {first} and {their} are filled
//! in per student on the student page.

use axum::{
    extract::{Path, State},
    response::Response,
};
use rusqlite::params;

use super::{F, flash_to, render};
use crate::{AppState, auth::Auth, error::AppResult};

pub async fn list(State(state): State<AppState>, auth: Auth) -> AppResult<Response> {
    let uid = auth.user.id;
    let items = auth
        .db
        .call(move |c| {
            let mut st = c.prepare("SELECT id, tag, body FROM comment_bank WHERE user_id = ?1 ORDER BY tag, id")?;
            let rows = st
                .query_map([uid], |r| {
                    Ok(ctx! { id => r.get::<_, i64>(0)?, tag => r.get::<_, String>(1)?, body => r.get::<_, String>(2)? })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await?;
    render(&state, &auth, "comments.html", ctx! { items }).await
}

pub async fn add(State(state): State<AppState>, auth: Auth, f: F) -> AppResult<Response> {
    let uid = auth.user.id;
    let tag: String = f.get("tag").chars().take(60).collect();
    // One comment per line when pasting several at once.
    let bodies: Vec<String> = f.raw("body").lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    if bodies.is_empty() {
        return flash_to(&state, &auth.session, "warning", "Write a comment first.", "/comments").await;
    }
    if bodies.iter().any(|b| b.len() > 2000) || bodies.len() > 200 {
        return flash_to(&state, &auth.session, "danger", "That's too much at once (max 200 comments of 2000 characters).", "/comments")
            .await;
    }
    let n = bodies.len();
    auth.db
        .call(move |c| {
            let tx = c.transaction()?;
            for b in bodies {
                tx.execute("INSERT INTO comment_bank (user_id, tag, body) VALUES (?1, ?2, ?3)", params![uid, tag, b])?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    flash_to(&state, &auth.session, "success", format!("Added {n} comment(s)."), "/comments").await
}

pub async fn delete(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> AppResult<Response> {
    let uid = auth.user.id;
    auth.db
        .call(move |c| {
            c.execute("DELETE FROM comment_bank WHERE id = ?1 AND user_id = ?2", params![id, uid])?;
            Ok(())
        })
        .await?;
    flash_to(&state, &auth.session, "success", "Comment removed.", "/comments").await
}
