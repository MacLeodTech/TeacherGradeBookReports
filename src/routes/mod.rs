//! HTTP routes. Pages are server-rendered with minijinja; the grade grid talks
//! to a small JSON API under `/api`.

/// Like `minijinja::context!`, but any `Serialize` value can be passed.
macro_rules! ctx {
    ($($key:ident $(=> $value:expr)?),* $(,)?) => {
        minijinja::context! { $($key => minijinja::value::Serde(&(ctx!(@v $key $(=> $value)?)))),* }
    };
    (@v $key:ident) => { $key };
    (@v $key:ident => $value:expr) => { $value };
}

mod account;
mod admin;
mod api;
mod classes;
mod comments;
mod imports;
mod login;
mod reports;
mod students;
mod terms;
mod workbook;

use axum::{
    Router,
    extract::{DefaultBodyLimit, Request},
    http::{HeaderValue, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use minijinja::{Value, context};

use crate::{
    AppState,
    auth::{self, Auth, Session},
    error::{AppError, AppResult},
    model,
};

pub fn router(state: AppState) -> Router {
    let pages = Router::new()
        // Auth
        .route("/login", get(login::login_page).post(login::login_submit))
        .route("/login/verify", get(login::totp_page).post(login::totp_submit))
        .route("/logout", post(login::logout))
        .route("/login/oidc", get(login::sso_start))
        .route("/login/oidc/callback", get(login::sso_callback))
        .route("/launch", get(login::launch))
        .route("/quit", post(login::quit))
        // The workbook file
        .route("/workbook", get(workbook::page).post(workbook::open))
        .route("/workbook.xlsx", get(workbook::download))
        // Dashboard, setup, settings, profile
        .route("/", get(classes::dashboard))
        .route("/setup", get(account::setup_page))
        .route("/settings", get(account::settings_page).post(account::settings_save))
        .route("/settings/scale", post(account::scale_save))
        .route("/settings/levels", post(account::levels_save))
        .route("/settings/logo", post(account::logo_upload))
        .route("/settings/logo/remove", post(account::logo_remove))
        .route("/settings/logo.img", get(account::logo_image))
        .route("/profile", get(account::profile_page).post(account::profile_save))
        .route("/profile/password", post(account::password_save))
        .route("/profile/totp", post(account::totp_enable))
        .route("/profile/totp/disable", post(account::totp_disable))
        .route("/profile/logout-everywhere", post(account::logout_everywhere))
        // Terms
        .route("/terms", get(terms::list).post(terms::create))
        .route("/terms/{id}", post(terms::update))
        .route("/terms/{id}/current", post(terms::make_current))
        .route("/terms/{id}/archive", post(terms::toggle_archive))
        .route("/terms/{id}/purge", post(terms::purge))
        .route("/terms/{id}/copy", post(terms::copy_setup))
        // Classes
        .route("/classes/new", get(classes::new_page).post(classes::create))
        .route("/classes/{id}", get(classes::grid_page))
        .route("/classes/{id}/settings", get(classes::settings_page).post(classes::update))
        .route("/classes/{id}/delete", post(classes::delete))
        .route("/classes/{id}/subjects", post(classes::subject_add))
        .route("/classes/{id}/subjects/{sid}", post(classes::subject_update))
        .route("/classes/{id}/subjects/{sid}/delete", post(classes::subject_delete))
        .route("/classes/{id}/subjects/{sid}/categories", post(classes::categories_save))
        .route("/classes/{id}/students", get(classes::roster_page).post(classes::roster_add))
        .route("/classes/{id}/students/{sid}/remove", post(classes::roster_remove))
        .route("/classes/{id}/insights", get(classes::insights))
        .route("/classes/{id}/export.xlsx", get(classes::export_xlsx))
        // Students
        .route("/classes/{id}/students/{sid}", get(students::page))
        .route("/classes/{id}/students/{sid}/details", post(students::details_save))
        .route("/classes/{id}/students/{sid}/comment", post(students::comment_save))
        .route("/classes/{id}/students/{sid}/skills", post(students::skills_save))
        .route("/classes/{id}/students/{sid}/override", post(students::override_save))
        .route("/classes/{id}/students/{sid}/report.pdf", get(reports::preview).post(reports::download))
        // Reports
        .route("/classes/{id}/reports", get(reports::page).post(reports::batch))
        // Import
        .route("/import", get(imports::upload_page).post(imports::upload))
        .route("/import/template.xlsx", get(imports::template_download))
        .route("/import/sheet/{id}", get(imports::template_preview).post(imports::template_apply))
        .route("/import/{id}", get(imports::map_page).post(imports::map_submit))
        .route("/import/{id}/apply", post(imports::apply))
        // Comment bank
        .route("/comments", get(comments::list).post(comments::add))
        .route("/comments/{id}/delete", post(comments::delete))
        // Admin
        .route("/admin/users", get(admin::users).post(admin::user_add))
        .route("/admin/users/{id}/delete", post(admin::user_delete))
        .route("/admin/users/{id}/password", post(admin::user_password))
        .route("/admin/users/{id}/admin", post(admin::user_toggle_admin))
        .route("/admin/backups", get(admin::backups).post(admin::backup_now))
        .route("/admin/backups/download", get(admin::backup_download))
        .route("/admin/audit", get(admin::audit))
        // Grid API
        .route("/api/subjects/{sid}/grid", get(api::grid))
        .route("/api/subjects/{sid}/marks", post(api::save_marks))
        .route("/api/subjects/{sid}/assessments", post(api::assessment_create))
        .route("/api/subjects/{sid}/undo", post(api::undo))
        .route("/api/assessments/{aid}", post(api::assessment_update))
        .route("/api/assessments/{aid}/delete", post(api::assessment_delete))
        .route("/api/assessments/{aid}/move", post(api::assessment_move))
        .route("/api/assessments/{aid}/fill", post(api::assessment_fill))
        .layer(DefaultBodyLimit::max(12 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), auth::session_middleware));

    let local = state.config.auth == crate::config::AuthMode::Local;
    let router = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/static/{*path}", get(crate::static_files::serve))
        .route("/favicon.ico", get(crate::static_files::favicon))
        .merge(pages);
    // Local mode: refuse requests whose Host isn't this computer (stops other
    // websites from reaching the app through DNS rebinding).
    let router = if local { router.layer(middleware::from_fn(crate::local::host_check)) } else { router };
    router.layer(middleware::from_fn(security_headers)).with_state(state)
}

async fn security_headers(req: Request, next: Next) -> Response {
    let is_static = req.uri().path().starts_with("/static/");
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    let set = |h: &mut axum::http::HeaderMap, k: &'static str, v: &'static str| {
        h.insert(k, HeaderValue::from_static(v));
    };
    set(h, "x-frame-options", "SAMEORIGIN");
    set(h, "x-content-type-options", "nosniff");
    set(h, "referrer-policy", "strict-origin-when-cross-origin");
    set(
        h,
        "content-security-policy",
        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
         font-src 'self'; object-src 'none'; frame-ancestors 'self'; base-uri 'self'; form-action 'self'",
    );
    if is_static {
        set(h, "cache-control", "public, max-age=3600");
    } else if !h.contains_key(header::CACHE_CONTROL) {
        set(h, "cache-control", "no-store");
    }
    resp
}

// ── Rendering ────────────────────────────────────────────────────────────────

impl From<minijinja::Error> for AppError {
    fn from(e: minijinja::Error) -> Self {
        AppError::Internal(anyhow::anyhow!("template: {e:#}"))
    }
}

/// Render a page for a logged-in teacher, with navigation and flash messages.
pub async fn render(state: &AppState, auth: &Auth, name: &str, ctx: Value) -> AppResult<Response> {
    let flashes = auth::take_flashes(state, &auth.session).await?;
    let uid = auth.user.id;
    let (settings, term, classes) = auth
        .db
        .call(move |c| {
            let s = model::settings(c, uid)?;
            let t = model::current_term(c, uid)?;
            let cl = match &t {
                Some(t) => model::classes_in_term(c, uid, t.id)?,
                None => vec![],
            };
            Ok((s, t, cl))
        })
        .await?;
    // Memory storage: is there work the teacher hasn't saved to their workbook?
    let workspace = auth.db.workspace().map(|ws| {
        let saved_at = ws
            .saved_at()
            .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
            .map(|t| t.with_timezone(&chrono::Local).format("%-I:%M %p").to_string());
        ctx! { unsaved => ws.unsaved(), saved_at }
    });
    let html = state.templates.get_template(name)?.render(context! {
        ..ctx! {
            user => auth.user,
            csrf => auth.session.csrf,
            flashes,
            settings,
            nav_term => term,
            nav_classes => classes,
            workspace,
            storage => state.config.storage,
            local => state.config.auth == crate::config::AuthMode::Local,
            password_login => state.config.password_login(),
            idle_minutes => state.config.idle_timeout_secs / 60,
        },
        ..ctx
    })?;
    Ok(Html(html).into_response())
}

/// Render a page that doesn't need a login.
pub async fn render_anon(state: &AppState, session: &Session, name: &str, ctx: Value) -> AppResult<Response> {
    let flashes = auth::take_flashes(state, session).await?;
    let html = state.templates.get_template(name)?.render(context! { ..ctx! { csrf => session.csrf, flashes }, ..ctx })?;
    Ok(Html(html).into_response())
}

pub fn to(path: &str) -> Response {
    Redirect::to(path).into_response()
}

pub async fn flash_to(state: &AppState, session: &Session, kind: &str, msg: impl Into<String>, path: &str) -> AppResult<Response> {
    auth::add_flash(state, session, kind, msg).await?;
    Ok(to(path))
}

// ── Form helper ──────────────────────────────────────────────────────────────

/// A urlencoded form as ordered pairs, so repeated fields work.
pub struct F(pub Vec<(String, String)>);

impl<S: Send + Sync> axum::extract::FromRequest<S> for F {
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let axum::Form(pairs) = axum::Form::<Vec<(String, String)>>::from_request(req, state)
            .await
            .map_err(|e| AppError::BadRequest(format!("Couldn't read the form: {e}")).into_response())?;
        Ok(F(pairs))
    }
}

impl F {
    pub fn get(&self, k: &str) -> String {
        self.0.iter().find(|(key, _)| key == k).map(|(_, v)| v.trim().to_string()).unwrap_or_default()
    }
    pub fn raw(&self, k: &str) -> String {
        self.0.iter().find(|(key, _)| key == k).map(|(_, v)| v.replace("\r\n", "\n")).unwrap_or_default()
    }
    pub fn all(&self, k: &str) -> Vec<String> {
        self.0.iter().filter(|(key, _)| key == k).map(|(_, v)| v.trim().to_string()).collect()
    }
    pub fn has(&self, k: &str) -> bool {
        self.0.iter().any(|(key, v)| key == k && !v.is_empty() && v != "0" && v != "off")
    }
    pub fn int(&self, k: &str) -> Option<i64> {
        self.get(k).parse().ok()
    }
    pub fn float(&self, k: &str) -> Option<f64> {
        self.get(k).trim_end_matches('%').trim().parse().ok().filter(|v: &f64| v.is_finite())
    }
}

pub fn non_empty(s: String) -> Option<String> {
    if s.trim().is_empty() { None } else { Some(s) }
}

pub fn valid_date(s: &str) -> Option<String> {
    chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok().map(|d| d.to_string())
}
