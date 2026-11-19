//! Audit log (who did what, never student details) and retention.

use rusqlite::params;

use crate::{AppState, config::Storage, error::AppResult};

/// Record an action. `detail` must not contain student names, marks or comments.
pub async fn log(state: &AppState, user_id: Option<i64>, username: &str, ip: &str, action: &str, detail: impl AsRef<str>) {
    let (username, ip, action, detail) = (username.to_string(), ip.to_string(), action.to_string(), detail.as_ref().to_string());
    let r = state
        .db
        .call(move |c| {
            c.execute(
                "INSERT INTO audit_log (user_id, username, ip, action, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![user_id, username, ip, action, detail],
            )?;
            Ok(())
        })
        .await;
    if let Err(e) = r {
        tracing::warn!("audit log write failed: {e}");
    }
}

/// Drop old audit entries; with saved storage and RETENTION_DAYS, delete terms
/// archived longer ago than that (with all their classes and marks).
pub async fn housekeeping(state: &AppState) -> AppResult<()> {
    let keep = state.config.audit_keep_days;
    let retention = (state.config.storage == Storage::Persistent).then_some(state.config.retention_days).flatten();
    let purged = state
        .db
        .call(move |c| {
            c.execute("DELETE FROM audit_log WHERE at < datetime('now', ?1)", [format!("-{keep} days")])?;
            let mut purged = 0;
            if let Some(days) = retention {
                let tx = c.transaction()?;
                // Students who were only in the purged terms go too.
                let ids: Vec<(i64, i64)> = tx
                    .prepare("SELECT id, user_id FROM terms WHERE archived = 1 AND archived_at IS NOT NULL AND archived_at < datetime('now', ?1)")?
                    .query_map([format!("-{days} days")], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<Result<_, _>>()?;
                for (term, user) in &ids {
                    tx.execute("DELETE FROM terms WHERE id = ?1", [term])?;
                    tx.execute(
                        "DELETE FROM students WHERE user_id = ?1 AND id NOT IN (SELECT student_id FROM enrolments)",
                        [user],
                    )?;
                    tx.execute(
                        "INSERT INTO audit_log (user_id, action, detail) VALUES (?1, 'retention.purge', ?2)",
                        params![user, format!("term {term} deleted {days} days after archiving")],
                    )?;
                    purged += 1;
                }
                tx.commit()?;
            }
            Ok(purged)
        })
        .await?;
    if purged > 0 {
        tracing::info!("retention: deleted {purged} archived term(s)");
    }
    Ok(())
}
