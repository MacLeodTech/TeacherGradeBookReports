use axum::{
    Json,
    extract::{Multipart, Query, State},
    http::header,
    response::{IntoResponse, Response},
};
use rusqlite::{OptionalExtension, params};

use super::{F, flash_to, non_empty, render, to, valid_date};
use crate::util::default_term_name;
use crate::{
    AppState,
    auth::{self, Auth},
    engine::{Band, Scale, is_hex_color},
    error::{AppError, AppResult, bad},
    model::{self, Mode},
};

pub async fn setup_page(State(state): State<AppState>, auth: Auth) -> AppResult<Response> {
    let uid = auth.user.id;
    let has_term = auth.db.call(move |c| Ok(model::current_term(c, uid)?.is_some())).await?;
    render(&state, &auth, "settings.html", ctx! { setup => true, has_term, term_default => default_term_name() }).await
}

pub async fn settings_page(State(state): State<AppState>, auth: Auth) -> AppResult<Response> {
    render(&state, &auth, "settings.html", ctx! { setup => false, has_term => true }).await
}

#[derive(serde::Deserialize)]
pub struct PaletteQ {
    #[serde(default)]
    main: String,
    #[serde(default)]
    second: String,
}

/// The report colours a pair of school colours gives, for the live preview.
pub async fn palette(_auth: Auth, Query(q): Query<PaletteQ>) -> Json<crate::color::Palette> {
    Json(crate::color::palette(&q.main, &q.second))
}

/// A made-up student's report in the colours being chosen (not yet saved).
pub async fn sample_report(State(state): State<AppState>, auth: Auth, Query(q): Query<PaletteQ>) -> AppResult<Response> {
    let uid = auth.user.id;
    let settings = auth.db.call(move |c| model::settings(c, uid)).await?;
    let json = crate::report::sample_json(&auth.user, &settings, &q.main, &q.second)?;
    let logo = super::reports::logo(&state, uid).await?;
    let pdf = super::reports::render_one(json, logo).await?;
    Ok(super::reports::pdf_response(pdf, "Sample_report.pdf", false))
}

/// Colours picked out of the teacher's logo, to suggest as school colours.
pub async fn logo_colors(auth: Auth) -> AppResult<Json<Vec<String>>> {
    let uid = auth.user.id;
    let Some((bytes, _)) = auth.db.call(move |c| model::logo(c, uid)).await? else { return Ok(Json(vec![])) };
    let found = tokio::task::spawn_blocking(move || crate::color::from_logo(&bytes)).await.map_err(|e| AppError::Internal(e.into()))?;
    Ok(Json(found))
}

pub async fn settings_save(State(state): State<AppState>, auth: Auth, f: F) -> AppResult<Response> {
    let uid = auth.user.id;
    let setup = f.has("setup");
    let mode = Mode::parse(&f.get("mode"));
    let periods = f.int("active_periods").unwrap_or(3).clamp(1, 3);
    let prep = f.int("prep_period").filter(|p| (1..=4).contains(p));
    let policy = if f.get("missing_policy") == "zero" { "zero" } else { "ignore" };
    let primary = f.get("primary_color");
    let accent = f.get("accent_color");
    if !is_hex_color(&primary) || !is_hex_color(&accent) {
        return flash_to(&state, &auth.session, "danger", "Colours must look like #003F5A.", if setup { "/setup" } else { "/settings" })
            .await;
    }
    let board = f.get("board_name");
    let title = non_empty(f.get("report_title")).unwrap_or_else(|| "Student Progress Report".into());
    let profile = (f.get("school"), f.get("full_name"), f.get("email"));
    let term = (f.get("term_name"), f.get("period_label"), valid_date(&f.get("start_date")), valid_date(&f.get("end_date")));
    if setup && !profile.2.is_empty() && !valid_email(&profile.2) {
        return flash_to(&state, &auth.session, "danger", "Enter a valid email address.", "/setup").await;
    }

    let over_limit = auth
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            model::settings(&tx, uid)?; // ensure row
            tx.execute(
                "UPDATE teacher_settings SET mode = ?1, active_periods = ?2, prep_period = ?3, missing_policy = ?4,
                 board_name = ?5, report_title = ?6, primary_color = ?7, accent_color = ?8, setup_done = 1 WHERE user_id = ?9",
                params![mode.as_str(), periods, prep, policy, board, title, primary.to_uppercase(), accent.to_uppercase(), uid],
            )?;
            if setup {
                let (school, name, email) = profile;
                if !school.is_empty() {
                    tx.execute("UPDATE users SET school = ?1 WHERE id = ?2", params![school, uid])?;
                }
                tx.execute("UPDATE users SET full_name = ?1, email = ?2 WHERE id = ?3", params![name, email, uid])?;
                let (tname, label, start, end) = term;
                if model::current_term(&tx, uid)?.is_none() {
                    let tname = non_empty(tname).unwrap_or_else(default_term_name);
                    tx.execute(
                        "INSERT INTO terms (user_id, name, period_label, start_date, end_date, is_current) VALUES (?1, ?2, ?3, ?4, ?5, 1)",
                        params![uid, tname, label, start, end],
                    )?;
                }
            }
            // Warn if the period count is now below the number of classes.
            let over = match model::current_term(&tx, uid)? {
                Some(t) if mode == Mode::Secondary => {
                    let n: i64 = tx.query_row("SELECT count(*) FROM classes WHERE term_id = ?1", [t.id], |r| r.get(0))?;
                    n > periods
                }
                _ => false,
            };
            tx.commit()?;
            Ok(over)
        })
        .await?;
    if over_limit {
        auth::add_flash(&state, &auth.session, "warning", format!(
            "You have more classes this term than the {periods} teaching period(s) you set. Existing classes are kept; new ones are blocked until you're under the limit."
        )).await?;
    }
    flash_to(&state, &auth.session, "success", "Settings saved.", if setup { "/" } else { "/settings" }).await
}

pub fn valid_email(e: &str) -> bool {
    let parts: Vec<&str> = e.split('@').collect();
    e.len() <= 254
        && parts.len() == 2
        && !parts[0].is_empty()
        && parts[1].contains('.')
        && !parts[1].starts_with('.')
        && !parts[1].ends_with('.')
        && !e.chars().any(char::is_whitespace)
}

/// What each level (R, 1–4) is worth, for level assessments such as weekly participation.
pub async fn levels_save(State(state): State<AppState>, auth: Auth, f: F) -> AppResult<Response> {
    let levels = if f.has("reset") {
        Some(crate::engine::Levels::default())
    } else {
        let mut pct = [0.0; 5];
        let mut ok = true;
        for (i, p) in pct.iter_mut().enumerate() {
            match f.get(&format!("level{i}")).trim().trim_end_matches('%').parse::<f64>() {
                Ok(v) => *p = v,
                Err(_) => ok = false,
            }
        }
        if ok { crate::engine::Levels::new(pct) } else { None }
    };
    let Some(levels) = levels else {
        return flash_to(
            &state,
            &auth.session,
            "danger",
            "Each level needs a percentage from 0 to 100, higher than the level below it.",
            "/settings#levels",
        )
        .await;
    };
    let json = serde_json::to_string(&levels).map_err(|e| AppError::Internal(e.into()))?;
    let uid = auth.user.id;
    auth.db
        .call(move |c| {
            model::settings(c, uid)?;
            c.execute("UPDATE teacher_settings SET levels_json = ?1 WHERE user_id = ?2", params![json, uid])?;
            Ok(())
        })
        .await?;
    flash_to(&state, &auth.session, "success", "Level percentages saved.", "/settings#levels").await
}

pub async fn scale_save(State(state): State<AppState>, auth: Auth, f: F) -> AppResult<Response> {
    let preset = f.get("preset");
    let scale = match preset.as_str() {
        "ontario-letters" => Scale::ontario_letters(),
        "ontario-levels" => Scale::ontario_levels(),
        "standard" => Scale::standard_letters(),
        _ => {
            let (mins, labels, colors) = (f.all("min"), f.all("label"), f.all("color"));
            let mut bands = Vec::new();
            for i in 0..labels.len() {
                if labels[i].is_empty() && mins.get(i).is_none_or(|m| m.is_empty()) {
                    continue;
                }
                let min: f64 = mins.get(i).and_then(|m| m.parse().ok()).ok_or_else(|| bad("Every band needs a minimum percentage."))?;
                bands.push(Band {
                    min,
                    label: labels[i].chars().take(16).collect(),
                    color: colors.get(i).cloned().unwrap_or_else(|| "#333333".into()).to_uppercase(),
                });
            }
            Scale::new(bands)
        }
    };
    if let Err(e) = scale.validate() {
        return flash_to(&state, &auth.session, "danger", e, "/settings#scale").await;
    }
    let json = serde_json::to_string(&scale).map_err(|e| AppError::Internal(e.into()))?;
    let uid = auth.user.id;
    auth.db
        .call(move |c| {
            model::settings(c, uid)?;
            c.execute("UPDATE teacher_settings SET scale_json = ?1 WHERE user_id = ?2", params![json, uid])?;
            Ok(())
        })
        .await?;
    flash_to(&state, &auth.session, "success", "Grade scale saved.", "/settings#scale").await
}

pub async fn logo_upload(State(state): State<AppState>, auth: Auth, mut mp: Multipart) -> AppResult<Response> {
    let mut data = None;
    while let Some(field) = mp.next_field().await.map_err(|e| bad(format!("Upload failed: {e}")))? {
        if field.name() == Some("logo") {
            data = Some(field.bytes().await.map_err(|e| bad(format!("Upload failed: {e}")))?);
        }
    }
    let Some(bytes) = data.filter(|b| !b.is_empty()) else {
        return flash_to(&state, &auth.session, "danger", "Choose an image to upload.", "/settings#branding").await;
    };
    if bytes.len() > 1024 * 1024 {
        return flash_to(&state, &auth.session, "danger", "Logos must be under 1 MB.", "/settings#branding").await;
    }
    let mime = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else {
        return flash_to(&state, &auth.session, "danger", "Logos must be PNG or JPEG images.", "/settings#branding").await;
    };
    // Make sure the PDF engine can actually use it before saving.
    let probe = bytes.to_vec();
    let check = tokio::task::spawn_blocking(move || crate::report::check_logo(&probe)).await.map_err(|e| AppError::Internal(e.into()))?;
    if let Err(e) = check {
        return flash_to(&state, &auth.session, "danger", e, "/settings#branding").await;
    }
    let uid = auth.user.id;
    auth.db
        .call(move |c| {
            model::settings(c, uid)?;
            c.execute("UPDATE teacher_settings SET logo = ?1, logo_mime = ?2 WHERE user_id = ?3", params![bytes.to_vec(), mime, uid])?;
            Ok(())
        })
        .await?;
    flash_to(&state, &auth.session, "success", "Logo saved. It appears in the report header.", "/settings#branding").await
}

pub async fn logo_remove(State(state): State<AppState>, auth: Auth) -> AppResult<Response> {
    let uid = auth.user.id;
    auth.db
        .call(move |c| {
            c.execute("UPDATE teacher_settings SET logo = NULL, logo_mime = NULL WHERE user_id = ?1", [uid])?;
            Ok(())
        })
        .await?;
    flash_to(&state, &auth.session, "success", "Logo removed.", "/settings#branding").await
}

pub async fn logo_image(auth: Auth) -> AppResult<Response> {
    let uid = auth.user.id;
    let logo = auth.db.call(move |c| model::logo(c, uid)).await?.ok_or(AppError::NotFound)?;
    Ok(([(header::CONTENT_TYPE, logo.1)], logo.0).into_response())
}

// ── Profile ──────────────────────────────────────────────────────────────────

pub async fn profile_page(State(state): State<AppState>, auth: Auth) -> AppResult<Response> {
    let (secret, qr) = if auth.user.totp_enabled {
        (String::new(), String::new())
    } else {
        let secret = auth::new_totp_secret();
        let qr = auth::totp_for(&secret, &auth.user.username).and_then(|t| t.to_qr_base64().ok()).unwrap_or_default();
        (secret, qr)
    };
    render(&state, &auth, "profile.html", ctx! { totp_secret => secret, totp_qr => qr }).await
}

pub async fn profile_save(State(state): State<AppState>, auth: Auth, f: F) -> AppResult<Response> {
    let (name, email, school) = (f.get("full_name"), f.get("email"), f.get("school"));
    if !email.is_empty() && !valid_email(&email) {
        return flash_to(&state, &auth.session, "danger", "Enter a valid email address.", "/profile").await;
    }
    let uid = auth.user.id;
    auth.db
        .call(move |c| {
            c.execute("UPDATE users SET full_name = ?1, email = ?2, school = ?3 WHERE id = ?4", params![name, email, school, uid])?;
            Ok(())
        })
        .await?;
    flash_to(&state, &auth.session, "success", "Profile saved.", "/profile").await
}

async fn check_current_password(state: &AppState, uid: i64, password: String) -> AppResult<bool> {
    let hash: Option<String> =
        state.db.call(move |c| Ok(c.query_row("SELECT password_hash FROM users WHERE id = ?1", [uid], |r| r.get(0)).optional()?)).await?;
    Ok(hash.is_some_and(|h| auth::verify_password(&password, &h)))
}

pub async fn password_save(State(state): State<AppState>, auth: Auth, f: F) -> AppResult<Response> {
    if auth.user.external {
        return Err(bad("Your account signs in without a password here, so there's no password or 2-step setting to change."));
    }
    let (new, confirm) = (f.raw("new_password"), f.raw("confirm_password"));
    if !check_current_password(&state, auth.user.id, f.raw("current_password")).await? {
        return flash_to(&state, &auth.session, "danger", "Your current password is wrong.", "/profile").await;
    }
    if new != confirm {
        return flash_to(&state, &auth.session, "danger", "The new passwords don't match.", "/profile").await;
    }
    if let Err(e) = auth::validate_password(&new) {
        return flash_to(&state, &auth.session, "danger", e, "/profile").await;
    }
    let (uid, hash, keep) = (auth.user.id, auth::hash_password(&new), auth.session.token_hash.clone());
    auth.db
        .call(move |c| {
            c.execute("UPDATE users SET password_hash = ?1 WHERE id = ?2", params![hash, uid])?;
            // Sign out other devices.
            c.execute("DELETE FROM sessions WHERE user_id = ?1 AND token_hash != ?2", params![uid, keep])?;
            Ok(())
        })
        .await?;
    flash_to(&state, &auth.session, "success", "Password changed. Other devices have been signed out.", "/profile").await
}

pub async fn totp_enable(State(state): State<AppState>, auth: Auth, f: F) -> AppResult<Response> {
    if auth.user.external {
        return Err(bad("Your account signs in without a password here, so there's no password or 2-step setting to change."));
    }
    let secret = f.get("secret");
    if secret.is_empty() || !auth::check_totp(&secret, &auth.user.username, &f.get("code")) {
        return flash_to(
            &state,
            &auth.session,
            "danger",
            "That code didn't match. Scan the QR code again and enter the current 6-digit code.",
            "/profile#security",
        )
        .await;
    }
    let uid = auth.user.id;
    auth.db
        .call(move |c| {
            c.execute("UPDATE users SET totp_secret = ?1 WHERE id = ?2", params![secret, uid])?;
            Ok(())
        })
        .await?;
    flash_to(
        &state,
        &auth.session,
        "success",
        "Two-step verification is on. You'll be asked for a code when you log in.",
        "/profile#security",
    )
    .await
}

pub async fn totp_disable(State(state): State<AppState>, auth: Auth, f: F) -> AppResult<Response> {
    if auth.user.external {
        return Err(bad("Your account signs in without a password here, so there's no password or 2-step setting to change."));
    }
    if !check_current_password(&state, auth.user.id, f.raw("current_password")).await? {
        return flash_to(&state, &auth.session, "danger", "Your password is wrong.", "/profile#security").await;
    }
    let uid = auth.user.id;
    auth.db
        .call(move |c| {
            c.execute("UPDATE users SET totp_secret = NULL WHERE id = ?1", [uid])?;
            Ok(())
        })
        .await?;
    flash_to(&state, &auth.session, "success", "Two-step verification is off.", "/profile#security").await
}

pub async fn logout_everywhere(State(state): State<AppState>, auth: Auth) -> AppResult<Response> {
    auth::delete_sessions_for_user(&state, auth.user.id).await?;
    Ok(to("/login"))
}
