/// The version this build reports. Releases set `GRADEBOOK_VERSION` (from the git tag, e.g. `v2.3.0`) when
/// building; otherwise it's the version in Cargo.toml.
pub fn app_version() -> &'static str {
    match option_env!("GRADEBOOK_VERSION") {
        Some(v) if !v.trim_start_matches('v').is_empty() => v.trim_start_matches('v'),
        _ => env!("CARGO_PKG_VERSION"),
    }
}

pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// URL-safe random token with `bytes` bytes of entropy.
pub fn random_token(bytes: usize) -> String {
    use base64::Engine;
    let mut buf = vec![0u8; bytes];
    rand::fill(&mut buf[..]);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

pub fn sha256_hex(s: &str) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(s.as_bytes()))
}

/// "2026–2027 School Year" (from August on, the year starting now).
pub fn default_term_name() -> String {
    use chrono::Datelike;
    let now = chrono::Local::now();
    let y = now.year();
    if now.month() >= 8 { format!("{y}–{} School Year", y + 1) } else { format!("{}–{y} School Year", y - 1) }
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Lower-cased, accent-folded, whitespace-collapsed name for matching.
pub fn name_key(s: &str) -> String {
    deunicode::deunicode(s).to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A safe ASCII file name such as `Lefevre_Zoe.pdf`.
pub fn file_stem(parts: &[&str]) -> String {
    let joined = parts.iter().filter(|p| !p.trim().is_empty()).cloned().collect::<Vec<_>>().join("_");
    let ascii = deunicode::deunicode(&joined);
    let mut out: String = ascii.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() { "report".into() } else { out.chars().take(80).collect() }
}

/// `Content-Disposition` value with an ASCII fallback and the UTF-8 name.
pub fn content_disposition(kind: &str, filename: &str) -> String {
    let ascii = deunicode::deunicode(filename).replace(['"', '\\'], "_");
    let encoded = percent_encoding::utf8_percent_encode(filename, percent_encoding::NON_ALPHANUMERIC);
    format!("{kind}; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

/// Round to one decimal, halves away from zero (86.25 → 86.3), the way a
/// teacher would. `format!("{:.1}")` alone rounds halves to even.
pub fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

pub fn fmt_pct(p: Option<f64>) -> String {
    match p {
        Some(v) => format!("{:.1}%", round1(v)),
        None => "—".into(),
    }
}

pub fn today_long() -> String {
    chrono::Local::now().format("%B %-d, %Y").to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn rounds_half_up() {
        assert_eq!(super::fmt_pct(Some(86.25)), "86.3%");
        assert_eq!(super::fmt_pct(Some(72.45)), "72.5%");
    }
    use super::*;

    #[test]
    fn stems_are_ascii_and_safe() {
        assert_eq!(file_stem(&["Lefèvre", "Zoë"]), "Lefevre_Zoe");
        assert_eq!(file_stem(&["Van Der Berg", "John"]), "Van_Der_Berg_John");
        assert_eq!(file_stem(&["../../etc", "x"]), "etc_x");
        assert_eq!(file_stem(&["", ""]), "report");
    }

    #[test]
    fn name_keys_fold_accents() {
        assert_eq!(name_key("  Zoë   LEFÈVRE "), "zoe lefevre");
    }
}

#[cfg(test)]
mod version_tests {
    #[test]
    fn app_version_is_a_bare_version_number() {
        let v = super::app_version();
        assert!(!v.starts_with('v') && v.split('.').count() == 3 && v.split('.').all(|p| p.parse::<u32>().is_ok()), "{v}");
    }
}
