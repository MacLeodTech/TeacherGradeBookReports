//! Save and open the workbook file (see `crate::workbook`).

use axum::{
    extract::{Multipart, State},
    http::header,
    response::{IntoResponse, Response},
};

use super::{flash_to, render};
use crate::{
    AppState, audit,
    auth::{Auth, ClientIp},
    config::Storage,
    error::{AppError, AppResult, bad},
    util::{content_disposition, file_stem},
    workbook::{self, Mode, Opened},
};

const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

pub async fn page(State(state): State<AppState>, auth: Auth) -> AppResult<Response> {
    let uid = auth.user.id;
    let classes: i64 =
        auth.db.call(move |c| Ok(c.query_row("SELECT count(*) FROM classes WHERE user_id = ?1", [uid], |r| r.get(0))?)).await?;
    render(&state, &auth, "workbook.html", ctx! { classes }).await
}

pub async fn download(State(state): State<AppState>, auth: Auth, ClientIp(ip): ClientIp) -> AppResult<Response> {
    let uid = auth.user.id;
    let teacher = if auth.user.full_name.is_empty() { auth.user.username.clone() } else { auth.user.full_name.clone() };
    let school = auth.user.school.clone();
    let t = teacher.clone();
    let (bytes, classes) = auth
        .db
        .call(move |c| {
            let bytes = workbook::save(c, uid, &workbook::Owner { teacher: &t, school: &school })?;
            let n: i64 = c.query_row("SELECT count(*) FROM classes WHERE user_id = ?1", [uid], |r| r.get(0))?;
            Ok((bytes, n))
        })
        .await?;
    if let Some(ws) = auth.db.workspace() {
        ws.mark_saved(true);
    }
    audit::log(&state, Some(uid), &auth.user.username, &ip.to_string(), "workbook.save", format!("{classes} classes")).await;
    let name = format!("{}.xlsx", file_stem(&["Gradebook", &teacher, &chrono::Local::now().format("%Y-%m-%d").to_string()]));
    Ok(([(header::CONTENT_TYPE, XLSX.to_string()), (header::CONTENT_DISPOSITION, content_disposition("attachment", &name))], bytes)
        .into_response())
}

pub async fn open(State(state): State<AppState>, auth: Auth, ClientIp(ip): ClientIp, mut mp: Multipart) -> AppResult<Response> {
    let mut file: Option<Vec<u8>> = None;
    let mut mode = None;
    while let Some(field) = mp.next_field().await.map_err(|e| bad(format!("Upload failed: {e}")))? {
        match field.name() {
            Some("file") => file = Some(field.bytes().await.map_err(|e| bad(format!("Upload failed: {e}")))?.to_vec()),
            Some("mode") => {
                let v = field.text().await.unwrap_or_default();
                mode = Some(if v == "add" { Mode::Add } else { Mode::Replace });
            }
            _ => {}
        }
    }
    let Some(bytes) = file.filter(|b| !b.is_empty()) else {
        return flash_to(&state, &auth.session, "danger", "Choose your workbook file to open.", "/workbook").await;
    };
    // The starter spreadsheet isn't a workbook; it goes through its own preview.
    if crate::starter::is_template(&bytes) {
        return match super::imports::stage_template(&auth, "spreadsheet.xlsx".into(), bytes).await {
            Ok(url) => Ok(super::to(&url)),
            Err(AppError::BadRequest(m)) => flash_to(&state, &auth.session, "danger", m, "/workbook").await,
            Err(e) => Err(e),
        };
    }
    let default = if state.config.storage == Storage::Memory { Mode::Replace } else { Mode::Add };
    match open_bytes(&state, &auth, &ip.to_string(), bytes, mode.unwrap_or(default)).await {
        Ok(o) => flash_to(&state, &auth.session, "success", summary(&o), "/").await,
        Err(AppError::BadRequest(m)) => flash_to(&state, &auth.session, "danger", m, "/workbook").await,
        Err(e) => Err(e),
    }
}

/// Open a workbook's bytes into the teacher's data (also used by Import).
pub async fn open_bytes(state: &AppState, auth: &Auth, ip: &str, bytes: Vec<u8>, mode: Mode) -> AppResult<Opened> {
    if bytes.len() > 20 * 1024 * 1024 {
        return Err(bad("Workbooks must be under 20 MB."));
    }
    let parsed = tokio::task::spawn_blocking(move || workbook::parse(&bytes)).await.map_err(|e| AppError::Internal(e.into()))??;
    let uid = auth.user.id;
    let opened = auth
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let o = workbook::open(&tx, uid, &parsed, mode)?;
            tx.commit()?;
            Ok(o)
        })
        .await?;
    // What's in memory now matches the file.
    if let Some(ws) = auth.db.workspace()
        && mode == Mode::Replace
    {
        ws.mark_saved(false);
    }
    audit::log(state, Some(uid), &auth.user.username, ip, "workbook.open", format!("{} classes ({mode:?})", opened.classes)).await;
    Ok(opened)
}

pub fn summary(o: &Opened) -> String {
    let mut s = format!(
        "Workbook opened: {} class{}, {} student{}, {} mark{}.",
        o.classes,
        if o.classes == 1 { "" } else { "es" },
        o.students,
        if o.students == 1 { "" } else { "s" },
        o.marks,
        if o.marks == 1 { "" } else { "s" }
    );
    if o.skipped > 0 {
        s.push_str(&format!(" {} damaged row(s) were skipped.", o.skipped));
    }
    if let Some(e) = &o.edits {
        let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let mut parts = Vec::new();
        if e.marks_changed > 0 {
            parts.push(plural(e.marks_changed, "mark", "marks"));
        }
        if e.assessments_new > 0 {
            parts.push(plural(e.assessments_new, "new assessment", "new assessments"));
        }
        if e.students_new > 0 {
            parts.push(plural(e.students_new, "new student", "new students"));
        }
        if !e.classes_new.is_empty() {
            parts.push(plural(e.classes_new.len(), "new class", "new classes"));
        }
        if e.settings_changed > 0 {
            parts.push(plural(e.settings_changed, "setting", "settings"));
        }
        if !parts.is_empty() {
            s.push_str(&format!(
                " Your changes in Excel were added: {}. Mark changes can be undone from the class's mark grid.",
                parts.join(", ")
            ));
        }
        let check: Vec<String> =
            e.problems
                .iter()
                .chain(&e.notes)
                .map(|p| {
                    if p.cell.is_empty() { format!("{}: {}", p.sheet, p.message) } else { format!("{} {}: {}", p.sheet, p.cell, p.message) }
                })
                .collect();
        if !check.is_empty() {
            s.push_str(&format!(" To check: {}", check.iter().take(4).cloned().collect::<Vec<_>>().join(" · ")));
            if check.len() > 4 {
                s.push_str(&format!(" · and {} more.", check.len() - 4));
            }
        }
    }
    s
}
