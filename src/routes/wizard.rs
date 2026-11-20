//! The guided setup: a few friendly steps (class, students, how marks are
//! weighted, first assignments) kept in the browser until the teacher finishes,
//! then created in one go. Creation reuses the spreadsheet importer, so
//! guessed details are flagged for review exactly like a spreadsheet's.

use axum::{Json, extract::State, response::Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    api::j,
    classes::{NewClass, can_add_class, free_periods, insert_class},
    flash_to, render,
};
use crate::{
    AppState,
    auth::Auth,
    engine::Kind,
    error::{AppError, AppResult, bad},
    model::{self, Mode},
    starter::{self, AssessmentSetup, ColumnIn, Parsed, SheetIn, StudentIn},
};

const MAX_CLASSES: usize = 12;
const MAX_STUDENTS: usize = 400;
const MAX_ASSESSMENTS: usize = 80;

#[derive(Serialize)]
struct Preset {
    value: &'static str,
    title: &'static str,
    detail: &'static str,
    categories: Vec<(String, f64)>,
}

/// The ways a final grade can be weighted, in plain words, for the teacher's level.
fn presets(mode: Mode) -> Vec<Preset> {
    let p = |value: &'static str, title: &'static str, detail: &'static str| Preset {
        value,
        title,
        detail,
        categories: model::category_preset(value).unwrap_or_default().into_iter().map(|(n, w)| (n.to_string(), w)).collect(),
    };
    let equal = p(
        "none",
        "Count everything equally",
        "Every assignment, quiz and test counts the same. Good for a start: you can add categories later.",
    );
    let ktca = p(
        "ontario-ktca",
        "Four achievement categories",
        "Knowledge, Thinking, Communication and Application, each worth a quarter (the Ontario achievement chart).",
    );
    match mode {
        Mode::Secondary => vec![
            p(
                "ontario-secondary",
                "Ontario high school: 70% coursework, 30% final",
                "Coursework is split across the four achievement categories; the final or culminating task is 30%.",
            ),
            p("ontario-70-30", "Coursework 70%, final 30%", "One term-work category and one final evaluation."),
            ktca,
            equal,
        ],
        Mode::University => {
            vec![p("university", "Assignments 40%, midterm 20%, final exam 40%", "A common college or university split."), equal]
        }
        Mode::Elementary => vec![equal, ktca],
    }
}

pub async fn page(State(state): State<AppState>, auth: Auth) -> AppResult<Response> {
    let uid = auth.user.id;
    let data = auth
        .db
        .call(move |c| {
            let settings = model::settings(c, uid)?;
            let Some(term) = model::current_term(c, uid)? else { return Ok(None) };
            let classes = model::classes_in_term(c, uid, term.id)?;
            let limit = can_add_class(&settings, &classes).err();
            let free = free_periods(&settings, &classes);
            let names: Vec<String> = classes.iter().map(|c| c.name.clone()).collect();
            Ok(Some((settings, term, classes.len(), limit, free, names)))
        })
        .await?;
    let Some((settings, term, existing, limit, free, names)) = data else {
        return flash_to(&state, &auth.session, "warning", "Create a term first, then set up your classes.", "/terms").await;
    };
    if let Some(e) = limit {
        return flash_to(&state, &auth.session, "warning", e, "/").await;
    }
    let presets = presets(settings.mode);
    let config = json!({
        "mode": settings.mode.as_str(),
        "freePeriods": free,
        "presets": presets,
        "existing": names,
        "maxClasses": MAX_CLASSES,
    });
    let first_run = existing == 0;
    let config_json = config.to_string();
    render(&state, &auth, "start.html", ctx! { term, first_run, config_json, settings }).await
}

#[derive(Deserialize)]
pub struct WizardIn {
    classes: Vec<ClassIn>,
}

#[derive(Deserialize)]
struct ClassIn {
    name: String,
    #[serde(default)]
    course_code: String,
    period: Option<i64>,
    #[serde(default)]
    grade_level: String,
    #[serde(default)]
    room: String,
    #[serde(default)]
    preset: String,
    #[serde(default)]
    subjects: Vec<String>,
    #[serde(default)]
    letter_grades: bool,
    #[serde(default)]
    students: Vec<PersonIn>,
    #[serde(default)]
    assessments: Vec<AssessmentIn>,
}

#[derive(Deserialize)]
struct PersonIn {
    first: String,
    last: String,
}

#[derive(Deserialize)]
struct AssessmentIn {
    name: String,
    /// `None`: the teacher will say later.
    out_of: Option<f64>,
    #[serde(default)]
    kind: String,
    /// Empty: decide later.
    #[serde(default)]
    category: String,
    /// Elementary: which subject.
    #[serde(default)]
    subject: String,
}

fn clip(s: &str, n: usize) -> String {
    s.trim().chars().take(n).collect()
}

pub async fn create(auth: Auth, Json(input): Json<WizardIn>) -> AppResult<Json<Value>> {
    let uid = auth.user.id;
    auth.db
        .call(move |c| {
            if input.classes.is_empty() {
                return Err(bad("Add at least one class."));
            }
            if input.classes.len() > MAX_CLASSES {
                return Err(bad(format!("Set up at most {MAX_CLASSES} classes at a time.")));
            }
            let settings = model::settings(c, uid)?;
            let term = model::current_term(c, uid)?.ok_or_else(|| bad("Create a term first."))?;
            let tx = c.transaction()?;
            let mut parsed = Parsed::default();
            let mut created = Vec::new();
            for cl in &input.classes {
                let name = clip(&cl.name, 120);
                if name.is_empty() {
                    return Err(bad("Every class needs a name."));
                }
                let classes = model::classes_in_term(&tx, uid, term.id)?;
                if classes.iter().any(|x| x.name.eq_ignore_ascii_case(&name)) {
                    return Err(bad(format!("You already have a class called {name} this term.")));
                }
                can_add_class(&settings, &classes).map_err(bad)?;
                let period = match (settings.mode, cl.period) {
                    (Mode::Secondary, Some(p)) if free_periods(&settings, &classes).contains(&p) => Some(p),
                    (Mode::Secondary, Some(p)) => return Err(bad(format!("Period {p} is already taken or is your prep period."))),
                    _ => None,
                };
                let preset = if model::category_preset(&cl.preset).is_some() { cl.preset.clone() } else { "none".to_string() };
                let mut subjects: Vec<String> = Vec::new();
                if settings.mode == Mode::Elementary {
                    for s in cl.subjects.iter().map(|s| clip(s, 80)).filter(|s| !s.is_empty()) {
                        if !subjects.iter().any(|x| x.eq_ignore_ascii_case(&s)) {
                            subjects.push(s);
                        }
                    }
                }
                insert_class(
                    &tx,
                    uid,
                    term.id,
                    &settings,
                    &NewClass {
                        name: name.clone(),
                        code: clip(&cl.course_code, 30),
                        period,
                        grade: clip(&cl.grade_level, 20),
                        room: clip(&cl.room, 30),
                        preset,
                        subject_lines: subjects.clone(),
                        letter_grades: cl.letter_grades,
                    },
                )?;
                created.push(name.clone());

                if cl.students.len() > MAX_STUDENTS {
                    return Err(bad(format!("{name} has more than {MAX_STUDENTS} students.")));
                }
                if cl.assessments.len() > MAX_ASSESSMENTS {
                    return Err(bad(format!("{name} has more than {MAX_ASSESSMENTS} assignments to start with; add the rest in the grid.")));
                }
                let students: Vec<StudentIn> = cl
                    .students
                    .iter()
                    .map(|p| StudentIn { first: clip(&p.first, 60), last: clip(&p.last, 60), ..Default::default() })
                    .filter(|s| !s.first.is_empty() || !s.last.is_empty())
                    .collect();
                // One sheet per subject (elementary), otherwise one for the class.
                let sheet_subjects: Vec<String> = if subjects.is_empty() { vec![String::new()] } else { subjects.clone() };
                for (i, subject) in sheet_subjects.iter().enumerate() {
                    let mine = cl.assessments.iter().filter(|a| {
                        let wanted = clip(&a.subject, 80);
                        if sheet_subjects.len() == 1 { true } else if wanted.is_empty() { i == 0 } else { wanted.eq_ignore_ascii_case(subject) }
                    });
                    let mut cols = Vec::new();
                    for a in mine {
                        let aname = clip(&a.name, 120);
                        if aname.is_empty() {
                            continue;
                        }
                        let kind = Kind::parse(&a.kind);
                        cols.push(ColumnIn {
                            heading: aname.clone(),
                            name: aname,
                            kind: Some(kind),
                            out_of: if kind == Kind::Scored { a.out_of.filter(|v| *v > 0.0 && *v <= 10_000.0) } else { None },
                            setup: AssessmentSetup { category: Some(clip(&a.category, 80)).filter(|c| !c.is_empty()), ..Default::default() },
                            ..Default::default()
                        });
                    }
                    parsed.sheets.push(SheetIn {
                        sheet: "Setup".into(),
                        class: name.clone(),
                        subject: subject.clone(),
                        assessments: cols,
                        students: if i == 0 { students.clone() } else { vec![] },
                        ..Default::default()
                    });
                }
            }
            let out = starter::apply(&tx, uid, &parsed, false)?;
            let review: i64 =
                tx.query_row("SELECT count(*) FROM assessments WHERE needs_review > 0 AND subject_id IN (SELECT s.id FROM subjects s JOIN classes c ON c.id = s.class_id WHERE c.user_id = ?1)", [uid], |r| r.get(0))?;
            tx.commit()?;
            let first = model::classes_in_term(c, uid, term.id)?.into_iter().find(|x| created.contains(&x.name)).map(|x| x.id);
            Ok(json!({
                "classes": created.len(),
                "students": out.students_new,
                "assessments": out.assessments_new,
                "review": review,
                "first_class": first,
            }))
        })
        .await
        .map(Json)
        .map_err(|e| match e {
            AppError::BadRequest(_) | AppError::NotFound | AppError::Forbidden => j(e),
            other => other,
        })
}
