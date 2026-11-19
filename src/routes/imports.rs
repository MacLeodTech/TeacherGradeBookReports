//! Import wizard: upload → map columns + choose target → preview → apply.

use axum::{
    extract::{Multipart, Path, Query, State},
    http::header,
    response::{IntoResponse, Response},
};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;

use super::{F, flash_to, non_empty, render, to};
use crate::{
    AppState,
    auth::{Auth, ClientIp},
    error::{AppError, AppResult, bad},
    import::{self, Cell, Grid, ImportOptions, Role},
    model::{self, Mode},
    starter,
    util::{content_disposition, file_stem, now_unix, random_token},
};

const ALLOWED: [&str; 6] = [".xlsx", ".xlsm", ".xls", ".ods", ".csv", ".txt"];

#[derive(Deserialize)]
pub struct Target {
    class: Option<i64>,
}

pub async fn upload_page(State(state): State<AppState>, auth: Auth, Query(t): Query<Target>) -> AppResult<Response> {
    render(&state, &auth, "import_upload.html", ctx! { class_id => t.class }).await
}

pub async fn upload(State(state): State<AppState>, auth: Auth, Query(t): Query<Target>, mut mp: Multipart) -> AppResult<Response> {
    let mut file: Option<(String, Vec<u8>)> = None;
    while let Some(field) = mp.next_field().await.map_err(|e| bad(format!("Upload failed: {e}")))? {
        if field.name() == Some("file") {
            let name = field.file_name().unwrap_or("upload.xlsx").to_string();
            let bytes = field.bytes().await.map_err(|e| bad(format!("Upload failed: {e}")))?;
            file = Some((name, bytes.to_vec()));
        }
    }
    let back = match t.class {
        Some(c) => format!("/import?class={c}"),
        None => "/import".into(),
    };
    let Some((name, bytes)) = file.filter(|f| !f.1.is_empty()) else {
        return flash_to(&state, &auth.session, "danger", "Choose a file to upload.", &back).await;
    };
    let lower = name.to_ascii_lowercase();
    if !ALLOWED.iter().any(|ext| lower.ends_with(ext)) {
        return flash_to(&state, &auth.session, "danger", "Upload an Excel (.xlsx, .xls), OpenDocument (.ods) or CSV file.", &back).await;
    }
    // The starter spreadsheet: straight to its preview.
    if lower.ends_with(".xlsx") && crate::starter::is_template(&bytes) {
        return match stage_template(&auth, name, bytes).await {
            Ok(url) => Ok(to(&url)),
            Err(AppError::BadRequest(m)) => flash_to(&state, &auth.session, "danger", m, &back).await,
            Err(e) => Err(e),
        };
    }
    // A Gradebook workbook (saved from this app) opens as-is, no mapping needed.
    if lower.ends_with(".xlsx") && crate::workbook::is_workbook(&bytes) {
        let uid = auth.user.id;
        let empty: bool = auth
            .db
            .call(move |c| Ok(c.query_row("SELECT NOT EXISTS(SELECT 1 FROM classes WHERE user_id = ?1)", [uid], |r| r.get(0))?))
            .await?;
        let mode = if empty && state.config.storage == crate::config::Storage::Memory {
            crate::workbook::Mode::Replace
        } else {
            crate::workbook::Mode::Add
        };
        return match super::workbook::open_bytes(&state, &auth, "", bytes, mode).await {
            Ok(o) => flash_to(&state, &auth.session, "success", super::workbook::summary(&o), "/").await,
            Err(AppError::BadRequest(m)) => flash_to(&state, &auth.session, "danger", m, &back).await,
            Err(e) => Err(e),
        };
    }
    if bytes.len() > 10 * 1024 * 1024 {
        return flash_to(&state, &auth.session, "danger", "Files must be under 10 MB.", &back).await;
    }
    let parse_name = name.clone();
    let grid = match tokio::task::spawn_blocking(move || import::parse_file(&bytes, &parse_name))
        .await
        .map_err(|e| AppError::Internal(e.into()))?
    {
        Ok(g) => g,
        Err(AppError::BadRequest(m)) => return flash_to(&state, &auth.session, "danger", m, &back).await,
        Err(e) => return Err(e),
    };
    if grid.len() < 2 || grid.len() > 2000 || grid.first().map_or(0, |r| r.len()) > 500 {
        return flash_to(
            &state,
            &auth.session,
            "danger",
            "The sheet needs a header row and at least one student row (and at most 2000 rows / 500 columns).",
            &back,
        )
        .await;
    }
    let id = random_token(12);
    let uid = auth.user.id;
    let json = serde_json::to_string(&grid).map_err(|e| AppError::Internal(e.into()))?;
    let sid = id.clone();
    auth.db
        .call(move |c| {
            c.execute(
                "INSERT INTO import_staging (id, user_id, filename, grid_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![sid, uid, name, json, now_unix()],
            )?;
            Ok(())
        })
        .await?;
    Ok(to(&match t.class {
        Some(c) => format!("/import/{id}?class={c}"),
        None => format!("/import/{id}"),
    }))
}

fn load_staged(c: &rusqlite::Connection, uid: i64, id: &str) -> AppResult<(String, Grid)> {
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT filename, grid_json FROM import_staging WHERE id = ?1 AND user_id = ?2 AND kind = 'grid'",
            params![id, uid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (name, json) = row.ok_or_else(|| bad("That upload has expired. Please upload the file again."))?;
    let grid: Grid = serde_json::from_str(&json).map_err(|e| AppError::Internal(e.into()))?;
    Ok((name, grid))
}

pub async fn map_page(State(state): State<AppState>, auth: Auth, Path(id): Path<String>, Query(t): Query<Target>) -> AppResult<Response> {
    let uid = auth.user.id;
    let ctx = auth
        .db
        .call(move |c| {
            let (filename, grid) = load_staged(c, uid, &id)?;
            let hdr = import::header_row(&grid);
            let headers: Vec<String> = grid[hdr].iter().map(Cell::text).collect();
            let roles = import::guess_roles(&grid);
            let sample: Vec<Vec<String>> = grid.iter().skip(hdr + 1).take(6).map(|r| r.iter().map(Cell::text).collect()).collect();
            let rows = grid.len() - hdr - 1;
            let columns: Vec<_> = headers
                .iter()
                .zip(&roles)
                .enumerate()
                .map(|(i, (h, r))| ctx! { index => i, header => h, role => r.as_str(), samples => sample.iter().map(|row| row.get(i).cloned().unwrap_or_default()).collect::<Vec<_>>() })
                .collect();
            let role_options: Vec<_> = Role::ALL.iter().map(|(r, l)| ctx! { value => r.as_str(), label => l }).collect();
            let term = model::current_term(c, uid)?;
            let classes = match &term {
                Some(t) => model::classes_in_term(c, uid, t.id)?,
                None => vec![],
            };
            let mut class_subjects = Vec::new();
            for cl in &classes {
                for s in model::subjects(c, cl.id)? {
                    class_subjects.push(ctx! { class_id => cl.id, subject_id => s.id, label => if cl.name == s.name { cl.name.clone() } else { format!("{} — {}", cl.name, s.name) } });
                }
            }
            let has_final = roles.contains(&Role::FinalMark);
            let legacy_like = grid.iter().skip(hdr + 1).flat_map(|r| r.iter()).any(|c| matches!(c, Cell::Num(n) if *n > 0.0 && *n < 1.0));
            let settings = model::settings(c, uid)?;
            let stem = std::path::Path::new(&filename).file_stem().and_then(|s| s.to_str()).unwrap_or("Imported class").to_string();
            Ok(ctx! {
                id, filename, columns, role_options, rows, class_subjects, has_final, legacy_like, term,
                preselect => t.class, suggested_name => stem, secondary => settings.mode == Mode::Secondary,
            })
        })
        .await?;
    render(&state, &auth, "import_map.html", ctx).await
}

fn read_options(f: &F, columns: usize) -> ImportOptions {
    let roles = (0..columns).map(|i| Role::parse(&f.get(&format!("role_{i}")))).collect();
    ImportOptions {
        roles,
        blank_as: if f.get("blank_as") == "skip" { "skip".into() } else { "missing".into() },
        zero_as: if f.get("zero_as") == "zero" { "zero".into() } else { "missing".into() },
        final_as_override: f.has("final_as_override"),
    }
}

/// Resolve or create the target subject. `create` = false only checks. Runs on
/// the caller's transaction so a preview can roll the new class back.
fn target(c: &rusqlite::Connection, uid: i64, f: &F, create: bool) -> AppResult<(i64, i64, Option<String>)> {
    let t = f.get("target");
    if t == "new" {
        let name = non_empty(f.get("new_class_name")).ok_or_else(|| bad("Give the new class a name."))?;
        if !create {
            return Ok((0, 0, Some(name)));
        }
        let settings = model::settings(c, uid)?;
        let term = model::current_term(c, uid)?.ok_or_else(|| bad("Create a term first."))?;
        let classes = model::classes_in_term(c, uid, term.id)?;
        if settings.mode == Mode::Secondary && classes.len() as i64 >= settings.active_periods {
            return Err(bad(format!(
                "You already have {} class(es), the number of teaching periods in your settings. Import into an existing class or raise the limit in Settings.",
                classes.len()
            )));
        }
        let period = f
            .int("new_class_period")
            .filter(|p| (1..=4).contains(p) && Some(*p) != settings.prep_period && !classes.iter().any(|c| c.period == Some(*p)));
        c.execute(
            "INSERT INTO classes (user_id, term_id, name, course_code, period) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![uid, term.id, name, f.get("new_class_code"), period.filter(|_| settings.mode == Mode::Secondary)],
        )?;
        let cid = c.last_insert_rowid();
        let mode = if settings.mode == Mode::Elementary { Mode::Secondary } else { settings.mode };
        model::create_default_subjects(c, cid, mode, &name)?;
        let sid = model::subjects(c, cid)?[0].id;
        return Ok((cid, sid, Some(name)));
    }
    let sid: i64 = t.parse().map_err(|_| bad("Choose where to import the marks."))?;
    let (_, cl) = model::subject(c, uid, sid)?;
    model::class_mut(c, uid, cl.id)?;
    Ok((cl.id, sid, None))
}

/// Show the preview of what will change.
pub async fn map_submit(State(state): State<AppState>, auth: Auth, Path(id): Path<String>, f: F) -> AppResult<Response> {
    let uid = auth.user.id;
    let back = format!("/import/{id}");
    let res = auth
        .db
        .call(move |c| {
            let (filename, grid) = load_staged(c, uid, &id)?;
            let hdr = import::header_row(&grid);
            let opts = read_options(&f, grid[hdr].len());
            let (_, sid, new_name) = target(c, uid, &f, false)?;
            // Preview inside a transaction that is always rolled back, so a new
            // class exists only for the duration of the preview.
            let tx = c.transaction()?;
            let (cid, psid, _) = target(&tx, uid, &f, true)?;
            let plan = import::preview(&tx, uid, cid, psid, &grid, &opts);
            tx.rollback()?;
            let plan = plan?;
            let target_label = match &new_name {
                Some(n) => format!("a new class, \"{n}\""),
                None => {
                    let (s, cl) = model::subject(c, uid, sid)?;
                    if s.name == cl.name { cl.name } else { format!("{} — {}", cl.name, s.name) }
                }
            };
            Ok(ctx! { id, filename, plan, target_label, form => f.0, roles => opts.roles.iter().map(|r| r.as_str()).collect::<Vec<_>>() })
        })
        .await;
    match res {
        Ok(ctx) => render(&state, &auth, "import_preview.html", ctx).await,
        Err(AppError::BadRequest(m)) => flash_to(&state, &auth.session, "danger", m, &back).await,
        Err(e) => Err(e),
    }
}

pub async fn apply(State(state): State<AppState>, auth: Auth, Path(id): Path<String>, f: F) -> AppResult<Response> {
    let uid = auth.user.id;
    let back = format!("/import/{id}");
    let res = auth
        .db
        .call(move |c| {
            let (_, grid) = load_staged(c, uid, &id)?;
            let hdr = import::header_row(&grid);
            let opts = read_options(&f, grid[hdr].len());
            let tx = c.transaction()?;
            let (cid, sid, _) = target(&tx, uid, &f, true)?;
            let plan = import::apply(&tx, uid, cid, sid, &grid, &opts)?;
            tx.execute("DELETE FROM import_staging WHERE id = ?1", [&id])?;
            tx.commit()?;
            Ok((cid, sid, plan))
        })
        .await;
    match res {
        Ok((cid, sid, plan)) => {
            let msg = format!(
                "Import finished: {} new student(s), {} new assessment(s), {} mark(s) updated. You can undo the mark changes from the grid.",
                plan.new_students.len(),
                plan.new_assessments.len(),
                plan.changed_marks
            );
            flash_to(&state, &auth.session, "success", msg, &format!("/classes/{cid}?subject={sid}")).await
        }
        Err(AppError::BadRequest(m)) => flash_to(&state, &auth.session, "danger", m, &back).await,
        Err(e) => Err(e),
    }
}

// ── The starter spreadsheet ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TemplateQuery {
    #[serde(default)]
    filled: String,
}

/// Download the starter spreadsheet, blank or with the current term filled in.
pub async fn template_download(auth: Auth, Query(q): Query<TemplateQuery>) -> AppResult<Response> {
    let uid = auth.user.id;
    let filled = matches!(q.filled.as_str(), "1" | "true" | "yes");
    let (bytes, term) = auth
        .db
        .call(move |c| {
            let settings = model::settings(c, uid)?;
            let t = if filled { starter::from_term(c, uid, &settings)? } else { starter::blank(&settings) };
            let term = model::current_term(c, uid)?.map(|t| t.name).unwrap_or_default();
            Ok((starter::build(&t)?, term))
        })
        .await?;
    let name = if filled {
        format!("{}.xlsx", file_stem(&["Gradebook", &term, "spreadsheet"]))
    } else {
        "Gradebook starter spreadsheet.xlsx".to_string()
    };
    Ok((
        [
            (header::CONTENT_TYPE, "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string()),
            (header::CONTENT_DISPOSITION, content_disposition("attachment", &name)),
        ],
        bytes,
    )
        .into_response())
}

/// Parse and stage an uploaded starter spreadsheet; returns its preview URL.
pub async fn stage_template(auth: &Auth, name: String, bytes: Vec<u8>) -> AppResult<String> {
    if bytes.len() > 20 * 1024 * 1024 {
        return Err(bad("Spreadsheets must be under 20 MB."));
    }
    let parsed = tokio::task::spawn_blocking(move || starter::parse(&bytes)).await.map_err(|e| AppError::Internal(e.into()))??;
    let json = serde_json::to_string(&parsed).map_err(|e| AppError::Internal(e.into()))?;
    let (id, uid) = (random_token(12), auth.user.id);
    let sid = id.clone();
    auth.db
        .call(move |c| {
            c.execute(
                "INSERT INTO import_staging (id, user_id, filename, grid_json, created_at, kind) VALUES (?1, ?2, ?3, ?4, ?5, 'template')",
                params![sid, uid, name, json, now_unix()],
            )?;
            Ok(())
        })
        .await?;
    Ok(format!("/import/sheet/{id}"))
}

fn load_template(c: &rusqlite::Connection, uid: i64, id: &str) -> AppResult<(String, starter::Parsed)> {
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT filename, grid_json FROM import_staging WHERE id = ?1 AND user_id = ?2 AND kind = 'template'",
            params![id, uid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (name, json) = row.ok_or_else(|| bad("That upload has expired. Please upload the spreadsheet again."))?;
    Ok((name, serde_json::from_str(&json).map_err(|e| AppError::Internal(e.into()))?))
}

/// What uploading the spreadsheet will do, without doing it.
pub async fn template_preview(State(state): State<AppState>, auth: Auth, Path(id): Path<String>) -> AppResult<Response> {
    let uid = auth.user.id;
    let sid = id.clone();
    let (filename, outcome) = auth
        .db
        .call(move |c| {
            let (name, parsed) = load_template(c, uid, &sid)?;
            // Run it for real, then roll back: the preview is exactly what will happen.
            let tx = c.transaction()?;
            let outcome = starter::apply(&tx, uid, &parsed)?;
            tx.rollback()?;
            Ok((name, outcome))
        })
        .await?;
    let changes = outcome.changes();
    render(&state, &auth, "import_sheet.html", ctx! { id, filename, outcome, changes }).await
}

pub async fn template_apply(
    State(state): State<AppState>,
    auth: Auth,
    ClientIp(ip): ClientIp,
    Path(id): Path<String>,
) -> AppResult<Response> {
    let uid = auth.user.id;
    let outcome = auth
        .db
        .call(move |c| {
            let (_, parsed) = load_template(c, uid, &id)?;
            let tx = c.transaction()?;
            let outcome = starter::apply(&tx, uid, &parsed)?;
            tx.execute("DELETE FROM import_staging WHERE id = ?1", [&id])?;
            tx.commit()?;
            Ok(outcome)
        })
        .await?;
    crate::audit::log(
        &state,
        Some(uid),
        &auth.user.username,
        &ip.to_string(),
        "import.spreadsheet",
        format!("{} new classes, {} updated, {} marks", outcome.classes_new.len(), outcome.classes_updated.len(), outcome.marks_changed),
    )
    .await;
    let mut msg = format!(
        "Spreadsheet imported: {} new class{}, {} new student{}, {} new assessment{}, {} mark{} added or changed.",
        outcome.classes_new.len(),
        if outcome.classes_new.len() == 1 { "" } else { "es" },
        outcome.students_new,
        if outcome.students_new == 1 { "" } else { "s" },
        outcome.assessments_new,
        if outcome.assessments_new == 1 { "" } else { "s" },
        outcome.marks_changed,
        if outcome.marks_changed == 1 { "" } else { "s" },
    );
    if outcome.marks_changed > 0 {
        msg.push_str(" Mark changes can be undone from each class's grid.");
    }
    flash_to(&state, &auth.session, "success", msg, "/").await
}
