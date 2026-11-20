//! The teacher's spreadsheet: classes, students and marks laid out to work on
//! in Excel (or Google Sheets, LibreOffice…), then uploaded to Gradebook.
//!
//! It's both the starter spreadsheet (blank, from Import) and the visible part
//! of the saved workbook (`crate::workbook`, which adds hidden data sheets):
//!
//! - **Start here**: a few steps and what to type in a mark cell.
//! - One tab per class (or per subject of a class): a plain grid. Names down
//!   the side, one column per assessment, with what it's out of in the
//!   heading (`Quiz 1 /20`, `Journal 1 (done)`, `Participation (level)`).
//!   Adding an assessment is typing a new heading. Mark cells are text, so
//!   Excel can't turn `3/4` into a date, and are coloured by status.
//! - **Setup** tabs at the back, all optional: course codes and periods,
//!   categories and weights, each assessment's category / weight / unit / due
//!   date, and student numbers and pronouns. Anything left out gets a sensible
//!   default (a new assessment goes in the same category as the column before
//!   it).
//!
//! Uploading is safe to repeat: classes, subjects, categories, assessments and
//! students are matched by name, marks that differ are updated (with undo) and
//! nothing is deleted. Blank cells leave marks alone, except when opening a
//! saved workbook, where the grid *is* the marks, so a cleared cell clears.
//!
//! The parser finds the parts of a sheet by their labels, so inserted rows,
//! renamed or copied tabs still work. Starter spreadsheets from before this
//! layout (version 1) are still read.

use std::{collections::HashMap, io::Cursor};

use calamine::{Data, Range, Reader, SheetVisible, Xlsx};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use rust_xlsxwriter::{
    Color, ConditionalFormatFormula, DataValidation, DataValidationRule, Format, FormatAlign, FormatBorder, Formula, Note, Workbook,
    Worksheet, utility::row_col_to_cell,
};
use serde::{Deserialize, Serialize};

use crate::{
    engine::{self, CellInput, Kind, MarkEntry, MissingPolicy, Status, fmt_num},
    error::{AppResult, bad},
    export::{err, sheet_name},
    marks,
    model::{self, Mode, Settings},
};

const FORMAT_ID: &str = "gradebook-template";
const VERSION: i64 = 2;
const MARKER_SHEET: &str = "_gradebook_template";
pub const START_SHEET: &str = "Start here";
const EXAMPLE_TITLE: &str = "Example";
const SETUP_CLASSES: &str = "Setup - Classes";
const SETUP_CATEGORIES: &str = "Setup - Categories";
const SETUP_ASSESSMENTS: &str = "Setup - Assessments";
const SETUP_STUDENTS: &str = "Setup - Students";
const SETUP_SHEETS: [&str; 4] = [SETUP_CLASSES, SETUP_CATEGORIES, SETUP_ASSESSMENTS, SETUP_STUDENTS];

// Labels and headings the parser looks for.
const L_CLASS: &str = "Class";
const L_SUBJECT: &str = "Subject";
const H_LAST: &str = "Last name";
const H_FIRST: &str = "First name";
const H_CODE: &str = "Course code";
const H_PERIOD: &str = "Period";
const H_CATEGORY: &str = "Category";
const H_WEIGHT_PCT: &str = "Weight %";
const H_ASSESSMENT: &str = "Assessment";
const H_WEIGHT: &str = "Weight";
const H_UNIT: &str = "Unit";
const H_DUE: &str = "Due date";
const H_REPORT: &str = "On report";
const H_NUMBER: &str = "Student #";
const H_PRONOUNS: &str = "Pronouns";
const DONE_TAG: &str = "(done)";
const LEVEL_TAG: &str = "(level)";

// Class tab layout (0-based rows and columns).
const CLASS_ROW: u32 = 0;
const SUBJECT_ROW: u32 = 1;
const HINT_ROW: u32 = 2;
const HEADER_ROW: u32 = 3;
const FIRST_STUDENT_ROW: u32 = HEADER_ROW + 1;
const FIRST_MARK_COL: u16 = 2;
const BLANK_ASSESSMENTS: u16 = 30;
const SPARE_ASSESSMENTS: u16 = 12;
const BLANK_STUDENTS: u32 = 40;
const SPARE_STUDENTS: u32 = 5;
const SPARE_SETUP_ROWS: u32 = 25;

const MAX_SHEETS: usize = 60;
const MAX_STUDENTS: u32 = 500;
const MAX_ASSESSMENTS: u32 = 300;
const MAX_SETUP_ROWS: u32 = 5000;

// ── The spreadsheet's content ────────────────────────────────────────────────

pub struct Template {
    pub missing_policy: MissingPolicy,
    pub sheets: Vec<SheetSpec>,
    /// A filled-in class showing every kind of entry (blank starter only).
    pub example: Option<SheetSpec>,
    /// Set when this is the visible part of a saved workbook.
    pub saved: Option<SavedInfo>,
}

/// What the Start here tab says about a saved workbook.
pub struct SavedInfo {
    pub teacher: String,
    pub school: String,
    pub term: String,
    pub contents: String,
}

#[derive(Default)]
pub struct SheetSpec {
    pub title: String,
    pub class: String,
    pub subject: String,
    pub course_code: String,
    pub period: Option<i64>,
    pub categories: Vec<(String, f64)>,
    pub assessments: Vec<AssessmentSpec>,
    pub students: Vec<StudentSpec>,
    /// `marks[student][assessment]`, as typed (`17`, `M`, `17L`, `3+`…).
    pub marks: Vec<Vec<String>>,
    /// Blank template, several subjects: the class name and the student list
    /// come from this (the first) sheet, so they're typed once.
    pub link_to: Option<String>,
}

pub struct AssessmentSpec {
    pub name: String,
    pub category: String,
    pub out_of: f64,
    pub weight: f64,
    pub kind: Kind,
    pub unit: String,
    pub due: Option<String>,
    pub on_report: bool,
}

impl AssessmentSpec {
    pub fn new(name: &str, category: &str, out_of: f64, weight: f64, kind: Kind) -> Self {
        AssessmentSpec {
            name: name.into(),
            category: category.into(),
            out_of,
            weight,
            kind,
            unit: String::new(),
            due: None,
            on_report: true,
        }
    }

    /// The column heading: the name, plus what it's out of or its type.
    pub fn heading(&self) -> String {
        match self.kind {
            Kind::Scored => format!("{} /{}", self.name, fmt_num(self.out_of)),
            Kind::Completion => format!("{} {DONE_TAG}", self.name),
            Kind::Level => format!("{} {LEVEL_TAG}", self.name),
        }
    }
}

#[derive(Default)]
pub struct StudentSpec {
    pub number: String,
    pub last: String,
    pub first: String,
    pub pronouns: String,
}

fn preset(name: &str) -> Vec<(String, f64)> {
    model::category_preset(name).unwrap_or_default().into_iter().map(|(n, w)| (n.to_string(), w)).collect()
}

/// An empty spreadsheet shaped for the teacher's level.
pub fn blank(settings: &Settings) -> Template {
    let sheets: Vec<SheetSpec> = match settings.mode {
        Mode::Elementary => {
            let subjects =
                ["Language", "Mathematics", "Science and Technology", "Social Studies", "The Arts", "Health and Physical Education"];
            let first = subjects[0].to_string();
            subjects
                .iter()
                .enumerate()
                .map(|(i, s)| SheetSpec {
                    // Sheet tabs are limited to 31 characters.
                    title: if s.len() > 24 { "Health and Phys Ed".into() } else { s.to_string() },
                    subject: s.to_string(),
                    link_to: (i > 0).then(|| first.clone()),
                    ..Default::default()
                })
                .collect()
        }
        Mode::Secondary => {
            let periods: Vec<i64> =
                (1..=4).filter(|p| Some(*p) != settings.prep_period).take(settings.active_periods.clamp(1, 4) as usize).collect();
            periods
                .into_iter()
                .map(|p| SheetSpec {
                    title: format!("Period {p}"),
                    period: Some(p),
                    categories: preset("ontario-secondary"),
                    ..Default::default()
                })
                .collect()
        }
        Mode::University => {
            (1..=3).map(|i| SheetSpec { title: format!("Course {i}"), categories: preset("university"), ..Default::default() }).collect()
        }
    };
    Template { missing_policy: settings.missing_policy, sheets, example: Some(example(settings.mode)), saved: None }
}

/// A filled-in class showing every kind of entry. Its class name ends in
/// "(example)", so uploading never imports it.
fn example(mode: Mode) -> SheetSpec {
    let (class, code, cats, k, t, a, p, f) = match mode {
        Mode::Secondary => (
            "Grade 9 Math (example)",
            "MTH1W",
            vec![
                ("Knowledge & Understanding".to_string(), 15.0),
                ("Thinking & Investigation".to_string(), 15.0),
                ("Communication".to_string(), 12.5),
                ("Application".to_string(), 12.5),
                ("Participation".to_string(), 15.0),
                ("Final / Culminating".to_string(), 30.0),
            ],
            "Knowledge & Understanding",
            "Thinking & Investigation",
            "Application",
            "Participation",
            "Final / Culminating",
        ),
        Mode::University => (
            "MATH 101 (example)",
            "MATH 101",
            vec![
                ("Assignments".to_string(), 35.0),
                ("Participation".to_string(), 10.0),
                ("Midterm".to_string(), 20.0),
                ("Final Exam".to_string(), 35.0),
            ],
            "Assignments",
            "Assignments",
            "Midterm",
            "Participation",
            "Final Exam",
        ),
        Mode::Elementary => ("Grade 4 (example)", "", Vec::new(), "", "", "", "", ""),
    };
    let a_ = AssessmentSpec::new;
    let row = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut assessments = vec![
        a_("Unit 1 Quiz", k, 20.0, 1.0, Kind::Scored),
        a_("Unit 1 Test", a, 50.0, 2.0, Kind::Scored),
        a_("Problem Set 1", t, 10.0, 1.0, Kind::Scored),
        a_("Math Journal", "", 1.0, 1.0, Kind::Completion),
        a_("Participation week 1", p, engine::MAX_LEVEL, 1.0, Kind::Level),
        a_("Warm-up", k, 10.0, 0.0, Kind::Scored),
        a_("Final Exam", f, 100.0, 1.0, Kind::Scored),
    ];
    assessments[0].unit = "Unit 1".into();
    assessments[1].unit = "Unit 1".into();
    assessments[5].on_report = false;
    SheetSpec {
        title: EXAMPLE_TITLE.into(),
        class: class.into(),
        subject: if mode == Mode::Elementary { "Mathematics".into() } else { String::new() },
        course_code: code.into(),
        categories: cats,
        assessments,
        students: vec![
            StudentSpec { number: "100001".into(), last: "Smith".into(), first: "John".into(), pronouns: "he".into() },
            StudentSpec { number: "100002".into(), last: "Doe".into(), first: "Jane".into(), pronouns: "she".into() },
        ],
        // 17 points · 85% · missing · done · level 3+ · practice mark · not assessed yet
        // 3/4 (fraction) · late · excused · not done · level 4 · handed in, not marked
        marks: vec![row(&["17", "85%", "M", "Y", "3+", "8", ""]), row(&["3/4", "41L", "X", "N", "4", "?", ""])],
        link_to: None,
        period: None,
    }
}

/// What an entry on the example sheet means, as a cell comment.
fn explain(v: &str, a: &AssessmentSpec) -> String {
    let out_of = fmt_num(a.out_of);
    let u = v.trim().to_uppercase();
    match u.as_str() {
        "" => "Blank: not assessed yet. Doesn't count.".into(),
        "M" => "M = missing (not handed in). Shown as missing on the grid and the report. Whether it counts as 0 is your Settings choice."
            .into(),
        "X" => "X = excused or exempt. Never counts.".into(),
        "?" => "? = handed in, marking in progress. Doesn't count until you replace it with the mark.".into(),
        "Y" => "Y = done. (done) columns show on the report but never change the mark.".into(),
        "N" => "N = not done. Shown as missing, but never changes the mark.".into(),
        _ if a.kind == Kind::Level => format!(
            "Level {v}. A (level) column takes 1 to 4, with + or − (3+, 4−). The report shows the level; it counts toward the grade as the percentage set for it in Settings."
        ),
        _ if u.ends_with('L') => format!("{} plus L = handed in late. Counts like any other mark.", &v[..v.len() - 1]),
        _ if v.contains('/') => {
            let mark = engine::parse_cell(v, a.out_of).ok().and_then(|c| match c {
                CellInput::Set(m) => m.value,
                CellInput::Clear => None,
            });
            format!("A fraction: {v} is converted to {} out of {out_of}.", mark.map(fmt_num).unwrap_or_default())
        }
        _ if v.ends_with('%') => format!("A percentage of what it's out of: {v} of {out_of}."),
        _ if a.weight == 0.0 => {
            format!("{v} out of {out_of}. Its weight is 0 (practice) on Setup - Assessments, so it's shown but doesn't count.")
        }
        _ => format!("{v} points out of {out_of}."),
    }
}

fn example_notes(ws: &mut Worksheet, s: &SheetSpec) -> AppResult<()> {
    let note = |t: String| Note::new(t).set_author("Gradebook").set_width(260).set_height(80);
    for (j, a) in s.assessments.iter().enumerate() {
        let c = FIRST_MARK_COL + j as u16;
        for (i, row) in s.marks.iter().enumerate() {
            let v = row.get(j).map(String::as_str).unwrap_or("");
            ws.insert_note(FIRST_STUDENT_ROW + i as u32, c, &note(explain(v, a))).map_err(err)?;
        }
        let about = match a.kind {
            Kind::Completion => "(done) after the name: a done / not done column, for journals or homework checks. Use Y or N.".to_string(),
            Kind::Level => "(level) after the name: a column of levels from 1 to 4, for example weekly participation.".to_string(),
            Kind::Scored if j == 0 => format!("/{} after the name: what it's out of.", fmt_num(a.out_of)),
            Kind::Scored if a.weight == 0.0 => {
                "Practice work: its weight is 0 on Setup - Assessments, so marks are recorded but don't count.".to_string()
            }
            Kind::Scored if a.weight > 1.0 => {
                "Its weight is 2 on Setup - Assessments, so it counts twice as much as a weight-1 assessment.".to_string()
            }
            Kind::Scored => continue,
        };
        ws.insert_note(HEADER_ROW, c, &note(about)).map_err(err)?;
    }
    ws.insert_note(CLASS_ROW, 1, &note("The class name. This one ends in \"(example)\", so this tab is never imported.".into()))
        .map_err(err)?;
    Ok(())
}

const EXAMPLE_STUDENTS: [(&str, &str); 2] = [("John", "Smith"), ("Jane", "Doe")];

pub fn is_example(class: &str) -> bool {
    class.trim().to_lowercase().ends_with("(example)")
}

/// A mark as it's typed in the spreadsheet.
fn typed(kind: Kind, m: &MarkEntry) -> String {
    match kind {
        Kind::Completion => match m.status {
            Status::Missing => "N".into(),
            Status::Marked | Status::Late => "Y".into(),
            s => s.code().into(),
        },
        k => engine::display_mark(k, m),
    }
}

/// The current term's classes, filled in, for working on in a spreadsheet.
pub fn from_term(c: &Connection, user_id: i64, settings: &Settings) -> AppResult<Template> {
    let term = model::current_term(c, user_id)?.ok_or_else(|| bad("Create a term first."))?;
    let mut sheets = Vec::new();
    for class in model::classes_in_term(c, user_id, term.id)? {
        let roster = model::roster(c, class.id)?;
        let subjects = model::subjects(c, class.id)?;
        let single = subjects.len() == 1;
        for s in subjects {
            let cats = model::categories(c, s.id)?;
            let cat_name: HashMap<i64, String> = cats.iter().map(|k| (k.id, k.name.clone())).collect();
            let assessments = model::assessments(c, s.id)?;
            let marks = model::marks_for_subject(c, s.id)?;
            let grid = roster
                .iter()
                .map(|st| {
                    assessments
                        .iter()
                        .map(|a| marks.get(&st.id).and_then(|m| m.get(&a.id)).map(|m| typed(Kind::parse(&a.kind), m)).unwrap_or_default())
                        .collect()
                })
                .collect();
            sheets.push(SheetSpec {
                title: match class.period {
                    Some(p) if single => format!("P{p} {}", class.name),
                    _ if single => class.name.clone(),
                    _ => format!("{} - {}", class.name, s.name),
                },
                class: class.name.clone(),
                // One subject named after the class: leave Subject blank.
                subject: if single && s.name.eq_ignore_ascii_case(&class.name) { String::new() } else { s.name.clone() },
                course_code: class.course_code.clone(),
                period: class.period,
                categories: cats.iter().map(|k| (k.name.clone(), k.weight)).collect(),
                assessments: assessments
                    .iter()
                    .map(|a| AssessmentSpec {
                        name: a.name.clone(),
                        category: a.category_id.and_then(|id| cat_name.get(&id).cloned()).unwrap_or_default(),
                        out_of: a.out_of,
                        weight: a.weight,
                        kind: Kind::parse(&a.kind),
                        unit: a.unit.clone(),
                        due: a.due_date.clone(),
                        on_report: a.include_in_report,
                    })
                    .collect(),
                students: roster
                    .iter()
                    .map(|st| StudentSpec {
                        number: st.student_number.clone(),
                        last: st.last_name.clone(),
                        first: st.first_name.clone(),
                        pronouns: st.pronouns.clone(),
                    })
                    .collect(),
                marks: grid,
                link_to: None,
            });
        }
    }
    if sheets.is_empty() {
        let mut t = blank(settings);
        t.example = None;
        return Ok(t);
    }
    Ok(Template { missing_policy: settings.missing_policy, sheets, example: None, saved: None })
}

// ── Writing ──────────────────────────────────────────────────────────────────

struct Styles {
    title: Format,
    note: Format,
    label: Format,
    class_name: Format,
    input_text: Format,
    input_num: Format,
    linked: Format,
    linked_name: Format,
    header: Format,
    new_heading: Format,
    setup_header: Format,
    missing: Format,
    excused: Format,
    pending: Format,
    late: Format,
    wrap: Format,
    bold_wrap: Format,
}

const PRIMARY: u32 = 0x003F5A;
const SETUP_GREY: u32 = 0x9AA5B1;
const INPUT_BG: u32 = 0xEEF5FB;
const INPUT_BORDER: u32 = 0xC9D7E3;
const MISSING_BG: u32 = 0xF8D7DA;
const EXCUSED_BG: u32 = 0xE2E3E5;
const PENDING_BG: u32 = 0xFFF3CD;
const LATE_BG: u32 = 0xFFE0B2;

fn styles() -> Styles {
    let input =
        Format::new().set_background_color(Color::RGB(INPUT_BG)).set_border(FormatBorder::Thin).set_border_color(Color::RGB(INPUT_BORDER));
    let linked = Format::new()
        .set_background_color(Color::RGB(0xF4F4F4))
        .set_font_color(Color::RGB(0x5F6B73))
        .set_border(FormatBorder::Thin)
        .set_border_color(Color::RGB(0xDDDDDD));
    let header = Format::new().set_bold().set_text_wrap().set_align(FormatAlign::Top).set_border(FormatBorder::Thin).set_num_format("@");
    Styles {
        title: Format::new().set_bold().set_font_size(15).set_font_color(Color::RGB(PRIMARY)),
        note: Format::new().set_italic().set_font_color(Color::RGB(0x5F6B73)),
        label: Format::new().set_bold().set_align(FormatAlign::Right),
        class_name: input.clone().set_bold().set_font_size(13).set_num_format("@"),
        input_text: input.clone().set_num_format("@"),
        input_num: input.clone().set_num_format("0.##"),
        linked_name: linked.clone().set_bold().set_font_size(13),
        linked,
        header: header.clone().set_font_color(Color::White).set_background_color(Color::RGB(PRIMARY)).set_border_color(Color::RGB(PRIMARY)),
        new_heading: header.set_background_color(Color::RGB(INPUT_BG)).set_border_color(Color::RGB(INPUT_BORDER)),
        setup_header: Format::new()
            .set_bold()
            .set_font_color(Color::White)
            .set_background_color(Color::RGB(0x5F6B73))
            .set_border(FormatBorder::Thin)
            .set_border_color(Color::RGB(0x5F6B73)),
        missing: Format::new().set_background_color(Color::RGB(MISSING_BG)),
        excused: Format::new().set_background_color(Color::RGB(EXCUSED_BG)),
        pending: Format::new().set_background_color(Color::RGB(PENDING_BG)),
        late: Format::new().set_background_color(Color::RGB(LATE_BG)),
        wrap: Format::new().set_text_wrap().set_align(FormatAlign::Top),
        bold_wrap: Format::new().set_bold().set_text_wrap().set_align(FormatAlign::Top),
    }
}

fn quoted(sheet: &str) -> String {
    format!("'{}'", sheet.replace('\'', "''"))
}

/// A formula showing another cell (blank while it's blank).
fn link(sheet: &str, cell: &str) -> String {
    let src = format!("{}!{cell}", quoted(sheet));
    format!("=IF({src}=\"\",\"\",{src})")
}

/// The starter spreadsheet as a file.
pub fn build(t: &Template) -> AppResult<Vec<u8>> {
    let mut wb = Workbook::new();
    write_visible(&mut wb, t)?;
    let m = wb.add_worksheet();
    m.set_name(MARKER_SHEET).map_err(err)?;
    m.write_string(0, 0, "format").map_err(err)?;
    m.write_string(0, 1, FORMAT_ID).map_err(err)?;
    m.write_string(1, 0, "version").map_err(err)?;
    m.write_number(1, 1, VERSION as f64).map_err(err)?;
    m.set_hidden(true);
    wb.save_to_buffer().map_err(err)
}

/// Start here, the class tabs and the setup tabs. Returns the tab names used.
pub fn write_visible(wb: &mut Workbook, t: &Template) -> AppResult<Vec<String>> {
    let st = styles();
    let mut used: Vec<String> = [START_SHEET].iter().chain(SETUP_SHEETS.iter()).map(|s| s.to_string()).collect();
    start_sheet(wb.add_worksheet(), &st, t)?;
    // Sheet names are fixed up for Excel first, so links can point at them.
    let all: Vec<&SheetSpec> = t.example.iter().chain(t.sheets.iter()).collect();
    let titles: Vec<String> = all.iter().map(|s| sheet_name(&s.title, &mut used)).collect();
    let filled = t.saved.is_some() || t.sheets.iter().any(|s| !s.class.is_empty());
    for (spec, title) in all.iter().zip(&titles) {
        let link = spec.link_to.as_ref().and_then(|l| all.iter().position(|s| &s.title == l)).map(|i| titles[i].as_str());
        let example = is_example(&spec.class);
        let (cols, rows) = if example {
            (0, 0)
        } else if filled {
            (SPARE_ASSESSMENTS, SPARE_STUDENTS)
        } else {
            (BLANK_ASSESSMENTS.saturating_sub(spec.assessments.len() as u16), BLANK_STUDENTS.saturating_sub(spec.students.len() as u32))
        };
        class_sheet(wb.add_worksheet(), &st, spec, title, link, cols, rows)?;
    }
    let tabs: Vec<(&SheetSpec, &str)> = all.iter().copied().zip(titles.iter().map(String::as_str)).collect();
    setup_sheets(wb, &st, &tabs)?;
    Ok(titles)
}

fn start_sheet(ws: &mut Worksheet, st: &Styles, t: &Template) -> AppResult<()> {
    ws.set_name(START_SHEET).map_err(err)?;
    ws.set_active(true);
    ws.set_tab_color(Color::RGB(0xB1D2E1));
    ws.set_column_width(0, 16).map_err(err)?;
    ws.set_column_width(1, 64).map_err(err)?;
    ws.set_column_width(2, 40).map_err(err)?;
    let saved = t.saved.as_ref();
    let heading = if saved.is_some() { "Your Gradebook workbook" } else { "Gradebook spreadsheet" };
    ws.write_string_with_format(0, 0, heading, &st.title).map_err(err)?;
    let intro = match saved {
        Some(s) => format!(
            "Your marks for {}, one tab per class. You can add marks and assessments here in Excel, then open this file in Gradebook (Open workbook) to carry on and make reports.",
            s.term
        ),
        None => "One tab per class: fill in your students and marks, save, then upload this file on Gradebook's Import page.".into(),
    };
    ws.write_string_with_format(1, 0, &intro, &st.note).map_err(err)?;

    let mut r = 3;
    let line = |ws: &mut Worksheet, r: &mut u32, head: &str, text: &str| -> AppResult<()> {
        ws.write_string_with_format(*r, 0, head, &st.bold_wrap).map_err(err)?;
        ws.merge_range(*r, 1, *r, 2, text, &st.wrap).map_err(err)?;
        ws.set_row_height(
            *r,
            if text.len() > 190 {
                48
            } else if text.len() > 95 {
                33
            } else {
                18
            },
        )
        .map_err(err)?;
        *r += 1;
        Ok(())
    };
    if let Some(s) = saved {
        line(ws, &mut r, "Teacher", &s.teacher)?;
        if !s.school.is_empty() {
            line(ws, &mut r, "School", &s.school)?;
        }
        line(ws, &mut r, "Saved", &chrono::Local::now().format("%B %-d, %Y at %-I:%M %p").to_string())?;
        line(ws, &mut r, "In this file", &s.contents)?;
        r += 1;
    }
    let mut steps: Vec<(&str, &str)> = Vec::new();
    if t.example.is_some() {
        steps.push((
            "Example",
            "The Example tab is filled in to show how it works (hover over a cell to see what it means). It's never imported, so keep it or delete it.",
        ));
    }
    if saved.is_none() {
        steps.push((
            "1. Your classes",
            "Each class has its own tab at the bottom. Type the class name at the top. Need another class? Right-click a tab → Move or Copy → Create a copy, then change the name.",
        ));
    }
    steps.extend([
        ("Students", "One per row: last name and first name."),
        (
            "Assessments",
            "One per column. Type the name and what it's out of in the heading: Quiz 2 /20. For a done / not done column, add (done): Journal 3 (done). For levels 1 to 4 (like weekly participation), add (level): Participation week 2 (level).",
        ),
        ("Marks", "Type them in the grid using the codes below. Leave a cell blank if it isn't marked yet."),
    ]);
    steps.push(match saved {
        Some(_) => (
            "When you're done",
            "Save the file, then open it in Gradebook (Open workbook). Your changes here are added; mark changes can be undone from the class's mark grid.",
        ),
        None => (
            "Upload",
            "Save the file and choose it on Gradebook's Import page. You'll see what will be created or changed, and any cells to fix, before anything is saved.",
        ),
    });
    steps.push((
        "Setup tabs",
        "Optional. The grey tabs at the end hold course codes, categories and weights, each assessment's category and weight, and student numbers and pronouns. Anything you leave out gets a sensible default: a new assessment goes in the same category as the column before it.",
    ));
    if saved.is_some() {
        steps.push((
            "Also saved",
            "Comments, learning skills, past terms and report history are saved in this file too, out of sight. They come back when you open it. Please don't edit or delete the hidden tabs.",
        ));
        steps.push((
            "Privacy",
            "This file contains student information. Keep it where your school board allows, such as your board OneDrive or network drive.",
        ));
    } else {
        steps.push((
            "Keep using it",
            "You can keep marking in this file and upload it again any time. Changed marks are updated (and can be undone), blank cells leave existing marks alone, and nothing is ever deleted.",
        ));
    }
    for (head, text) in steps {
        line(ws, &mut r, head, text)?;
    }

    r += 1;
    ws.write_string_with_format(r, 0, "What to type in a mark cell", &st.title).map_err(err)?;
    r += 1;
    for (c, h) in ["You type", "It means", "Does it count toward the mark?"].iter().enumerate() {
        ws.write_string_with_format(r, c as u16, *h, &st.header).map_err(err)?;
    }
    r += 1;
    let missing_counts = match t.missing_policy {
        MissingPolicy::Ignore => {
            "No. It's left out of the average until it's handed in (your current setting; you can make missing work count as 0 in Settings)."
        }
        MissingPolicy::Zero => "Yes, as 0 (your current setting; you can have missing work left out of the average in Settings).",
    };
    let rows: [(&str, &str, &str, Option<&Format>); 10] = [
        ("17", "17 points out of what the assessment is out of (Quiz /20: 17 out of 20). Decimals like 17.5 are fine.", "Yes", None),
        ("17/20", "A fraction. It's converted, so 3/4 on a test out of 40 is 30.", "Yes", None),
        ("85%", "A percentage.", "Yes", None),
        ("17L  85%L", "Any mark followed by L: handed in late. Shown as late.", "Yes, like any other mark", Some(&st.late)),
        ("M", "Missing: not handed in. Shown as missing on the grid and the report.", missing_counts, Some(&st.missing)),
        ("X", "Excused or exempt: this student doesn't need to do it.", "No, never", Some(&st.excused)),
        ("?", "Handed in, marking in progress.", "Not until you replace it with the mark", Some(&st.pending)),
        ("(blank)", "Not assessed yet (or not due yet).", "No", None),
        (
            "Y  /  N",
            "In a (done) column, for journals or homework checks: done, or not done. X and ? work too.",
            "No, these never change the mark (they show on the report)",
            None,
        ),
        (
            "1 2 3 4  3+  4-",
            "In a (level) column: the level, with + or − if you like. R is below level 1.",
            "Yes, as the percentage set for each level in Settings. Reports show the level.",
            None,
        ),
    ];
    for (code, meaning, counts, fill) in rows {
        let code_fmt = fill.cloned().unwrap_or_default().set_bold().set_align(FormatAlign::Center).set_align(FormatAlign::Top);
        ws.write_string_with_format(r, 0, code, &code_fmt).map_err(err)?;
        ws.write_string_with_format(r, 1, meaning, &st.wrap).map_err(err)?;
        ws.write_string_with_format(r, 2, counts, &st.wrap).map_err(err)?;
        ws.set_row_height(r, if meaning.len().max(counts.len() * 64 / 40) > 64 { 33 } else { 18 }).map_err(err)?;
        r += 1;
    }
    r += 1;
    ws.write_string_with_format(r, 0, "Tip", &st.bold_wrap).map_err(err)?;
    ws.merge_range(
        r,
        1,
        r,
        2,
        "Mark cells are formatted as text so Excel won't turn 3/4 into a date. If you paste marks in from another file and some show as dates, retype them.",
        &st.wrap,
    )
    .map_err(err)?;
    ws.set_row_height(r, 33).map_err(err)?;
    Ok(())
}

fn class_sheet(
    ws: &mut Worksheet,
    st: &Styles,
    s: &SheetSpec,
    title: &str,
    linked: Option<&str>,
    spare_cols: u16,
    spare_rows: u32,
) -> AppResult<()> {
    ws.set_name(title).map_err(err)?;
    // Linked cells read as blank (not 0) until the spreadsheet recalculates.
    ws.set_formula_result_default("");
    let n_cols = (s.assessments.len() as u16 + spare_cols).max(1);
    let n_rows = (s.students.len() as u32 + spare_rows).max(1);
    let last_col = FIRST_MARK_COL + n_cols - 1;
    let last_row = FIRST_STUDENT_ROW + n_rows - 1;

    ws.set_column_width(0, 18).map_err(err)?;
    ws.set_column_width(1, 16).map_err(err)?;
    ws.set_column_range_width(FIRST_MARK_COL, last_col, 12).map_err(err)?;

    // Class and subject.
    ws.write_string_with_format(CLASS_ROW, 0, L_CLASS, &st.label).map_err(err)?;
    ws.write_string_with_format(SUBJECT_ROW, 0, L_SUBJECT, &st.label).map_err(err)?;
    match linked {
        Some(l) => {
            ws.write_formula_with_format(CLASS_ROW, 1, Formula::new(link(l, "$B$1")).set_result(""), &st.linked_name).map_err(err)?;
        }
        None => {
            ws.write_string_with_format(CLASS_ROW, 1, &s.class, &st.class_name).map_err(err)?;
        }
    }
    ws.write_string_with_format(SUBJECT_ROW, 1, &s.subject, &st.input_text).map_err(err)?;
    ws.write_string_with_format(SUBJECT_ROW, 2, "Only for a class with several subjects", &st.note).map_err(err)?;
    ws.set_row_height(CLASS_ROW, 21).map_err(err)?;
    ws.write_string_with_format(
        HINT_ROW,
        0,
        "Marks: 17, 17/20, 85%, M missing, X excused, ? to mark, 17L late. New assessment: type it in the next empty heading, like Quiz 3 /20. More on the Start here tab.",
        &st.note,
    )
    .map_err(err)?;

    // Headings.
    ws.write_string_with_format(HEADER_ROW, 0, H_LAST, &st.header).map_err(err)?;
    ws.write_string_with_format(HEADER_ROW, 1, H_FIRST, &st.header).map_err(err)?;
    for i in 0..n_cols {
        let c = FIRST_MARK_COL + i;
        match s.assessments.get(i as usize) {
            Some(a) => ws.write_string_with_format(HEADER_ROW, c, a.heading(), &st.header).map_err(err)?,
            None => ws.write_blank(HEADER_ROW, c, &st.new_heading).map_err(err)?,
        };
    }
    ws.set_row_height(HEADER_ROW, 46).map_err(err)?;

    // Students and marks.
    for i in 0..n_rows {
        let r = FIRST_STUDENT_ROW + i;
        let student = s.students.get(i as usize);
        for c in 0..2u16 {
            match linked {
                Some(l) => {
                    let f = Formula::new(link(l, &row_col_to_cell(r, c))).set_result("");
                    ws.write_formula_with_format(r, c, f, &st.linked).map_err(err)?;
                }
                None => {
                    let v = student.map(|p| if c == 0 { p.last.as_str() } else { p.first.as_str() }).unwrap_or("");
                    ws.write_string_with_format(r, c, v, &st.input_text).map_err(err)?;
                }
            }
        }
        for j in 0..n_cols {
            let c = FIRST_MARK_COL + j;
            match s.marks.get(i as usize).and_then(|row| row.get(j as usize)).filter(|v| !v.is_empty()) {
                Some(v) => ws.write_string_with_format(r, c, v, &st.input_text).map_err(err)?,
                None => ws.write_blank(r, c, &st.input_text).map_err(err)?,
            };
        }
    }
    // Colour the codes, like the grid does.
    let tl = row_col_to_cell(FIRST_STUDENT_ROW, FIRST_MARK_COL);
    let rules = [
        (format!("=OR(UPPER(TRIM({tl}))=\"M\",UPPER(TRIM({tl}))=\"N\")"), &st.missing),
        (format!("=UPPER(TRIM({tl}))=\"X\""), &st.excused),
        (format!("=TRIM({tl})=\"?\""), &st.pending),
        (format!("=AND(LEN(TRIM({tl}))>1,UPPER(RIGHT(TRIM({tl}),1))=\"L\")"), &st.late),
    ];
    for (rule, fmt) in rules {
        let cf = ConditionalFormatFormula::new().set_rule(rule.as_str()).set_format(fmt);
        ws.add_conditional_format(FIRST_STUDENT_ROW, FIRST_MARK_COL, last_row, last_col, &cf).map_err(err)?;
    }
    ws.set_freeze_panes(FIRST_STUDENT_ROW, FIRST_MARK_COL).map_err(err)?;
    // Printing: landscape, as wide as one page, headings repeated on every page.
    ws.set_landscape().set_print_fit_to_pages(1, 0);
    ws.set_repeat_rows(HEADER_ROW, HEADER_ROW).map_err(err)?;
    if is_example(&s.class) {
        ws.set_tab_color(Color::RGB(0xB1D2E1));
        example_notes(ws, s)?;
    } else {
        ws.set_tab_color(Color::RGB(PRIMARY));
    }
    Ok(())
}

/// A value in a setup table.
enum Val {
    Text(String),
    Num(f64),
    /// Shows a class tab's cell, so typing the class name there fills it in here.
    Link(String, String),
    Blank,
}

fn text_or_blank(s: &str) -> Val {
    if s.is_empty() { Val::Blank } else { Val::Text(s.to_string()) }
}

fn setup_sheet(
    ws: &mut Worksheet,
    st: &Styles,
    name: &str,
    about: &str,
    headers: &[(&str, f64)],
    rows: &[Vec<Val>],
) -> AppResult<(u32, u32)> {
    ws.set_name(name).map_err(err)?;
    ws.set_tab_color(Color::RGB(SETUP_GREY));
    ws.set_formula_result_default("");
    ws.write_string_with_format(0, 0, about, &st.note).map_err(err)?;
    for (c, (h, w)) in headers.iter().enumerate() {
        ws.write_string_with_format(1, c as u16, *h, &st.setup_header).map_err(err)?;
        ws.set_column_width(c as u16, *w).map_err(err)?;
    }
    let first = 2;
    let last = first + rows.len() as u32 + SPARE_SETUP_ROWS - 1;
    for r in first..=last {
        let row = rows.get((r - first) as usize);
        for c in 0..headers.len() as u16 {
            match row.and_then(|v| v.get(c as usize)).unwrap_or(&Val::Blank) {
                Val::Text(t) => ws.write_string_with_format(r, c, t, &st.input_text).map_err(err)?,
                Val::Num(n) => ws.write_number_with_format(r, c, *n, &st.input_num).map_err(err)?,
                Val::Link(f, v) => ws.write_formula_with_format(r, c, Formula::new(f).set_result(v), &st.linked).map_err(err)?,
                Val::Blank => ws.write_blank(r, c, &st.input_text).map_err(err)?,
            };
        }
    }
    ws.set_freeze_panes(2, 0).map_err(err)?;
    Ok((first, last))
}

fn setup_sheets(wb: &mut Workbook, st: &Styles, tabs: &[(&SheetSpec, &str)]) -> AppResult<()> {
    // The class name, linked to the first tab of that class.
    let class_cell = |i: usize| -> Val {
        let (s, title) = tabs[i];
        let home = s.link_to.as_ref().and_then(|l| tabs.iter().find(|t| &t.0.title == l)).map_or(title, |t| t.1);
        Val::Link(link(home, "$B$1"), s.class.clone())
    };
    let first_of_class = |i: usize| {
        let s = tabs[i].0;
        s.link_to.is_none()
            && (s.class.is_empty() || !tabs[..i].iter().any(|t| t.0.link_to.is_none() && t.0.class.eq_ignore_ascii_case(&s.class)))
    };

    let mut rows = Vec::new();
    for (i, (s, _)) in tabs.iter().enumerate() {
        if first_of_class(i) {
            rows.push(vec![class_cell(i), text_or_blank(&s.course_code), s.period.map_or(Val::Blank, |p| Val::Num(p as f64))]);
        }
    }
    let ws = wb.add_worksheet();
    let (a, b) = setup_sheet(
        ws,
        st,
        SETUP_CLASSES,
        "Optional. Course codes and periods. The class name comes from the top of each class tab.",
        &[(L_CLASS, 34.0), (H_CODE, 14.0), (H_PERIOD, 9.0)],
        &rows,
    )?;
    ws.add_data_validation(a, 2, b, 2, &DataValidation::new().allow_whole_number(DataValidationRule::Between(1, 9))).map_err(err)?;

    let mut rows = Vec::new();
    for (i, (s, _)) in tabs.iter().enumerate() {
        for (name, w) in &s.categories {
            rows.push(vec![class_cell(i), text_or_blank(&s.subject), Val::Text(name.clone()), Val::Num(*w)]);
        }
    }
    let ws = wb.add_worksheet();
    let (a, b) = setup_sheet(
        ws,
        st,
        SETUP_CATEGORIES,
        "Optional. Categories and their weights for each class; the weights should add up to 100. With no categories, every assessment is weighted equally.",
        &[(L_CLASS, 34.0), (L_SUBJECT, 22.0), (H_CATEGORY, 30.0), (H_WEIGHT_PCT, 10.0)],
        &rows,
    )?;
    ws.add_data_validation(a, 3, b, 3, &DataValidation::new().allow_decimal_number(DataValidationRule::Between(0.0, 100.0)))
        .map_err(err)?;

    let mut rows = Vec::new();
    for (i, (s, _)) in tabs.iter().enumerate() {
        for x in &s.assessments {
            rows.push(vec![
                class_cell(i),
                text_or_blank(&s.subject),
                Val::Text(x.name.clone()),
                text_or_blank(&x.category),
                Val::Num(x.weight),
                text_or_blank(&x.unit),
                x.due.as_deref().map_or(Val::Blank, text_or_blank),
                Val::Text(if x.on_report { "Yes" } else { "No" }.into()),
            ]);
        }
    }
    let ws = wb.add_worksheet();
    let (a, b) = setup_sheet(
        ws,
        st,
        SETUP_ASSESSMENTS,
        "Optional. The assessment name is the class tab's heading without /20 or (done). Weight: 1 normal, 2 counts double, 0 practice (shown, not counted). Due dates like 2026-10-15.",
        &[
            (L_CLASS, 30.0),
            (L_SUBJECT, 18.0),
            (H_ASSESSMENT, 28.0),
            (H_CATEGORY, 26.0),
            (H_WEIGHT, 8.0),
            (H_UNIT, 14.0),
            (H_DUE, 12.0),
            (H_REPORT, 10.0),
        ],
        &rows,
    )?;
    ws.add_data_validation(a, 4, b, 4, &DataValidation::new().allow_decimal_number(DataValidationRule::Between(0.0, 100.0)))
        .map_err(err)?;
    ws.add_data_validation(a, 7, b, 7, &DataValidation::new().allow_list_strings(&["Yes", "No"]).map_err(err)?).map_err(err)?;

    let mut rows = Vec::new();
    for (i, (s, _)) in tabs.iter().enumerate() {
        if !first_of_class(i) {
            continue;
        }
        for p in &s.students {
            rows.push(vec![
                class_cell(i),
                Val::Text(p.last.clone()),
                Val::Text(p.first.clone()),
                text_or_blank(&p.number),
                text_or_blank(&p.pronouns),
            ]);
        }
    }
    let ws = wb.add_worksheet();
    let (a, b) = setup_sheet(
        ws,
        st,
        SETUP_STUDENTS,
        "Optional. Student numbers, and pronouns for report comments (she, he or they). Students listed here but not on a class tab are added to that class.",
        &[(L_CLASS, 34.0), (H_LAST, 18.0), (H_FIRST, 18.0), (H_NUMBER, 14.0), (H_PRONOUNS, 11.0)],
        &rows,
    )?;
    ws.add_data_validation(a, 4, b, 4, &DataValidation::new().allow_list_strings(&["she", "he", "they"]).map_err(err)?).map_err(err)?;
    Ok(())
}

// ── Reading ──────────────────────────────────────────────────────────────────

pub fn is_template(bytes: &[u8]) -> bool {
    let Ok(mut x) = calamine::open_workbook_from_rs::<Xlsx<_>, _>(Cursor::new(bytes)) else { return false };
    x.worksheet_range(MARKER_SHEET).ok().is_some_and(|r| text(r.get_value((0, 1))) == FORMAT_ID)
}

/// Something in the file that needs the teacher's attention.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Problem {
    pub sheet: String,
    /// e.g. `F17`; empty for the sheet as a whole.
    pub cell: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Parsed {
    pub sheets: Vec<SheetIn>,
    pub problems: Vec<Problem>,
}

/// One class tab, with what the setup tabs say about it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SheetIn {
    pub sheet: String,
    pub class: String,
    /// Where the class name is, for problems about the class.
    pub class_cell: String,
    pub subject: String,
    pub course_code: Option<String>,
    pub period: Option<i64>,
    /// `None`: not given, leave the categories as they are.
    pub categories: Option<Vec<(String, f64)>>,
    pub assessments: Vec<ColumnIn>,
    pub students: Vec<StudentIn>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ColumnIn {
    /// The heading as typed.
    pub heading: String,
    /// The name, without `/20`, `(done)` or `(level)`.
    pub name: String,
    pub cell: String,
    /// From the heading; `None` when it doesn't say.
    pub kind: Option<Kind>,
    pub out_of: Option<f64>,
    pub setup: AssessmentSetup,
    /// One per student, as typed, with its cell (blank for students only on
    /// Setup - Students).
    pub marks: Vec<(String, String)>,
}

/// An assessment's row on Setup - Assessments. `None` = not given.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AssessmentSetup {
    pub category: Option<String>,
    pub weight: Option<f64>,
    pub unit: Option<String>,
    pub due: Option<String>,
    pub on_report: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StudentIn {
    pub number: String,
    pub last: String,
    pub first: String,
    /// `None`: not given.
    pub pronouns: Option<String>,
    pub cell: String,
}

fn text(d: Option<&Data>) -> String {
    match d {
        Some(Data::String(s)) => s.trim().to_string(),
        Some(Data::Float(f)) => fmt_num(*f),
        Some(Data::Int(i)) => i.to_string(),
        Some(Data::Bool(b)) => if *b { "TRUE" } else { "FALSE" }.into(),
        Some(Data::DateTimeIso(s)) => s.clone(),
        _ => String::new(),
    }
}

fn number(d: Option<&Data>) -> Option<f64> {
    match d {
        Some(Data::Float(f)) => Some(*f),
        Some(Data::Int(i)) => Some(*i as f64),
        Some(Data::String(s)) => s.trim().trim_end_matches('%').trim().parse().ok(),
        _ => None,
    }
}

fn is_date(d: Option<&Data>) -> bool {
    matches!(d, Some(Data::DateTime(_)))
}

/// A due date typed as text (2026-10-15) or entered as an Excel date.
fn date(d: Option<&Data>) -> Option<String> {
    match d {
        Some(Data::DateTime(x)) => {
            let days = x.as_f64().floor() as i64;
            chrono::NaiveDate::from_ymd_opt(1899, 12, 30)
                .and_then(|base| base.checked_add_signed(chrono::Duration::days(days)))
                .map(|d| d.format("%Y-%m-%d").to_string())
        }
        other => crate::routes::valid_date(&text(other)),
    }
}

fn clip(s: String, n: usize) -> String {
    s.chars().take(n).collect()
}

fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Find a label: the first cell in the given rows/columns whose text matches.
fn find(r: &Range<Data>, label: &str, rows: std::ops::Range<u32>, cols: std::ops::Range<u32>) -> Option<(u32, u32)> {
    rows.flat_map(|row| cols.clone().map(move |col| (row, col)))
        .find(|&(row, col)| text(r.get_value((row, col))).eq_ignore_ascii_case(label))
}

fn cell_ref(row: u32, col: u32) -> String {
    row_col_to_cell(row, col as u16)
}

/// Read a heading such as `Quiz 1 /20`, `Quiz 1 (/20)`, `Journal (done)` or
/// `Participation (level)`.
fn read_heading(h: &str) -> (String, Option<Kind>, Option<f64>) {
    let t = h.trim();
    let lower = t.to_lowercase();
    for (tag, kind) in [(DONE_TAG, Kind::Completion), ("(y/n)", Kind::Completion), (LEVEL_TAG, Kind::Level), ("(levels)", Kind::Level)] {
        if lower.ends_with(tag) {
            return (t[..t.len() - tag.len()].trim().to_string(), Some(kind), None);
        }
    }
    let body = t.strip_suffix(')').and_then(|b| b.rsplit_once("(/").map(|(n, o)| format!("{n}/{o}"))).unwrap_or_else(|| t.to_string());
    if let Some((name, out)) = body.rsplit_once('/')
        && !name.trim().is_empty()
        && let Ok(o) = out.trim().parse::<f64>()
        && o > 0.0
        && o <= 10_000.0
    {
        return (name.trim().to_string(), Some(Kind::Scored), Some(o));
    }
    (t.to_string(), None, None)
}

/// A starter spreadsheet upload: an error if nothing is filled in.
pub fn parse(bytes: &[u8]) -> AppResult<Parsed> {
    let mut out = parse_tabs(bytes)?;
    if out.sheets.is_empty() && out.problems.is_empty() {
        return Err(bad(
            "The spreadsheet doesn't have any classes filled in yet. Type a class name at the top of a class tab, add students and marks, then upload it again.",
        ));
    }
    // The example's students, left in when the example was typed over.
    let mut found = Vec::new();
    for s in &out.sheets {
        for st in s.students.iter().filter(|st| EXAMPLE_STUDENTS.iter().any(|(f, l)| same(&st.first, f) && same(&st.last, l))) {
            found.push(Problem {
                sheet: s.sheet.clone(),
                cell: st.cell.clone(),
                message: format!(
                    "{} {} is one of the example students. Delete the row if they're not in your class (they'll be added otherwise).",
                    st.first, st.last
                ),
            });
        }
    }
    out.problems.extend(found);
    Ok(out)
}

/// Every class tab, with the setup tabs applied (also used for workbooks).
pub fn parse_tabs(bytes: &[u8]) -> AppResult<Parsed> {
    let mut x =
        calamine::open_workbook_from_rs::<Xlsx<_>, _>(Cursor::new(bytes)).map_err(|_| bad("That file isn't an Excel workbook (.xlsx)."))?;
    let version = x.worksheet_range(MARKER_SHEET).ok().map(|r| number(r.get_value((1, 1))).unwrap_or(1.0) as i64);
    if version == Some(1) {
        return Ok(v1::parse(&mut x));
    }
    let visible: Vec<String> = x
        .sheets_metadata()
        .iter()
        .filter(|s| s.visible == SheetVisible::Visible && !s.name.starts_with('_'))
        .map(|s| s.name.clone())
        .collect();
    let mut out = Parsed::default();
    for name in visible.iter().filter(|n| *n != START_SHEET && !SETUP_SHEETS.contains(&n.as_str())).take(MAX_SHEETS) {
        let Ok(range) = x.worksheet_range(name) else { continue };
        if let Some(sheet) = class_tab(name, &range, &mut out.problems) {
            out.sheets.push(sheet);
        }
    }
    let setup = |x: &mut Xlsx<_>, name: &str, tabs: &[SheetIn]| {
        let range = x.worksheet_range(name).ok()?;
        SetupTable::read(name, range, x.worksheet_formula(name).ok(), tabs)
    };
    if let Some(t) = setup(&mut x, SETUP_CLASSES, &out.sheets) {
        apply_classes(&t, &mut out);
    }
    if let Some(t) = setup(&mut x, SETUP_CATEGORIES, &out.sheets) {
        apply_categories(&t, &mut out);
    }
    if let Some(t) = setup(&mut x, SETUP_ASSESSMENTS, &out.sheets) {
        apply_assessments(&t, &mut out);
    }
    if let Some(t) = setup(&mut x, SETUP_STUDENTS, &out.sheets) {
        apply_students(&t, &mut out);
    }
    Ok(out)
}

fn class_tab(name: &str, r: &Range<Data>, problems: &mut Vec<Problem>) -> Option<SheetIn> {
    let end = r.end().unwrap_or((0, 0));
    let (max_row, max_col) = (end.0 + 1, end.1 + 1);
    // Not a class tab (e.g. the teacher's own notes): skip quietly.
    let (class_row, class_col) = find(r, L_CLASS, 0..8, 0..3)?;
    let (h_row, c_last) = find(r, H_LAST, class_row + 1..class_row + 12, 0..4)?;
    let c_first = find(r, H_FIRST, h_row..h_row + 1, 0..6).map(|p| p.1);
    let mut problem = |cell: String, message: String| problems.push(Problem { sheet: name.to_string(), cell, message });
    let value = |label: &str| find(r, label, class_row..class_row + 4, class_col..class_col + 1).map(|(row, col)| (row, col + 1));

    let (cr, cc) = value(L_CLASS)?;
    let class = clip(text(r.get_value((cr, cc))), 120);
    // The example is a guide, never imported.
    if is_example(&class) {
        return None;
    }
    let subject = value(L_SUBJECT).map(|(row, col)| clip(text(r.get_value((row, col))), 120)).unwrap_or_default();
    let first_mark_col = c_last.max(c_first.unwrap_or(0)) + 1;

    // Students.
    let mut students = Vec::new();
    let mut rows = Vec::new();
    let get = |row: u32, col: Option<u32>| col.map(|c| clip(text(r.get_value((row, c))), 80)).unwrap_or_default();
    for row in h_row + 1..max_row.min(h_row + 1 + MAX_STUDENTS) {
        let (last, first) = (get(row, Some(c_last)), get(row, c_first));
        if first.is_empty() && last.is_empty() {
            let has_marks = (first_mark_col..max_col).any(|c| !text(r.get_value((row, c))).is_empty() || is_date(r.get_value((row, c))));
            if has_marks {
                problem(cell_ref(row, c_last), "This row has marks but no name; it was left out.".into());
            }
            continue;
        }
        let (first, last) = if first.is_empty() { (last, String::new()) } else { (first, last) };
        students.push(StudentIn { number: String::new(), last, first, pronouns: None, cell: cell_ref(row, c_last) });
        rows.push(row);
    }

    // Assessments, one per column.
    let mut assessments = Vec::new();
    for col in first_mark_col..max_col.min(first_mark_col + MAX_ASSESSMENTS) {
        let heading = clip(text(r.get_value((h_row, col))), 140);
        let any_marks = rows.iter().any(|&row| !text(r.get_value((row, col))).is_empty() || is_date(r.get_value((row, col))));
        if heading.is_empty() {
            if any_marks {
                problem(cell_ref(h_row, col), "This column has marks but no heading; it was left out.".into());
            }
            continue;
        }
        let (name, kind, out_of) = read_heading(&heading);
        let marks = rows
            .iter()
            .map(|&row| {
                let v = r.get_value((row, col));
                let t = if is_date(v) {
                    problem(cell_ref(row, col), "Excel changed this mark into a date. Retype it (for example 3/4 or 75%).".into());
                    // Neither a mark nor a blank: left alone.
                    "\u{0}".to_string()
                } else {
                    text(v)
                };
                (t, cell_ref(row, col))
            })
            .collect();
        assessments.push(ColumnIn {
            heading: heading.clone(),
            name: clip(name, 120),
            cell: cell_ref(h_row, col),
            kind,
            out_of,
            setup: AssessmentSetup::default(),
            marks,
        });
    }

    if class.is_empty() {
        if !students.is_empty() || !assessments.is_empty() {
            problem(cell_ref(cr, cc), "This tab has students or assessments but no class name; it was left out.".into());
        }
        return None;
    }
    Some(SheetIn {
        sheet: name.to_string(),
        class,
        class_cell: cell_ref(cr, cc),
        subject,
        course_code: None,
        period: None,
        categories: None,
        assessments,
        students,
    })
}

/// A setup tab: its rows, by heading.
struct SetupTable {
    name: String,
    cols: HashMap<&'static str, u32>,
    range: Range<Data>,
    rows: std::ops::Range<u32>,
    /// Class cells whose formula links to a class tab but that have no saved
    /// result (some tools other than Excel don't save one): the tab's class.
    linked: HashMap<u32, String>,
}

/// The tab a formula like `IF('P1 Math'!$B$1="","",'P1 Math'!$B$1)` links to.
fn linked_tab(formula: &str) -> Option<String> {
    let at = formula.find("!$B$1")?;
    let before = &formula[..at];
    if let Some(inner) = before.strip_suffix('\'') {
        // Back to the opening quote ('' inside is a quote in the name).
        let b = inner.as_bytes();
        let mut i = inner.len();
        while i > 0 {
            i -= 1;
            if b[i] == b'\'' {
                if i > 0 && b[i - 1] == b'\'' {
                    i -= 1;
                    continue;
                }
                return Some(inner[i + 1..].replace("''", "'"));
            }
        }
        None
    } else {
        let start = before.rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.')).map_or(0, |i| i + 1);
        Some(before[start..].to_string()).filter(|t| !t.is_empty())
    }
}

impl SetupTable {
    fn read(name: &str, range: Range<Data>, formulas: Option<Range<String>>, tabs: &[SheetIn]) -> Option<SetupTable> {
        let (h_row, _) = find(&range, L_CLASS, 0..6, 0..1)?;
        let end = range.end().unwrap_or((0, 0));
        let mut cols = HashMap::new();
        for h in [
            L_CLASS,
            L_SUBJECT,
            H_CODE,
            H_PERIOD,
            H_CATEGORY,
            H_WEIGHT_PCT,
            H_ASSESSMENT,
            H_WEIGHT,
            H_UNIT,
            H_DUE,
            H_REPORT,
            H_LAST,
            H_FIRST,
            H_NUMBER,
            H_PRONOUNS,
        ] {
            if let Some((_, c)) = find(&range, h, h_row..h_row + 1, 0..(end.1 + 1).min(30)) {
                cols.insert(h, c);
            }
        }
        let rows = h_row + 1..(end.0 + 1).min(h_row + 1 + MAX_SETUP_ROWS);
        let mut linked = HashMap::new();
        if let (Some(f), Some(&c)) = (formulas, cols.get(L_CLASS)) {
            for row in rows.clone() {
                if !text(range.get_value((row, c))).is_empty() {
                    continue;
                }
                let tab = f.get_value((row, c)).and_then(|f| linked_tab(f));
                if let Some(s) = tab.and_then(|tab| tabs.iter().find(|s| s.sheet == tab)) {
                    linked.insert(row, s.class.clone());
                }
            }
        }
        Some(SetupTable { name: name.to_string(), cols, range, rows, linked })
    }

    fn get(&self, row: u32, h: &str) -> Option<&Data> {
        self.cols.get(h).and_then(|c| self.range.get_value((row, *c)))
    }

    fn text(&self, row: u32, h: &str) -> String {
        text(self.get(row, h))
    }

    fn cell(&self, row: u32, h: &str) -> String {
        cell_ref(row, self.cols.get(h).copied().unwrap_or(0))
    }

    /// Rows with a class that isn't the example.
    fn rows(&self) -> impl Iterator<Item = (u32, String)> + '_ {
        self.rows.clone().filter_map(|row| {
            let mut class = self.text(row, L_CLASS);
            if class.is_empty() {
                class = self.linked.get(&row).cloned().unwrap_or_default();
            }
            (!class.is_empty() && !is_example(&class)).then_some((row, class))
        })
    }

    fn problem(&self, out: &mut Parsed, row: u32, h: &str, message: String) {
        out.problems.push(Problem { sheet: self.name.clone(), cell: self.cell(row, h), message });
    }
}

/// The class tabs a setup row is about: same class, and the same subject
/// unless the row leaves Subject blank.
fn matching<'a>(out: &'a mut Parsed, class: &str, subject: &str) -> impl Iterator<Item = &'a mut SheetIn> {
    let (class, subject) = (class.to_string(), subject.to_string());
    out.sheets.iter_mut().filter(move |s| same(&s.class, &class) && (subject.is_empty() || same(&s.subject, &subject)))
}

fn no_tab(t: &SetupTable, out: &mut Parsed, row: u32, class: &str) {
    t.problem(out, row, L_CLASS, format!("There's no class tab for {class}, so this row was left out."));
}

fn apply_classes(t: &SetupTable, out: &mut Parsed) {
    for (row, class) in t.rows() {
        let code = clip(t.text(row, H_CODE), 40);
        let period = match t.get(row, H_PERIOD) {
            v if text(v).is_empty() => None,
            v => match number(v) {
                Some(p) if p.fract() == 0.0 && (1.0..=9.0).contains(&p) => Some(p as i64),
                _ => {
                    t.problem(out, row, H_PERIOD, format!("\"{}\" isn't a period number; it was left out.", text(v)));
                    None
                }
            },
        };
        let mut found = false;
        for s in matching(out, &class, "") {
            found = true;
            if !code.is_empty() {
                s.course_code = Some(code.clone());
            }
            s.period = s.period.or(period);
        }
        if !found {
            no_tab(t, out, row, &class);
        }
    }
}

fn apply_categories(t: &SetupTable, out: &mut Parsed) {
    for (row, class) in t.rows() {
        let subject = t.text(row, L_SUBJECT);
        let name = clip(t.text(row, H_CATEGORY), 80);
        let w = t.get(row, H_WEIGHT_PCT);
        if name.is_empty() {
            if !text(w).is_empty() {
                t.problem(out, row, H_CATEGORY, "This weight has no category name; it was left out.".into());
            }
            continue;
        }
        let weight = match number(w) {
            Some(v) if (0.0..=100.0).contains(&v) => v,
            None if text(w).is_empty() => {
                t.problem(out, row, H_WEIGHT_PCT, format!("{name} has no weight, so assessments in it won't count."));
                0.0
            }
            _ => {
                t.problem(out, row, H_WEIGHT_PCT, format!("The weight for {name} should be a number from 0 to 100; it was left out."));
                continue;
            }
        };
        let mut found = false;
        for s in matching(out, &class, &subject) {
            found = true;
            let cats = s.categories.get_or_insert_with(Vec::new);
            match cats.iter_mut().find(|c| same(&c.0, &name)) {
                Some(c) => c.1 = weight,
                None => cats.push((name.clone(), weight)),
            }
        }
        if !found {
            no_tab(t, out, row, &class);
        }
    }
    // Weights that don't add up to 100, once per tab.
    let mut notes = Vec::new();
    for s in &out.sheets {
        if let Some(cats) = &s.categories {
            let total: f64 = cats.iter().map(|c| c.1).sum();
            if !cats.is_empty() && (total - 100.0).abs() > 0.01 {
                notes.push(Problem {
                    sheet: t.name.clone(),
                    cell: String::new(),
                    message: format!(
                        "The category weights for {}{} add up to {}, not 100. They're used as given (relative to each other).",
                        s.class,
                        if s.subject.is_empty() { String::new() } else { format!(" ({})", s.subject) },
                        fmt_num(total)
                    ),
                });
            }
        }
    }
    out.problems.extend(notes);
}

fn apply_assessments(t: &SetupTable, out: &mut Parsed) {
    for (row, class) in t.rows() {
        let subject = t.text(row, L_SUBJECT);
        let name = t.text(row, H_ASSESSMENT);
        if name.is_empty() {
            continue;
        }
        let weight = match t.get(row, H_WEIGHT) {
            v if text(v).is_empty() => None,
            v => match number(v) {
                Some(w) if (0.0..=100.0).contains(&w) => Some(w),
                _ => {
                    t.problem(
                        out,
                        row,
                        H_WEIGHT,
                        format!("The weight for {name} should be a number from 0 to 100; it was left as it was."),
                    );
                    None
                }
            },
        };
        let due = match t.get(row, H_DUE) {
            v if text(v).is_empty() && !is_date(v) => None,
            v => match date(v) {
                Some(d) => Some(d),
                None => {
                    t.problem(out, row, H_DUE, format!("\"{}\" isn't a date (use 2026-10-15); it was left out.", text(v)));
                    None
                }
            },
        };
        let on_report = match t.text(row, H_REPORT).to_lowercase().as_str() {
            "" => None,
            "yes" | "y" | "true" | "1" => Some(true),
            "no" | "n" | "false" | "0" => Some(false),
            other => {
                t.problem(out, row, H_REPORT, format!("\"{other}\" should be Yes or No; it was left as it was."));
                None
            }
        };
        let text_opt = |h: &str, n: usize| Some(clip(t.text(row, h), n)).filter(|s| !s.is_empty());
        let setup = AssessmentSetup { category: text_opt(H_CATEGORY, 80), weight, unit: text_opt(H_UNIT, 60), due, on_report };
        let mut found = false;
        for s in matching(out, &class, &subject) {
            for c in s.assessments.iter_mut().filter(|c| same(&c.name, &name) || same(&c.heading, &name)) {
                c.setup = setup.clone();
                found = true;
            }
        }
        if !found {
            t.problem(out, row, H_ASSESSMENT, format!("{name} isn't a column on the {class} tab, so this row was left out."));
        }
    }
}

fn apply_students(t: &SetupTable, out: &mut Parsed) {
    for (row, class) in t.rows() {
        let (last, first) = (clip(t.text(row, H_LAST), 80), clip(t.text(row, H_FIRST), 80));
        if last.is_empty() && first.is_empty() {
            continue;
        }
        let (first, last) = if first.is_empty() { (last, String::new()) } else { (first, last) };
        let number = clip(t.text(row, H_NUMBER), 40);
        let pronouns = match t.text(row, H_PRONOUNS).to_lowercase().as_str() {
            "" => None,
            "she" | "her" | "f" => Some("she"),
            "he" | "him" | "m" => Some("he"),
            "they" | "them" => Some("they"),
            other => {
                t.problem(out, row, H_PRONOUNS, format!("Pronouns should be she, he or they (not \"{other}\"); they was used."));
                Some("they")
            }
        };
        let cell = t.cell(row, H_LAST);
        let mut found = false;
        let mut first_sheet = true;
        for s in matching(out, &class, "") {
            found = true;
            match s.students.iter_mut().find(|x| same(&x.first, &first) && same(&x.last, &last)) {
                Some(x) => {
                    x.number = number.clone();
                    x.pronouns = pronouns.map(String::from);
                }
                // Only on this tab: added to the class (once).
                None if first_sheet => {
                    let n = s.students.len();
                    s.students.push(StudentIn {
                        number: number.clone(),
                        last: last.clone(),
                        first: first.clone(),
                        pronouns: pronouns.map(String::from),
                        cell: format!("{}!{cell}", t.name),
                    });
                    for c in &mut s.assessments {
                        c.marks.resize(n + 1, (String::new(), String::new()));
                    }
                }
                None => {}
            }
            first_sheet = false;
        }
        if !found {
            no_tab(t, out, row, &class);
        }
    }
}

/// Starter spreadsheets from before the plain grid (version 1): class details
/// and a categories table at the top, then one row each for an assessment's
/// name, category, out of, weight and type above the students.
mod v1 {
    use super::*;

    const HELP_SHEET: &str = "How to fill this in";
    const L_CODE: &str = "Course code";
    const L_PERIOD: &str = "Period";
    const L_CATEGORY: &str = "Category";
    const L_WEIGHT_PCT: &str = "Weight %";
    const L_ASSESSMENT: &str = "Assessment";
    const L_OUT_OF: &str = "Out of";
    const L_WEIGHT: &str = "Weight";
    const L_TYPE: &str = "Type";
    const H_NUMBER: &str = "Student #";

    pub fn parse(x: &mut Xlsx<Cursor<&[u8]>>) -> Parsed {
        let names = x.sheet_names().to_vec();
        let mut out = Parsed::default();
        for name in names.into_iter().filter(|n| n != MARKER_SHEET && n != HELP_SHEET).take(MAX_SHEETS) {
            let Ok(range) = x.worksheet_range(&name) else { continue };
            if let Some(sheet) = sheet(&name, &range, &mut out.problems) {
                out.sheets.push(sheet);
            }
        }
        out
    }

    fn sheet(name: &str, r: &Range<Data>, problems: &mut Vec<Problem>) -> Option<SheetIn> {
        let end = r.end().unwrap_or((0, 0));
        let (max_row, max_col) = (end.0 + 1, end.1 + 1);
        let (class_row, class_col) = find(r, L_CLASS, 0..20, 0..3)?;
        let (a_row, a_col) = find(r, L_ASSESSMENT, 0..40, 0..8)?;
        let (h_row, _) = find(r, H_NUMBER, a_row..a_row + 20, 0..1).or_else(|| find(r, H_LAST, a_row..a_row + 20, 0..8))?;
        let mut problem = |cell: String, message: String| problems.push(Problem { sheet: name.to_string(), cell, message });
        let value = |label: &str| {
            find(r, label, class_row..class_row + 8, class_col..class_col + 1).map(|(row, col)| (row, col + 1, r.get_value((row, col + 1))))
        };
        let class = value(L_CLASS).map(|v| clip(text(v.2), 120)).unwrap_or_default();
        if is_example(&class) {
            return None;
        }
        let class_cell = value(L_CLASS).map(|v| cell_ref(v.0, v.1)).unwrap_or_default();
        let subject = value(L_SUBJECT).map(|v| clip(text(v.2), 120)).unwrap_or_default();
        let course_code = value(L_CODE).map(|v| clip(text(v.2), 40)).filter(|c| !c.is_empty());
        let period = match value(L_PERIOD) {
            Some((row, col, v)) if !text(v).is_empty() => match number(v) {
                Some(p) if p.fract() == 0.0 && (1.0..=9.0).contains(&p) => Some(p as i64),
                _ => {
                    problem(cell_ref(row, col), format!("\"{}\" isn't a period number; it was left out.", text(v)));
                    None
                }
            },
            _ => None,
        };

        let mut categories: Vec<(String, f64)> = Vec::new();
        if let Some((w_row, w_col)) = find(r, L_WEIGHT_PCT, 0..a_row, 0..max_col.min(40)) {
            for col in w_col + 1..max_col.min(w_col + 1 + 50) {
                let n = clip(text(r.get_value((w_row - 1, col))), 80);
                let w = r.get_value((w_row, col));
                if n.is_empty() {
                    continue;
                }
                match number(w) {
                    Some(v) if (0.0..=100.0).contains(&v) => categories.push((n, v)),
                    None if text(w).is_empty() => categories.push((n, 0.0)),
                    _ => problem(cell_ref(w_row, col), format!("The weight for {n} should be a number from 0 to 100.")),
                }
            }
        }

        let col_of = |h: &str| find(r, h, h_row..h_row + 1, 0..max_col.min(20)).map(|p| p.1);
        let (c_num, c_last, c_first, c_pron) = (col_of(H_NUMBER), col_of(H_LAST), col_of(H_FIRST), col_of(H_PRONOUNS));
        let get = |row: u32, col: Option<u32>| col.map(|c| clip(text(r.get_value((row, c))), 80)).unwrap_or_default();
        let mut students = Vec::new();
        let mut rows = Vec::new();
        for row in h_row + 1..max_row.min(h_row + 1 + MAX_STUDENTS) {
            let (number, last, first) = (get(row, c_num), get(row, c_last), get(row, c_first));
            if first.is_empty() && last.is_empty() {
                continue;
            }
            let pronouns = match get(row, c_pron).to_lowercase().as_str() {
                "" => None,
                "she" | "her" | "f" => Some("she".to_string()),
                "he" | "him" | "m" => Some("he".to_string()),
                _ => Some("they".to_string()),
            };
            let (first, last) = if first.is_empty() { (last, String::new()) } else { (first, last) };
            students.push(StudentIn { number, last, first, pronouns, cell: cell_ref(row, c_last.unwrap_or(1)) });
            rows.push(row);
        }

        let row_of = |l: &str| find(r, l, a_row + 1..a_row + 8, a_col..a_col + 1).map(|p| p.0);
        let (r_cat, r_out, r_weight, r_type) = (row_of(L_CATEGORY), row_of(L_OUT_OF), row_of(L_WEIGHT), row_of(L_TYPE));
        let mut assessments = Vec::new();
        for col in a_col + 1..max_col.min(a_col + 1 + MAX_ASSESSMENTS) {
            let name = clip(text(r.get_value((a_row, col))), 120);
            if name.is_empty() {
                continue;
            }
            let at = |row: Option<u32>| row.map(|rw| r.get_value((rw, col)));
            let done = r_type.is_some_and(|rt| text(r.get_value((rt, col))).to_lowercase().starts_with("done"));
            let out_of = match at(r_out).flatten() {
                v if text(v).is_empty() => 100.0,
                v => number(v).filter(|o| *o > 0.0 && *o <= 10_000.0).unwrap_or(100.0),
            };
            let weight = number(at(r_weight).flatten()).filter(|w| (0.0..=100.0).contains(w));
            let category = r_cat.map(|rc| clip(text(r.get_value((rc, col))), 80)).filter(|c| !c.is_empty());
            let marks = rows
                .iter()
                .map(|&row| {
                    let v = r.get_value((row, col));
                    if is_date(v) {
                        problem(cell_ref(row, col), "Excel changed this mark into a date. Retype it (for example 3/4 or 75%).".into());
                        ("\u{0}".to_string(), cell_ref(row, col))
                    } else {
                        (text(v), cell_ref(row, col))
                    }
                })
                .collect();
            assessments.push(ColumnIn {
                heading: name.clone(),
                name,
                cell: cell_ref(a_row, col),
                kind: Some(if done { Kind::Completion } else { Kind::Scored }),
                out_of: (!done).then_some(out_of),
                setup: AssessmentSetup { category, weight: Some(weight.unwrap_or(1.0)), ..Default::default() },
                marks,
            });
        }
        if class.is_empty() {
            return None;
        }
        Some(SheetIn {
            sheet: name.to_string(),
            class,
            class_cell,
            subject,
            course_code,
            period,
            categories: (!categories.is_empty()).then_some(categories),
            assessments,
            students,
        })
    }
}

// ── Importing ────────────────────────────────────────────────────────────────

#[derive(Debug, Default, Serialize)]
pub struct Outcome {
    pub classes_new: Vec<String>,
    pub classes_updated: Vec<String>,
    pub subjects_new: usize,
    pub students_new: usize,
    pub assessments_new: usize,
    pub marks_changed: usize,
    /// Categories, weights, units, course codes… that changed.
    pub settings_changed: usize,
    pub problems: Vec<Problem>,
    /// Defaults Gradebook chose, worth a look.
    pub notes: Vec<Problem>,
}

impl Outcome {
    pub fn changes(&self) -> usize {
        self.classes_new.len() + self.subjects_new + self.students_new + self.assessments_new + self.marks_changed + self.settings_changed
    }
}

/// Whether a mark read from the sheet is the one already saved (the sheet
/// shows two decimals, and done columns show done as Y whether late or not).
fn unchanged(kind: Kind, old: &MarkEntry, new: &MarkEntry) -> bool {
    let done = |m: &MarkEntry| matches!(m.status, Status::Marked | Status::Late);
    if kind == Kind::Completion && done(old) && done(new) {
        return true;
    }
    old.status == new.status
        && match (old.value, new.value) {
            (Some(a), Some(b)) => (a - b).abs() < 0.006,
            (a, b) => a == b,
        }
}

/// Create or update everything in the file, in the current term. Run inside
/// a transaction; the preview runs it the same way and rolls back.
///
/// `clear_blanks`: a blank cell clears the mark (a saved workbook, whose grid
/// is the marks) rather than leaving it alone (the starter spreadsheet).
pub fn apply(tx: &Transaction, user_id: i64, p: &Parsed, clear_blanks: bool) -> AppResult<Outcome> {
    let settings = model::settings(tx, user_id)?;
    let term = model::current_term(tx, user_id)?.ok_or_else(|| bad("Create a term first."))?;
    let mut out = Outcome { problems: p.problems.clone(), ..Default::default() };
    fn problem(list: &mut Vec<Problem>, sheet: &str, cell: &str, message: String) {
        list.push(Problem { sheet: sheet.to_string(), cell: cell.to_string(), message })
    }

    // Tabs with the same class name are one class (one tab per subject).
    let mut groups: Vec<(String, Vec<&SheetIn>)> = Vec::new();
    for s in &p.sheets {
        match groups.iter_mut().find(|g| same(&g.0, &s.class)) {
            Some(g) => g.1.push(s),
            None => groups.push((s.class.clone(), vec![s])),
        }
    }

    for (class_name, sheets) in groups {
        let classes = model::classes_in_term(tx, user_id, term.id)?;
        let first = sheets[0];
        let code = sheets.iter().find_map(|s| s.course_code.clone());
        let class_id = match classes.iter().find(|c| same(&c.name, &class_name)) {
            Some(c) => {
                out.classes_updated.push(c.name.clone());
                if let Some(code) = code {
                    out.settings_changed +=
                        tx.execute("UPDATE classes SET course_code = ?1 WHERE id = ?2 AND course_code != ?1", params![code, c.id])?;
                }
                c.id
            }
            None => {
                if settings.mode == Mode::Secondary && classes.len() as i64 >= settings.active_periods {
                    problem(
                        &mut out.problems,
                        &first.sheet,
                        &first.class_cell,
                        format!(
                            "{class_name} wasn't added: you're set up for {} teaching period(s) and they're all used. Change the number of periods in Settings, then upload again.",
                            settings.active_periods
                        ),
                    );
                    continue;
                }
                let period = match sheets.iter().find_map(|s| s.period) {
                    Some(pd) if settings.mode != Mode::Secondary => Some(pd),
                    Some(pd) if Some(pd) == settings.prep_period || classes.iter().any(|c| c.period == Some(pd)) => {
                        problem(
                            &mut out.problems,
                            SETUP_CLASSES,
                            "",
                            format!("Period {pd} is your prep period or already has a class, so {class_name} was added without a period."),
                        );
                        None
                    }
                    pd => pd,
                };
                tx.execute(
                    "INSERT INTO classes (user_id, term_id, name, course_code, period, sort) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![user_id, term.id, class_name, code.unwrap_or_default(), period, classes.len() as i64],
                )?;
                out.classes_new.push(class_name.clone());
                tx.last_insert_rowid()
            }
        };

        let mut roster = model::roster(tx, class_id)?;
        for s in sheets {
            // Subject.
            let mut subjects = model::subjects(tx, class_id)?;
            let wanted = if s.subject.is_empty() { class_name.as_str() } else { s.subject.as_str() };
            let subject_id = match subjects.iter().find(|x| same(&x.name, wanted)) {
                Some(x) => x.id,
                None if s.subject.is_empty() && subjects.len() == 1 => subjects.remove(0).id,
                None => {
                    tx.execute(
                        "INSERT INTO subjects (class_id, name, sort) VALUES (?1, ?2, ?3)",
                        params![class_id, wanted, subjects.len() as i64],
                    )?;
                    out.subjects_new += 1;
                    tx.last_insert_rowid()
                }
            };

            // Categories: add new ones, update weights. None are removed.
            let mut cats: Vec<(i64, String)> = model::categories(tx, subject_id)?.into_iter().map(|c| (c.id, c.name)).collect();
            for (name, weight) in s.categories.iter().flatten() {
                match cats.iter().find(|c| same(&c.1, name)) {
                    Some(c) => {
                        out.settings_changed += tx
                            .execute("UPDATE categories SET weight = ?1 WHERE id = ?2 AND abs(weight - ?1) > 1e-9", params![weight, c.0])?;
                    }
                    None => {
                        tx.execute(
                            "INSERT INTO categories (subject_id, name, weight, sort) VALUES (?1, ?2, ?3, ?4)",
                            params![subject_id, name, weight, cats.len() as i64],
                        )?;
                        out.settings_changed += 1;
                        cats.push((tx.last_insert_rowid(), name.clone()));
                    }
                }
            }

            // Students: in the class by number, then by name; then the
            // teacher's other classes by number; otherwise added.
            let mut ids = Vec::with_capacity(s.students.len());
            for st in &s.students {
                let found = roster
                    .iter()
                    .find(|x| !st.number.is_empty() && x.student_number == st.number)
                    .or_else(|| roster.iter().find(|x| same(&x.first_name, &st.first) && same(&x.last_name, &st.last)))
                    .map(|x| x.id);
                let id = match found {
                    Some(id) => id,
                    None => {
                        let elsewhere: Option<i64> = if st.number.is_empty() {
                            None
                        } else {
                            tx.query_row(
                                "SELECT id FROM students WHERE user_id = ?1 AND student_number = ?2",
                                params![user_id, st.number],
                                |r| r.get(0),
                            )
                            .optional()?
                        };
                        let id = match elsewhere {
                            Some(id) => id,
                            None => {
                                tx.execute(
                                    "INSERT INTO students (user_id, first_name, last_name, student_number, pronouns) VALUES (?1, ?2, ?3, ?4, ?5)",
                                    params![user_id, st.first, st.last, st.number, st.pronouns.as_deref().unwrap_or("they")],
                                )?;
                                out.students_new += 1;
                                tx.last_insert_rowid()
                            }
                        };
                        tx.execute(
                            "INSERT INTO enrolments (class_id, student_id, active) VALUES (?1, ?2, 1)
                             ON CONFLICT(class_id, student_id) DO UPDATE SET active = 1",
                            params![class_id, id],
                        )?;
                        roster = model::roster(tx, class_id)?;
                        id
                    }
                };
                if let Some(p) = &st.pronouns {
                    out.settings_changed +=
                        tx.execute("UPDATE students SET pronouns = ?1 WHERE id = ?2 AND pronouns != ?1", params![p, id])?;
                }
                if !st.number.is_empty() {
                    out.settings_changed += tx.execute(
                        "UPDATE students SET student_number = ?1 WHERE id = ?2 AND student_number != ?1",
                        params![st.number, id],
                    )?;
                }
                ids.push(id);
            }

            // Assessments and marks.
            let existing = model::assessments(tx, subject_id)?;
            let saved = model::marks_for_subject(tx, subject_id)?;
            let mut sort: i64 =
                tx.query_row("SELECT COALESCE(MAX(sort), 0) + 1 FROM assessments WHERE subject_id = ?1", [subject_id], |r| r.get(0))?;
            let mut changes = Vec::new();
            // The category of the last column of each type, for new ones.
            let mut prev_cat: HashMap<Kind, i64> = HashMap::new();
            for col in &s.assessments {
                let su = &col.setup;
                let category = match &su.category {
                    Some(name) => match cats.iter().find(|c| same(&c.1, name)) {
                        Some(c) => Some(c.0),
                        None => {
                            problem(
                                &mut out.problems,
                                &s.sheet,
                                &col.cell,
                                format!("\"{name}\" (for {}) isn't one of the categories; it was added with weight 0.", col.name),
                            );
                            tx.execute(
                                "INSERT INTO categories (subject_id, name, weight, sort) VALUES (?1, ?2, 0, ?3)",
                                params![subject_id, name, cats.len() as i64],
                            )?;
                            cats.push((tx.last_insert_rowid(), name.clone()));
                            Some(tx.last_insert_rowid())
                        }
                    },
                    None => None,
                };
                let found =
                    existing.iter().find(|x| same(&x.name, &col.heading)).or_else(|| existing.iter().find(|x| same(&x.name, &col.name)));
                // (assessment id, kind, what the sheet's marks are out of, scale onto Gradebook's)
                let (aid, kind, sheet_out_of, scale) = match found {
                    Some(x) => {
                        let kind = Kind::parse(&x.kind);
                        if col.kind.is_some_and(|k| k != kind) {
                            problem(
                                &mut out.problems,
                                &s.sheet,
                                &col.cell,
                                format!("{} is already in Gradebook as a different type; its type wasn't changed.", x.name),
                            );
                        }
                        let sheet_out_of = match (kind, col.kind, col.out_of) {
                            (Kind::Scored, Some(Kind::Scored), Some(o)) => o,
                            _ => x.out_of,
                        };
                        if kind == Kind::Scored && (x.out_of - sheet_out_of).abs() > 1e-9 {
                            problem(
                                &mut out.problems,
                                &s.sheet,
                                &col.cell,
                                format!(
                                    "{} is out of {} in Gradebook but {} here. Marks were read as out of {} and converted; to change what it's out of, edit the assessment in Gradebook.",
                                    x.name,
                                    fmt_num(x.out_of),
                                    fmt_num(sheet_out_of),
                                    fmt_num(sheet_out_of)
                                ),
                            );
                        }
                        let (unit, due) = (su.unit.as_deref(), su.due.as_deref());
                        out.settings_changed += tx.execute(
                            "UPDATE assessments SET weight = COALESCE(?1, weight), category_id = COALESCE(?2, category_id),
                                unit = COALESCE(?3, unit), due_date = COALESCE(?4, due_date), include_in_report = COALESCE(?5, include_in_report)
                             WHERE id = ?6 AND (weight IS NOT COALESCE(?1, weight) OR category_id IS NOT COALESCE(?2, category_id)
                                OR unit IS NOT COALESCE(?3, unit) OR due_date IS NOT COALESCE(?4, due_date)
                                OR include_in_report IS NOT COALESCE(?5, include_in_report))",
                            params![su.weight, category, unit, due, su.on_report, x.id],
                        )?;
                        if let Some(c) = category.or(x.category_id) {
                            prev_cat.insert(kind, c);
                        }
                        let scale = if kind == Kind::Scored { x.out_of / sheet_out_of } else { 1.0 };
                        (x.id, kind, sheet_out_of, scale)
                    }
                    None => {
                        let typed: Vec<&str> = col.marks.iter().map(|m| m.0.trim()).filter(|t| !t.is_empty()).collect();
                        let yes_no = |t: &&str| matches!(t.to_lowercase().as_str(), "y" | "n" | "yes" | "no" | "done" | "✓");
                        let kind = col.kind.unwrap_or_else(|| {
                            if typed.iter().any(yes_no) && typed.iter().all(|t| yes_no(t) || matches!(*t, "x" | "X" | "?")) {
                                Kind::Completion
                            } else {
                                Kind::Scored
                            }
                        });
                        let out_of = match kind {
                            Kind::Completion => 1.0,
                            Kind::Level => engine::MAX_LEVEL,
                            Kind::Scored => col.out_of.unwrap_or(100.0),
                        };
                        if kind == Kind::Scored && col.out_of.is_none() {
                            problem(
                                &mut out.notes,
                                &s.sheet,
                                &col.cell,
                                format!(
                                    "{} doesn't say what it's out of, so it's out of 100. Add it to the heading if not, like {} /20.",
                                    col.name, col.name
                                ),
                            );
                        }
                        let mut review = if kind == Kind::Scored && col.out_of.is_none() { 2 } else { 0 };
                        // A new assessment goes in the category of the column before it.
                        let category = match category {
                            Some(c) => Some(c),
                            None if kind == Kind::Completion || cats.is_empty() => None,
                            None => {
                                let (cid, why) = match prev_cat.get(&kind) {
                                    Some(&c) => (c, "like the column of the same type before it"),
                                    None => (cats[0].0, "the first category"),
                                };
                                let cname = cats.iter().find(|c| c.0 == cid).map(|c| c.1.clone()).unwrap_or_default();
                                problem(
                                    &mut out.notes,
                                    &s.sheet,
                                    &col.cell,
                                    format!(
                                        "{} was put in {cname} ({why}). To change it, use Setup - Assessments or edit it in Gradebook.",
                                        col.name
                                    ),
                                );
                                review = review.max(1);
                                Some(cid)
                            }
                        };
                        tx.execute(
                            "INSERT INTO assessments (subject_id, category_id, name, unit, out_of, weight, kind, due_date, include_in_report, sort, needs_review)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                            params![
                                subject_id,
                                category,
                                col.name,
                                su.unit.clone().unwrap_or_default(),
                                out_of,
                                su.weight.unwrap_or(1.0),
                                kind.as_str(),
                                su.due,
                                su.on_report.unwrap_or(true),
                                sort,
                                review
                            ],
                        )?;
                        sort += 1;
                        out.assessments_new += 1;
                        if let Some(c) = category {
                            prev_cat.insert(kind, c);
                        }
                        (tx.last_insert_rowid(), kind, out_of, 1.0)
                    }
                };
                for (i, (input, cell)) in col.marks.iter().enumerate() {
                    let Some(&sid) = ids.get(i) else { continue };
                    if input == "\u{0}" {
                        continue;
                    }
                    let old = saved.get(&sid).and_then(|m| m.get(&aid));
                    match engine::parse_input(kind, input, sheet_out_of) {
                        Ok(CellInput::Clear) => {
                            if clear_blanks && old.is_some() {
                                changes.push(marks::Change { assessment_id: aid, student_id: sid, new: None });
                            }
                        }
                        Ok(CellInput::Set(m)) => {
                            let m = MarkEntry { value: m.value.map(|v| (v * scale * 1000.0).round() / 1000.0), status: m.status };
                            if !old.is_some_and(|o| unchanged(kind, o, &m)) {
                                changes.push(marks::Change { assessment_id: aid, student_id: sid, new: Some(m) });
                            }
                        }
                        Err(e) => problem(&mut out.problems, &s.sheet, cell, format!("\"{input}\": {e}")),
                    }
                }
            }
            out.marks_changed += marks::apply(tx, class_id, &changes, "spreadsheet")?;
        }
    }
    out.classes_updated.sort();
    out.classes_updated.dedup();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(mode: Mode) -> Settings {
        Settings {
            mode,
            active_periods: 3,
            prep_period: Some(2),
            missing_policy: MissingPolicy::Ignore,
            board_name: String::new(),
            report_title: String::new(),
            primary_color: String::new(),
            accent_color: String::new(),
            has_logo: false,
            scale: engine::Scale::ontario_letters(),
            levels: engine::Levels::default(),
            setup_done: true,
        }
    }

    fn sheet_names(bytes: &[u8]) -> Vec<String> {
        calamine::open_workbook_from_rs::<Xlsx<_>, _>(Cursor::new(bytes)).unwrap().sheet_names().to_vec()
    }

    #[test]
    fn headings_say_what_a_column_is() {
        let h = |s: &str| read_heading(s);
        assert_eq!(h("Quiz 1 /20"), ("Quiz 1".into(), Some(Kind::Scored), Some(20.0)));
        assert_eq!(h("Quiz 1/20"), ("Quiz 1".into(), Some(Kind::Scored), Some(20.0)));
        assert_eq!(h("Quiz 1 (/20)"), ("Quiz 1".into(), Some(Kind::Scored), Some(20.0)));
        assert_eq!(h("Unit 3/4 Test / 50"), ("Unit 3/4 Test".into(), Some(Kind::Scored), Some(50.0)));
        assert_eq!(h("Journal 2 (done)"), ("Journal 2".into(), Some(Kind::Completion), None));
        assert_eq!(h("Participation wk 1 (Level)"), ("Participation wk 1".into(), Some(Kind::Level), None));
        assert_eq!(h("Essay"), ("Essay".into(), None, None));
        assert_eq!(h("/20"), ("/20".into(), None, None));
        for kind in [Kind::Scored, Kind::Completion, Kind::Level] {
            let a = AssessmentSpec::new("Lab 2", "", 40.0, 1.0, kind);
            let (name, k, _) = read_heading(&a.heading());
            assert_eq!((name.as_str(), k), ("Lab 2", Some(kind)));
        }
    }

    #[test]
    fn blank_spreadsheets_parse_as_empty() {
        for mode in [Mode::Elementary, Mode::Secondary, Mode::University] {
            let bytes = build(&blank(&settings(mode))).unwrap();
            assert!(is_template(&bytes));
            assert!(!crate::workbook::is_workbook(&bytes));
            // Nothing filled in yet: a friendly error, not an empty import.
            let r = parse(&bytes);
            assert!(r.is_err(), "{mode:?}: {:?}", r.ok());
        }
        let names = sheet_names(&build(&blank(&settings(Mode::Secondary))).unwrap());
        assert_eq!(
            names,
            [
                START_SHEET,
                EXAMPLE_TITLE,
                "Period 1",
                "Period 3",
                "Period 4",
                SETUP_CLASSES,
                SETUP_CATEGORIES,
                SETUP_ASSESSMENTS,
                SETUP_STUDENTS,
                MARKER_SHEET
            ],
            "skips the prep period; setup at the back"
        );
    }

    fn bio() -> SheetSpec {
        let marks = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let mut lab = AssessmentSpec::new("Lab", "Nope", 40.0, 2.0, Kind::Scored);
        lab.unit = "Cells".into();
        lab.due = Some("2026-10-15".into());
        lab.on_report = false;
        SheetSpec {
            title: "Bio".into(),
            class: "Biology".into(),
            course_code: "SBI3U".into(),
            period: Some(1),
            categories: vec![("Knowledge".into(), 60.0), ("Application".into(), 30.0)],
            assessments: vec![
                AssessmentSpec::new("Quiz", "Knowledge", 20.0, 1.0, Kind::Scored),
                lab,
                AssessmentSpec::new("Journal", "", 1.0, 1.0, Kind::Completion),
                AssessmentSpec::new("Participation", "Application", 4.0, 1.0, Kind::Level),
            ],
            students: vec![
                StudentSpec { number: "1".into(), last: "Lee".into(), first: "Ana".into(), pronouns: "she".into() },
                StudentSpec { last: "Khan".into(), first: "Omar".into(), ..Default::default() },
            ],
            marks: vec![marks(&["17", "3/4", "Y", "3+"]), marks(&["abc", "85%L", "N", ""])],
            ..Default::default()
        }
    }

    #[test]
    fn filled_spreadsheet_reads_back_with_its_setup() {
        let t = Template { missing_policy: MissingPolicy::Ignore, sheets: vec![bio()], example: None, saved: None };
        let p = parse(&build(&t).unwrap()).unwrap();
        assert_eq!(p.sheets.len(), 1);
        let s = &p.sheets[0];
        assert_eq!((s.class.as_str(), s.course_code.as_deref(), s.period), ("Biology", Some("SBI3U"), Some(1)));
        assert_eq!(s.categories, Some(vec![("Knowledge".to_string(), 60.0), ("Application".to_string(), 30.0)]));
        assert_eq!(s.students.len(), 2);
        assert_eq!((s.students[0].number.as_str(), s.students[0].pronouns.as_deref()), ("1", Some("she")));
        let names: Vec<_> = s.assessments.iter().map(|a| (a.name.as_str(), a.kind, a.out_of)).collect();
        assert_eq!(
            names,
            [
                ("Quiz", Some(Kind::Scored), Some(20.0)),
                ("Lab", Some(Kind::Scored), Some(40.0)),
                ("Journal", Some(Kind::Completion), None),
                ("Participation", Some(Kind::Level), None)
            ]
        );
        let lab = &s.assessments[1].setup;
        assert_eq!(lab.category.as_deref(), Some("Nope"));
        assert_eq!(
            (lab.weight, lab.unit.as_deref(), lab.due.as_deref(), lab.on_report),
            (Some(2.0), Some("Cells"), Some("2026-10-15"), Some(false))
        );
        assert_eq!(s.assessments[0].marks[1], ("abc".to_string(), "C6".to_string()));
        assert_eq!(s.assessments[3].marks[0].0, "3+");
        assert!(p.problems.iter().any(|x| x.message.contains("add up to 90")), "{:?}", p.problems);
    }

    #[test]
    fn example_explains_itself_and_is_never_imported() {
        for mode in [Mode::Elementary, Mode::Secondary, Mode::University] {
            let t = blank(&settings(mode));
            let ex = t.example.as_ref().unwrap();
            assert!(is_example(&ex.class), "{mode:?}");
            assert_eq!(ex.students.iter().map(|s| s.first.as_str()).collect::<Vec<_>>(), ["John", "Jane"]);
            // Every example mark is valid, and has an explanation.
            for (i, row) in ex.marks.iter().enumerate() {
                for (j, v) in row.iter().enumerate() {
                    let a = &ex.assessments[j];
                    assert!(engine::parse_input(a.kind, v, a.out_of).is_ok(), "{mode:?} {i},{j} {v}");
                    assert!(!explain(v, a).is_empty());
                }
            }
            // Categories used in the example exist in it.
            for a in &ex.assessments {
                assert!(a.category.is_empty() || ex.categories.iter().any(|c| c.0 == a.category), "{mode:?} {}", a.category);
            }
            let bytes = build(&t).unwrap();
            let mut zip = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
            assert!((0..zip.len()).any(|i| zip.by_index(i).unwrap().name().unwrap().contains("comments")), "{mode:?}");
            // Uploading the untouched spreadsheet imports nothing, example included.
            assert!(parse(&bytes).is_err(), "{mode:?}");
        }
        let q = AssessmentSpec::new("Q", "", 20.0, 1.0, Kind::Scored);
        assert_eq!(explain("3/4", &q), "A fraction: 3/4 is converted to 15 out of 20.");
        assert!(explain("41L", &q).starts_with("41 plus L"));

        // Typed over the example but kept John Smith: imported, with a warning.
        let mut t = blank(&settings(Mode::Secondary));
        t.example.as_mut().unwrap().class = "Grade 9 Math".into();
        let p = parse(&build(&t).unwrap()).unwrap();
        assert_eq!(p.sheets.len(), 1);
        assert_eq!(p.problems.iter().filter(|x| x.message.contains("example students")).count(), 2, "{:?}", p.problems);
    }

    #[test]
    fn subject_tabs_share_the_class_and_students() {
        // Elementary: the other subject tabs show the first tab's class and
        // students. Here the values are in the file as if Excel calculated them.
        let mut t = blank(&settings(Mode::Elementary));
        t.example = None;
        t.sheets[0].class = "Grade 4".into();
        t.sheets[0].students = vec![StudentSpec { last: "Lee".into(), first: "Ana".into(), ..Default::default() }];
        let bytes = build(&t).unwrap();
        let p = parse_tabs(&bytes).unwrap();
        assert_eq!(p.sheets.len(), 1, "the linked tabs read blank until Excel recalculates");
        assert_eq!(p.sheets[0].subject, "Language");
    }

    /// The layout before the plain grid, written the way version 1 did.
    fn v1_file() -> Vec<u8> {
        let mut wb = Workbook::new();
        let ws = wb.add_worksheet();
        ws.set_name("Period 1").unwrap();
        for (r, (l, v)) in [("Class", "Chemistry"), ("Subject", ""), ("Course code", "SCH4U"), ("Period", "")].iter().enumerate() {
            ws.write(2 + r as u32, 0, *l).unwrap();
            ws.write(2 + r as u32, 1, *v).unwrap();
        }
        ws.write(2, 5, "Category").unwrap();
        ws.write(2, 6, "Knowledge").unwrap();
        ws.write(3, 5, "Weight %").unwrap();
        ws.write(3, 6, 100).unwrap();
        for (r, l) in ["Assessment", "Category", "Out of", "Weight", "Type"].iter().enumerate() {
            ws.write(7 + r as u32, 3, *l).unwrap();
        }
        for (c, (name, out_of, kind)) in [("Test", "50", "Mark"), ("Log", "", "Done / not done")].iter().enumerate() {
            let c = 4 + c as u16;
            ws.write(7, c, *name).unwrap();
            ws.write(8, c, "Knowledge").unwrap();
            ws.write(9, c, *out_of).unwrap();
            ws.write(10, c, 1).unwrap();
            ws.write(11, c, *kind).unwrap();
        }
        for (c, h) in ["Student #", "Last name", "First name", "Pronouns"].iter().enumerate() {
            ws.write(12, c as u16, *h).unwrap();
        }
        for (c, v) in ["7", "Lee", "Ana", "she", "45", "Y"].iter().enumerate() {
            ws.write(13, c as u16, *v).unwrap();
        }
        let m = wb.add_worksheet();
        m.set_name(MARKER_SHEET).unwrap();
        m.write(0, 1, FORMAT_ID).unwrap();
        m.write(1, 1, 1).unwrap();
        m.set_hidden(true);
        wb.save_to_buffer().unwrap()
    }

    #[test]
    fn version_1_starter_files_still_read() {
        let p = parse(&v1_file()).unwrap();
        assert_eq!(p.sheets.len(), 1, "{:?}", p.problems);
        let s = &p.sheets[0];
        assert_eq!((s.class.as_str(), s.course_code.as_deref()), ("Chemistry", Some("SCH4U")));
        assert_eq!(s.categories, Some(vec![("Knowledge".to_string(), 100.0)]));
        assert_eq!(s.students[0].number, "7");
        assert_eq!((s.assessments[0].kind, s.assessments[0].out_of), (Some(Kind::Scored), Some(50.0)));
        assert_eq!(s.assessments[1].kind, Some(Kind::Completion));
        assert_eq!(s.assessments[0].marks[0].0, "45");
    }

    #[test]
    fn class_links_are_followed_without_saved_results() {
        assert_eq!(linked_tab(&link("P1 Math", "$B$1")[1..]).as_deref(), Some("P1 Math"));
        assert_eq!(linked_tab(&link("Ms. O'Neil's", "$B$1")[1..]).as_deref(), Some("Ms. O'Neil's"));
        assert_eq!(linked_tab("IF(Math!$B$1=\"\",\"\",Math!$B$1)").as_deref(), Some("Math"));
        assert_eq!(linked_tab("SUM(A1:A3)"), None);
    }

    #[test]
    fn unchanged_tolerates_what_the_sheet_shows() {
        let m = |v: f64| MarkEntry::marked(v);
        assert!(unchanged(Kind::Scored, &m(17.333), &m(17.33)));
        assert!(!unchanged(Kind::Scored, &m(17.0), &m(18.0)));
        let late = MarkEntry { value: Some(1.0), status: Status::Late };
        assert!(unchanged(Kind::Completion, &late, &m(1.0)));
        assert!(!unchanged(Kind::Scored, &late, &m(1.0)));
    }
}
