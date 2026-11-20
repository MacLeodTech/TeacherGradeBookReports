use std::io::Write;

use axum::{
    extract::{Path, Query, State},
    http::header,
    response::{IntoResponse, Response},
};
use rusqlite::Connection;

use super::{F, flash_to, render};
use crate::{
    AppState,
    auth::Auth,
    error::{AppError, AppResult},
    model,
    report::{self, Options, REPORT_TYPES, ReportInput},
    util::content_disposition,
};

fn options_from(f: &F) -> Options {
    let rt = f.get("report_type");
    Options {
        report_type: if rt.is_empty() { Options::default().report_type } else { rt.chars().take(60).collect() },
        show_assessments: f.has("show_assessments"),
        show_categories: f.has("show_categories"),
        show_comments: f.has("show_comments"),
        show_missing: f.has("show_missing"),
        show_skills: f.has("show_skills"),
        show_legend: f.has("show_legend"),
        show_signature: f.has("show_signature"),
    }
}

struct Built {
    json: String,
    filename: String,
    overall: Option<f64>,
    student_id: i64,
}

/// Build the report model for one student. `combined` adds the student's other
/// classes in the same term.
fn build(c: &Connection, user: &crate::auth::User, cid: i64, sid: i64, opts: &Options, combined: bool) -> AppResult<Built> {
    let uid = user.id;
    let class = model::class(c, uid, cid)?;
    let student = model::student_in_class(c, cid, sid)?;
    let settings = model::settings(c, uid)?;
    let term = model::term(c, uid, class.term_id)?;
    let mut classes = vec![class.clone()];
    if combined {
        classes.extend(model::student_classes_in_term(c, uid, sid, class.term_id)?.into_iter().filter(|x| x.id != cid));
    }
    let mut parts = Vec::new();
    for cl in classes {
        let sds = model::subjects(c, cl.id)?.into_iter().map(|s| model::subject_data(c, s, &settings)).collect::<AppResult<Vec<_>>>()?;
        parts.push((cl, sds));
    }
    let overall = parts.first().and_then(|(_, sds)| {
        let v: Vec<f64> = sds.iter().filter_map(|sd| sd.result_for(sid).overall).collect();
        (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
    });
    let filename = report::file_name(&student, if combined { None } else { Some(&class) });
    let input = ReportInput { user, settings: &settings, term: &term, student: &student, classes: parts, options: opts };
    let json = report::build_json(c, &input)?;
    Ok(Built { json, filename, overall, student_id: sid })
}

async fn logo(state: &AppState, uid: i64) -> AppResult<Option<Vec<u8>>> {
    state.db.call(move |c| Ok(model::logo(c, uid)?.map(|l| l.0))).await
}

async fn render_one(json: String, logo: Option<Vec<u8>>) -> AppResult<Vec<u8>> {
    tokio::task::spawn_blocking(move || report::render_pdf(&json, logo.as_deref())).await.map_err(|e| AppError::Internal(e.into()))?
}

fn pdf_response(bytes: Vec<u8>, filename: &str, attachment: bool) -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/pdf".to_string()),
            (header::CONTENT_DISPOSITION, content_disposition(if attachment { "attachment" } else { "inline" }, filename)),
        ],
        bytes,
    )
        .into_response()
}

/// Inline preview. Doesn't count as "generated".
pub async fn preview(
    State(state): State<AppState>,
    auth: Auth,
    Path((cid, sid)): Path<(i64, i64)>,
    Query(q): Query<Vec<(String, String)>>,
) -> AppResult<Response> {
    let f = F(q);
    let opts = if f.0.is_empty() { Options::default() } else { options_from(&f) };
    let combined = f.has("combined");
    let user = auth.user.clone();
    let built = auth.db.call(move |c| build(c, &user, cid, sid, &opts, combined)).await?;
    let pdf = render_one(built.json, logo(&state, auth.user.id).await?).await?;
    Ok(pdf_response(pdf, &built.filename, false))
}

/// Download, recording the run for "changed since last report".
pub async fn download(State(state): State<AppState>, auth: Auth, Path((cid, sid)): Path<(i64, i64)>, f: F) -> AppResult<Response> {
    let opts = options_from(&f);
    let combined = f.has("combined");
    let user = auth.user.clone();
    let kind = opts.report_type.clone();
    let built = auth.db.call(move |c| build(c, &user, cid, sid, &opts, combined)).await?;
    let pdf = render_one(built.json, logo(&state, auth.user.id).await?).await?;
    let overall = built.overall;
    auth.db.call(move |c| report::record_run(c, cid, sid, &kind, overall)).await?;
    crate::audit::log(&state, Some(auth.user.id), &auth.user.username, "", "report.download", format!("class {cid}, 1 report")).await;
    Ok(pdf_response(pdf, &built.filename, true))
}

pub async fn page(State(state): State<AppState>, auth: Auth, Path(cid): Path<i64>) -> AppResult<Response> {
    let uid = auth.user.id;
    let ctx = auth
        .db
        .call(move |c| {
            let class = model::class(c, uid, cid)?;
            let settings = model::settings(c, uid)?;
            let roster = model::roster(c, cid)?;
            let sds = model::subjects(c, cid)?
                .into_iter()
                .map(|s| model::subject_data(c, s, &settings))
                .collect::<AppResult<Vec<_>>>()?;
            let status = model::report_status(c, cid)?;
            let mut comments_stmt = c.prepare(
                "SELECT count(*) FROM report_comments rc JOIN subjects s ON s.id = rc.subject_id WHERE s.class_id = ?1 AND rc.student_id = ?2 AND rc.comment != ''",
            )?;
            let mut rows = Vec::new();
            for st in &roster {
                let results: Vec<_> = sds.iter().map(|sd| sd.result_for(st.id)).collect();
                let grades: Vec<f64> = results.iter().filter_map(|r| r.overall).collect();
                let avg = (!grades.is_empty()).then(|| grades.iter().sum::<f64>() / grades.len() as f64);
                let (last, changed) = status.get(&st.id).cloned().map(|(l, n)| (Some(l), n)).unwrap_or((None, 0));
                let comments: i64 = comments_stmt.query_row(rusqlite::params![cid, st.id], |r| r.get(0))?;
                rows.push(ctx! {
                    id => st.id, name => st.sort_name(), avg, label => avg.map(|a| settings.scale.label(a)),
                    color => avg.map(|a| settings.scale.color(a)), missing => results.iter().map(|r| r.missing).sum::<usize>(),
                    pending => results.iter().map(|r| r.pending).sum::<usize>(), last, changed,
                    needs => last.is_none() || changed > 0, comments, subjects => sds.len(),
                });
            }
            Ok(ctx! { class, rows, report_types => REPORT_TYPES.to_vec(), defaults => Options::default() })
        })
        .await?;
    render(&state, &auth, "reports.html", ctx).await
}

/// Generate several reports as one ZIP.
pub async fn batch(State(state): State<AppState>, auth: Auth, Path(cid): Path<i64>, f: F) -> AppResult<Response> {
    let ids: Vec<i64> = f.all("student").iter().filter_map(|x| x.parse().ok()).collect();
    if ids.is_empty() {
        return flash_to(&state, &auth.session, "warning", "Select at least one student.", &format!("/classes/{cid}/reports")).await;
    }
    let opts = options_from(&f);
    let combined = f.has("combined");
    let user = auth.user.clone();
    let (class, built) = auth
        .db
        .call(move |c| {
            let class = model::class(c, user.id, cid)?;
            let built = ids.iter().map(|&sid| build(c, &user, cid, sid, &opts, combined)).collect::<AppResult<Vec<_>>>()?;
            Ok((class, built))
        })
        .await?;
    let logo = logo(&state, auth.user.id).await?;

    // Render in parallel; each PDF is independent.
    let (zip_bytes, runs) = tokio::task::spawn_blocking(move || -> AppResult<(Vec<u8>, Vec<(i64, Option<f64>)>)> {
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).min(8);
        let chunk = built.len().div_ceil(threads).max(1);
        let pdfs: Vec<AppResult<(String, Vec<u8>)>> = std::thread::scope(|s| {
            let handles: Vec<_> = built
                .chunks(chunk)
                .map(|part| {
                    let logo = logo.as_deref();
                    s.spawn(move || part.iter().map(|b| Ok((b.filename.clone(), report::render_pdf(&b.json, logo)?))).collect::<Vec<_>>())
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap_or_else(|_| vec![Err(AppError::Internal(anyhow::anyhow!("render thread panicked")))]))
                .collect()
        });
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            let mut used = std::collections::HashSet::new();
            for p in pdfs {
                let (mut name, bytes) = p?;
                // Two students with the same name get Smith_John.pdf and Smith_John_2.pdf.
                let stem = name.trim_end_matches(".pdf").to_string();
                let mut i = 2;
                while !used.insert(name.clone()) {
                    name = format!("{stem}_{i}.pdf");
                    i += 1;
                }
                zip.start_file(name, options).map_err(|e| AppError::Internal(e.into()))?;
                zip.write_all(&bytes).map_err(|e| AppError::Internal(e.into()))?;
            }
            zip.finish().map_err(|e| AppError::Internal(e.into()))?;
        }
        Ok((buf.into_inner(), built.iter().map(|b| (b.student_id, b.overall)).collect()))
    })
    .await
    .map_err(|e| AppError::Internal(e.into()))??;

    let kind = f.get("report_type");
    let n = runs.len();
    auth.db
        .call(move |c| {
            for (sid, overall) in runs {
                report::record_run(c, cid, sid, if kind.is_empty() { "Progress Report" } else { &kind }, overall)?;
            }
            Ok(())
        })
        .await?;
    crate::audit::log(&state, Some(auth.user.id), &auth.user.username, "", "report.batch", format!("class {cid}, {n} reports")).await;
    let name = format!("{}_reports_{}.zip", crate::util::file_stem(&[&class.name]), chrono::Local::now().format("%Y-%m-%d"));
    Ok((
        [(header::CONTENT_TYPE, "application/zip".to_string()), (header::CONTENT_DISPOSITION, content_disposition("attachment", &name))],
        zip_bytes,
    )
        .into_response())
}
