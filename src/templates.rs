//! HTML templates, embedded in the binary so the image ships one file.

use minijinja::{AutoEscape, Environment, value::Value};

macro_rules! templates {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_str!(concat!("../templates/", $name)))),*]
    };
}

const TEMPLATES: &[(&str, &str)] = templates![
    "base.html",
    "macros.html",
    "login.html",
    "login_totp.html",
    "settings.html",
    "profile.html",
    "terms.html",
    "dashboard.html",
    "class_new.html",
    "class_settings.html",
    "grid.html",
    "roster.html",
    "insights.html",
    "student.html",
    "reports.html",
    "import_upload.html",
    "import_map.html",
    "import_preview.html",
    "import_sheet.html",
    "comments.html",
    "admin_users.html",
    "admin_backups.html",
    "admin_audit.html",
    "workbook.html",
    "closed.html",
];

/// "85.0%" or "—".
fn pct(v: Value) -> String {
    match f64::try_from(v) {
        Ok(f) if f.is_finite() => crate::util::fmt_pct(Some(f)),
        _ => "—".into(),
    }
}

/// Trim trailing zeros: 17.50 → "17.5", 20.0 → "20".
fn num(v: Value) -> String {
    match f64::try_from(v.clone()) {
        Ok(f) => crate::engine::fmt_num(f),
        Err(_) => v.to_string(),
    }
}

pub fn environment() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_auto_escape_callback(|_| AutoEscape::Html);
    for (name, src) in TEMPLATES {
        env.add_template(name, src).unwrap_or_else(|e| panic!("template {name}: {e:#}"));
    }
    env.add_filter("pct", pct);
    env.add_filter("num", num);
    env
}

#[cfg(test)]
mod tests {
    #[test]
    fn templates_parse() {
        let env = super::environment();
        assert!(env.get_template("base.html").is_ok());
    }
}
