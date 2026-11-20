//! Loading teaching data from SQLite, with ownership checks.
//!
//! Every loader takes the teacher's `user_id` and returns `NotFound` for rows
//! that belong to someone else, so handlers can't forget the check.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::{
    engine::{self, AssessmentIn, CategoryIn, Kind, Levels, MarkEntry, MissingPolicy, Scale, Setup, Status, StudentResult},
    error::{AppError, AppResult},
};

// ── Settings ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Elementary,
    Secondary,
    University,
}

impl Mode {
    pub fn parse(s: &str) -> Mode {
        match s {
            "elementary" => Mode::Elementary,
            "university" => Mode::University,
            _ => Mode::Secondary,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Elementary => "elementary",
            Mode::Secondary => "secondary",
            Mode::University => "university",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Settings {
    pub mode: Mode,
    pub active_periods: i64,
    pub prep_period: Option<i64>,
    pub missing_policy: MissingPolicy,
    pub board_name: String,
    pub report_title: String,
    pub primary_color: String,
    pub accent_color: String,
    pub has_logo: bool,
    pub scale: Scale,
    pub levels: Levels,
    pub setup_done: bool,
}

pub fn settings(c: &Connection, user_id: i64) -> AppResult<Settings> {
    c.execute("INSERT OR IGNORE INTO teacher_settings (user_id) VALUES (?1)", [user_id])?;
    Ok(c.query_row(
        "SELECT mode, active_periods, prep_period, missing_policy, board_name, report_title,
                primary_color, accent_color, logo IS NOT NULL, scale_json, setup_done, levels_json
         FROM teacher_settings WHERE user_id = ?1",
        [user_id],
        |r| {
            let scale_json: Option<String> = r.get(9)?;
            Ok(Settings {
                mode: Mode::parse(&r.get::<_, String>(0)?),
                active_periods: r.get(1)?,
                prep_period: r.get(2)?,
                missing_policy: MissingPolicy::parse(&r.get::<_, String>(3)?),
                board_name: r.get(4)?,
                report_title: r.get(5)?,
                primary_color: r.get(6)?,
                accent_color: r.get(7)?,
                has_logo: r.get::<_, i64>(8)? != 0,
                scale: scale_json.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(Scale::ontario_letters),
                setup_done: r.get::<_, i64>(10)? != 0,
                levels: r.get::<_, Option<String>>(11)?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
            })
        },
    )?)
}

pub fn logo(c: &Connection, user_id: i64) -> AppResult<Option<(Vec<u8>, String)>> {
    Ok(c.query_row("SELECT logo, logo_mime FROM teacher_settings WHERE user_id = ?1 AND logo IS NOT NULL", [user_id], |r| {
        Ok((r.get(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_else(|| "image/png".into())))
    })
    .optional()?)
}

// ── Terms and classes ────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize)]
pub struct Term {
    pub id: i64,
    pub name: String,
    pub period_label: String,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub is_current: bool,
    pub archived: bool,
}

fn term_row(r: &rusqlite::Row) -> rusqlite::Result<Term> {
    Ok(Term {
        id: r.get(0)?,
        name: r.get(1)?,
        period_label: r.get(2)?,
        start_date: r.get(3)?,
        end_date: r.get(4)?,
        is_current: r.get::<_, i64>(5)? != 0,
        archived: r.get::<_, i64>(6)? != 0,
    })
}

const TERM_COLS: &str = "id, name, period_label, start_date, end_date, is_current, archived";

pub fn terms(c: &Connection, user_id: i64) -> AppResult<Vec<Term>> {
    let mut st = c.prepare(&format!(
        "SELECT {TERM_COLS} FROM terms WHERE user_id = ?1 ORDER BY archived, is_current DESC, COALESCE(start_date, created_at) DESC, id DESC"
    ))?;
    Ok(st.query_map([user_id], term_row)?.collect::<Result<_, _>>()?)
}

pub fn term(c: &Connection, user_id: i64, id: i64) -> AppResult<Term> {
    c.query_row(&format!("SELECT {TERM_COLS} FROM terms WHERE id = ?1 AND user_id = ?2"), params![id, user_id], term_row)
        .optional()?
        .ok_or(AppError::NotFound)
}

pub fn current_term(c: &Connection, user_id: i64) -> AppResult<Option<Term>> {
    Ok(c.query_row(
        &format!("SELECT {TERM_COLS} FROM terms WHERE user_id = ?1 AND archived = 0 ORDER BY is_current DESC, id DESC LIMIT 1"),
        [user_id],
        term_row,
    )
    .optional()?)
}

#[derive(Clone, Debug, Serialize)]
pub struct Class {
    pub id: i64,
    pub term_id: i64,
    pub name: String,
    pub course_code: String,
    pub period: Option<i64>,
    pub grade_level: String,
    pub room: String,
    pub archived: bool,
    pub term_name: String,
}

const CLASS_COLS: &str = "c.id, c.term_id, c.name, c.course_code, c.period, c.grade_level, c.room, t.archived, t.name";

fn class_row(r: &rusqlite::Row) -> rusqlite::Result<Class> {
    Ok(Class {
        id: r.get(0)?,
        term_id: r.get(1)?,
        name: r.get(2)?,
        course_code: r.get(3)?,
        period: r.get(4)?,
        grade_level: r.get(5)?,
        room: r.get(6)?,
        archived: r.get::<_, i64>(7)? != 0,
        term_name: r.get(8)?,
    })
}

pub fn class(c: &Connection, user_id: i64, id: i64) -> AppResult<Class> {
    c.query_row(
        &format!("SELECT {CLASS_COLS} FROM classes c JOIN terms t ON t.id = c.term_id WHERE c.id = ?1 AND c.user_id = ?2"),
        params![id, user_id],
        class_row,
    )
    .optional()?
    .ok_or(AppError::NotFound)
}

/// Like [`class`] but refuses archived terms, for anything that writes.
pub fn class_mut(c: &Connection, user_id: i64, id: i64) -> AppResult<Class> {
    let cl = class(c, user_id, id)?;
    if cl.archived {
        return Err(AppError::BadRequest(format!(
            "{} is archived (read-only). Un-archive the term in Terms to make changes.",
            cl.term_name
        )));
    }
    Ok(cl)
}

pub fn classes_in_term(c: &Connection, user_id: i64, term_id: i64) -> AppResult<Vec<Class>> {
    let mut st = c.prepare(&format!(
        "SELECT {CLASS_COLS} FROM classes c JOIN terms t ON t.id = c.term_id
         WHERE c.user_id = ?1 AND c.term_id = ?2 ORDER BY c.period IS NULL, c.period, c.sort, c.name"
    ))?;
    Ok(st.query_map(params![user_id, term_id], class_row)?.collect::<Result<_, _>>()?)
}

// ── Subjects, categories, assessments ───────────────────────────────────────

#[derive(Clone, Debug, Serialize)]
pub struct Subject {
    pub id: i64,
    pub class_id: i64,
    pub name: String,
    pub report_style: String,
}

pub fn subjects(c: &Connection, class_id: i64) -> AppResult<Vec<Subject>> {
    let mut st = c.prepare("SELECT id, class_id, name, report_style FROM subjects WHERE class_id = ?1 ORDER BY sort, id")?;
    Ok(st
        .query_map([class_id], |r| Ok(Subject { id: r.get(0)?, class_id: r.get(1)?, name: r.get(2)?, report_style: r.get(3)? }))?
        .collect::<Result<_, _>>()?)
}

/// A subject plus its class, checked against the owner.
pub fn subject(c: &Connection, user_id: i64, subject_id: i64) -> AppResult<(Subject, Class)> {
    let class_id: i64 = c
        .query_row(
            "SELECT s.class_id FROM subjects s JOIN classes c ON c.id = s.class_id WHERE s.id = ?1 AND c.user_id = ?2",
            params![subject_id, user_id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(AppError::NotFound)?;
    let cl = class(c, user_id, class_id)?;
    let s = subjects(c, class_id)?.into_iter().find(|s| s.id == subject_id).ok_or(AppError::NotFound)?;
    Ok((s, cl))
}

pub fn categories(c: &Connection, subject_id: i64) -> AppResult<Vec<CategoryIn>> {
    let mut st = c.prepare("SELECT id, name, weight FROM categories WHERE subject_id = ?1 ORDER BY sort, id")?;
    Ok(st.query_map([subject_id], |r| Ok(CategoryIn { id: r.get(0)?, name: r.get(1)?, weight: r.get(2)? }))?.collect::<Result<_, _>>()?)
}

#[derive(Clone, Debug, Serialize)]
pub struct Assessment {
    pub id: i64,
    pub subject_id: i64,
    pub category_id: Option<i64>,
    pub name: String,
    pub unit: String,
    pub out_of: f64,
    pub weight: f64,
    pub kind: String,
    pub due_date: Option<String>,
    pub include_in_report: bool,
    /// 0 confirmed, 1 details guessed, 2 "out of" unknown.
    pub needs_review: i64,
}

impl Assessment {
    pub fn engine(&self) -> AssessmentIn {
        AssessmentIn {
            id: self.id,
            name: self.name.clone(),
            category_id: self.category_id,
            unit: self.unit.clone(),
            out_of: self.out_of,
            weight: self.weight,
            kind: Kind::parse(&self.kind),
        }
    }
}

pub fn assessments(c: &Connection, subject_id: i64) -> AppResult<Vec<Assessment>> {
    let mut st = c.prepare(
        "SELECT id, subject_id, category_id, name, unit, out_of, weight, kind, due_date, include_in_report, needs_review
         FROM assessments WHERE subject_id = ?1 ORDER BY sort, id",
    )?;
    Ok(st
        .query_map([subject_id], |r| {
            Ok(Assessment {
                id: r.get(0)?,
                subject_id: r.get(1)?,
                category_id: r.get(2)?,
                name: r.get(3)?,
                unit: r.get(4)?,
                out_of: r.get(5)?,
                weight: r.get(6)?,
                kind: r.get(7)?,
                due_date: r.get(8)?,
                include_in_report: r.get::<_, i64>(9)? != 0,
                needs_review: r.get(10)?,
            })
        })?
        .collect::<Result<_, _>>()?)
}

/// An assessment plus its subject and class, checked against the owner.
pub fn assessment(c: &Connection, user_id: i64, id: i64) -> AppResult<(Assessment, Subject, Class)> {
    let subject_id: i64 = c
        .query_row(
            "SELECT a.subject_id FROM assessments a JOIN subjects s ON s.id = a.subject_id
             JOIN classes c ON c.id = s.class_id WHERE a.id = ?1 AND c.user_id = ?2",
            params![id, user_id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(AppError::NotFound)?;
    let (s, cl) = subject(c, user_id, subject_id)?;
    let a = assessments(c, subject_id)?.into_iter().find(|a| a.id == id).ok_or(AppError::NotFound)?;
    Ok((a, s, cl))
}

// ── Students ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize)]
pub struct Student {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
    pub preferred_name: String,
    pub student_number: String,
    pub pronouns: String,
}

impl Student {
    pub fn first(&self) -> &str {
        if self.preferred_name.is_empty() { &self.first_name } else { &self.preferred_name }
    }
    pub fn full_name(&self) -> String {
        format!("{} {}", self.first(), self.last_name).trim().to_string()
    }
    pub fn sort_name(&self) -> String {
        if self.last_name.is_empty() { self.first().to_string() } else { format!("{}, {}", self.last_name, self.first()) }
    }
}

const STUDENT_COLS: &str = "s.id, s.first_name, s.last_name, s.preferred_name, s.student_number, s.pronouns";

fn student_row(r: &rusqlite::Row) -> rusqlite::Result<Student> {
    Ok(Student {
        id: r.get(0)?,
        first_name: r.get(1)?,
        last_name: r.get(2)?,
        preferred_name: r.get(3)?,
        student_number: r.get(4)?,
        pronouns: r.get(5)?,
    })
}

/// Active students in a class, sorted by last name.
pub fn roster(c: &Connection, class_id: i64) -> AppResult<Vec<Student>> {
    let mut st = c.prepare(&format!(
        "SELECT {STUDENT_COLS} FROM students s JOIN enrolments e ON e.student_id = s.id
         WHERE e.class_id = ?1 AND e.active = 1
         ORDER BY s.last_name COLLATE NOCASE, s.first_name COLLATE NOCASE"
    ))?;
    Ok(st.query_map([class_id], student_row)?.collect::<Result<_, _>>()?)
}

/// A student enrolled in a class owned by the user.
pub fn student_in_class(c: &Connection, class_id: i64, student_id: i64) -> AppResult<Student> {
    c.query_row(
        &format!(
            "SELECT {STUDENT_COLS} FROM students s JOIN enrolments e ON e.student_id = s.id
             WHERE e.class_id = ?1 AND s.id = ?2"
        ),
        params![class_id, student_id],
        student_row,
    )
    .optional()?
    .ok_or(AppError::NotFound)
}

pub fn all_students(c: &Connection, user_id: i64) -> AppResult<Vec<Student>> {
    let mut st = c.prepare(&format!("SELECT {STUDENT_COLS} FROM students s WHERE s.user_id = ?1"))?;
    Ok(st.query_map([user_id], student_row)?.collect::<Result<_, _>>()?)
}

/// Other classes in the same term that this student is in (for combined reports).
pub fn student_classes_in_term(c: &Connection, user_id: i64, student_id: i64, term_id: i64) -> AppResult<Vec<Class>> {
    let mut st = c.prepare(&format!(
        "SELECT {CLASS_COLS} FROM classes c JOIN terms t ON t.id = c.term_id
         JOIN enrolments e ON e.class_id = c.id
         WHERE c.user_id = ?1 AND e.student_id = ?2 AND c.term_id = ?3 AND e.active = 1
         ORDER BY c.period IS NULL, c.period, c.name"
    ))?;
    Ok(st.query_map(params![user_id, student_id, term_id], class_row)?.collect::<Result<_, _>>()?)
}

// ── Marks and computed grades ────────────────────────────────────────────────

/// student_id → assessment_id → mark, for one subject.
pub type MarkMap = HashMap<i64, HashMap<i64, MarkEntry>>;

pub fn marks_for_subject(c: &Connection, subject_id: i64) -> AppResult<MarkMap> {
    let mut st = c.prepare(
        "SELECT m.student_id, m.assessment_id, m.value, m.status FROM marks m
         JOIN assessments a ON a.id = m.assessment_id WHERE a.subject_id = ?1",
    )?;
    let mut out: MarkMap = HashMap::new();
    let rows = st
        .query_map([subject_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<f64>>(2)?, r.get::<_, String>(3)?)))?;
    for row in rows {
        let (sid, aid, value, status) = row?;
        let status = Status::parse(&status).unwrap_or(Status::Marked);
        out.entry(sid).or_default().insert(aid, MarkEntry { value, status });
    }
    Ok(out)
}

pub fn overrides_for_subject(c: &Connection, subject_id: i64) -> AppResult<HashMap<i64, (f64, String)>> {
    let mut st = c.prepare("SELECT student_id, pct, reason FROM grade_overrides WHERE subject_id = ?1")?;
    let rows = st.query_map([subject_id], |r| Ok((r.get::<_, i64>(0)?, (r.get::<_, f64>(1)?, r.get::<_, String>(2)?))))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Everything needed to show or compute one subject.
pub struct SubjectData {
    pub subject: Subject,
    pub categories: Vec<CategoryIn>,
    pub assessments: Vec<Assessment>,
    pub marks: MarkMap,
    pub overrides: HashMap<i64, (f64, String)>,
    pub setup: Setup,
}

pub fn subject_data(c: &Connection, subject: Subject, settings: &Settings) -> AppResult<SubjectData> {
    let categories = categories(c, subject.id)?;
    let assessments = assessments(c, subject.id)?;
    let marks = marks_for_subject(c, subject.id)?;
    let overrides = overrides_for_subject(c, subject.id)?;
    let setup = Setup {
        categories: categories.clone(),
        assessments: assessments.iter().map(Assessment::engine).collect(),
        policy: settings.missing_policy,
        levels: settings.levels,
    };
    Ok(SubjectData { subject, categories, assessments, marks, overrides, setup })
}

impl SubjectData {
    pub fn result_for(&self, student_id: i64) -> StudentResult {
        let empty = HashMap::new();
        let marks = self.marks.get(&student_id).unwrap_or(&empty);
        engine::compute(&self.setup, marks, self.overrides.get(&student_id).map(|o| o.0))
    }

    /// One mark as a percentage, if it counts.
    pub fn mark_pct(&self, a: &Assessment, m: &MarkEntry) -> Option<f64> {
        engine::mark_pct(&a.engine(), m, &self.setup.levels)
    }

    pub fn mark(&self, student_id: i64, assessment_id: i64) -> Option<&MarkEntry> {
        self.marks.get(&student_id).and_then(|m| m.get(&assessment_id))
    }
}

pub const LEARNING_SKILLS: [&str; 6] =
    ["Responsibility", "Organization", "Independent Work", "Collaboration", "Initiative", "Self-Regulation"];

pub fn learning_skills(c: &Connection, class_id: i64, student_id: i64) -> AppResult<HashMap<String, String>> {
    let mut st = c.prepare("SELECT skill, rating FROM learning_skills WHERE class_id = ?1 AND student_id = ?2")?;
    let rows = st.query_map(params![class_id, student_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn comment(c: &Connection, subject_id: i64, student_id: i64) -> AppResult<(String, String)> {
    Ok(c.query_row(
        "SELECT comment, next_steps FROM report_comments WHERE subject_id = ?1 AND student_id = ?2",
        params![subject_id, student_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?
    .unwrap_or_default())
}

/// Last report time for each student in a class, and how many marks changed since.
pub fn report_status(c: &Connection, class_id: i64) -> AppResult<HashMap<i64, (String, i64)>> {
    let mut st = c.prepare(
        "SELECT r.student_id, MAX(r.created_at) AS last,
                (SELECT count(*) FROM mark_history h WHERE h.class_id = r.class_id AND h.student_id = r.student_id
                   AND h.changed_at > MAX(r.created_at) AND h.undone = 0)
         FROM report_runs r WHERE r.class_id = ?1 GROUP BY r.student_id",
    )?;
    let rows = st.query_map([class_id], |r| Ok((r.get::<_, i64>(0)?, (r.get::<_, String>(1)?, r.get::<_, i64>(2)?))))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Create the subject(s) a new class needs for the teacher's mode.
pub fn create_default_subjects(c: &Connection, class_id: i64, mode: Mode, class_name: &str) -> AppResult<()> {
    match mode {
        Mode::Elementary => {
            for (i, name) in
                ["Language", "Mathematics", "Science and Technology", "Social Studies", "The Arts", "Health and Physical Education"]
                    .iter()
                    .enumerate()
            {
                c.execute("INSERT INTO subjects (class_id, name, sort) VALUES (?1, ?2, ?3)", params![class_id, name, i as i64])?;
            }
        }
        Mode::Secondary | Mode::University => {
            c.execute("INSERT INTO subjects (class_id, name) VALUES (?1, ?2)", params![class_id, class_name])?;
        }
    }
    Ok(())
}

/// Category presets offered on the weights page.
pub fn category_preset(name: &str) -> Option<Vec<(&'static str, f64)>> {
    Some(match name {
        "ontario-secondary" => vec![
            ("Knowledge & Understanding", 17.5),
            ("Thinking & Investigation", 17.5),
            ("Communication", 17.5),
            ("Application", 17.5),
            ("Final / Culminating", 30.0),
        ],
        "ontario-70-30" => vec![("Term Work", 70.0), ("Final / Culminating", 30.0)],
        "ontario-ktca" => {
            vec![("Knowledge & Understanding", 25.0), ("Thinking & Investigation", 25.0), ("Communication", 25.0), ("Application", 25.0)]
        }
        "university" => vec![("Assignments", 40.0), ("Midterm", 20.0), ("Final Exam", 40.0)],
        "none" => vec![],
        _ => return None,
    })
}
