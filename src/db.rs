//! SQLite access.
//!
//! The server's database file holds accounts, sessions, teacher settings, the
//! comment bank and the audit log. Student data (terms, classes, students,
//! marks, comments…) lives either in the same file (`STORAGE=persistent`) or
//! in a per-teacher in-memory database (`STORAGE=memory`, the default).
//!
//! A memory workspace is an in-memory SQLite database with the server file
//! attached as `sys`. SQLite looks up unqualified table names in `main`
//! (the workspace) first and then in `sys`, so every query in the app runs
//! unchanged in both modes. Connections to the file in memory mode get an
//! authorizer that refuses to write student tables, so a query routed to the
//! wrong database fails loudly instead of quietly saving student data to disk.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, AtomicU64, Ordering},
    },
};

use anyhow::Context;
use rusqlite::{
    Connection, OptionalExtension,
    hooks::{AuthAction, AuthContext, Authorization},
    params,
};

use crate::{error::AppError, util::now_unix};

pub const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/001_init.sql"),
    include_str!("../migrations/002_board.sql"),
    include_str!("../migrations/003_staging_kind.sql"),
    include_str!("../migrations/004_levels.sql"),
];

/// Tables that hold student data. In memory mode these only ever exist in a
/// teacher's in-memory workspace.
pub const WORKSPACE_TABLES: &[&str] = &[
    "terms",
    "classes",
    "subjects",
    "categories",
    "assessments",
    "students",
    "enrolments",
    "marks",
    "mark_history",
    "grade_overrides",
    "report_comments",
    "learning_skills",
    "report_runs",
    "import_staging",
];

/// Tables that stay in the server's file.
const SYSTEM_TABLES: &[&str] = &["sessions", "teacher_settings", "comment_bank", "audit_log", "users"];

pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         PRAGMA synchronous = NORMAL;",
    )?;
    Ok(conn)
}

fn is_workspace_table(t: &str) -> bool {
    WORKSPACE_TABLES.iter().any(|w| w.eq_ignore_ascii_case(t))
}

/// Refuse writes to student tables in the server file, which is `main` on a
/// plain file connection and `sys` inside a workspace.
fn guard(ctx: AuthContext<'_>, file_db: &str) -> Authorization {
    let in_file = match ctx.database_name {
        Some(d) => d == file_db,
        None => file_db == "main",
    };
    match ctx.action {
        AuthAction::Insert { table_name } | AuthAction::Update { table_name, .. } if in_file && is_workspace_table(table_name) => {
            Authorization::Deny
        }
        _ => Authorization::Allow,
    }
}

#[derive(Clone)]
pub struct Db {
    inner: Inner,
}

#[derive(Clone)]
enum Inner {
    /// The server's database file. `guard`: refuse writes to student tables.
    File { path: PathBuf, guard: bool },
    /// One teacher's in-memory workspace.
    Workspace(Arc<Workspace>),
}

impl Db {
    pub fn new(path: PathBuf) -> Self {
        Db { inner: Inner::File { path, guard: false } }
    }

    /// The server file, refusing student-data writes (memory storage).
    pub fn guarded(path: PathBuf) -> Self {
        Db { inner: Inner::File { path, guard: true } }
    }

    pub fn path(&self) -> &Path {
        match &self.inner {
            Inner::File { path, .. } => path,
            Inner::Workspace(ws) => &ws.sys_path,
        }
    }

    pub fn workspace(&self) -> Option<&Arc<Workspace>> {
        match &self.inner {
            Inner::Workspace(ws) => Some(ws),
            Inner::File { .. } => None,
        }
    }

    /// Run `f` with a connection on the blocking pool.
    pub async fn call<T, F>(&self, f: F) -> Result<T, AppError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, AppError> + Send + 'static,
    {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || match inner {
            Inner::File { path, guard: g } => {
                let mut conn = open(&path).map_err(AppError::from)?;
                if g {
                    conn.authorizer(Some(|ctx: AuthContext<'_>| guard(ctx, "main"))).map_err(AppError::from)?;
                }
                f(&mut conn)
            }
            Inner::Workspace(ws) => {
                ws.touch();
                let mut conn = ws.conn.lock().unwrap_or_else(|e| e.into_inner());
                f(&mut conn)
            }
        })
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("db task panicked: {e}")))?
    }

    /// Create the database if needed and apply pending migrations.
    pub fn migrate(&self) -> anyhow::Result<()> {
        let path = self.path().to_path_buf();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let mut conn = open(&path).with_context(|| format!("opening {}", path.display()))?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            let tx = conn.transaction()?;
            tx.execute_batch(sql).with_context(|| format!("migration {}", i + 1))?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
            tracing::info!("applied database migration {}", i + 1);
        }
        Ok(())
    }

    /// Rows of student data in the server file (memory mode warns about these).
    pub fn stored_student_rows(&self) -> anyhow::Result<i64> {
        let conn = open(self.path())?;
        let mut n = 0;
        for t in ["students", "marks", "report_comments"] {
            n += conn.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get::<_, i64>(0))?;
        }
        Ok(n)
    }

    /// Copy accounts from the old Python app's `users.db` when this database
    /// has no users yet. Old werkzeug password hashes keep working and are
    /// upgraded to argon2 on the next login.
    pub fn import_legacy_users(&self, legacy: &Path) -> anyhow::Result<usize> {
        if !legacy.exists() || legacy == self.path() {
            return Ok(0);
        }
        let conn = open(self.path())?;
        let have: i64 = conn.query_row("SELECT count(*) FROM users", [], |r| r.get(0))?;
        if have > 0 {
            return Ok(0);
        }
        let old = Connection::open_with_flags(legacy, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let has_table: Option<String> =
            old.query_row("SELECT name FROM sqlite_master WHERE type='table' AND name='users'", [], |r| r.get(0)).optional()?;
        if has_table.is_none() {
            return Ok(0);
        }
        let cols: Vec<String> =
            old.prepare("SELECT name FROM pragma_table_info('users')")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        let pick = |c: &str| if cols.iter().any(|x| x == c) { c.to_string() } else { "''".to_string() };
        let sql =
            format!("SELECT username, password_hash, {}, {}, {}, is_admin FROM users", pick("school"), pick("full_name"), pick("email"));
        let mut stmt = old.prepare(&sql)?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?;
        let mut n = 0;
        for row in rows {
            let (username, hash, school, full_name, email, is_admin) = row?;
            conn.execute(
                "INSERT INTO users (username, password_hash, school, full_name, email, is_admin)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![username, hash, school, full_name, email, is_admin],
            )?;
            n += 1;
        }
        Ok(n)
    }
}

// ── Memory workspaces ────────────────────────────────────────────────────────

/// One teacher's student data, held only in memory.
pub struct Workspace {
    conn: Mutex<Connection>,
    sys_path: PathBuf,
    pub user_id: i64,
    last_used: AtomicI64,
    /// Rows changed since the workspace was created (student tables only).
    changes: Arc<AtomicU64>,
    /// `changes` when the teacher last saved or opened a workbook.
    saved_changes: AtomicU64,
    /// Unix time of the last save, 0 = never.
    saved_at: AtomicI64,
}

impl Workspace {
    fn create(sys_path: &Path, user_id: i64, max_mb: i64) -> anyhow::Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // The student tables, without the foreign keys to `users` (that table
        // stays in the server file, and SQLite can't enforce keys across
        // databases). Everything else, cascades included, works as usual.
        let schema = MIGRATIONS.join("\n").replace("REFERENCES users(id) ON DELETE CASCADE", "");
        conn.execute_batch(&schema).context("creating workspace schema")?;
        for t in SYSTEM_TABLES {
            conn.execute_batch(&format!("DROP TABLE {t};"))?;
        }
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA temp_store = MEMORY;")?;
        conn.pragma_update(None, "max_page_count", max_mb * 1024 * 1024 / 4096)?;
        conn.execute("ATTACH DATABASE ?1 AS sys", [sys_path.to_string_lossy()])?;
        conn.execute_batch("PRAGMA sys.synchronous = NORMAL;")?;
        // Even here, never write student tables into the file.
        conn.authorizer(Some(|ctx: AuthContext<'_>| guard(ctx, "sys")))?;
        let changes = Arc::new(AtomicU64::new(0));
        {
            let changes = changes.clone();
            conn.update_hook(Some(move |_, db: &str, table: &str, _| {
                if db == "main" && table != "import_staging" {
                    changes.fetch_add(1, Ordering::Relaxed);
                }
            }))?;
        }
        Ok(Workspace {
            conn: Mutex::new(conn),
            sys_path: sys_path.to_path_buf(),
            user_id,
            last_used: AtomicI64::new(now_unix()),
            changes,
            saved_changes: AtomicU64::new(0),
            saved_at: AtomicI64::new(0),
        })
    }

    fn touch(&self) {
        self.last_used.store(now_unix(), Ordering::Relaxed);
    }

    /// Changes not yet saved to a workbook.
    pub fn unsaved(&self) -> bool {
        self.changes.load(Ordering::Relaxed) != self.saved_changes.load(Ordering::Relaxed)
    }

    /// Mark the current state as saved (after a download or opening a file).
    pub fn mark_saved(&self, downloaded: bool) {
        self.saved_changes.store(self.changes.load(Ordering::Relaxed), Ordering::Relaxed);
        if downloaded {
            self.saved_at.store(now_unix(), Ordering::Relaxed);
        }
    }

    pub fn saved_at(&self) -> Option<i64> {
        Some(self.saved_at.load(Ordering::Relaxed)).filter(|t| *t > 0)
    }
}

/// All open memory workspaces, one per signed-in teacher.
pub struct Workspaces {
    sys_path: PathBuf,
    max_mb: i64,
    open: Mutex<HashMap<i64, Arc<Workspace>>>,
}

impl Workspaces {
    pub fn new(sys_path: PathBuf, max_mb: i64) -> Self {
        Workspaces { sys_path, max_mb, open: Mutex::new(HashMap::new()) }
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<i64, Arc<Workspace>>> {
        self.open.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The teacher's workspace, created empty (with one current term) if needed.
    pub fn get(&self, user_id: i64, term_name: impl FnOnce() -> String) -> Result<Db, AppError> {
        if let Some(ws) = self.map().get(&user_id) {
            return Ok(Db { inner: Inner::Workspace(ws.clone()) });
        }
        let ws = Workspace::create(&self.sys_path, user_id, self.max_mb).map_err(AppError::Internal)?;
        {
            let conn = ws.conn.lock().unwrap_or_else(|e| e.into_inner());
            conn.execute("INSERT INTO terms (user_id, name, is_current) VALUES (?1, ?2, 1)", params![user_id, term_name()])?;
        }
        ws.mark_saved(false);
        let ws = Arc::new(ws);
        // Another request may have raced us; keep whichever got there first.
        let ws = self.map().entry(user_id).or_insert(ws).clone();
        Ok(Db { inner: Inner::Workspace(ws) })
    }

    pub fn peek(&self, user_id: i64) -> Option<Arc<Workspace>> {
        self.map().get(&user_id).cloned()
    }

    /// Forget a teacher's workspace (sign-out, account deleted, timeout).
    pub fn discard(&self, user_id: i64) -> bool {
        self.map().remove(&user_id).is_some()
    }

    /// Discard workspaces nobody has used for `idle_secs`. Returns how many.
    pub fn discard_idle(&self, idle_secs: i64) -> usize {
        let cutoff = now_unix() - idle_secs;
        let mut map = self.map();
        let before = map.len();
        map.retain(|_, ws| ws.last_used.load(Ordering::Relaxed) >= cutoff);
        before - map.len()
    }

    pub fn len(&self) -> usize {
        self.map().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gb-db-{}", crate::util::random_token(6)));
        let path = dir.join("gradebook.db");
        Db::new(path.clone()).migrate().unwrap();
        open(&path).unwrap().execute("INSERT INTO users (username, password_hash) VALUES ('t', 'x')", []).unwrap();
        path
    }

    #[tokio::test]
    async fn workspace_keeps_students_out_of_the_file() {
        let path = temp_db();
        let wss = Workspaces::new(path.clone(), 32);
        let db = wss.get(1, || "Term".into()).unwrap();
        let ws = db.workspace().unwrap().clone();
        assert!(!ws.unsaved());
        db.call(|c| {
            // Settings go to the file; student rows to memory.
            c.execute("INSERT INTO teacher_settings (user_id, board_name) VALUES (1, 'Board')", [])?;
            let term: i64 = c.query_row("SELECT id FROM terms", [], |r| r.get(0))?;
            c.execute("INSERT INTO classes (user_id, term_id, name) VALUES (1, ?1, 'Bio')", [term])?;
            c.execute("INSERT INTO students (user_id, first_name) VALUES (1, 'Ana')", [])?;
            // Cascades still work inside the workspace.
            c.execute("DELETE FROM terms", [])?;
            assert_eq!(c.query_row("SELECT count(*) FROM classes", [], |r| r.get::<_, i64>(0))?, 0);
            Ok(())
        })
        .await
        .unwrap();
        assert!(ws.unsaved());
        let file = open(&path).unwrap();
        let count = |t: &str| file.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(count("students"), 0);
        assert_eq!(count("terms"), 0);
        assert_eq!(count("teacher_settings"), 1);

        // Explicitly writing student tables in the file is refused.
        let err = db.call(|c| Ok(c.execute("INSERT INTO sys.students (user_id, first_name) VALUES (1, 'X')", [])?)).await;
        assert!(err.is_err());
        let guarded = Db::guarded(path.clone());
        let err = guarded.call(|c| Ok(c.execute("INSERT INTO students (user_id, first_name) VALUES (1, 'X')", [])?)).await;
        assert!(err.is_err());
        // …but sessions and settings are fine.
        guarded.call(|c| Ok(c.execute("UPDATE teacher_settings SET board_name = 'B2'", [])?)).await.unwrap();

        assert!(wss.discard(1));
        assert_eq!(wss.len(), 0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn workspace_size_is_capped() {
        let path = temp_db();
        let wss = Workspaces::new(path.clone(), 16);
        let db = wss.get(1, || "Term".into()).unwrap();
        let ws = db.workspace().unwrap();
        let conn = ws.conn.lock().unwrap();
        let big = "x".repeat(1024 * 1024);
        let mut err = None;
        for _ in 0..40 {
            if let Err(e) = conn.execute("INSERT INTO students (user_id, first_name) VALUES (1, ?1)", [&big]) {
                err = Some(e);
                break;
            }
        }
        assert!(err.is_some(), "a 16 MB workspace accepted 40 MB");
        drop(conn);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
