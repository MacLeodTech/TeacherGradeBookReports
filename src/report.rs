//! Builds the report view-model from the database and renders it to PDF with
//! an embedded Typst engine. PDFs are rendered in memory and never written to
//! disk.

use std::sync::OnceLock;

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use typst::foundations::{Bytes, Dict, IntoValue};
use typst_as_lib::{TypstEngine, TypstTemplateMainFile};

use crate::{
    auth::User,
    engine::{self, Kind, Scale, Status, fmt_num},
    error::{AppError, AppResult},
    model::{self, Class, LEARNING_SKILLS, Mode, Settings, Student, SubjectData, Term},
    util::{fmt_pct, today_long},
};

static TEMPLATE: &str = include_str!("../report/report.typ");
static FONTS: [&[u8]; 5] = [
    include_bytes!("../assets/fonts/SourceSans3-Regular.ttf"),
    include_bytes!("../assets/fonts/SourceSans3-Bold.ttf"),
    include_bytes!("../assets/fonts/SourceSans3-It.ttf"),
    include_bytes!("../assets/fonts/SourceSans3-BoldIt.ttf"),
    include_bytes!("../assets/fonts/SourceSans3-Semibold.ttf"),
];

fn engine() -> &'static TypstEngine<TypstTemplateMainFile> {
    static ENGINE: OnceLock<TypstEngine<TypstTemplateMainFile>> = OnceLock::new();
    ENGINE.get_or_init(|| TypstEngine::builder().main_file(TEMPLATE).fonts(FONTS).build())
}

// ── Options ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Options {
    #[serde(default = "default_type")]
    pub report_type: String,
    #[serde(default)]
    pub show_assessments: bool,
    #[serde(default)]
    pub show_categories: bool,
    #[serde(default)]
    pub show_comments: bool,
    #[serde(default)]
    pub show_missing: bool,
    #[serde(default)]
    pub show_skills: bool,
    #[serde(default)]
    pub show_legend: bool,
    #[serde(default)]
    pub show_signature: bool,
}

fn default_type() -> String {
    "Progress Report".into()
}

impl Default for Options {
    fn default() -> Self {
        Options {
            report_type: default_type(),
            show_assessments: true,
            show_categories: true,
            show_comments: true,
            show_missing: true,
            show_skills: true,
            show_legend: true,
            show_signature: false,
        }
    }
}

pub const REPORT_TYPES: [&str; 5] = ["Progress Report", "Midterm Report", "Final Report", "Interim Report", "Term Report"];

// ── View-model ───────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct Model {
    school: String,
    subtitle: String,
    footer: String,
    title: String,
    date: String,
    /// Every colour on the report, derived from the school colours.
    colors: crate::color::Palette,
    has_logo: bool,
    student: StudentM,
    meta_lines: Vec<String>,
    subjects: Vec<SubjectM>,
    skills: Vec<SkillM>,
    legend: Vec<LegendM>,
    contact: String,
    options: Options,
}

#[derive(Serialize)]
struct StudentM {
    name: String,
}

#[derive(Serialize)]
struct LegendM {
    label: String,
    range: String,
    color: String,
}

#[derive(Serialize)]
struct SkillM {
    name: String,
    rating: String,
}

#[derive(Serialize)]
struct SubjectM {
    name: String,
    style: String,
    overall_pct: String,
    overall_label: String,
    overall_color: String,
    overall_frac: f64,
    overridden: bool,
    submitted: String,
    missing: usize,
    pending: usize,
    excused: usize,
    categories: Vec<CategoryM>,
    units: Vec<UnitM>,
    completion_title: String,
    completion_summary: String,
    completion: Vec<CompletionM>,
    missing_list: Vec<String>,
    comment: String,
    next_steps: String,
}

#[derive(Serialize)]
struct CategoryM {
    name: String,
    weight: String,
    pct: String,
    label: String,
    color: String,
    /// −1 = no marks.
    frac: f64,
}

#[derive(Serialize)]
struct UnitM {
    name: String,
    avg: String,
    label: String,
    color: String,
    rows: Vec<RowM>,
}

#[derive(Serialize)]
struct RowM {
    name: String,
    score: String,
    pct: String,
    label: String,
    color: String,
    frac: f64,
    status_text: String,
}

#[derive(Serialize)]
struct CompletionM {
    name: String,
    done: bool,
}

const GREY: &str = "#5A6B75";

fn subject_model(sd: &SubjectData, student: &Student, scale: &Scale, opts: &Options, comment: (String, String)) -> SubjectM {
    let r = sd.result_for(student.id);
    let (pct, label, color, frac) = match r.overall {
        Some(p) => (fmt_pct(Some(p)), scale.label(p), scale.color(p), p / 100.0),
        None => ("—".into(), "—".into(), GREY.into(), 0.0),
    };

    // A category or unit made only of levels shows its average as a level.
    let levels = &sd.setup.levels;
    let all_levels = |keep: &dyn Fn(&crate::model::Assessment) -> bool| {
        let mut graded = sd.assessments.iter().filter(|a| a.kind != "completion" && keep(a)).peekable();
        graded.peek().is_some() && graded.all(|a| a.kind == "level")
    };
    let grade_label = |p: f64, as_level: bool| {
        if as_level { format!("Level {}", engine::level_label(levels.level_of(p))) } else { scale.label(p) }
    };
    let categories = r
        .categories
        .iter()
        .map(|c| CategoryM {
            name: c.name.clone(),
            weight: format!("{}%", fmt_num(c.weight)),
            pct: fmt_pct(c.avg),
            label: c.avg.map(|p| grade_label(p, all_levels(&|a| a.category_id == c.id))).unwrap_or_default(),
            color: c.avg.map(|p| scale.color(p)).unwrap_or_else(|| GREY.into()),
            frac: c.avg.map(|p| p / 100.0).unwrap_or(-1.0),
        })
        .collect();

    // Units, in assessment order.
    let mut units: Vec<UnitM> = Vec::new();
    let mut missing_list = Vec::new();
    for a in sd.assessments.iter().filter(|a| a.kind != "completion") {
        let m = sd.mark(student.id, a.id);
        if matches!(m.map(|m| m.status), Some(Status::Missing)) {
            missing_list.push(a.name.clone());
        }
        if !a.include_in_report {
            continue;
        }
        let row = match m.and_then(|m| Some((m, sd.mark_pct(a, m)?))) {
            Some((m, p)) if a.kind == "level" => {
                let late = if m.status == Status::Late { " (late)" } else { "" };
                RowM {
                    name: format!("{}{}", a.name, if a.weight == 0.0 { " (practice)" } else { "" }),
                    score: late.trim().into(),
                    pct: String::new(),
                    label: format!("Level {}", engine::level_text(m.value.unwrap_or(0.0))),
                    color: scale.color(p),
                    frac: p / 100.0,
                    status_text: String::new(),
                }
            }
            Some((m, p)) => {
                let late = if m.status == Status::Late { " (late)" } else { "" };
                RowM {
                    name: format!("{}{}", a.name, if a.weight == 0.0 { " (practice)" } else { "" }),
                    score: format!("{} / {}{late}", fmt_num(m.value.unwrap_or(0.0)), fmt_num(a.out_of)),
                    pct: crate::util::fmt_pct(Some(p)),
                    label: scale.label(p),
                    color: scale.color(p),
                    frac: p / 100.0,
                    status_text: String::new(),
                }
            }
            None => {
                let (text, color) = match m.map(|m| m.status) {
                    Some(Status::Missing) => ("Not submitted", engine::RED),
                    Some(Status::Excused) => ("Excused", GREY),
                    Some(Status::Pending) => ("Submitted – not yet marked", engine::YELLOW),
                    _ => ("Not yet assessed", GREY),
                };
                RowM {
                    name: a.name.clone(),
                    score: String::new(),
                    pct: String::new(),
                    label: String::new(),
                    color: color.into(),
                    frac: -1.0,
                    status_text: text.into(),
                }
            }
        };
        match units.iter_mut().find(|u| u.name == a.unit) {
            Some(u) => u.rows.push(row),
            None => {
                units.push(UnitM { name: a.unit.clone(), avg: String::new(), label: String::new(), color: GREY.into(), rows: vec![row] })
            }
        }
    }
    for u in &mut units {
        if let Some(avg) = r.units.iter().find(|x| x.name == u.name).and_then(|x| x.avg) {
            u.avg = fmt_pct(Some(avg));
            u.label = format!("({})", grade_label(avg, all_levels(&|a| a.unit == u.name)));
            u.color = scale.color(avg);
        } else {
            u.avg = "—".into();
        }
    }

    let completion: Vec<CompletionM> = sd
        .assessments
        .iter()
        .filter(|a| a.kind == Kind::Completion.as_str() && a.include_in_report)
        .map(|a| CompletionM {
            name: a.name.clone(),
            done: sd
                .mark(student.id, a.id)
                .is_some_and(|m| m.value.is_some() || matches!(m.status, Status::Marked | Status::Late | Status::Pending)),
        })
        .collect();
    let done = completion.iter().filter(|c| c.done).count();
    let mut unit_titles: Vec<&str> =
        sd.assessments.iter().filter(|a| a.kind == "completion").map(|a| a.unit.as_str()).filter(|u| !u.is_empty()).collect();
    unit_titles.dedup();
    let completion_title = if unit_titles.len() == 1 { unit_titles[0].to_string() } else { "Completion".into() };

    SubjectM {
        name: sd.subject.name.clone(),
        style: sd.subject.report_style.clone(),
        overall_pct: pct,
        overall_label: label,
        overall_color: color,
        overall_frac: frac,
        overridden: r.overridden,
        submitted: format!("{}/{}", r.submitted, r.expected),
        missing: r.missing,
        pending: r.pending,
        excused: r.excused,
        categories: if opts.show_categories { categories } else { vec![] },
        units,
        completion_title,
        completion_summary: format!("{done} of {} complete", completion.len()),
        completion,
        missing_list,
        comment: comment.0,
        next_steps: comment.1,
    }
}

/// One student's report: one or more classes (several = combined report).
pub struct ReportInput<'a> {
    pub user: &'a User,
    pub settings: &'a Settings,
    pub term: &'a Term,
    pub student: &'a Student,
    /// (class, subjects of that class to include)
    pub classes: Vec<(Class, Vec<SubjectData>)>,
    pub options: &'a Options,
}

pub fn build_json(c: &Connection, input: &ReportInput) -> AppResult<String> {
    let s = input.settings;
    let o = input.options;
    let student = input.student;

    let mut subjects = Vec::new();
    for (class, sds) in &input.classes {
        for sd in sds {
            let mut m = subject_model(sd, student, &s.scale, o, model::comment(c, sd.subject.id, student.id)?);
            // Combined secondary report: label each course with its code.
            if input.classes.len() > 1 && !class.course_code.is_empty() && !m.name.contains(&class.course_code) {
                m.name = format!("{} ({})", m.name, class.course_code);
            }
            subjects.push(m);
        }
    }

    let mut skills = Vec::new();
    if (s.mode == Mode::Elementary || o.show_skills)
        && let Some((class, _)) = input.classes.first()
    {
        let map = model::learning_skills(c, class.id, student.id)?;
        for k in LEARNING_SKILLS {
            if let Some(r) = map.get(k) {
                skills.push(SkillM { name: k.into(), rating: r.clone() });
            }
        }
    }

    let school = if input.user.school.is_empty() { "Student Report".to_string() } else { input.user.school.clone() };
    let period = if input.term.period_label.is_empty() { input.term.name.clone() } else { input.term.period_label.clone() };
    let title = if o.report_type.trim().is_empty() { s.report_title.clone() } else { o.report_type.clone() };

    let mut meta_lines = vec![format!("Report date: {}", today_long())];
    let class_names: Vec<String> = input
        .classes
        .iter()
        .map(|(c, _)| {
            let mut n = c.name.clone();
            if !c.course_code.is_empty() && !n.contains(&c.course_code) {
                n = format!("{} · {}", c.course_code, n);
            }
            if s.mode == Mode::Secondary
                && let Some(p) = c.period
            {
                n = format!("{n} · Period {p}");
            }
            n
        })
        .collect();
    if input.classes.len() == 1 {
        meta_lines.push(class_names[0].clone());
    }
    if let Some(g) = input.classes.first().map(|(c, _)| c.grade_level.clone()).filter(|g| !g.is_empty()) {
        meta_lines.push(format!("Grade {g}"));
    }
    if !input.user.full_name.is_empty() {
        meta_lines.push(format!("Teacher: {}", input.user.full_name));
    }
    if !student.student_number.is_empty() {
        meta_lines.push(format!("Student #: {}", student.student_number));
    }

    let name = student.full_name();
    let teacher = &input.user.full_name;
    let email = &input.user.email;
    let contact = match (teacher.is_empty(), email.is_empty()) {
        (false, false) => format!("Please contact {name}'s teacher, {teacher}, at {email} with any questions or concerns."),
        (false, true) => format!("Please contact {name}'s teacher, {teacher}, with any questions or concerns."),
        (true, false) => format!("Please contact {name}'s teacher at {email} with any questions or concerns."),
        (true, true) => format!("Please contact {name}'s teacher with any questions or concerns."),
    };

    let footer = if s.board_name.is_empty() { school.clone() } else { format!("{school} · {}", s.board_name) };
    let model = Model {
        subtitle: format!("{title}  ·  {period}"),
        footer,
        school,
        title,
        date: today_long(),
        colors: crate::color::palette(&s.primary_color, &s.accent_color),
        has_logo: s.has_logo,
        student: StudentM { name },
        meta_lines,
        subjects,
        skills,
        legend: s.scale.legend().into_iter().map(|(label, range, color)| LegendM { label, range, color }).collect(),
        contact,
        options: o.clone(),
    };
    serde_json::to_string(&model).map_err(|e| AppError::Internal(e.into()))
}

/// A made-up student's report with the teacher's own header, grade scale and
/// levels, in the colours given (not yet saved), for trying out the look.
pub fn sample_json(user: &User, s: &Settings, main: &str, second: &str) -> AppResult<String> {
    use crate::engine::{CategoryIn, MarkEntry, MissingPolicy, Setup};
    use crate::model::{Assessment, Subject};
    use std::collections::HashMap;

    let student = Student {
        id: 1,
        first_name: "Alex".into(),
        last_name: "Sample".into(),
        preferred_name: String::new(),
        student_number: String::new(),
        pronouns: "they".into(),
    };
    let mk = |id: i64, cat: i64, name: &str, unit: &str, out_of: f64, kind: &str| Assessment {
        id,
        subject_id: 1,
        category_id: Some(cat),
        name: name.into(),
        unit: unit.into(),
        out_of,
        weight: 1.0,
        kind: kind.into(),
        due_date: None,
        include_in_report: true,
    };
    let assessments = vec![
        mk(1, 1, "Cells quiz", "Unit 1: Cells", 20.0, "scored"),
        mk(2, 2, "Microscope lab", "Unit 1: Cells", 25.0, "scored"),
        mk(3, 1, "Ecosystems test", "Unit 2: Ecosystems", 50.0, "scored"),
        mk(4, 2, "Food web poster", "Unit 2: Ecosystems", 30.0, "scored"),
        mk(5, 3, "Participation", "Unit 2: Ecosystems", 4.0, "level"),
        mk(6, 1, "Lab journal 1", "Journals", 1.0, "completion"),
        mk(7, 1, "Lab journal 2", "Journals", 1.0, "completion"),
    ];
    let categories = vec![
        CategoryIn { id: 1, name: "Knowledge".into(), weight: 50.0 },
        CategoryIn { id: 2, name: "Application".into(), weight: 35.0 },
        CategoryIn { id: 3, name: "Participation".into(), weight: 15.0 },
    ];
    let marks = HashMap::from([(
        1,
        HashMap::from([
            (1, MarkEntry::marked(17.0)),
            (2, MarkEntry::marked(19.5)),
            (3, MarkEntry::marked(41.0)),
            (4, MarkEntry::status(Status::Missing)),
            (5, MarkEntry::marked(3.0)),
            (6, MarkEntry::marked(1.0)),
        ]),
    )]);
    let setup = Setup {
        categories: categories.clone(),
        assessments: assessments.iter().map(Assessment::engine).collect(),
        policy: MissingPolicy::Ignore,
        levels: s.levels,
    };
    let sd = SubjectData {
        subject: Subject { id: 1, class_id: 1, name: "Science".into(), report_style: "percent".into() },
        categories,
        assessments,
        marks,
        overrides: HashMap::new(),
        setup,
    };
    let o = Options::default();
    let comment = (
        "Alex has made a strong start this term and asks thoughtful questions in class.".to_string(),
        "Hand in the food web poster, and keep the lab journal up to date.".to_string(),
    );
    let subjects = vec![subject_model(&sd, &student, &s.scale, &o, comment)];
    let school = if user.school.is_empty() { "Your School".to_string() } else { user.school.clone() };
    let footer = if s.board_name.is_empty() { school.clone() } else { format!("{school} · {}", s.board_name) };
    let mut meta_lines = vec![format!("Report date: {}", today_long()), "Grade 7 Science".into()];
    if !user.full_name.is_empty() {
        meta_lines.push(format!("Teacher: {}", user.full_name));
    }
    let model = Model {
        subtitle: format!("{}  ·  Sample", s.report_title),
        footer,
        school,
        title: s.report_title.clone(),
        date: today_long(),
        colors: crate::color::palette(main, second),
        has_logo: s.has_logo,
        student: StudentM { name: student.full_name() },
        meta_lines,
        subjects,
        skills: ["Responsibility", "Organization", "Collaboration", "Initiative"]
            .iter()
            .zip(["G", "S", "E", "G"])
            .map(|(k, r)| SkillM { name: (*k).into(), rating: r.into() })
            .collect(),
        legend: s.scale.legend().into_iter().map(|(label, range, color)| LegendM { label, range, color }).collect(),
        contact: "This is a sample report with made-up marks, to show how your reports will look.".into(),
        options: o,
    };
    serde_json::to_string(&model).map_err(|e| AppError::Internal(e.into()))
}

/// Render a report JSON model to PDF bytes. CPU-bound: call from a blocking thread.
pub fn render_pdf(json: &str, logo: Option<&[u8]>) -> AppResult<Vec<u8>> {
    render_many(&[json], logo)
}

/// Several students' reports as one PDF (for printing a class at once). Each
/// starts on a new page and numbers its own pages.
pub fn render_many<S: AsRef<str>>(jsons: &[S], logo: Option<&[u8]>) -> AppResult<Vec<u8>> {
    let list = format!("[{}]", jsons.iter().map(|j| j.as_ref()).collect::<Vec<_>>().join(","));
    let mut inputs = Dict::new();
    inputs.insert("data".into(), list.into_value());
    if let Some(l) = logo {
        inputs.insert("logo".into(), Bytes::new(l.to_vec()).into_value());
    }
    let doc: typst_layout::PagedDocument =
        engine().compile_with_input(inputs).output.map_err(|e| AppError::Internal(anyhow::anyhow!("report layout failed: {e:?}")))?;
    typst_pdf::pdf(&doc, &typst_pdf::PdfOptions::default()).map_err(|e| AppError::Internal(anyhow::anyhow!("PDF export failed: {e:?}")))
}

/// Check that an uploaded logo can be laid out, by rendering a minimal report
/// with it. Returns a teacher-facing message on failure.
pub fn check_logo(bytes: &[u8]) -> Result<(), String> {
    let model = Model {
        school: String::new(),
        subtitle: String::new(),
        footer: String::new(),
        title: "Logo check".into(),
        date: String::new(),
        colors: crate::color::palette(crate::color::DEFAULT_MAIN, crate::color::DEFAULT_TRIM),
        has_logo: true,
        student: StudentM { name: "Test".into() },
        meta_lines: vec![],
        subjects: vec![],
        skills: vec![],
        legend: vec![],
        contact: String::new(),
        options: Options::default(),
    };
    let json = serde_json::to_string(&model).map_err(|e| e.to_string())?;
    render_pdf(&json, Some(bytes))
        .map(|_| ())
        .map_err(|_| "That image couldn't be used in a PDF. Try saving it again as a PNG.".to_string())
}

/// Record that a report was generated (for "changed since last report").
pub fn record_run(c: &Connection, class_id: i64, student_id: i64, report_type: &str, overall: Option<f64>) -> AppResult<()> {
    c.execute(
        "INSERT INTO report_runs (class_id, student_id, report_type, overall) VALUES (?1, ?2, ?3, ?4)",
        params![class_id, student_id, report_type, overall],
    )?;
    Ok(())
}

pub fn file_name(student: &Student, class: Option<&Class>) -> String {
    let mut parts = vec![student.last_name.as_str(), student.first()];
    let code;
    if let Some(c) = class {
        code = if c.course_code.is_empty() { c.name.clone() } else { c.course_code.clone() };
        parts.push(&code);
    }
    format!("{}.pdf", crate::util::file_stem(&parts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{CategoryIn, MarkEntry, MissingPolicy, Setup};
    use crate::model::{Assessment, Subject};
    use std::collections::HashMap;

    fn sample() -> (Student, SubjectData) {
        let student = Student {
            id: 1,
            first_name: "Zoë".into(),
            last_name: "Lefèvre <O'Brien> & #co".into(),
            preferred_name: String::new(),
            student_number: "123".into(),
            pronouns: "she".into(),
        };
        let mk = |id: i64, name: &str, unit: &str, kind: &str| Assessment {
            id,
            subject_id: 1,
            category_id: Some(1),
            name: name.into(),
            unit: unit.into(),
            out_of: 20.0,
            weight: 1.0,
            kind: kind.into(),
            due_date: None,
            include_in_report: true,
        };
        let assessments = vec![
            mk(1, "Quiz #1 [draft] *bold*", "Unit 1", "scored"),
            mk(2, "Lab $x$", "Unit 1", "scored"),
            mk(3, "Essay", "Unit 2", "scored"),
            mk(4, "Journal 1", "Journals", "completion"),
            Assessment { category_id: Some(2), out_of: 4.0, ..mk(5, "Participation week 1", "Participation", "level") },
        ];
        let categories = vec![
            CategoryIn { id: 1, name: "Knowledge".into(), weight: 85.0 },
            CategoryIn { id: 2, name: "Participation".into(), weight: 15.0 },
        ];
        let marks = HashMap::from([(
            1,
            HashMap::from([
                (1, MarkEntry::marked(17.0)),
                (2, MarkEntry::status(Status::Missing)),
                (3, MarkEntry::status(Status::Pending)),
                (4, MarkEntry::marked(1.0)),
                (5, MarkEntry::marked(3.0 + 1.0 / 3.0)),
            ]),
        )]);
        let setup = Setup {
            categories: categories.clone(),
            assessments: assessments.iter().map(Assessment::engine).collect(),
            policy: MissingPolicy::Ignore,
            levels: crate::engine::Levels::default(),
        };
        let sd = SubjectData {
            subject: Subject { id: 1, class_id: 1, name: "Science".into(), report_style: "percent".into() },
            categories,
            assessments,
            marks,
            overrides: HashMap::new(),
            setup,
        };
        (student, sd)
    }

    /// Renders a real PDF from a model full of characters that would break
    /// markup if they weren't passed as data.
    #[test]
    fn renders_pdf_with_awkward_text() {
        let (student, sd) = sample();
        let opts = Options { show_signature: true, ..Options::default() };
        let subj =
            subject_model(&sd, &student, &Scale::ontario_letters(), &opts, ("Great work, #1! <b>x</b>".into(), "Keep $going$".into()));
        // 85% in Knowledge (85%), Level 3+ = 80% in Participation (15%).
        assert_eq!(subj.overall_pct, "84.3%");
        assert_eq!(subj.missing_list, vec!["Lab $x$".to_string()]);
        assert_eq!(subj.categories[1].label, "Level 3+");
        let level_row = subj.units.iter().flat_map(|u| &u.rows).find(|r| r.name.starts_with("Participation")).unwrap();
        assert_eq!((level_row.label.as_str(), level_row.pct.as_str()), ("Level 3+", ""));
        let model = Model {
            school: "École Saint-Joseph & Co".into(),
            subtitle: "Progress Report  ·  Semester 1".into(),
            footer: "École".into(),
            title: "Progress Report".into(),
            date: today_long(),
            // REPORT_SAMPLE_COLORS="#main,#second" to look at other school colours.
            colors: match std::env::var("REPORT_SAMPLE_COLORS").ok().as_deref().and_then(|v| v.split_once(',')) {
                Some((m, t)) => crate::color::palette(m, t),
                None => crate::color::palette("#0033A0", "#C8102E"),
            },
            has_logo: false,
            student: StudentM { name: student.full_name() },
            meta_lines: vec!["Report date: today".into(), "Teacher: Ms. M".into()],
            subjects: vec![subj],
            skills: vec![SkillM { name: "Responsibility".into(), rating: "E".into() }],
            legend: Scale::ontario_letters().legend().into_iter().map(|(label, range, color)| LegendM { label, range, color }).collect(),
            contact: "Please contact us.".into(),
            options: opts,
        };
        let json = serde_json::to_string(&model).unwrap();
        let pdf = render_pdf(&json, None).expect("pdf renders");
        assert!(pdf.starts_with(b"%PDF"));
        assert!(pdf.len() > 2000);
        // A class printed as one document.
        // The middle student's report runs over several pages.
        let mut long: serde_json::Value = serde_json::from_str(&json).unwrap();
        let subj = long["subjects"][0].clone();
        long["subjects"] = serde_json::Value::Array(vec![subj; 6]);
        let long = long.to_string();
        let many = render_many(&[json.as_str(), long.as_str(), json.as_str()], None).expect("combined pdf renders");
        assert!(many.len() > pdf.len());
        if let Ok(dir) = std::env::var("REPORT_SAMPLE_DIR") {
            std::fs::write(format!("{dir}/sample.pdf"), &pdf).unwrap();
            std::fs::write(format!("{dir}/sample-many.pdf"), &many).unwrap();
        }
    }
}
