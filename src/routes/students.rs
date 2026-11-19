use axum::{
    extract::{Path, State},
    response::Response,
};
use rusqlite::params;

use super::{F, flash_to, render};
use crate::{
    AppState,
    auth::Auth,
    engine::{self, Status, display_cell, fmt_num},
    error::AppResult,
    model::{self, LEARNING_SKILLS, Student},
    report::{Options, REPORT_TYPES},
};

/// Fill comment-bank placeholders for a student.
pub fn personalise(text: &str, s: &Student) -> String {
    let (they, them, their, is, has) = match s.pronouns.as_str() {
        "she" => ("she", "her", "her", "is", "has"),
        "he" => ("he", "him", "his", "is", "has"),
        _ => ("they", "them", "their", "are", "have"),
    };
    let cap = |w: &str| {
        let mut c = w.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    };
    text.replace("{first}", s.first())
        .replace("{name}", &s.full_name())
        .replace("{They}", &cap(they))
        .replace("{they}", they)
        .replace("{Them}", &cap(them))
        .replace("{them}", them)
        .replace("{Their}", &cap(their))
        .replace("{their}", their)
        .replace("{is}", is)
        .replace("{has}", has)
}

pub async fn page(State(state): State<AppState>, auth: Auth, Path((cid, sid)): Path<(i64, i64)>) -> AppResult<Response> {
    let uid = auth.user.id;
    let ctx = auth
        .db
        .call(move |c| {
            let class = model::class(c, uid, cid)?;
            let student = model::student_in_class(c, cid, sid)?;
            let settings = model::settings(c, uid)?;
            let scale = &settings.scale;
            let roster = model::roster(c, cid)?;
            let pos = roster.iter().position(|s| s.id == sid);
            let prev = pos.and_then(|p| p.checked_sub(1)).and_then(|p| roster.get(p)).map(|s| (s.id, s.sort_name()));
            let next = pos.and_then(|p| roster.get(p + 1)).map(|s| (s.id, s.sort_name()));

            let mut subjects = Vec::new();
            for s in model::subjects(c, cid)? {
                let sd = model::subject_data(c, s, settings.missing_policy)?;
                let r = sd.result_for(sid);
                let cats: Vec<_> = r
                    .categories
                    .iter()
                    .map(|cr| {
                        ctx! { name => cr.name, weight => fmt_num(cr.weight), avg => cr.avg,
                        label => cr.avg.map(|a| scale.label(a)), color => cr.avg.map(|a| scale.color(a)) }
                    })
                    .collect();
                let rows: Vec<_> = sd
                    .assessments
                    .iter()
                    .map(|a| {
                        let m = sd.mark(sid, a.id);
                        let pct = m.and_then(|m| engine::mark_pct(&a.engine(), m));
                        let cat = sd.categories.iter().find(|x| Some(x.id) == a.category_id).map(|x| x.name.clone());
                        ctx! {
                            name => a.name, unit => a.unit, out_of => fmt_num(a.out_of), kind => a.kind, category => cat,
                            weight => fmt_num(a.weight), formative => a.weight == 0.0,
                            text => m.map(display_cell), status => m.map(|m| m.status.as_str()),
                            pct, label => pct.map(|p| scale.label(p)), color => pct.map(|p| scale.color(p)),
                        }
                    })
                    .collect();
                let missing: Vec<String> = sd
                    .assessments
                    .iter()
                    .filter(|a| sd.mark(sid, a.id).is_some_and(|m| m.status == Status::Missing))
                    .map(|a| a.name.clone())
                    .collect();
                let (comment, next_steps) = model::comment(c, sd.subject.id, sid)?;
                let ov = sd.overrides.get(&sid).cloned();
                subjects.push(ctx! {
                    subject => sd.subject, result => r, cats, rows, missing, comment, next_steps,
                    override_pct => ov.as_ref().map(|o| fmt_num(o.0)), override_reason => ov.map(|o| o.1),
                    label => r.overall.map(|o| scale.label(o)), color => r.overall.map(|o| scale.color(o)),
                    calc_label => r.calculated.map(|o| scale.label(o)),
                });
            }

            let skills = model::learning_skills(c, cid, sid)?;
            let mut bank_stmt = c.prepare("SELECT tag, body FROM comment_bank WHERE user_id = ?1 ORDER BY tag, id")?;
            let bank: Vec<(String, String)> = bank_stmt
                .query_map([uid], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .map(|(tag, body)| (tag, personalise(&body, &student)))
                .collect();
            let others: Vec<_> = model::student_classes_in_term(c, uid, sid, class.term_id)?.into_iter().filter(|x| x.id != cid).collect();
            let mut runs_stmt = c.prepare(
                "SELECT report_type, overall, created_at FROM report_runs WHERE class_id = ?1 AND student_id = ?2 ORDER BY id DESC LIMIT 5",
            )?;
            let runs: Vec<_> = runs_stmt
                .query_map(params![cid, sid], |r| {
                    Ok(ctx! { kind => r.get::<_, String>(0)?, overall => r.get::<_, Option<f64>>(1)?, at => r.get::<_, String>(2)? })
                })?
                .collect::<Result<_, _>>()?;
            Ok(ctx! {
                class, student, prev, next, subjects, skills, skill_names => LEARNING_SKILLS.to_vec(),
                bank, others, runs, report_types => REPORT_TYPES.to_vec(), defaults => Options::default(),
                full_name => student.full_name(),
            })
        })
        .await?;
    render(&state, &auth, "student.html", ctx).await
}

pub async fn details_save(State(state): State<AppState>, auth: Auth, Path((cid, sid)): Path<(i64, i64)>, f: F) -> AppResult<Response> {
    let uid = auth.user.id;
    let back = format!("/classes/{cid}/students/{sid}");
    let first = f.get("first_name");
    if first.is_empty() {
        return flash_to(&state, &auth.session, "danger", "First name can't be empty.", &back).await;
    }
    let pronouns = match f.get("pronouns").as_str() {
        "she" => "she",
        "he" => "he",
        _ => "they",
    };
    let (last, pref, number) = (f.get("last_name"), f.get("preferred_name"), f.get("student_number"));
    auth
        .db
        .call(move |c| {
            model::class(c, uid, cid)?;
            model::student_in_class(c, cid, sid)?;
            c.execute(
                "UPDATE students SET first_name = ?1, last_name = ?2, preferred_name = ?3, student_number = ?4, pronouns = ?5 WHERE id = ?6 AND user_id = ?7",
                params![first, last, pref, number, pronouns, sid, uid],
            )?;
            Ok(())
        })
        .await?;
    flash_to(&state, &auth.session, "success", "Student details saved.", &back).await
}

pub async fn comment_save(State(state): State<AppState>, auth: Auth, Path((cid, sid)): Path<(i64, i64)>, f: F) -> AppResult<Response> {
    let uid = auth.user.id;
    let subject_id = f.int("subject_id").unwrap_or(0);
    let (comment, next) = (f.raw("comment").trim().to_string(), f.raw("next_steps").trim().to_string());
    if comment.len() > 4000 || next.len() > 4000 {
        return flash_to(
            &state,
            &auth.session,
            "danger",
            "Comments are limited to 4000 characters.",
            &format!("/classes/{cid}/students/{sid}"),
        )
        .await;
    }
    auth
        .db
        .call(move |c| {
            let (_, cl) = model::subject(c, uid, subject_id)?;
            if cl.id != cid {
                return Err(crate::error::AppError::NotFound);
            }
            model::class_mut(c, uid, cid)?;
            model::student_in_class(c, cid, sid)?;
            c.execute(
                "INSERT INTO report_comments (student_id, subject_id, comment, next_steps, updated_at) VALUES (?1, ?2, ?3, ?4, datetime('now'))
                 ON CONFLICT(student_id, subject_id) DO UPDATE SET comment = excluded.comment, next_steps = excluded.next_steps, updated_at = excluded.updated_at",
                params![sid, subject_id, comment, next],
            )?;
            Ok(())
        })
        .await?;
    flash_to(&state, &auth.session, "success", "Comment saved.", &format!("/classes/{cid}/students/{sid}#subject-{subject_id}")).await
}

pub async fn skills_save(State(state): State<AppState>, auth: Auth, Path((cid, sid)): Path<(i64, i64)>, f: F) -> AppResult<Response> {
    let uid = auth.user.id;
    let ratings: Vec<(String, String)> = LEARNING_SKILLS
        .iter()
        .map(|k| (k.to_string(), f.get(&format!("skill_{k}"))))
        .filter(|(_, v)| matches!(v.as_str(), "E" | "G" | "S" | "N" | ""))
        .collect();
    auth.db
        .call(move |c| {
            model::class_mut(c, uid, cid)?;
            model::student_in_class(c, cid, sid)?;
            let tx = c.transaction()?;
            for (k, v) in ratings {
                if v.is_empty() {
                    tx.execute("DELETE FROM learning_skills WHERE student_id = ?1 AND class_id = ?2 AND skill = ?3", params![sid, cid, k])?;
                } else {
                    tx.execute(
                        "INSERT INTO learning_skills (student_id, class_id, skill, rating) VALUES (?1, ?2, ?3, ?4)
                         ON CONFLICT(student_id, class_id, skill) DO UPDATE SET rating = excluded.rating",
                        params![sid, cid, k, v],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    flash_to(&state, &auth.session, "success", "Learning skills saved.", &format!("/classes/{cid}/students/{sid}#skills")).await
}

/// Set or clear a manual final grade (e.g. for "most consistent, more recent" evidence).
pub async fn override_save(State(state): State<AppState>, auth: Auth, Path((cid, sid)): Path<(i64, i64)>, f: F) -> AppResult<Response> {
    let uid = auth.user.id;
    let subject_id = f.int("subject_id").unwrap_or(0);
    let back = format!("/classes/{cid}/students/{sid}#subject-{subject_id}");
    let clear = f.has("clear");
    let pct = f.float("pct");
    let reason = f.get("reason");
    if !clear && !pct.is_some_and(|p| (0.0..=100.0).contains(&p)) {
        return flash_to(&state, &auth.session, "danger", "Enter a final grade between 0 and 100.", &back).await;
    }
    auth.db
        .call(move |c| {
            let (_, cl) = model::subject(c, uid, subject_id)?;
            if cl.id != cid {
                return Err(crate::error::AppError::NotFound);
            }
            model::class_mut(c, uid, cid)?;
            model::student_in_class(c, cid, sid)?;
            if clear {
                c.execute("DELETE FROM grade_overrides WHERE student_id = ?1 AND subject_id = ?2", params![sid, subject_id])?;
            } else {
                c.execute(
                    "INSERT INTO grade_overrides (student_id, subject_id, pct, reason) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(student_id, subject_id) DO UPDATE SET pct = excluded.pct, reason = excluded.reason",
                    params![sid, subject_id, pct, reason],
                )?;
            }
            Ok(())
        })
        .await?;
    let msg = if clear { "Final grade override removed; the calculated grade is used again." } else { "Final grade set." };
    flash_to(&state, &auth.session, "success", msg, &back).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders() {
        let s = Student {
            id: 1,
            first_name: "Alexandra".into(),
            last_name: "Smith".into(),
            preferred_name: "Alex".into(),
            student_number: String::new(),
            pronouns: "they".into(),
        };
        assert_eq!(
            personalise("{first} works hard. {Their} effort shows; {they} {has} improved.", &s),
            "Alex works hard. Their effort shows; they have improved."
        );
        let s = Student { pronouns: "she".into(), ..s };
        assert_eq!(personalise("{They} {is} kind to {their} peers.", &s), "She is kind to her peers.");
    }
}
