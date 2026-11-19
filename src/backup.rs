//! Database backups. Once the gradebook is the system of record, losing the
//! database means losing a term of marks.

use std::path::{Path, PathBuf};

use crate::{AppState, error::AppResult};

/// Write a consistent copy of the live database to `dest` (`VACUUM INTO`).
pub fn snapshot(db_path: &Path, dest: &Path) -> anyhow::Result<()> {
    if dest.exists() {
        std::fs::remove_file(dest)?;
    }
    let conn = crate::db::open(db_path)?;
    conn.execute("VACUUM INTO ?1", [dest.to_string_lossy()])?;
    Ok(())
}

/// Take today's backup if there isn't one yet, and prune old ones.
pub async fn nightly(state: &AppState) -> AppResult<()> {
    let Some(dir) = state.config.backup_dir.clone() else { return Ok(()) };
    let db_path = state.db.path().to_path_buf();
    let keep = state.config.backup_keep;
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        std::fs::create_dir_all(&dir)?;
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let dest = dir.join(format!("gradebook-{today}.db"));
        if !dest.exists() {
            let tmp = dir.join(format!(".gradebook-{today}.db.tmp"));
            snapshot(&db_path, &tmp)?;
            std::fs::rename(&tmp, &dest)?;
            tracing::info!("backup written to {}", dest.display());
        }
        prune(&dir, keep)?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!(e))??;
    Ok(())
}

fn prune(dir: &Path, keep: usize) -> anyhow::Result<()> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("gradebook-") && n.ends_with(".db")))
        .collect();
    files.sort();
    let excess = files.len().saturating_sub(keep);
    for f in files.into_iter().take(excess) {
        std::fs::remove_file(&f)?;
    }
    Ok(())
}

/// List existing backups (name, size in bytes), newest first.
pub fn list(dir: &Path) -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            (name.starts_with("gradebook-") && name.ends_with(".db")).then(|| (name, e.metadata().map(|m| m.len()).unwrap_or(0)))
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_and_prune() {
        let dir = std::env::temp_dir().join(format!("gb-backup-test-{}", crate::util::random_token(6)));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("live.db");
        crate::db::Db::new(db.clone()).migrate().unwrap();
        for d in ["2026-01-01", "2026-01-02", "2026-01-03"] {
            snapshot(&db, &dir.join(format!("gradebook-{d}.db"))).unwrap();
        }
        prune(&dir, 2).unwrap();
        let names: Vec<String> = list(&dir).into_iter().map(|x| x.0).collect();
        assert_eq!(names, ["gradebook-2026-01-03.db", "gradebook-2026-01-02.db"]);
        let copy = rusqlite::Connection::open(dir.join("gradebook-2026-01-03.db")).unwrap();
        let v: i64 = copy.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v as usize, crate::db::MIGRATIONS.len());
        std::fs::remove_dir_all(&dir).ok();
    }
}
