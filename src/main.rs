//! Gradebook: a self-hosted teacher gradebook with PDF report generation.

mod audit;
mod auth;
mod backup;
mod color;
mod config;
mod db;
mod engine;
mod error;
mod export;
mod import;
mod local;
mod marks;
mod model;
mod oidc;
mod report;
mod routes;
mod starter;
mod static_files;
mod templates;
#[cfg(test)]
mod tests;
mod util;
mod workbook;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use auth::RateLimiter;
use config::{AuthMode, Config, Storage};
use db::{Db, Workspaces};
use error::AppResult;

#[derive(Clone)]
pub struct AppState {
    /// The server's database file (accounts, settings, sessions, audit log;
    /// and student data too when storage is persistent).
    pub db: Db,
    pub workspaces: Arc<Workspaces>,
    pub config: Arc<Config>,
    pub templates: Arc<minijinja::Environment<'static>>,
    pub login_limiter: Arc<RateLimiter>,
    pub oidc: Option<Arc<oidc::Provider>>,
    pub shutdown: Arc<tokio::sync::Notify>,
}

impl AppState {
    /// Where a signed-in teacher's student data lives.
    pub fn user_db(&self, user_id: i64) -> AppResult<Db> {
        match self.config.storage {
            Storage::Persistent => Ok(self.db.clone()),
            Storage::Memory => self.workspaces.get(user_id, util::default_term_name),
        }
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    match args.first().map(String::as_str) {
        // Used by the container HEALTHCHECK (no curl in the image).
        Some("healthcheck") => runtime.block_on(healthcheck()),
        Some("local") => runtime.block_on(local::run(&args[1..])),
        Some("serve") => runtime.block_on(serve_from_env()),
        Some("--help" | "-h" | "help") => {
            println!("{}", local::USAGE);
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown command '{other}'\n\n{}", local::USAGE),
        // Double-clicked on a teacher's Windows or Mac computer: local mode.
        None if cfg!(any(windows, target_os = "macos")) && std::env::var_os("DB_PATH").is_none() => runtime.block_on(local::run(&[])),
        None => runtime.block_on(serve_from_env()),
    }
}

fn init_logging(default: &str) {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| default.into()))
        .init();
}

async fn serve_from_env() -> anyhow::Result<()> {
    init_logging("gradebook=info,tower_http=warn");
    let config = Config::from_env()?;
    let state = build_state(config).await?;
    bootstrap(&state)?;
    let addr: SocketAddr = state.config.bind.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("listening on http://{addr} (storage: {:?}, sign-in: {:?})", state.config.storage, state.config.auth);
    serve(state, listener).await
}

/// Run the app on `listener` until Ctrl-C, SIGTERM or a local-mode quit.
pub async fn serve(state: AppState, listener: tokio::net::TcpListener) -> anyhow::Result<()> {
    spawn_housekeeping(state.clone());
    let shutdown = state.shutdown.clone();
    let app = routes::router(state);
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown_signal(shutdown))
        .await?;
    Ok(())
}

/// Every minute: forget idle memory workspaces. Hourly: prune sessions and
/// the audit log, apply retention, take the nightly backup.
fn spawn_housekeeping(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        let mut n: u64 = 0;
        loop {
            tick.tick().await;
            if state.config.storage == Storage::Memory && state.config.auth != AuthMode::Local {
                let gone = state.workspaces.discard_idle(state.config.idle_timeout_secs);
                if gone > 0 {
                    tracing::info!("discarded {gone} idle workspace(s)");
                }
            }
            if n.is_multiple_of(60) {
                if let Err(e) = auth::prune_sessions(&state).await {
                    tracing::warn!("session pruning failed: {e}");
                }
                if let Err(e) = audit::housekeeping(&state).await {
                    tracing::warn!("retention/audit housekeeping failed: {e}");
                }
                if let Err(e) = backup::nightly(&state).await {
                    tracing::warn!("backup failed: {e}");
                }
            }
            n += 1;
        }
    });
}

pub async fn build_state(config: Config) -> anyhow::Result<AppState> {
    let db = match config.storage {
        Storage::Persistent => Db::new(config.db_path.clone()),
        Storage::Memory => Db::guarded(config.db_path.clone()),
    };
    db.migrate()?;
    if config.storage == Storage::Memory {
        let n = db.stored_student_rows()?;
        if n > 0 {
            tracing::warn!(
                "storage is 'memory', but {} still holds {n} rows of student data saved earlier in 'persistent' mode. \
                 They are not shown in memory mode; switch back to export them as workbooks, then purge those terms.",
                config.db_path.display()
            );
        }
    }
    let oidc = match &config.oidc {
        Some(o) => Some(Arc::new(oidc::Provider::discover(o).await?)),
        None => None,
    };
    Ok(AppState {
        db,
        workspaces: Arc::new(Workspaces::new(config.db_path.clone(), config.workspace_max_mb)),
        config: Arc::new(config),
        templates: Arc::new(templates::environment()),
        login_limiter: Arc::new(RateLimiter::new(10, Duration::from_secs(60))),
        oidc,
        shutdown: Arc::new(tokio::sync::Notify::new()),
    })
}

/// Stop on Ctrl-C, SIGTERM (`docker stop`) or a local-mode quit, letting
/// requests in flight finish.
async fn shutdown_signal(quit: Arc<tokio::sync::Notify>) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {}, _ = quit.notified() => {} }
    tracing::info!("shutting down");
}

async fn healthcheck() -> anyhow::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let bind = std::env::var("BIND").unwrap_or_else(|_| "0.0.0.0:8000".into());
    let port = bind.rsplit(':').next().unwrap_or("8000");
    let mut stream = tokio::time::timeout(Duration::from_secs(3), tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))).await??;
    stream.write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\n\r\n").await?;
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut buf)).await??;
    if buf.starts_with(b"HTTP/1.0 200") || buf.starts_with(b"HTTP/1.1 200") {
        Ok(())
    } else {
        anyhow::bail!("unhealthy: {}", String::from_utf8_lossy(&buf[..buf.len().min(80)]))
    }
}

/// Import accounts from the old app, or create the first admin (or, in local
/// mode, the one local teacher).
fn bootstrap(state: &AppState) -> anyhow::Result<()> {
    let conn = db::open(state.db.path())?;
    if state.config.auth == AuthMode::Local {
        conn.execute("INSERT OR IGNORE INTO users (username, password_hash) VALUES (?1, '!local')", [local::LOCAL_USER])?;
        return Ok(());
    }
    if let Some(legacy) = &state.config.legacy_db_path {
        let n = state.db.import_legacy_users(legacy)?;
        if n > 0 {
            tracing::info!("imported {n} user account(s) from {}", legacy.display());
        }
    }
    let users: i64 = conn.query_row("SELECT count(*) FROM users", [], |r| r.get(0))?;
    if users == 0 {
        let (Some(username), Some(password)) = (&state.config.admin_username, &state.config.admin_password) else {
            if state.config.auth == AuthMode::Oidc {
                // Accounts are created at first sign-in; admins come from OIDC_ADMIN_EMAILS.
                return Ok(());
            }
            anyhow::bail!("No user accounts exist yet. Set ADMIN_USERNAME and ADMIN_PASSWORD to create the first admin account.");
        };
        auth::validate_username(username).map_err(anyhow::Error::msg)?;
        auth::validate_password(password).map_err(|e| anyhow::anyhow!("ADMIN_PASSWORD: {e}"))?;
        conn.execute(
            "INSERT INTO users (username, password_hash, school, is_admin) VALUES (?1, ?2, ?3, 1)",
            rusqlite::params![username, auth::hash_password(password), state.config.admin_school.clone().unwrap_or_default()],
        )?;
        tracing::info!("created admin account '{username}'");
    }
    Ok(())
}
