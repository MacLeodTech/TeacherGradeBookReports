//! JSON API behind the grade grid. Every write returns the recomputed grid, so
//! the browser never calculates grades itself.

use std::collections::HashMap;

use axum::{Json, extract::Path, http::StatusCode};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    auth::Auth,
    engine::{self, CellInput, Kind, display_mark},
    error::{AppError, AppResult},
    marks::{self, Change},
    model,
};

/// Turn page-style errors into JSON errors for the grid.
fn j(e: AppError) -> AppError {
    match e {
        AppError::BadRequest(m) => AppError::Json(StatusCode::BAD_REQUEST, m),
        AppError::NotFound => AppError::Json(StatusCode::NOT_FOUND, "Not found. Reload the page.".into()),
        AppError::Forbidden => AppError::Json(StatusCode::FORBIDDEN, "Not allowed.".into()),
        other => other,
    }
}

fn grid_json(c: &Connection, user_id: i64, subject_id: i64, errors: Vec<Value>) -> AppResult<Value> {
    let (subject, class) = model::subject(c, user_id, subject_id)?;
    let settings = model::settings(c, user_id)?;
    let roster = model::roster(c, class.id)?;
    let sd = model::subject_data(c, subject, &settings)?;
    let scale = &settings.scale;

    let mut cells = serde_json::Map::new();
    let mut results = serde_json::Map::new();
    let mut overall_sum = (0.0, 0usize);
    let mut warnings: Vec<String> = Vec::new();
    for st in &roster {
        let mut row = serde_json::Map::new();
        if let Some(ms) = sd.marks.get(&st.id) {
            for (aid, m) in ms {
                let kind = sd.assessments.iter().find(|a| a.id == *aid).map_or(Kind::Scored, |a| Kind::parse(&a.kind));
                // The percentage the mark counts as, for the hint after typing.
                let pct = sd.setup.assessments.iter().find(|a| a.id == *aid).and_then(|a| crate::engine::mark_pct(a, m, &sd.setup.levels));
                row.insert(aid.to_string(), json!({ "text": display_mark(kind, m), "status": m.status.as_str(), "pct": pct }));
            }
        }
        cells.insert(st.id.to_string(), Value::Object(row));
        let r = sd.result_for(st.id);
        if let Some(o) = r.overall {
            overall_sum.0 += o;
            overall_sum.1 += 1;
        }
        for w in &r.warnings {
            if !warnings.contains(w) {
                warnings.push(w.clone());
            }
        }
        let cats: serde_json::Map<String, Value> =
            r.categories.iter().filter_map(|c| c.id.map(|id| (id.to_string(), json!(c.avg)))).collect();
        results.insert(
            st.id.to_string(),
            json!({
                "overall": r.overall,
                "label": r.overall.map(|o| scale.label(o)),
                "color": r.overall.map(|o| scale.color(o)),
                "overridden": r.overridden,
                "categories": cats,
                "missing": r.missing,
                "pending": r.pending,
                "submitted": r.submitted,
                "expected": r.expected,
            }),
        );
    }

    let assessments: Vec<Value> = sd
        .assessments
        .iter()
        .map(|a| {
            let pcts: Vec<f64> = roster.iter().filter_map(|st| sd.mark(st.id, a.id).and_then(|m| sd.mark_pct(a, m))).collect();
            let avg = (!pcts.is_empty()).then(|| pcts.iter().sum::<f64>() / pcts.len() as f64);
            let mut v = serde_json::to_value(a).unwrap_or_default();
            v["avg"] = json!(avg);
            // Levels average as a level (3+), not a percentage.
            if Kind::parse(&a.kind) == Kind::Level {
                v["avg_text"] = json!(avg.map(|p| format!("L{}", engine::level_label(sd.setup.levels.level_of(p)))));
            }
            v
        })
        .collect();

    let class_avg = (overall_sum.1 > 0).then(|| overall_sum.0 / overall_sum.1 as f64);
    let undo = marks::undo_preview(c, class.id)?.map(|(source, n)| json!({ "source": source, "count": n }));
    let students: Vec<Value> = roster.iter().map(|s| json!({ "id": s.id, "name": s.sort_name() })).collect();

    Ok(json!({
        "subject": sd.subject,
        "class_id": class.id,
        "readonly": class.archived,
        "policy": settings.missing_policy.as_str(),
        "categories": sd.categories,
        "assessments": assessments,
        "students": students,
        "cells": cells,
        "results": results,
        "class_avg": class_avg,
        "class_label": class_avg.map(|a| scale.label(a)),
        "class_color": class_avg.map(|a| scale.color(a)),
        "scale": scale.bands,
        "warnings": warnings,
        "undo": undo,
        "errors": errors,
    }))
}

pub async fn grid(auth: Auth, Path(sid): Path<i64>) -> AppResult<Json<Value>> {
    let uid = auth.user.id;
    auth.db.call(move |c| grid_json(c, uid, sid, vec![])).await.map(Json).map_err(j)
}

// ── Marks ────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CellChange {
    student_id: i64,
    assessment_id: i64,
    input: String,
}

#[derive(Deserialize)]
pub struct SaveMarks {
    changes: Vec<CellChange>,
    #[serde(default)]
    source: String,
}

fn parse_for(kind: Kind, input: &str, out_of: f64) -> Result<CellInput, String> {
    engine::parse_input(kind, input, out_of)
}

pub async fn save_marks(auth: Auth, Path(sid): Path<i64>, Json(body): Json<SaveMarks>) -> AppResult<Json<Value>> {
    let uid = auth.user.id;
    if body.changes.len() > 5000 {
        return Err(AppError::Json(StatusCode::BAD_REQUEST, "Too many cells at once.".into()));
    }
    let source = if body.source == "paste" { "paste" } else { "grid" };
    auth.db
        .call(move |c| {
            let (_, class) = model::subject(c, uid, sid)?;
            model::class_mut(c, uid, class.id)?;
            let assessments: HashMap<i64, model::Assessment> = model::assessments(c, sid)?.into_iter().map(|a| (a.id, a)).collect();
            let roster: Vec<i64> = model::roster(c, class.id)?.iter().map(|s| s.id).collect();
            let mut changes = Vec::new();
            let mut errors = Vec::new();
            for ch in body.changes {
                let Some(a) = assessments.get(&ch.assessment_id) else { continue };
                if !roster.contains(&ch.student_id) {
                    continue;
                }
                match parse_for(Kind::parse(&a.kind), &ch.input, a.out_of) {
                    Ok(CellInput::Clear) => changes.push(Change { assessment_id: a.id, student_id: ch.student_id, new: None }),
                    Ok(CellInput::Set(m)) => changes.push(Change { assessment_id: a.id, student_id: ch.student_id, new: Some(m) }),
                    Err(e) => errors.push(json!({ "student_id": ch.student_id, "assessment_id": a.id, "input": ch.input, "error": e })),
                }
            }
            let tx = c.transaction()?;
            marks::apply(&tx, class.id, &changes, source)?;
            tx.commit()?;
            grid_json(c, uid, sid, errors)
        })
        .await
        .map(Json)
        .map_err(j)
}

pub async fn undo(auth: Auth, Path(sid): Path<i64>) -> AppResult<Json<Value>> {
    let uid = auth.user.id;
    auth.db
        .call(move |c| {
            let (_, class) = model::subject(c, uid, sid)?;
            model::class_mut(c, uid, class.id)?;
            let tx = c.transaction()?;
            marks::undo_last(&tx, class.id)?;
            tx.commit()?;
            grid_json(c, uid, sid, vec![])
        })
        .await
        .map(Json)
        .map_err(j)
}

// ── Assessments ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AssessmentIn {
    name: String,
    #[serde(default)]
    unit: String,
    #[serde(default = "hundred")]
    out_of: f64,
    #[serde(default = "one")]
    weight: f64,
    #[serde(default)]
    kind: String,
    category_id: Option<i64>,
    due_date: Option<String>,
    #[serde(default = "yes")]
    include_in_report: bool,
    /// When `out_of` changes, rescale existing marks so percentages stay the same.
    #[serde(default)]
    rescale: bool,
}

fn hundred() -> f64 {
    100.0
}
fn one() -> f64 {
    1.0
}
fn yes() -> bool {
    true
}

fn validate(a: &mut AssessmentIn, c: &Connection, subject_id: i64) -> AppResult<()> {
    a.name = a.name.trim().chars().take(120).collect();
    a.unit = a.unit.trim().chars().take(60).collect();
    if a.name.is_empty() {
        return Err(crate::error::bad("Give the assessment a name."));
    }
    let kind = Kind::parse(&a.kind);
    match kind {
        Kind::Completion => a.out_of = 1.0,
        Kind::Level => a.out_of = engine::MAX_LEVEL,
        Kind::Scored => {}
    }
    if !(a.out_of > 0.0 && a.out_of <= 10_000.0) {
        return Err(crate::error::bad("\"Out of\" must be more than 0."));
    }
    if !(0.0..=100.0).contains(&a.weight) {
        return Err(crate::error::bad("Weight must be between 0 and 100."));
    }
    if let Some(cid) = a.category_id
        && !model::categories(c, subject_id)?.iter().any(|x| x.id == cid)
    {
        a.category_id = None;
    }
    a.due_date = a.due_date.as_deref().and_then(super::valid_date);
    Ok(())
}

pub async fn assessment_create(auth: Auth, Path(sid): Path<i64>, Json(mut a): Json<AssessmentIn>) -> AppResult<Json<Value>> {
    let uid = auth.user.id;
    auth.db
        .call(move |c| {
            let (_, class) = model::subject(c, uid, sid)?;
            model::class_mut(c, uid, class.id)?;
            validate(&mut a, c, sid)?;
            c.execute(
                "INSERT INTO assessments (subject_id, category_id, name, unit, out_of, weight, kind, due_date, include_in_report, sort)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, (SELECT COALESCE(MAX(sort), 0) + 1 FROM assessments WHERE subject_id = ?1))",
                params![
                    sid,
                    a.category_id,
                    a.name,
                    a.unit,
                    a.out_of,
                    a.weight,
                    Kind::parse(&a.kind).as_str(),
                    a.due_date,
                    a.include_in_report
                ],
            )?;
            grid_json(c, uid, sid, vec![])
        })
        .await
        .map(Json)
        .map_err(j)
}

pub async fn assessment_update(auth: Auth, Path(aid): Path<i64>, Json(mut a): Json<AssessmentIn>) -> AppResult<Json<Value>> {
    let uid = auth.user.id;
    auth
        .db
        .call(move |c| {
            let (old, subject, class) = model::assessment(c, uid, aid)?;
            model::class_mut(c, uid, class.id)?;
            validate(&mut a, c, subject.id)?;
            let (from, to) = (Kind::parse(&old.kind), Kind::parse(&a.kind));
            if from != to && (from == Kind::Level || to == Kind::Level) {
                let marked: i64 =
                    c.query_row("SELECT count(*) FROM marks WHERE assessment_id = ?1 AND value IS NOT NULL", [aid], |r| r.get(0))?;
                if marked > 0 {
                    return Err(crate::error::bad(
                        "This assessment has marks, so it can't be changed to or from levels. Add a new assessment instead.",
                    ));
                }
            }
            let tx = c.transaction()?;
            if a.rescale && (old.out_of - a.out_of).abs() > 1e-9 && old.kind == "scored" {
                tx.execute(
                    "UPDATE marks SET value = ROUND(value * ?1 / ?2, 3) WHERE assessment_id = ?3 AND value IS NOT NULL",
                    params![a.out_of, old.out_of, aid],
                )?;
            }
            tx.execute(
                "UPDATE assessments SET category_id = ?1, name = ?2, unit = ?3, out_of = ?4, weight = ?5, kind = ?6, due_date = ?7, include_in_report = ?8 WHERE id = ?9",
                params![a.category_id, a.name, a.unit, a.out_of, a.weight, Kind::parse(&a.kind).as_str(), a.due_date, a.include_in_report, aid],
            )?;
            tx.commit()?;
            grid_json(c, uid, subject.id, vec![])
        })
        .await
        .map(Json)
        .map_err(j)
}

pub async fn assessment_delete(auth: Auth, Path(aid): Path<i64>) -> AppResult<Json<Value>> {
    let uid = auth.user.id;
    auth.db
        .call(move |c| {
            let (_, subject, class) = model::assessment(c, uid, aid)?;
            model::class_mut(c, uid, class.id)?;
            c.execute("DELETE FROM assessments WHERE id = ?1", [aid])?;
            grid_json(c, uid, subject.id, vec![])
        })
        .await
        .map(Json)
        .map_err(j)
}

#[derive(Deserialize)]
pub struct Move {
    dir: i64,
}

pub async fn assessment_move(auth: Auth, Path(aid): Path<i64>, Json(m): Json<Move>) -> AppResult<Json<Value>> {
    let uid = auth.user.id;
    auth.db
        .call(move |c| {
            let (_, subject, class) = model::assessment(c, uid, aid)?;
            model::class_mut(c, uid, class.id)?;
            let ids: Vec<i64> = model::assessments(c, subject.id)?.iter().map(|a| a.id).collect();
            let pos = ids.iter().position(|x| *x == aid).unwrap_or(0) as i64;
            let target = (pos + m.dir.signum()).clamp(0, ids.len() as i64 - 1);
            let mut ids = ids;
            let item = ids.remove(pos as usize);
            ids.insert(target as usize, item);
            let tx = c.transaction()?;
            for (i, id) in ids.iter().enumerate() {
                tx.execute("UPDATE assessments SET sort = ?1 WHERE id = ?2", params![i as i64, id])?;
            }
            tx.commit()?;
            grid_json(c, uid, subject.id, vec![])
        })
        .await
        .map(Json)
        .map_err(j)
}

#[derive(Deserialize)]
pub struct Fill {
    input: String,
    #[serde(default)]
    only_empty: bool,
}

/// Fill a whole column, e.g. mark every blank as missing after the due date.
pub async fn assessment_fill(auth: Auth, Path(aid): Path<i64>, Json(f): Json<Fill>) -> AppResult<Json<Value>> {
    let uid = auth.user.id;
    auth.db
        .call(move |c| {
            let (a, subject, class) = model::assessment(c, uid, aid)?;
            model::class_mut(c, uid, class.id)?;
            let new = match parse_for(Kind::parse(&a.kind), &f.input, a.out_of).map_err(crate::error::bad)? {
                CellInput::Clear => None,
                CellInput::Set(m) => Some(m),
            };
            let roster = model::roster(c, class.id)?;
            let mut changes = Vec::new();
            for st in roster {
                let has: Option<i64> = c
                    .query_row("SELECT 1 FROM marks WHERE assessment_id = ?1 AND student_id = ?2", params![aid, st.id], |r| r.get(0))
                    .optional()?;
                if f.only_empty && has.is_some() {
                    continue;
                }
                changes.push(Change { assessment_id: aid, student_id: st.id, new });
            }
            let tx = c.transaction()?;
            marks::apply(&tx, class.id, &changes, "fill")?;
            tx.commit()?;
            grid_json(c, uid, subject.id, vec![])
        })
        .await
        .map(Json)
        .map_err(j)
}
