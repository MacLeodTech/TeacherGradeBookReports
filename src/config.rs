use std::path::PathBuf;

/// Where student data (classes, students, marks, comments) lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Storage {
    /// Only in memory while the teacher is signed in. The teacher keeps their
    /// workbook file; nothing about students is written to the server's disk.
    Memory,
    /// In the database file, like a normal web app (for a board that is the
    /// data custodian, or a teacher's own computer).
    Persistent,
}

/// How people sign in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    /// Accounts with passwords (and optional TOTP), managed by an admin.
    Password,
    /// Single sign-on with the board's identity provider (Microsoft Entra ID,
    /// Google Workspace or any OpenID Connect provider).
    Oidc,
    /// Local mode: one teacher on their own computer. Only reachable from that
    /// computer, through the link the app opens.
    Local,
}

#[derive(Clone, Debug)]
pub struct Oidc {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    /// Public base URL of this app, e.g. https://gradebook.example-board.ca
    pub public_url: String,
    /// Button text, e.g. "Sign in with Microsoft".
    pub display_name: String,
    /// If set, only these email domains may sign in (lower case).
    pub allowed_domains: Vec<String>,
    /// Accounts with these emails become admins (lower case).
    pub admin_emails: Vec<String>,
    /// Keep the password form too (for a break-glass admin account).
    pub allow_password_login: bool,
}

/// Runtime configuration from environment variables.
#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    pub db_path: PathBuf,
    pub storage: Storage,
    pub auth: AuthMode,
    pub oidc: Option<Oidc>,
    /// The old Python app's database, imported once on first start.
    pub legacy_db_path: Option<PathBuf>,
    pub backup_dir: Option<PathBuf>,
    pub backup_keep: usize,
    pub cookie_secure: bool,
    pub trust_proxy: bool,
    pub idle_timeout_secs: i64,
    pub admin_username: Option<String>,
    pub admin_password: Option<String>,
    pub admin_school: Option<String>,
    /// Saved storage: delete archived terms this many days after archiving.
    pub retention_days: Option<i64>,
    /// Keep audit log entries this many days.
    pub audit_keep_days: i64,
    /// Memory storage: cap on one teacher's workspace, in MiB.
    pub workspace_max_mb: i64,
    /// Local mode: secret in the link the app opens in the browser.
    pub launch_token: Option<String>,
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn flag(name: &str, default: bool) -> bool {
    var(name).map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")).unwrap_or(default)
}

fn list(name: &str) -> Vec<String> {
    var(name)
        .map(|v| v.split([',', ' ', ';']).map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

pub fn parse_storage(v: Option<&str>) -> anyhow::Result<Storage> {
    match v.map(str::to_ascii_lowercase).as_deref() {
        None | Some("memory") => Ok(Storage::Memory),
        Some("persistent") | Some("saved") | Some("database") => Ok(Storage::Persistent),
        Some(other) => anyhow::bail!("STORAGE must be 'memory' or 'persistent', not '{other}'"),
    }
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let db_path = PathBuf::from(var("DB_PATH").unwrap_or_else(|| "/data/gradebook.db".into()));
        let data_dir = db_path.parent().map(PathBuf::from).unwrap_or_default();
        let backup_dir = match var("BACKUP_DIR").as_deref() {
            Some("off") | Some("none") => None,
            Some(p) => Some(PathBuf::from(p)),
            None => Some(data_dir.join("backups")),
        };
        let auth = match var("AUTH").map(|v| v.to_ascii_lowercase()).as_deref() {
            None | Some("password") => AuthMode::Password,
            Some("oidc") | Some("sso") => AuthMode::Oidc,
            Some(other) => anyhow::bail!("AUTH must be 'password' or 'oidc', not '{other}' (local mode is `gradebook local`)"),
        };
        let oidc = if auth == AuthMode::Oidc {
            let need = |k: &str| var(k).ok_or_else(|| anyhow::anyhow!("AUTH=oidc needs {k}"));
            Some(Oidc {
                issuer: need("OIDC_ISSUER")?,
                client_id: need("OIDC_CLIENT_ID")?,
                client_secret: var("OIDC_CLIENT_SECRET"),
                public_url: need("PUBLIC_URL")?.trim_end_matches('/').to_string(),
                display_name: var("OIDC_DISPLAY_NAME").unwrap_or_else(|| "your school board".into()),
                allowed_domains: list("OIDC_ALLOWED_DOMAINS"),
                admin_emails: list("OIDC_ADMIN_EMAILS"),
                allow_password_login: flag("ALLOW_PASSWORD_LOGIN", false),
            })
        } else {
            None
        };
        Ok(Config {
            bind: var("BIND").unwrap_or_else(|| "0.0.0.0:8000".into()),
            legacy_db_path: Some(PathBuf::from(var("LEGACY_DB_PATH").unwrap_or_else(|| data_dir.join("users.db").display().to_string()))),
            db_path,
            storage: parse_storage(var("STORAGE").as_deref())?,
            auth,
            oidc,
            backup_dir,
            backup_keep: var("BACKUP_KEEP").and_then(|v| v.parse().ok()).unwrap_or(14),
            cookie_secure: flag("COOKIE_SECURE", true),
            trust_proxy: flag("TRUST_PROXY", false),
            idle_timeout_secs: var("IDLE_TIMEOUT_MINUTES").and_then(|v| v.parse::<i64>().ok()).unwrap_or(120).max(5) * 60,
            admin_username: var("ADMIN_USERNAME"),
            admin_password: var("ADMIN_PASSWORD"),
            admin_school: var("ADMIN_SCHOOL"),
            retention_days: var("RETENTION_DAYS").and_then(|v| v.parse().ok()).filter(|d: &i64| *d > 0),
            audit_keep_days: var("AUDIT_KEEP_DAYS").and_then(|v| v.parse().ok()).unwrap_or(365).max(1),
            workspace_max_mb: var("WORKSPACE_MAX_MB").and_then(|v| v.parse().ok()).unwrap_or(256).max(16),
            launch_token: None,
        })
    }

    /// `gradebook local`: one teacher on their own computer.
    pub fn local(data_dir: PathBuf, port: u16, storage: Storage) -> Self {
        Config {
            bind: format!("127.0.0.1:{port}"),
            db_path: data_dir.join("gradebook.db"),
            storage,
            auth: AuthMode::Local,
            oidc: None,
            legacy_db_path: None,
            backup_dir: (storage == Storage::Persistent).then(|| data_dir.join("backups")),
            backup_keep: 14,
            cookie_secure: false,
            trust_proxy: false,
            // The workspace lives as long as the app is open; the session just
            // needs to outlast a working day.
            idle_timeout_secs: 12 * 3600,
            admin_username: None,
            admin_password: None,
            admin_school: None,
            retention_days: None,
            audit_keep_days: 365,
            workspace_max_mb: 512,
            launch_token: Some(crate::util::random_token(24)),
        }
    }

    pub fn password_login(&self) -> bool {
        match self.auth {
            AuthMode::Password => true,
            AuthMode::Oidc => self.oidc.as_ref().is_some_and(|o| o.allow_password_login),
            AuthMode::Local => false,
        }
    }

    #[cfg(test)]
    pub fn for_tests(db_path: PathBuf) -> Self {
        Config {
            bind: "127.0.0.1:0".into(),
            db_path,
            storage: Storage::Persistent,
            auth: AuthMode::Password,
            oidc: None,
            legacy_db_path: None,
            backup_dir: None,
            backup_keep: 3,
            cookie_secure: false,
            trust_proxy: false,
            idle_timeout_secs: 3600,
            admin_username: Some("admin".into()),
            admin_password: Some("Password1234".into()),
            admin_school: Some("Test School".into()),
            retention_days: None,
            audit_keep_days: 365,
            workspace_max_mb: 64,
            launch_token: None,
        }
    }
}
