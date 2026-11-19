use axum::{
    Json,
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not found")]
    NotFound,
    #[error("forbidden")]
    Forbidden,
    /// A problem with what the user sent. The message is shown to them.
    #[error("{0}")]
    BadRequest(String),
    /// Same as BadRequest but answered as JSON (for the grid API).
    #[error("{0}")]
    Json(StatusCode, String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        if e.sqlite_error_code() == Some(rusqlite::ErrorCode::DiskFull) {
            return AppError::BadRequest(
                "There's no room left for more data. Save your workbook, then remove old classes or terms you no longer need.".into(),
            );
        }
        AppError::Internal(e.into())
    }
}

pub type AppResult<T> = Result<T, AppError>;

pub fn bad(msg: impl Into<String>) -> AppError {
    AppError::BadRequest(msg.into())
}

fn page(status: StatusCode, title: &str, body: &str) -> Response {
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>{title}</title>\
         <link href=\"/static/vendor/bootstrap.min.css\" rel=\"stylesheet\"><link href=\"/static/app.css\" rel=\"stylesheet\"></head>\
         <body class=\"p-5\"><div class=\"container\" style=\"max-width:640px\"><h1 class=\"h3\">{title}</h1>\
         <p class=\"text-muted\">{body}</p><a class=\"btn btn-primary\" href=\"/\">Back to the dashboard</a></div></body></html>",
        title = crate::util::html_escape(title),
        body = crate::util::html_escape(body)
    );
    (status, Html(html)).into_response()
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::NotFound => page(StatusCode::NOT_FOUND, "Not found", "That page doesn't exist, or it isn't yours."),
            AppError::Forbidden => page(StatusCode::FORBIDDEN, "Not allowed", "You don't have access to that."),
            AppError::BadRequest(m) => page(StatusCode::BAD_REQUEST, "Something's not right", &m),
            AppError::Json(code, m) => (code, Json(serde_json::json!({ "error": m }))).into_response(),
            AppError::Internal(e) => {
                tracing::error!("internal error: {e:#}");
                page(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong", "The error has been logged. Please try again.")
            }
        }
    }
}
