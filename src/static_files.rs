//! CSS, JavaScript and icon fonts, embedded so the app is one self-contained
//! file (the local Windows/Mac app has nothing else to unpack).

use axum::{
    extract::Path,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

macro_rules! files {
    ($($path:literal => $mime:literal),* $(,)?) => {
        &[$(($path, $mime, include_bytes!(concat!("../static/", $path)) as &[u8])),*]
    };
}

static FILES: &[(&str, &str, &[u8])] = files! {
    "app.css" => "text/css; charset=utf-8",
    "js/app.js" => "text/javascript; charset=utf-8",
    "js/grid.js" => "text/javascript; charset=utf-8",
    "js/reports.js" => "text/javascript; charset=utf-8",
    "js/settings.js" => "text/javascript; charset=utf-8",
    "js/theme.js" => "text/javascript; charset=utf-8",
    "js/wizard.js" => "text/javascript; charset=utf-8",
    "icons/favicon.svg" => "image/svg+xml",
    "icons/favicon-32.png" => "image/png",
    "icons/apple-touch-icon.png" => "image/png",
    "vendor/bootstrap.min.css" => "text/css; charset=utf-8",
    "vendor/bootstrap-icons.min.css" => "text/css; charset=utf-8",
    "vendor/bootstrap.bundle.min.js" => "text/javascript; charset=utf-8",
    "vendor/fonts/bootstrap-icons.woff" => "font/woff",
    "vendor/fonts/bootstrap-icons.woff2" => "font/woff2",
    "vendor/fonts/source-sans-3-regular.woff2" => "font/woff2",
    "vendor/fonts/source-sans-3-semibold.woff2" => "font/woff2",
    "vendor/fonts/source-sans-3-bold.woff2" => "font/woff2",
    "vendor/fonts/source-sans-3-it.woff2" => "font/woff2",
};

pub async fn serve(Path(path): Path<String>) -> Response {
    match FILES.iter().find(|(p, _, _)| *p == path) {
        Some((_, mime, body)) => ([(header::CONTENT_TYPE, *mime)], *body).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Browsers ask for /favicon.ico on their own (and for pages without our <link>s).
pub async fn favicon() -> Response {
    serve(Path("icons/favicon-32.png".into())).await
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_static_file_is_embedded() {
        let mut on_disk = Vec::new();
        fn walk(dir: &std::path::Path, base: &std::path::Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, base, out);
                } else {
                    out.push(p.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/"));
                }
            }
        }
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("static");
        walk(&base, &base, &mut on_disk);
        for f in on_disk {
            assert!(super::FILES.iter().any(|(p, _, _)| *p == f), "static/{f} is not embedded in src/static_files.rs");
        }
    }
}
