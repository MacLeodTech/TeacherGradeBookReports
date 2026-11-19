use axum::{
    extract::{Path, Query, State},
    http::header,
    response::{IntoResponse, Response},
};
use rusqlite::{Connection, params};
use serde::Deserialize;

use super::{F, flash_to, non_empty, render, to};
use crate::{
    AppState,
    auth::Auth,
    engine::fmt_num,
    error::{AppResult, bad},
    model::{self, Class, Mode, Settings},
    util::{content_disposition, file_stem, name_key},
};

#[derive(serde::Serialize)]
struct ClassCard {
    class: Class,
    students: usize,
    average: Option<f64>,
    label: String,
    color: String,
    missing: usize,
    pending: usize,
    subjects: usize,
}

fn class_card(c: &Connection, settings: &Settings, class: Class) -> AppResult<ClassCard> {
    let roster = model::roster(c, class.id)?;
    let subjects = model::subjects(c, class.id)?;
    let (mut sum, mut n, mut missing, mut pending) = (0.0, 0usize, 0, 0);
    for s in &subjects {
        let sd = model::subject_data(c, s.clone(), settings.missing_policy)?;
        for st in &roster {
            let r = sd.result_for(st.id);
            if let Some(o) = r.overall {
                sum += o;
                n += 1;
            }
            missing += r.missing;
            pending += r.pending;
        }
    }
    let average = (n > 0).then(|| sum / n as f64);
    Ok(ClassCard {
        label: average.map(|a| settings.scale.label(a)).unwrap_or_default(),
        color: average.map(|a| settings.scale.color(a)).unwrap_or_else(|| "#6c757d".into()),
        students: roster.len(),
        average,
        missing,
        pending,
        subjects: subjects.len(),
        class,
    })
}

/// Periods that can still take a class (secondary mode).
fn free_periods(settings: &Settings, classes: &[Class]) -> Vec<i64> {
    (1..=4).filter(|p| Some(*p) != settings.prep_period && !classes.iter().any(|c| c.period == Some(*p))).collect()
}

fn can_add_class(settings: &Settings, classes: &[Class]) -> Result<(), String> {
    if settings.mode == Mode::Secondary && classes.len() as i64 >= settings.active_periods {
        return Err(format!(
            "You're set up for {} teaching period(s) and already have {} class(es) this term. Change the number of periods in Settings to add more.",
            settings.active_periods,
            classes.len()
        ));
    }
    Ok(())
}

pub async fn dashboard(State(state): State<AppState>, auth: Auth) -> AppResult<Response> {
    let uid = auth.user.id;
    let data = auth
        .db
        .call(move |c| {
            let settings = model::settings(c, uid)?;
            if !settings.setup_done {
                return Ok(None);
            }
            let term = model::current_term(c, uid)?;
            let (cards, add_error, terms) = match &term {
                Some(t) => {
                    let classes = model::classes_in_term(c, uid, t.id)?;
                    let add_error = can_add_class(&settings, &classes).err();
                    let cards = classes.into_iter().map(|cl| class_card(c, &settings, cl)).collect::<AppResult<Vec<_>>>()?;
                    (cards, add_error, model::terms(c, uid)?)
                }
                None => (vec![], None, model::terms(c, uid)?),
            };
            Ok(Some((term, cards, add_error, terms, settings)))
        })
        .await?;
    let Some((term, cards, add_error, terms, settings)) = data else { return Ok(to("/setup")) };
    // High school: lay cards out by period, with the prep period in its slot.
    let mut slots = Vec::new();
    if settings.mode == Mode::Secondary {
        for p in 1..=4 {
            if let Some(card) = cards.iter().find(|c| c.class.period == Some(p)) {
                slots.push(ctx! { prep => false, card });
            } else if settings.prep_period == Some(p) {
                slots.push(ctx! { prep => true, period => p });
            }
        }
    }
    for card in cards.iter().filter(|c| settings.mode != Mode::Secondary || c.class.period.is_none()) {
        slots.push(ctx! { prep => false, card });
    }
    let has_cards = !cards.is_empty();
    render(&state, &auth, "dashboard.html", ctx! { term, slots, has_cards, add_error, terms }).await
}

pub async fn new_page(State(state): State<AppState>, auth: Auth) -> AppResult<Response> {
    let uid = auth.user.id;
    let (term, free, add_error) = auth
        .db
        .call(move |c| {
            let settings = model::settings(c, uid)?;
            let term = model::current_term(c, uid)?.ok_or_else(|| bad("Create a term first (Terms page)."))?;
            let classes = model::classes_in_term(c, uid, term.id)?;
            Ok((term, free_periods(&settings, &classes), can_add_class(&settings, &classes).err()))
        })
        .await?;
    if let Some(e) = add_error {
        return flash_to(&state, &auth.session, "warning", e, "/").await;
    }
    render(&state, &auth, "class_new.html", ctx! { term, free_periods => free }).await
}

pub async fn create(State(state): State<AppState>, auth: Auth, f: F) -> AppResult<Response> {
    let uid = auth.user.id;
    let name = f.get("name");
    let code = f.get("course_code");
    let display = non_empty(name.clone()).or_else(|| non_empty(code.clone()));
    let Some(display) = display else {
        return flash_to(&state, &auth.session, "danger", "Give the class a name.", "/classes/new").await;
    };
    let (period, grade, room, preset) = (f.int("period"), f.get("grade_level"), f.get("room"), f.get("preset"));
    let subject_lines: Vec<String> = f.raw("subjects").lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    let letter_grades = f.has("letter_grades");
    let created = auth
        .db
        .call(move |c| {
            let settings = model::settings(c, uid)?;
            let term = model::current_term(c, uid)?.ok_or_else(|| bad("Create a term first."))?;
            let classes = model::classes_in_term(c, uid, term.id)?;
            if let Err(e) = can_add_class(&settings, &classes) {
                return Ok(Err(e));
            }
            let period = if settings.mode == Mode::Secondary {
                match period {
                    Some(p) if free_periods(&settings, &classes).contains(&p) => Some(p),
                    Some(p) => return Ok(Err(format!("Period {p} is already taken or is your prep period."))),
                    None => None,
                }
            } else {
                None
            };
            let tx = c.transaction()?;
            tx.execute(
                "INSERT INTO classes (user_id, term_id, name, course_code, period, grade_level, room) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![uid, term.id, display, code, period, grade, room],
            )?;
            let class_id = tx.last_insert_rowid();
            if settings.mode == Mode::Elementary && !subject_lines.is_empty() {
                for (i, s) in subject_lines.iter().enumerate() {
                    tx.execute(
                        "INSERT INTO subjects (class_id, name, report_style, sort) VALUES (?1, ?2, ?3, ?4)",
                        params![class_id, s, if letter_grades { "letter" } else { "percent" }, i as i64],
                    )?;
                }
            } else {
                model::create_default_subjects(&tx, class_id, settings.mode, &display)?;
            }
            if let Some(cats) = model::category_preset(&preset) {
                for s in model::subjects(&tx, class_id)? {
                    for (i, (n, w)) in cats.iter().enumerate() {
                        tx.execute(
                            "INSERT INTO categories (subject_id, name, weight, sort) VALUES (?1, ?2, ?3, ?4)",
                            params![s.id, n, w, i as i64],
                        )?;
                    }
                }
            }
            tx.commit()?;
            Ok(Ok(class_id))
        })
        .await?;
    match created {
        Ok(id) => {
            flash_to(&state, &auth.session, "success", "Class created. Add your students next.", &format!("/classes/{id}/students")).await
        }
        Err(e) => flash_to(&state, &auth.session, "danger", e, "/classes/new").await,
    }
}

#[derive(Deserialize)]
pub struct SubjectQuery {
    subject: Option<i64>,
}

pub async fn grid_page(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Query(q): Query<SubjectQuery>,
) -> AppResult<Response> {
    let uid = auth.user.id;
    let (class, subjects, roster_n) = auth
        .db
        .call(move |c| {
            let class = model::class(c, uid, id)?;
            Ok((class, model::subjects(c, id)?, model::roster(c, id)?.len()))
        })
        .await?;
    let current = q.subject.and_then(|s| subjects.iter().find(|x| x.id == s)).or(subjects.first()).cloned();
    render(&state, &auth, "grid.html", ctx! { class, subjects, current, roster_n }).await
}

pub async fn settings_page(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> AppResult<Response> {
    let uid = auth.user.id;
    let (class, subjects) = auth
        .db
        .call(move |c| {
            let class = model::class(c, uid, id)?;
            let mut subjects = Vec::new();
            for s in model::subjects(c, id)? {
                let cats = model::categories(c, s.id)?;
                let total: f64 = cats.iter().map(|c| c.weight).sum();
                let n_assess = model::assessments(c, s.id)?.len();
                subjects.push(ctx! { subject => s, categories => cats, total => fmt_num(total), total_ok => cats.is_empty() || (total - 100.0).abs() < 0.01, n_assess });
            }
            Ok((class, subjects))
        })
        .await?;
    render(&state, &auth, "class_settings.html", ctx! { class, subjects }).await
}

pub async fn update(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>, f: F) -> AppResult<Response> {
    let uid = auth.user.id;
    let Some(name) = non_empty(f.get("name")) else {
        return flash_to(&state, &auth.session, "danger", "Give the class a name.", &format!("/classes/{id}/settings")).await;
    };
    let (code, period, grade, room) = (f.get("course_code"), f.int("period"), f.get("grade_level"), f.get("room"));
    let res = auth
        .db
        .call(move |c| {
            let class = model::class_mut(c, uid, id)?;
            let settings = model::settings(c, uid)?;
            let others: Vec<Class> = model::classes_in_term(c, uid, class.term_id)?.into_iter().filter(|x| x.id != id).collect();
            if let Some(p) = period
                && settings.mode == Mode::Secondary
                && !free_periods(&settings, &others).contains(&p)
            {
                return Ok(Err(format!("Period {p} is already taken or is your prep period.")));
            }
            c.execute(
                "UPDATE classes SET name = ?1, course_code = ?2, period = ?3, grade_level = ?4, room = ?5 WHERE id = ?6",
                params![name, code, period.filter(|_| settings.mode == Mode::Secondary), grade, room, id],
            )?;
            // A one-subject class keeps its subject named after the class.
            let subjects = model::subjects(c, id)?;
            if settings.mode != Mode::Elementary && subjects.len() == 1 && subjects[0].name == class.name {
                c.execute("UPDATE subjects SET name = ?1 WHERE id = ?2", params![name, subjects[0].id])?;
            }
            Ok(Ok(()))
        })
        .await?;
    match res {
        Ok(()) => flash_to(&state, &auth.session, "success", "Class saved.", &format!("/classes/{id}/settings")).await,
        Err(e) => flash_to(&state, &auth.session, "danger", e, &format!("/classes/{id}/settings")).await,
    }
}

pub async fn delete(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>, f: F) -> AppResult<Response> {
    let uid = auth.user.id;
    let typed = f.get("confirm_name");
    let ok = auth
        .db
        .call(move |c| {
            let class = model::class(c, uid, id)?;
            if typed != class.name {
                return Ok(false);
            }
            c.execute("DELETE FROM classes WHERE id = ?1", [id])?;
            c.execute("DELETE FROM students WHERE user_id = ?1 AND id NOT IN (SELECT student_id FROM enrolments)", [uid])?;
            Ok(true)
        })
        .await?;
    if ok {
        flash_to(&state, &auth.session, "success", "Class deleted.", "/").await
    } else {
        flash_to(&state, &auth.session, "danger", "Type the class name exactly to delete it.", &format!("/classes/{id}/settings")).await
    }
}

pub async fn subject_add(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>, f: F) -> AppResult<Response> {
    let uid = auth.user.id;
    let Some(name) = non_empty(f.get("name")) else {
        return flash_to(&state, &auth.session, "danger", "Give the subject a name.", &format!("/classes/{id}/settings")).await;
    };
    let style = if f.get("report_style") == "letter" { "letter" } else { "percent" };
    auth
        .db
        .call(move |c| {
            model::class_mut(c, uid, id)?;
            c.execute(
                "INSERT INTO subjects (class_id, name, report_style, sort) VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(sort), 0) + 1 FROM subjects WHERE class_id = ?1))",
                params![id, name, style],
            )?;
            Ok(())
        })
        .await?;
    flash_to(&state, &auth.session, "success", "Subject added.", &format!("/classes/{id}/settings")).await
}

pub async fn subject_update(State(state): State<AppState>, auth: Auth, Path((id, sid)): Path<(i64, i64)>, f: F) -> AppResult<Response> {
    let uid = auth.user.id;
    let Some(name) = non_empty(f.get("name")) else {
        return flash_to(&state, &auth.session, "danger", "Give the subject a name.", &format!("/classes/{id}/settings")).await;
    };
    let style = if f.get("report_style") == "letter" { "letter" } else { "percent" };
    auth.db
        .call(move |c| {
            let (_, cl) = model::subject(c, uid, sid)?;
            model::class_mut(c, uid, cl.id)?;
            c.execute("UPDATE subjects SET name = ?1, report_style = ?2 WHERE id = ?3", params![name, style, sid])?;
            Ok(())
        })
        .await?;
    flash_to(&state, &auth.session, "success", "Subject saved.", &format!("/classes/{id}/settings")).await
}

pub async fn subject_delete(State(state): State<AppState>, auth: Auth, Path((id, sid)): Path<(i64, i64)>) -> AppResult<Response> {
    let uid = auth.user.id;
    let res = auth
        .db
        .call(move |c| {
            let (_, cl) = model::subject(c, uid, sid)?;
            model::class_mut(c, uid, cl.id)?;
            if model::subjects(c, cl.id)?.len() <= 1 {
                return Ok(Err("A class needs at least one subject."));
            }
            c.execute("DELETE FROM subjects WHERE id = ?1", [sid])?;
            Ok(Ok(()))
        })
        .await?;
    match res {
        Ok(()) => flash_to(&state, &auth.session, "success", "Subject deleted.", &format!("/classes/{id}/settings")).await,
        Err(e) => flash_to(&state, &auth.session, "danger", e, &format!("/classes/{id}/settings")).await,
    }
}

pub async fn categories_save(State(state): State<AppState>, auth: Auth, Path((id, sid)): Path<(i64, i64)>, f: F) -> AppResult<Response> {
    let uid = auth.user.id;
    let preset = f.get("preset");
    let rows: Vec<(Option<i64>, String, f64)> = {
        let (ids, names, weights) = (f.all("cat_id"), f.all("cat_name"), f.all("cat_weight"));
        names
            .iter()
            .enumerate()
            .filter(|(_, n)| !n.is_empty())
            .map(|(i, n)| {
                (
                    ids.get(i).and_then(|x| x.parse().ok()),
                    n.chars().take(80).collect(),
                    weights.get(i).and_then(|w| w.trim_end_matches('%').trim().parse().ok()).unwrap_or(0.0f64).clamp(0.0, 1000.0),
                )
            })
            .collect()
    };
    let msg = auth
        .db
        .call(move |c| {
            let (_, cl) = model::subject(c, uid, sid)?;
            model::class_mut(c, uid, cl.id)?;
            let tx = c.transaction()?;
            if let Some(cats) = model::category_preset(&preset).filter(|_| !preset.is_empty()) {
                tx.execute("DELETE FROM categories WHERE subject_id = ?1", [sid])?;
                let mut first = None;
                for (i, (n, w)) in cats.iter().enumerate() {
                    tx.execute(
                        "INSERT INTO categories (subject_id, name, weight, sort) VALUES (?1, ?2, ?3, ?4)",
                        params![sid, n, w, i as i64],
                    )?;
                    first.get_or_insert(tx.last_insert_rowid());
                }
                tx.execute("UPDATE assessments SET category_id = ?1 WHERE subject_id = ?2 AND kind = 'scored'", params![first, sid])?;
                tx.commit()?;
                return Ok(
                    "Categories replaced with the preset. Existing assessments were put in the first category; adjust them from the grid."
                        .to_string(),
                );
            }
            let existing = model::categories(&tx, sid)?;
            let keep: Vec<i64> = rows.iter().filter_map(|r| r.0).collect();
            for e in existing.iter().filter(|e| !keep.contains(&e.id)) {
                tx.execute("DELETE FROM categories WHERE id = ?1", [e.id])?;
            }
            for (i, (cid, name, weight)) in rows.iter().enumerate() {
                match cid.filter(|c| existing.iter().any(|e| e.id == *c)) {
                    Some(cid) => {
                        tx.execute(
                            "UPDATE categories SET name = ?1, weight = ?2, sort = ?3 WHERE id = ?4",
                            params![name, weight, i as i64, cid],
                        )?;
                    }
                    None => {
                        tx.execute(
                            "INSERT INTO categories (subject_id, name, weight, sort) VALUES (?1, ?2, ?3, ?4)",
                            params![sid, name, weight, i as i64],
                        )?;
                    }
                }
            }
            let total: f64 = rows.iter().map(|r| r.2).sum();
            tx.commit()?;
            Ok(if rows.is_empty() || (total - 100.0).abs() < 0.01 {
                "Categories saved.".into()
            } else {
                format!("Categories saved. Note: the weights add up to {}%, so they'll be scaled to 100%.", fmt_num(total))
            })
        })
        .await?;
    flash_to(&state, &auth.session, "success", msg, &format!("/classes/{id}/settings#subject-{sid}")).await
}

// ── Roster ───────────────────────────────────────────────────────────────────

pub async fn roster_page(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> AppResult<Response> {
    let uid = auth.user.id;
    let (class, roster, others) = auth
        .db
        .call(move |c| {
            let class = model::class(c, uid, id)?;
            let roster = model::roster(c, id)?;
            // Students in this term's other classes, for quick adding.
            let mut st = c.prepare(
                "SELECT DISTINCT s.id, s.first_name, s.last_name FROM students s JOIN enrolments e ON e.student_id = s.id
                 JOIN classes c ON c.id = e.class_id WHERE c.term_id = ?1 AND c.user_id = ?2 AND e.active = 1
                 AND s.id NOT IN (SELECT student_id FROM enrolments WHERE class_id = ?3 AND active = 1)
                 ORDER BY s.last_name, s.first_name",
            )?;
            let others: Vec<(i64, String)> = st
                .query_map(params![class.term_id, uid, id], |r| {
                    Ok((r.get(0)?, format!("{}, {}", r.get::<_, String>(2)?, r.get::<_, String>(1)?)))
                })?
                .collect::<Result<_, _>>()?;
            Ok((class, roster, others))
        })
        .await?;
    render(&state, &auth, "roster.html", ctx! { class, roster, others }).await
}

fn parse_name_line(line: &str) -> Option<(String, String, String)> {
    let mut parts = line.split('\t').map(str::trim);
    let name = parts.next()?.trim();
    let number = parts.next().unwrap_or("").to_string();
    if name.is_empty() {
        return None;
    }
    let (first, last) = if let Some((last, first)) = name.split_once(',') {
        (first.trim().to_string(), last.trim().to_string())
    } else {
        match name.rsplit_once(' ') {
            Some((f, l)) => (f.trim().to_string(), l.trim().to_string()),
            None => (name.to_string(), String::new()),
        }
    };
    Some((first, last, number))
}

pub async fn roster_add(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>, f: F) -> AppResult<Response> {
    let uid = auth.user.id;
    let mut entries: Vec<(String, String, String)> = Vec::new();
    if let Some(first) = non_empty(f.get("first_name")) {
        entries.push((first, f.get("last_name"), f.get("student_number")));
    }
    entries.extend(f.raw("bulk").lines().filter_map(parse_name_line));
    let existing_ids: Vec<i64> = f.all("existing").iter().filter_map(|x| x.parse().ok()).collect();
    if entries.is_empty() && existing_ids.is_empty() {
        return flash_to(&state, &auth.session, "warning", "Nothing to add.", &format!("/classes/{id}/students")).await;
    }
    let (added, reused) = auth
        .db
        .call(move |c| {
            model::class_mut(c, uid, id)?;
            let everyone = model::all_students(c, uid)?;
            let tx = c.transaction()?;
            let (mut added, mut reused) = (0, 0);
            let enrol = |tx: &rusqlite::Transaction, sid: i64| -> rusqlite::Result<()> {
                tx.execute(
                    "INSERT INTO enrolments (class_id, student_id, active) VALUES (?1, ?2, 1) ON CONFLICT DO UPDATE SET active = 1",
                    params![id, sid],
                )?;
                Ok(())
            };
            for sid in existing_ids {
                if everyone.iter().any(|s| s.id == sid) {
                    enrol(&tx, sid)?;
                    reused += 1;
                }
            }
            for (first, last, number) in entries {
                let key = name_key(&format!("{first} {last}"));
                let found = everyone.iter().find(|s| {
                    (!number.is_empty() && s.student_number == number)
                        || (number.is_empty() && name_key(&format!("{} {}", s.first_name, s.last_name)) == key)
                });
                match found {
                    Some(s) => {
                        enrol(&tx, s.id)?;
                        reused += 1;
                    }
                    None => {
                        tx.execute(
                            "INSERT INTO students (user_id, first_name, last_name, student_number) VALUES (?1, ?2, ?3, ?4)",
                            params![uid, first, last, number],
                        )?;
                        enrol(&tx, tx.last_insert_rowid())?;
                        added += 1;
                    }
                }
            }
            tx.commit()?;
            Ok((added, reused))
        })
        .await?;
    let mut msg = format!("Added {added} new student(s).");
    if reused > 0 {
        msg.push_str(&format!(" {reused} existing student(s) were enrolled (same name or number as a student you already have)."));
    }
    flash_to(&state, &auth.session, "success", msg, &format!("/classes/{id}/students")).await
}

pub async fn roster_remove(State(state): State<AppState>, auth: Auth, Path((id, sid)): Path<(i64, i64)>) -> AppResult<Response> {
    let uid = auth.user.id;
    auth.db
        .call(move |c| {
            model::class_mut(c, uid, id)?;
            model::student_in_class(c, id, sid)?;
            c.execute("UPDATE enrolments SET active = 0 WHERE class_id = ?1 AND student_id = ?2", params![id, sid])?;
            Ok(())
        })
        .await?;
    flash_to(
        &state,
        &auth.session,
        "success",
        "Student removed from the class. Their marks are kept and come back if you re-add them.",
        &format!("/classes/{id}/students"),
    )
    .await
}

// ── Insights ─────────────────────────────────────────────────────────────────

pub async fn insights(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> AppResult<Response> {
    let uid = auth.user.id;
    let (class, subjects) = auth
        .db
        .call(move |c| {
            let class = model::class(c, uid, id)?;
            let settings = model::settings(c, uid)?;
            let roster = model::roster(c, id)?;
            let mut out = Vec::new();
            for s in model::subjects(c, id)? {
                let sd = model::subject_data(c, s, settings.missing_policy)?;
                let results: Vec<_> = roster.iter().map(|st| (st, sd.result_for(st.id))).collect();
                let graded: Vec<f64> = results.iter().filter_map(|(_, r)| r.overall).collect();
                let average = (!graded.is_empty()).then(|| graded.iter().sum::<f64>() / graded.len() as f64);
                let median = {
                    let mut g = graded.clone();
                    g.sort_by(f64::total_cmp);
                    (!g.is_empty()).then(|| if g.len() % 2 == 1 { g[g.len() / 2] } else { (g[g.len() / 2 - 1] + g[g.len() / 2]) / 2.0 })
                };
                // Distribution by scale band.
                let mut dist: Vec<(String, String, usize)> = settings.scale.bands.iter().map(|b| (b.label.clone(), b.color.clone(), 0)).collect();
                for g in &graded {
                    if let Some(b) = settings.scale.band(*g)
                        && let Some(d) = dist.iter_mut().find(|d| d.0 == b.label) {
                            d.2 += 1;
                        }
                }
                let max = dist.iter().map(|d| d.2).max().unwrap_or(0).max(1);
                let dist: Vec<_> = dist.into_iter().map(|(l, c, n)| ctx! { label => l, color => c, n, pct => n * 100 / max }).collect();
                let at_risk: Vec<_> = results
                    .iter()
                    .filter(|(_, r)| r.overall.is_some_and(|o| o < 60.0) || r.missing >= 3)
                    .map(|(st, r)| ctx! {
                        id => st.id, name => st.sort_name(), overall => r.overall,
                        label => r.overall.map(|o| settings.scale.label(o)), color => r.overall.map(|o| settings.scale.color(o)),
                        missing => r.missing,
                    })
                    .collect();
                let assessments: Vec<_> = sd
                    .assessments
                    .iter()
                    .filter(|a| a.kind == "scored")
                    .map(|a| {
                        let ai = a.engine();
                        let pcts: Vec<f64> = roster.iter().filter_map(|st| sd.mark(st.id, a.id).and_then(|m| crate::engine::mark_pct(&ai, m))).collect();
                        let count = |s: crate::engine::Status| roster.iter().filter(|st| sd.mark(st.id, a.id).is_some_and(|m| m.status == s)).count();
                        let avg = (!pcts.is_empty()).then(|| pcts.iter().sum::<f64>() / pcts.len() as f64);
                        ctx! {
                            name => a.name, unit => a.unit, avg, color => avg.map(|v| settings.scale.color(v)),
                            marked => pcts.len(), missing => count(crate::engine::Status::Missing), pending => count(crate::engine::Status::Pending),
                            blank => roster.iter().filter(|st| sd.mark(st.id, a.id).is_none()).count(),
                        }
                    })
                    .collect();
                out.push(ctx! {
                    subject => sd.subject, average, median, graded => graded.len(), students => roster.len(),
                    avg_label => average.map(|a| settings.scale.label(a)), avg_color => average.map(|a| settings.scale.color(a)),
                    dist, at_risk, assessments,
                });
            }
            Ok((class, out))
        })
        .await?;
    render(&state, &auth, "insights.html", ctx! { class, subjects }).await
}

pub async fn export_xlsx(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> AppResult<Response> {
    crate::audit::log(&state, Some(auth.user.id), &auth.user.username, "", "class.export", format!("class {id}")).await;
    let uid = auth.user.id;
    let (class, bytes) = auth
        .db
        .call(move |c| {
            let class = model::class(c, uid, id)?;
            let settings = model::settings(c, uid)?;
            let roster = model::roster(c, id)?;
            let sds = model::subjects(c, id)?
                .into_iter()
                .map(|s| model::subject_data(c, s, settings.missing_policy))
                .collect::<AppResult<Vec<_>>>()?;
            let bytes = crate::export::class_workbook(&roster, &sds, &settings.scale)?;
            Ok((class, bytes))
        })
        .await?;
    let name = format!("{}_{}.xlsx", file_stem(&[&class.name]), chrono::Local::now().format("%Y-%m-%d"));
    Ok((
        [
            (header::CONTENT_TYPE, "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string()),
            (header::CONTENT_DISPOSITION, content_disposition("attachment", &name)),
        ],
        bytes,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    #[test]
    fn name_lines() {
        assert_eq!(super::parse_name_line("Lefèvre, Zoë\t123"), Some(("Zoë".into(), "Lefèvre".into(), "123".into())));
        assert_eq!(super::parse_name_line("Mary Ann Smith"), Some(("Mary Ann".into(), "Smith".into(), "".into())));
        assert_eq!(super::parse_name_line("   "), None);
    }
}
