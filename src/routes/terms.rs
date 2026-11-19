use axum::{
    extract::{Path, State},
    response::Response,
};
use rusqlite::params;

use super::{F, flash_to, non_empty, render, valid_date};
use crate::{AppState, auth::Auth, error::AppResult, model};

pub async fn list(State(state): State<AppState>, auth: Auth) -> AppResult<Response> {
    let uid = auth.user.id;
    let terms = auth
        .db
        .call(move |c| {
            let terms = model::terms(c, uid)?;
            let mut out = Vec::new();
            for t in terms {
                let classes = model::classes_in_term(c, uid, t.id)?;
                let students: i64 = c.query_row(
                    "SELECT count(DISTINCT e.student_id) FROM enrolments e JOIN classes c ON c.id = e.class_id WHERE c.term_id = ?1",
                    [t.id],
                    |r| r.get(0),
                )?;
                out.push(ctx! { term => t, classes, students });
            }
            Ok(out)
        })
        .await?;
    render(&state, &auth, "terms.html", ctx! { terms }).await
}

pub async fn create(State(state): State<AppState>, auth: Auth, f: F) -> AppResult<Response> {
    let Some(name) = non_empty(f.get("name")) else {
        return flash_to(&state, &auth.session, "danger", "Give the term a name.", "/terms").await;
    };
    let (label, start, end, current) =
        (f.get("period_label"), valid_date(&f.get("start_date")), valid_date(&f.get("end_date")), f.has("is_current"));
    let uid = auth.user.id;
    auth.db
        .call(move |c| {
            let tx = c.transaction()?;
            if current {
                tx.execute("UPDATE terms SET is_current = 0 WHERE user_id = ?1", [uid])?;
            }
            tx.execute(
                "INSERT INTO terms (user_id, name, period_label, start_date, end_date, is_current) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![uid, name, label, start, end, current],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    flash_to(&state, &auth.session, "success", "Term created.", "/terms").await
}

pub async fn update(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>, f: F) -> AppResult<Response> {
    let Some(name) = non_empty(f.get("name")) else {
        return flash_to(&state, &auth.session, "danger", "Give the term a name.", "/terms").await;
    };
    let (label, start, end) = (f.get("period_label"), valid_date(&f.get("start_date")), valid_date(&f.get("end_date")));
    let uid = auth.user.id;
    auth.db
        .call(move |c| {
            model::term(c, uid, id)?;
            c.execute(
                "UPDATE terms SET name = ?1, period_label = ?2, start_date = ?3, end_date = ?4 WHERE id = ?5",
                params![name, label, start, end, id],
            )?;
            Ok(())
        })
        .await?;
    flash_to(&state, &auth.session, "success", "Term saved.", "/terms").await
}

pub async fn make_current(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> AppResult<Response> {
    let uid = auth.user.id;
    auth.db
        .call(move |c| {
            let t = model::term(c, uid, id)?;
            if t.archived {
                return Err(crate::error::bad("Un-archive the term before making it current."));
            }
            c.execute("UPDATE terms SET is_current = (id = ?1) WHERE user_id = ?2", params![id, uid])?;
            Ok(())
        })
        .await?;
    flash_to(&state, &auth.session, "success", "Switched terms.", "/").await
}

pub async fn toggle_archive(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> AppResult<Response> {
    let uid = auth.user.id;
    let archived = auth
        .db
        .call(move |c| {
            let t = model::term(c, uid, id)?;
            c.execute(
                "UPDATE terms SET archived = ?1, is_current = 0, archived_at = CASE WHEN ?1 THEN datetime('now') END WHERE id = ?2",
                params![!t.archived, id],
            )?;
            Ok(!t.archived)
        })
        .await?;
    let msg = if archived { "Term archived. Its classes are now read-only." } else { "Term restored." };
    flash_to(&state, &auth.session, "success", msg, "/terms").await
}

/// Permanently delete a term, its classes, and students no longer in any class.
pub async fn purge(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>, f: F) -> AppResult<Response> {
    let uid = auth.user.id;
    let typed = f.get("confirm_name");
    let result = auth
        .db
        .call(move |c| {
            let t = model::term(c, uid, id)?;
            if typed != t.name {
                return Ok(Err(t.name));
            }
            let tx = c.transaction()?;
            tx.execute("DELETE FROM terms WHERE id = ?1", [id])?;
            let orphans = tx.execute("DELETE FROM students WHERE user_id = ?1 AND id NOT IN (SELECT student_id FROM enrolments)", [uid])?;
            tx.commit()?;
            Ok(Ok((t.name, orphans)))
        })
        .await?;
    match result {
        Ok((name, n)) => {
            flash_to(
                &state,
                &auth.session,
                "success",
                format!("Deleted {name} and {n} student record(s) that weren't in any other term."),
                "/terms",
            )
            .await
        }
        Err(name) => {
            flash_to(&state, &auth.session, "danger", format!("Type the term name exactly (\"{name}\") to delete it."), "/terms").await
        }
    }
}

/// Copy class set-up (subjects, categories, optionally assessments and
/// students) from one term into another, e.g. for next semester.
pub async fn copy_setup(State(state): State<AppState>, auth: Auth, Path(from): Path<i64>, f: F) -> AppResult<Response> {
    let uid = auth.user.id;
    let to_term = f.int("to_term").unwrap_or(0);
    let (with_assessments, with_students) = (f.has("assessments"), f.has("students"));
    let n = auth
        .db
        .call(move |c| {
            model::term(c, uid, from)?;
            model::term(c, uid, to_term)?;
            if from == to_term {
                return Err(crate::error::bad("Pick a different term to copy into."));
            }
            let tx = c.transaction()?;
            let classes = model::classes_in_term(&tx, uid, from)?;
            for cl in &classes {
                tx.execute(
                    "INSERT INTO classes (user_id, term_id, name, course_code, period, grade_level, room) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![uid, to_term, cl.name, cl.course_code, cl.period, cl.grade_level, cl.room],
                )?;
                let new_class = tx.last_insert_rowid();
                for s in model::subjects(&tx, cl.id)? {
                    tx.execute(
                        "INSERT INTO subjects (class_id, name, report_style, sort) SELECT ?1, name, report_style, sort FROM subjects WHERE id = ?2",
                        params![new_class, s.id],
                    )?;
                    let new_subject = tx.last_insert_rowid();
                    let mut cat_map = std::collections::HashMap::new();
                    for cat in model::categories(&tx, s.id)? {
                        tx.execute(
                            "INSERT INTO categories (subject_id, name, weight, sort) SELECT ?1, name, weight, sort FROM categories WHERE id = ?2",
                            params![new_subject, cat.id],
                        )?;
                        cat_map.insert(cat.id, tx.last_insert_rowid());
                    }
                    if with_assessments {
                        for a in model::assessments(&tx, s.id)? {
                            tx.execute(
                                "INSERT INTO assessments (subject_id, category_id, name, unit, out_of, weight, kind, include_in_report, sort)
                                 SELECT ?1, ?2, name, unit, out_of, weight, kind, include_in_report, sort FROM assessments WHERE id = ?3",
                                params![new_subject, a.category_id.and_then(|c| cat_map.get(&c).copied()), a.id],
                            )?;
                        }
                    }
                }
                if with_students {
                    tx.execute(
                        "INSERT INTO enrolments (class_id, student_id) SELECT ?1, student_id FROM enrolments WHERE class_id = ?2 AND active = 1",
                        params![new_class, cl.id],
                    )?;
                }
            }
            tx.commit()?;
            Ok(classes.len())
        })
        .await?;
    flash_to(&state, &auth.session, "success", format!("Copied {n} class(es)."), "/terms").await
}
