//! Writing marks. Every change is logged in `mark_history` so it can be undone
//! and so reports know what changed since they were last generated.

use rusqlite::{OptionalExtension, Transaction, params};

use crate::{
    engine::{MarkEntry, Status},
    error::AppResult,
    util::random_token,
};

pub struct Change {
    pub assessment_id: i64,
    pub student_id: i64,
    /// `None` clears the mark.
    pub new: Option<MarkEntry>,
}

fn current(tx: &Transaction, aid: i64, sid: i64) -> rusqlite::Result<Option<MarkEntry>> {
    tx.query_row("SELECT value, status FROM marks WHERE assessment_id = ?1 AND student_id = ?2", params![aid, sid], |r| {
        Ok(MarkEntry { value: r.get(0)?, status: Status::parse(&r.get::<_, String>(1)?).unwrap_or(Status::Marked) })
    })
    .optional()
}

fn write(tx: &Transaction, aid: i64, sid: i64, m: Option<MarkEntry>) -> rusqlite::Result<()> {
    match m {
        Some(m) => tx.execute(
            "INSERT INTO marks (assessment_id, student_id, value, status, updated_at) VALUES (?1, ?2, ?3, ?4, datetime('now'))
             ON CONFLICT(assessment_id, student_id) DO UPDATE SET value = excluded.value, status = excluded.status, updated_at = excluded.updated_at",
            params![aid, sid, m.value, m.status.as_str()],
        )?,
        None => tx.execute("DELETE FROM marks WHERE assessment_id = ?1 AND student_id = ?2", params![aid, sid])?,
    };
    Ok(())
}

/// Apply changes; unchanged cells are skipped. Returns how many changed.
/// Callers must have checked that every assessment and student belongs to `class_id`.
pub fn apply(tx: &Transaction, class_id: i64, changes: &[Change], source: &str) -> AppResult<usize> {
    let batch = random_token(9);
    let mut n = 0;
    for ch in changes {
        let old = current(tx, ch.assessment_id, ch.student_id)?;
        if old == ch.new {
            continue;
        }
        write(tx, ch.assessment_id, ch.student_id, ch.new)?;
        tx.execute(
            "INSERT INTO mark_history (batch, class_id, assessment_id, student_id, old_value, old_status, new_value, new_status, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                batch,
                class_id,
                ch.assessment_id,
                ch.student_id,
                old.and_then(|m| m.value),
                old.map(|m| m.status.as_str()),
                ch.new.and_then(|m| m.value),
                ch.new.map(|m| m.status.as_str()),
                source
            ],
        )?;
        n += 1;
    }
    Ok(n)
}

/// Undo the most recent change batch in a class. Returns the number of cells
/// restored (0 if there's nothing to undo).
pub fn undo_last(tx: &Transaction, class_id: i64) -> AppResult<usize> {
    let batch: Option<String> = tx
        .query_row(
            "SELECT batch FROM mark_history WHERE class_id = ?1 AND undone = 0 AND source != 'undo' ORDER BY id DESC LIMIT 1",
            [class_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(batch) = batch else { return Ok(0) };
    let rows: Vec<(i64, i64, Option<f64>, Option<String>)> = {
        let mut st =
            tx.prepare("SELECT assessment_id, student_id, old_value, old_status FROM mark_history WHERE batch = ?1 ORDER BY id DESC")?;
        st.query_map([&batch], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<_, _>>()?
    };
    let changes: Vec<Change> = rows
        .into_iter()
        .map(|(aid, sid, value, status)| Change {
            assessment_id: aid,
            student_id: sid,
            new: status.and_then(|s| Status::parse(&s)).map(|status| MarkEntry { value, status }),
        })
        .collect();
    let n = changes.len();
    apply(tx, class_id, &changes, "undo")?;
    tx.execute("UPDATE mark_history SET undone = 1 WHERE batch = ?1", [&batch])?;
    Ok(n)
}

/// Description of what "Undo" would revert, for the button tooltip.
pub fn undo_preview(conn: &rusqlite::Connection, class_id: i64) -> AppResult<Option<(String, i64)>> {
    Ok(conn
        .query_row(
            "SELECT source, count(*) FROM mark_history WHERE batch = (
                SELECT batch FROM mark_history WHERE class_id = ?1 AND undone = 0 AND source != 'undo' ORDER BY id DESC LIMIT 1)",
            [class_id],
            |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?
        .and_then(|(s, n)| s.map(|s| (s, n))))
}
