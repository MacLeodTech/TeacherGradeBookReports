//! The starter spreadsheet: a fill-in `.xlsx` a teacher downloads, fills in
//! Excel (or Google Sheets, LibreOffice…) and uploads to create their
//! classes, students, assessments and marks in one go.
//!
//! - **How to fill this in**: the steps, and a legend of what to type in a
//!   mark cell (the same codes as the grid).
//! - One sheet per class (or per subject of a class): class details, a small
//!   table of categories and weights, then one column per assessment and one
//!   row per student. Mark cells are text, so Excel can't turn `3/4` into a
//!   date, and are coloured by status (missing, excused, not marked, late).
//! - A hidden marker sheet so uploads are recognised.
//!
//! Uploading is safe to repeat: classes, subjects, categories, assessments and
//! students are matched by name (students by number first), marks that differ
//! are updated (with undo), blank cells leave marks alone, nothing is deleted.
//! The same layout, filled in, is offered as a download of the current term.
//!
//! The parser finds the parts of a sheet by their labels, so inserted rows,
//! renamed or copied sheets still work.

use std::{collections::HashMap, io::Cursor};

use calamine::{Data, Range, Reader, Xlsx};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use rust_xlsxwriter::{
    Color, ConditionalFormatFormula, DataValidation, DataValidationRule, Format, FormatAlign, FormatBorder, Formula, Workbook, Worksheet,
    utility::row_col_to_cell,
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
const VERSION: i64 = 1;
const MARKER_SHEET: &str = "_gradebook_template";
const HELP_SHEET: &str = "How to fill this in";

// Labels the parser looks for.
const L_CLASS: &str = "Class";
const L_SUBJECT: &str = "Subject";
const L_CODE: &str = "Course code";
const L_PERIOD: &str = "Period";
const L_CATEGORY: &str = "Category";
const L_WEIGHT_PCT: &str = "Weight %";
const L_ASSESSMENT: &str = "Assessment";
const L_OUT_OF: &str = "Out of";
const L_WEIGHT: &str = "Weight";
const L_TYPE: &str = "Type";
const H_NUMBER: &str = "Student #";
const H_LAST: &str = "Last name";
const H_FIRST: &str = "First name";
const H_PRONOUNS: &str = "Pronouns";
const TYPE_MARK: &str = "Mark";
const TYPE_DONE: &str = "Done / not done";

// Layout (0-based rows and columns).
const INFO_ROW: u32 = 2; // Class, Subject, Course code, Period on rows 3–6
const CAT_LABEL_COL: u16 = 5; // F
const CAT_FIRST_COL: u16 = 6; // G…
const CAT_SLOTS: u16 = 10;
const A_LABEL_COL: u16 = 3; // D
const A_FIRST_COL: u16 = 4; // E…
const A_NAME_ROW: u32 = 7; // Assessment, Category, Out of, Weight, Type on rows 8–12
const HEADER_ROW: u32 = 12;
const FIRST_STUDENT_ROW: u32 = HEADER_ROW + 1;
const BLANK_ASSESSMENTS: u16 = 30;
const BLANK_STUDENTS: u32 = 40;

const MAX_SHEETS: usize = 60;
const MAX_STUDENTS: u32 = 500;
const MAX_ASSESSMENTS: u16 = 300;

const MARK_HINT: &str = "17 = points out of the \"Out of\" · 17/20 · 85%\n\
    Add L if late: 17L\n\
    M = missing · X = excused · ? = handed in, not marked yet\n\
    Done / not done columns: Y or N\n\
    Blank = not assessed yet";

// ── The spreadsheet's content ────────────────────────────────────────────────

pub struct Template {
    pub missing_policy: MissingPolicy,
    pub sheets: Vec<SheetSpec>,
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
    /// `marks[student][assessment]`, as typed (`17`, `M`, `17L`…).
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
    pub done: bool,
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

/// An empty template shaped for the teacher's level.
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
    let mut sheets = sheets;
    sheets.insert(0, example(settings.mode));
    Template { missing_policy: settings.missing_policy, sheets }
}

/// A filled-in class showing every kind of entry. Its class name ends in
/// "(example)", so uploading never imports it.
fn example(mode: Mode) -> SheetSpec {
    let (class, code, cats, k, t, c, a) = match mode {
        Mode::Secondary => (
            "Grade 9 Math (example)",
            "MTH1W",
            preset("ontario-secondary"),
            "Knowledge & Understanding",
            "Thinking & Investigation",
            "Communication",
            "Application",
        ),
        Mode::University => ("MATH 101 (example)", "MATH 101", preset("university"), "Assignments", "Assignments", "", "Midterm"),
        Mode::Elementary => ("Grade 4 (example)", "", Vec::new(), "", "", "", ""),
    };
    let a_ = |name: &str, category: &str, out_of: f64, weight: f64, done: bool| AssessmentSpec {
        name: name.into(),
        category: category.into(),
        out_of,
        weight,
        done,
    };
    let row = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    SheetSpec {
        title: "Example - Math".into(),
        class: class.into(),
        subject: if mode == Mode::Elementary { "Mathematics".into() } else { String::new() },
        course_code: code.into(),
        categories: cats,
        assessments: vec![
            a_("Unit 1 Quiz", k, 20.0, 1.0, false),
            a_("Unit 1 Test", a, 50.0, 2.0, false),
            a_("Problem Set 1", t, 10.0, 1.0, false),
            a_("Math Journal", c, 1.0, 1.0, true),
            a_("Warm-up (practice)", k, 10.0, 0.0, false),
            a_(
                "Final Exam",
                if mode == Mode::Secondary {
                    "Final / Culminating"
                } else if mode == Mode::University {
                    "Final Exam"
                } else {
                    ""
                },
                100.0,
                1.0,
                false,
            ),
        ],
        students: vec![
            StudentSpec { number: "100001".into(), last: "Smith".into(), first: "John".into(), pronouns: "he".into() },
            StudentSpec { number: "100002".into(), last: "Doe".into(), first: "Jane".into(), pronouns: "she".into() },
        ],
        // 17 points · 85% · missing · done · practice mark · not assessed yet
        // 3/4 (fraction) · late · excused · not done · handed in, not marked
        marks: vec![row(&["17", "85%", "M", "Y", "8", ""]), row(&["3/4", "41L", "X", "N", "?", ""])],
        link_to: None,
        period: None,
    }
}

pub fn is_example(class: &str) -> bool {
    class.trim().to_lowercase().ends_with("(example)")
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
                        .map(|a| match marks.get(&st.id).and_then(|m| m.get(&a.id)) {
                            None => String::new(),
                            Some(m) if Kind::parse(&a.kind) == Kind::Completion => match m.status {
                                Status::Missing => "N".into(),
                                Status::Marked | Status::Late => "Y".into(),
                                s => s.code().into(),
                            },
                            Some(m) => engine::display_cell(m),
                        })
                        .collect()
                })
                .collect();
            sheets.push(SheetSpec {
                title: if single { class.name.clone() } else { format!("{} - {}", class.name, s.name) },
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
                        done: Kind::parse(&a.kind) == Kind::Completion,
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
        return Ok(blank(settings));
    }
    Ok(Template { missing_policy: settings.missing_policy, sheets })
}

// ── Writing ──────────────────────────────────────────────────────────────────

struct Styles {
    title: Format,
    note: Format,
    label: Format,
    label_right: Format,
    input: Format,
    input_text: Format,
    input_num: Format,
    linked: Format,
    header: Format,
    total: Format,
    missing: Format,
    excused: Format,
    pending: Format,
    late: Format,
    wrap: Format,
    bold_wrap: Format,
}

const PRIMARY: u32 = 0x003F5A;
const INPUT_BG: u32 = 0xEEF5FB;
const INPUT_BORDER: u32 = 0xC9D7E3;
const MISSING_BG: u32 = 0xF8D7DA;
const EXCUSED_BG: u32 = 0xE2E3E5;
const PENDING_BG: u32 = 0xFFF3CD;
const LATE_BG: u32 = 0xFFE0B2;

fn styles() -> Styles {
    let input =
        Format::new().set_background_color(Color::RGB(INPUT_BG)).set_border(FormatBorder::Thin).set_border_color(Color::RGB(INPUT_BORDER));
    Styles {
        title: Format::new().set_bold().set_font_size(15).set_font_color(Color::RGB(PRIMARY)),
        note: Format::new().set_italic().set_font_color(Color::RGB(0x5F6B73)),
        label: Format::new().set_bold(),
        label_right: Format::new().set_bold().set_align(FormatAlign::Right),
        input_text: input.clone().set_num_format("@"),
        input_num: input.clone().set_num_format("0.##"),
        linked: Format::new()
            .set_background_color(Color::RGB(0xF4F4F4))
            .set_font_color(Color::RGB(0x5F6B73))
            .set_border(FormatBorder::Thin)
            .set_border_color(Color::RGB(0xDDDDDD)),
        input,
        header: Format::new()
            .set_bold()
            .set_font_color(Color::White)
            .set_background_color(Color::RGB(PRIMARY))
            .set_border(FormatBorder::Thin)
            .set_border_color(Color::RGB(PRIMARY)),
        total: Format::new().set_italic().set_num_format("0.##").set_font_color(Color::RGB(0x5F6B73)),
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

pub fn build(t: &Template) -> AppResult<Vec<u8>> {
    let st = styles();
    let mut wb = Workbook::new();
    let mut used = vec![HELP_SHEET.to_string()];
    help_sheet(wb.add_worksheet(), &st, t.missing_policy)?;
    // Sheet names are fixed up for Excel first, so links can point at them.
    let titles: Vec<String> = t.sheets.iter().map(|s| sheet_name(&s.title, &mut used)).collect();
    for (spec, title) in t.sheets.iter().zip(&titles) {
        let link = spec.link_to.as_ref().and_then(|l| t.sheets.iter().position(|s| &s.title == l)).map(|i| titles[i].as_str());
        class_sheet(wb.add_worksheet(), &st, spec, title, link)?;
    }
    let m = wb.add_worksheet();
    m.set_name(MARKER_SHEET).map_err(err)?;
    m.write_string(0, 0, "format").map_err(err)?;
    m.write_string(0, 1, FORMAT_ID).map_err(err)?;
    m.write_string(1, 0, "version").map_err(err)?;
    m.write_number(1, 1, VERSION as f64).map_err(err)?;
    m.set_hidden(true);
    wb.save_to_buffer().map_err(err)
}

fn help_sheet(ws: &mut Worksheet, st: &Styles, policy: MissingPolicy) -> AppResult<()> {
    ws.set_name(HELP_SHEET).map_err(err)?;
    ws.set_active(true);
    ws.set_column_width(0, 14).map_err(err)?;
    ws.set_column_width(1, 62).map_err(err)?;
    ws.set_column_width(2, 40).map_err(err)?;
    ws.write_string_with_format(0, 0, "Gradebook starter spreadsheet", &st.title).map_err(err)?;
    ws.write_string_with_format(1, 0, "Fill in the shaded cells, save, then upload this file on Gradebook's Import page.", &st.note)
        .map_err(err)?;

    let steps = [
        (
            "Example",
            "The \"Example - Math\" sheet is filled in to show how everything works: John Smith and Jane Doe, with one of each kind of mark. It's never imported (its class name ends in \"(example)\"), so keep it as a guide or delete it.",
        ),
        (
            "1. Your classes",
            "Each class has its own sheet (the tabs at the bottom). Type the class name at the top; the course code and period are optional. Need more classes? Right-click a tab → Move or Copy → Create a copy, then change the class name. Delete sheets you don't need, or leave the class name blank.",
        ),
        (
            "2. Categories",
            "Optional. Categories (like Knowledge, Thinking, Communication, Application) and their weights go in the small table at the top. The weights should add up to 100. Leave the table empty to weight every assessment equally.",
        ),
        (
            "3. Assessments",
            "One per column, from column E: the name, the category (pick from the list), what it's out of (blank = 100), its weight (blank = 1; 2 = counts double; 0 = practice work that shows but doesn't count) and the type.",
        ),
        (
            "4. Students",
            "One per row: last name and first name, plus optionally the student number and pronouns (she, he or they, used in report comments).",
        ),
        (
            "5. Marks",
            "Type marks in the shaded grid using the codes below. The cells turn colour for missing, excused, not-yet-marked and late work.",
        ),
        (
            "6. Upload",
            "On the Import page, choose this file. You'll see exactly what will be created or changed, and any cells that need fixing, before anything is saved.",
        ),
        (
            "Keep using it",
            "You can keep marking in this spreadsheet and upload it again any time. Changed marks are updated (and can be undone), blank cells leave existing marks alone, and nothing is ever deleted.",
        ),
        (
            "Several subjects",
            "For a class with several subjects (elementary), use one sheet per subject with the same class name. On this template, the class name and student list typed on the first subject's sheet fill in on the others.",
        ),
    ];
    let mut r = 3;
    for (head, text) in steps {
        ws.write_string_with_format(r, 0, head, &st.bold_wrap).map_err(err)?;
        ws.merge_range(r, 1, r, 2, text, &st.wrap).map_err(err)?;
        ws.set_row_height(
            r,
            if text.len() > 200 {
                48
            } else if text.len() > 110 {
                33
            } else {
                18
            },
        )
        .map_err(err)?;
        r += 1;
    }

    r += 1;
    ws.write_string_with_format(r, 0, "What to type in a mark cell", &st.title).map_err(err)?;
    r += 1;
    for (c, h) in ["You type", "It means", "Does it count toward the mark?"].iter().enumerate() {
        ws.write_string_with_format(r, c as u16, *h, &st.header).map_err(err)?;
    }
    r += 1;
    let missing_counts = match policy {
        MissingPolicy::Ignore => {
            "No. It's left out of the average until it's handed in (your current setting; you can make missing work count as 0 in Settings)."
        }
        MissingPolicy::Zero => "Yes, as 0 (your current setting; you can have missing work left out of the average in Settings).",
    };
    let rows: [(&str, &str, &str, Option<&Format>); 10] = [
        ("17", "17 points, out of the assessment's \"Out of\" (e.g. 17 out of 20). Decimals like 17.5 are fine.", "Yes", None),
        ("17/20", "A fraction. It's converted to the assessment's \"Out of\", so 3/4 on a test out of 40 is 30.", "Yes", None),
        ("85%", "A percentage of the \"Out of\".", "Yes", None),
        ("17L  85%L", "Any mark followed by L: handed in late. Shown as late.", "Yes, like any other mark", Some(&st.late)),
        ("M", "Missing: not handed in. Shown as missing on the grid and the report.", missing_counts, Some(&st.missing)),
        ("X", "Excused or exempt: this student doesn't need to do it.", "No, never", Some(&st.excused)),
        ("?", "Handed in, marking in progress.", "Not until you replace it with the mark", Some(&st.pending)),
        ("(blank)", "Not assessed yet (or not due yet).", "No", None),
        (
            "Y  /  N",
            "For \"Done / not done\" columns (journals, homework checks): done, or not done. X and ? work too.",
            "No, these never change the mark (they show on the report)",
            None,
        ),
        ("Weight 0", "Set an assessment's weight to 0 for practice or formative work.", "No, but the marks are recorded and shown", None),
    ];
    for (code, meaning, counts, fill) in rows {
        let code_fmt = fill.cloned().unwrap_or_default().set_bold().set_align(FormatAlign::Center).set_align(FormatAlign::Top);
        ws.write_string_with_format(r, 0, code, &code_fmt).map_err(err)?;
        ws.write_string_with_format(r, 1, meaning, &st.wrap).map_err(err)?;
        ws.write_string_with_format(r, 2, counts, &st.wrap).map_err(err)?;
        ws.set_row_height(r, if meaning.len().max(counts.len()) > 60 { 33 } else { 18 }).map_err(err)?;
        r += 1;
    }
    r += 1;
    ws.write_string_with_format(r, 0, "Tip", &st.bold_wrap).map_err(err)?;
    ws.merge_range(
        r,
        1,
        r,
        2,
        "The mark cells are formatted as text so Excel won't turn 3/4 into a date. If you paste marks in from another file and some show as dates, retype them. If you think in percentages, type 75% rather than 75 when the \"Out of\" isn't 100.",
        &st.wrap,
    )
    .map_err(err)?;
    ws.set_row_height(r, 48).map_err(err)?;
    Ok(())
}

fn class_sheet(ws: &mut Worksheet, st: &Styles, s: &SheetSpec, title: &str, link: Option<&str>) -> AppResult<()> {
    ws.set_name(title).map_err(err)?;
    // Linked cells read as blank (not 0) until the spreadsheet recalculates.
    ws.set_formula_result_default("");
    let n_assess = (s.assessments.len() as u16).max(BLANK_ASSESSMENTS);
    let n_students = (s.students.len() as u32).max(BLANK_STUDENTS);
    let last_col = A_FIRST_COL + n_assess - 1;
    let last_row = FIRST_STUDENT_ROW + n_students - 1;

    ws.set_column_width(0, 12).map_err(err)?;
    ws.set_column_width(1, 18).map_err(err)?;
    ws.set_column_width(2, 18).map_err(err)?;
    ws.set_column_width(3, 12).map_err(err)?;
    for c in A_FIRST_COL..=last_col.max(CAT_FIRST_COL + CAT_SLOTS) {
        ws.set_column_width(c, 13).map_err(err)?;
    }
    ws.write_string_with_format(0, 0, "Class sheet: fill in the shaded cells", &st.title).map_err(err)?;
    ws.write_string_with_format(1, 0, "Mark codes and help are on the first sheet, \"How to fill this in\".", &st.note).map_err(err)?;

    // Class details.
    let info: [(&str, String); 4] = [
        (L_CLASS, s.class.clone()),
        (L_SUBJECT, s.subject.clone()),
        (L_CODE, s.course_code.clone()),
        (L_PERIOD, s.period.map(|p| p.to_string()).unwrap_or_default()),
    ];
    for (i, (label, value)) in info.iter().enumerate() {
        let r = INFO_ROW + i as u32;
        ws.write_string_with_format(r, 0, *label, &st.label).map_err(err)?;
        match (link, *label) {
            (Some(l), L_CLASS) => {
                let src = format!("{}!B{}", quoted(l), r + 1);
                ws.merge_range(r, 1, r, 2, "", &st.linked).map_err(err)?;
                ws.write_formula_with_format(r, 1, Formula::new(format!("=IF({src}=\"\",\"\",{src})")).set_result(""), &st.linked)
                    .map_err(err)?;
            }
            (_, L_PERIOD) if !value.is_empty() => {
                ws.merge_range(r, 1, r, 2, "", &st.input).map_err(err)?;
                ws.write_number_with_format(r, 1, value.parse::<f64>().unwrap_or_default(), &st.input).map_err(err)?;
            }
            _ => {
                ws.merge_range(r, 1, r, 2, value, &st.input_text).map_err(err)?;
            }
        }
    }
    ws.add_data_validation(INFO_ROW + 3, 1, INFO_ROW + 3, 2, &DataValidation::new().allow_whole_number(DataValidationRule::Between(1, 9)))
        .map_err(err)?;
    ws.write_string_with_format(INFO_ROW + 1, 3, "(only if the class has several)", &st.note).map_err(err)?;

    // Categories and weights.
    ws.write_string_with_format(INFO_ROW, CAT_LABEL_COL, L_CATEGORY, &st.label_right).map_err(err)?;
    ws.write_string_with_format(INFO_ROW + 1, CAT_LABEL_COL, L_WEIGHT_PCT, &st.label_right).map_err(err)?;
    ws.write_string_with_format(INFO_ROW + 2, CAT_LABEL_COL, "Total", &st.label_right).map_err(err)?;
    let slots = CAT_SLOTS.max(s.categories.len() as u16);
    for i in 0..slots {
        let c = CAT_FIRST_COL + i;
        match s.categories.get(i as usize) {
            Some((name, w)) => {
                ws.write_string_with_format(INFO_ROW, c, name, &st.input_text).map_err(err)?;
                ws.write_number_with_format(INFO_ROW + 1, c, *w, &st.input_num).map_err(err)?;
            }
            None => {
                ws.write_blank(INFO_ROW, c, &st.input_text).map_err(err)?;
                ws.write_blank(INFO_ROW + 1, c, &st.input_num).map_err(err)?;
            }
        }
    }
    let last_cat = CAT_FIRST_COL + slots - 1;
    let weights = format!("{}:{}", row_col_to_cell(INFO_ROW + 1, CAT_FIRST_COL), row_col_to_cell(INFO_ROW + 1, last_cat));
    let total: f64 = s.categories.iter().map(|c| c.1).sum();
    ws.write_formula_with_format(
        INFO_ROW + 2,
        CAT_FIRST_COL,
        Formula::new(format!("=SUM({weights})")).set_result(fmt_num(total)),
        &st.total,
    )
    .map_err(err)?;
    ws.add_data_validation(
        INFO_ROW + 1,
        CAT_FIRST_COL,
        INFO_ROW + 1,
        last_cat,
        &DataValidation::new().allow_decimal_number(DataValidationRule::Between(0.0, 100.0)),
    )
    .map_err(err)?;
    // Names longer than a column are fine; wrap the header a little.
    ws.set_row_height(INFO_ROW, 30).map_err(err)?;
    ws.write_string_with_format(A_NAME_ROW - 1, A_FIRST_COL, "Assessments: one per column →", &st.note).map_err(err)?;

    // Assessment settings.
    let labels = [L_ASSESSMENT, L_CATEGORY, L_OUT_OF, L_WEIGHT, L_TYPE];
    for (i, l) in labels.iter().enumerate() {
        ws.write_string_with_format(A_NAME_ROW + i as u32, A_LABEL_COL, *l, &st.label_right).map_err(err)?;
    }
    for i in 0..n_assess {
        let c = A_FIRST_COL + i;
        match s.assessments.get(i as usize) {
            Some(a) => {
                ws.write_string_with_format(A_NAME_ROW, c, &a.name, &st.input_text).map_err(err)?;
                ws.write_string_with_format(A_NAME_ROW + 1, c, &a.category, &st.input_text).map_err(err)?;
                if a.done {
                    ws.write_blank(A_NAME_ROW + 2, c, &st.input_num).map_err(err)?;
                } else {
                    ws.write_number_with_format(A_NAME_ROW + 2, c, a.out_of, &st.input_num).map_err(err)?;
                }
                ws.write_number_with_format(A_NAME_ROW + 3, c, a.weight, &st.input_num).map_err(err)?;
                ws.write_string_with_format(A_NAME_ROW + 4, c, if a.done { TYPE_DONE } else { TYPE_MARK }, &st.input).map_err(err)?;
            }
            None => {
                ws.write_blank(A_NAME_ROW, c, &st.input_text).map_err(err)?;
                ws.write_blank(A_NAME_ROW + 1, c, &st.input_text).map_err(err)?;
                ws.write_blank(A_NAME_ROW + 2, c, &st.input_num).map_err(err)?;
                ws.write_blank(A_NAME_ROW + 3, c, &st.input_num).map_err(err)?;
                ws.write_blank(A_NAME_ROW + 4, c, &st.input).map_err(err)?;
            }
        }
    }
    ws.set_row_height(A_NAME_ROW, 30).map_err(err)?;
    let cat_range = format!("={}:{}", row_col_to_cell_abs(INFO_ROW, CAT_FIRST_COL), row_col_to_cell_abs(INFO_ROW, last_cat));
    ws.add_data_validation(
        A_NAME_ROW + 1,
        A_FIRST_COL,
        A_NAME_ROW + 1,
        last_col,
        &DataValidation::new().allow_list_formula(Formula::new(cat_range)).ignore_blank(true),
    )
    .map_err(err)?;
    ws.add_data_validation(
        A_NAME_ROW + 2,
        A_FIRST_COL,
        A_NAME_ROW + 2,
        last_col,
        &DataValidation::new().allow_decimal_number(DataValidationRule::Between(0.01, 10000.0)),
    )
    .map_err(err)?;
    ws.add_data_validation(
        A_NAME_ROW + 3,
        A_FIRST_COL,
        A_NAME_ROW + 3,
        last_col,
        &DataValidation::new().allow_decimal_number(DataValidationRule::Between(0.0, 100.0)),
    )
    .map_err(err)?;
    ws.add_data_validation(
        A_NAME_ROW + 4,
        A_FIRST_COL,
        A_NAME_ROW + 4,
        last_col,
        &DataValidation::new().allow_list_strings(&[TYPE_MARK, TYPE_DONE]).map_err(err)?,
    )
    .map_err(err)?;

    // Students and marks.
    for (c, h) in [H_NUMBER, H_LAST, H_FIRST, H_PRONOUNS].iter().enumerate() {
        ws.write_string_with_format(HEADER_ROW, c as u16, *h, &st.header).map_err(err)?;
    }
    for i in 0..n_assess {
        let c = A_FIRST_COL + i;
        let src = row_col_to_cell(A_NAME_ROW, c);
        let name = s.assessments.get(i as usize).map(|a| a.name.clone()).unwrap_or_default();
        ws.write_formula_with_format(HEADER_ROW, c, Formula::new(format!("=IF({src}=\"\",\"\",{src})")).set_result(name), &st.header)
            .map_err(err)?;
    }
    for i in 0..n_students {
        let r = FIRST_STUDENT_ROW + i;
        let student = s.students.get(i as usize);
        for c in 0..4u16 {
            match link {
                Some(l) => {
                    let src = format!("{}!{}", quoted(l), row_col_to_cell(r, c));
                    ws.write_formula_with_format(r, c, Formula::new(format!("=IF({src}=\"\",\"\",{src})")).set_result(""), &st.linked)
                        .map_err(err)?;
                }
                None => {
                    let v = student.map(|p| match c {
                        0 => p.number.as_str(),
                        1 => p.last.as_str(),
                        2 => p.first.as_str(),
                        _ => p.pronouns.as_str(),
                    });
                    match v.filter(|v| !v.is_empty()) {
                        Some(v) => ws.write_string_with_format(r, c, v, &st.input_text).map_err(err)?,
                        None => ws.write_blank(r, c, &st.input_text).map_err(err)?,
                    };
                }
            }
        }
        for j in 0..n_assess {
            let c = A_FIRST_COL + j;
            match s.marks.get(i as usize).and_then(|row| row.get(j as usize)).filter(|v| !v.is_empty()) {
                Some(v) => ws.write_string_with_format(r, c, v, &st.input_text).map_err(err)?,
                None => ws.write_blank(r, c, &st.input_text).map_err(err)?,
            };
        }
    }
    if link.is_none() {
        ws.add_data_validation(
            FIRST_STUDENT_ROW,
            3,
            last_row,
            3,
            &DataValidation::new().allow_list_strings(&["she", "he", "they"]).map_err(err)?,
        )
        .map_err(err)?;
    }
    ws.add_data_validation(
        FIRST_STUDENT_ROW,
        A_FIRST_COL,
        last_row,
        last_col,
        &DataValidation::new().allow_any_value().set_input_title("What to type").map_err(err)?.set_input_message(MARK_HINT).map_err(err)?,
    )
    .map_err(err)?;
    // Colour the codes, like the grid does.
    let tl = row_col_to_cell(FIRST_STUDENT_ROW, A_FIRST_COL);
    let rules = [
        (format!("=OR(UPPER(TRIM({tl}))=\"M\",UPPER(TRIM({tl}))=\"N\")"), &st.missing),
        (format!("=UPPER(TRIM({tl}))=\"X\""), &st.excused),
        (format!("=TRIM({tl})=\"?\""), &st.pending),
        (format!("=AND(LEN(TRIM({tl}))>1,UPPER(RIGHT(TRIM({tl}),1))=\"L\")"), &st.late),
    ];
    for (rule, fmt) in rules {
        let cf = ConditionalFormatFormula::new().set_rule(rule.as_str()).set_format(fmt);
        ws.add_conditional_format(FIRST_STUDENT_ROW, A_FIRST_COL, last_row, last_col, &cf).map_err(err)?;
    }
    ws.set_freeze_panes(FIRST_STUDENT_ROW, A_FIRST_COL).map_err(err)?;
    Ok(())
}

fn row_col_to_cell_abs(row: u32, col: u16) -> String {
    rust_xlsxwriter::utility::row_col_to_cell_absolute(row, col)
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Parsed {
    pub sheets: Vec<SheetIn>,
    pub problems: Vec<Problem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SheetIn {
    pub sheet: String,
    pub class: String,
    pub subject: String,
    pub course_code: String,
    pub period: Option<i64>,
    pub categories: Vec<(String, f64)>,
    pub assessments: Vec<AssessmentIn>,
    pub students: Vec<StudentIn>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssessmentIn {
    pub name: String,
    pub cell: String,
    pub category: String,
    pub out_of: f64,
    pub weight: f64,
    pub done: bool,
    /// (index into `students`, mark).
    pub marks: Vec<(usize, MarkEntry)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StudentIn {
    pub number: String,
    pub last: String,
    pub first: String,
    pub pronouns: String,
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

fn clip(s: String, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Find a label: the first cell in the given rows/columns whose text matches.
fn find(r: &Range<Data>, label: &str, rows: std::ops::Range<u32>, cols: std::ops::Range<u32>) -> Option<(u32, u32)> {
    rows.flat_map(|row| cols.clone().map(move |col| (row, col)))
        .find(|&(row, col)| text(r.get_value((row, col))).eq_ignore_ascii_case(label))
}

pub fn parse(bytes: &[u8]) -> AppResult<Parsed> {
    let mut x =
        calamine::open_workbook_from_rs::<Xlsx<_>, _>(Cursor::new(bytes)).map_err(|_| bad("That file isn't an Excel workbook (.xlsx)."))?;
    let names = x.sheet_names().to_vec();
    let mut out = Parsed { sheets: Vec::new(), problems: Vec::new() };
    for name in names.into_iter().filter(|n| n != MARKER_SHEET && n != HELP_SHEET).take(MAX_SHEETS) {
        let Ok(range) = x.worksheet_range(&name) else { continue };
        if let Some(sheet) = parse_sheet(&name, &range, &mut out.problems) {
            out.sheets.push(sheet);
        }
    }
    if out.sheets.is_empty() && out.problems.is_empty() {
        return Err(bad(
            "The spreadsheet doesn't have any classes filled in yet. Type a class name at the top of a class sheet, add students and marks, then upload it again.",
        ));
    }
    Ok(out)
}

fn parse_sheet(name: &str, r: &Range<Data>, problems: &mut Vec<Problem>) -> Option<SheetIn> {
    let end = r.end().unwrap_or((0, 0));
    let (max_row, max_col) = (end.0 + 1, end.1 + 1);
    // Not a class sheet (e.g. the teacher's own notes): skip quietly.
    let (class_row, class_col) = find(r, L_CLASS, 0..20, 0..3)?;
    let (a_row, a_col) = find(r, L_ASSESSMENT, 0..40, 0..8)?;
    let (h_row, _) = find(r, H_NUMBER, a_row..a_row + 20, 0..1).or_else(|| find(r, H_LAST, a_row..a_row + 20, 0..8))?;
    let mut problem = |cell: String, message: String| problems.push(Problem { sheet: name.to_string(), cell, message });
    let value = |label: &str| {
        find(r, label, class_row..class_row + 8, class_col..class_col + 1).map(|(row, col)| (row, col + 1, r.get_value((row, col + 1))))
    };

    let class = value(L_CLASS).map(|v| clip(text(v.2), 120)).unwrap_or_default();
    // The example sheet is a guide, never imported.
    if is_example(&class) {
        return None;
    }
    let subject = value(L_SUBJECT).map(|v| clip(text(v.2), 120)).unwrap_or_default();
    let course_code = value(L_CODE).map(|v| clip(text(v.2), 40)).unwrap_or_default();
    let period = match value(L_PERIOD) {
        Some((row, col, v)) if !text(v).is_empty() => match number(v) {
            Some(p) if p.fract() == 0.0 && (1.0..=9.0).contains(&p) => Some(p as i64),
            _ => {
                problem(row_col_to_cell(row, col as u16), format!("\"{}\" isn't a period number; it was left out.", text(v)));
                None
            }
        },
        _ => None,
    };

    // Categories: names on the row above "Weight %", weights beside the label.
    let mut categories: Vec<(String, f64)> = Vec::new();
    if let Some((w_row, w_col)) = find(r, L_WEIGHT_PCT, 0..a_row, 0..max_col.min(40)) {
        for col in w_col + 1..max_col.min(w_col + 1 + 50) {
            let n = clip(text(r.get_value((w_row - 1, col))), 80);
            let w = r.get_value((w_row, col));
            if n.is_empty() {
                if !text(w).is_empty() {
                    problem(row_col_to_cell(w_row, col as u16), "This weight has no category name above it; it was left out.".into());
                }
                continue;
            }
            match number(w) {
                Some(v) if (0.0..=100.0).contains(&v) => categories.push((n, v)),
                None if text(w).is_empty() => categories.push((n, 0.0)),
                _ => problem(row_col_to_cell(w_row, col as u16), format!("The weight for {n} should be a number from 0 to 100.")),
            }
        }
        if let Some((n, _)) = categories.iter().find(|c| c.1 == 0.0) {
            problem(row_col_to_cell(w_row, w_col as u16 + 1), format!("{n} has no weight, so assessments in it won't count."));
        }
        let total: f64 = categories.iter().map(|c| c.1).sum();
        if !categories.is_empty() && (total - 100.0).abs() > 0.01 {
            problem(
                row_col_to_cell(w_row, w_col as u16 + 1),
                format!("The category weights add up to {}, not 100. They're used as given (relative to each other).", fmt_num(total)),
            );
        }
    }

    // Student columns, by their headers.
    let col_of = |h: &str| find(r, h, h_row..h_row + 1, 0..max_col.min(20)).map(|p| p.1);
    let (c_num, c_last, c_first, c_pron) = (col_of(H_NUMBER), col_of(H_LAST), col_of(H_FIRST), col_of(H_PRONOUNS));
    let mut students = Vec::new();
    let mut student_rows = Vec::new();
    let get = |row: u32, col: Option<u32>| col.map(|c| clip(text(r.get_value((row, c))), 80)).unwrap_or_default();
    let first_mark_col = a_col + 1;
    for row in h_row + 1..max_row.min(h_row + 1 + MAX_STUDENTS) {
        let (number, last, first) = (get(row, c_num), get(row, c_last), get(row, c_first));
        let pronouns = get(row, c_pron).to_lowercase();
        let has_marks = (first_mark_col..max_col).any(|c| !text(r.get_value((row, c))).is_empty() || is_date(r.get_value((row, c))));
        if first.is_empty() && last.is_empty() {
            if !number.is_empty() || has_marks {
                problem(
                    row_col_to_cell(row, c_last.unwrap_or(1) as u16),
                    "This row has a student number or marks but no name; it was left out.".into(),
                );
            }
            continue;
        }
        let pronouns = match pronouns.as_str() {
            "" | "they" => "they",
            "she" | "her" | "f" => "she",
            "he" | "him" | "m" => "he",
            other => {
                problem(
                    row_col_to_cell(row, c_pron.unwrap_or(3) as u16),
                    format!("Pronouns should be she, he or they (not \"{other}\"); they was used."),
                );
                "they"
            }
        };
        let (first, last) = if first.is_empty() { (last, String::new()) } else { (first, last) };
        students.push(StudentIn { number, last, first, pronouns: pronouns.into() });
        student_rows.push(row);
    }

    // Assessments, one per column.
    let row_of = |l: &str| find(r, l, a_row + 1..a_row + 8, a_col..a_col + 1).map(|p| p.0);
    let (r_cat, r_out, r_weight, r_type) = (row_of(L_CATEGORY), row_of(L_OUT_OF), row_of(L_WEIGHT), row_of(L_TYPE));
    let mut assessments = Vec::new();
    for col in first_mark_col..max_col.min(first_mark_col + MAX_ASSESSMENTS as u32) {
        let cell = |row: Option<u32>| row.map(|rw| r.get_value((rw, col)));
        let at = |row: u32| row_col_to_cell(row, col as u16);
        let name = clip(text(r.get_value((a_row, col))), 120);
        let any_marks = student_rows.iter().any(|&row| !text(r.get_value((row, col))).is_empty() || is_date(r.get_value((row, col))));
        if name.is_empty() {
            if any_marks {
                problem(at(a_row), "This column has marks but no assessment name; it was left out.".into());
            }
            continue;
        }
        let done = r_type.is_some_and(|rt| text(r.get_value((rt, col))).to_lowercase().starts_with("done"));
        let out_of = match cell(r_out).flatten() {
            v if text(v).is_empty() => 100.0,
            v => match number(v) {
                Some(o) if o > 0.0 && o <= 10_000.0 => o,
                _ => {
                    problem(at(r_out.unwrap_or(a_row)), format!("\"Out of\" for {name} should be a number above 0; 100 was used."));
                    100.0
                }
            },
        };
        let out_of = if done { 1.0 } else { out_of };
        let weight = match cell(r_weight).flatten() {
            v if text(v).is_empty() => 1.0,
            v => match number(v) {
                Some(w) if (0.0..=100.0).contains(&w) => w,
                _ => {
                    problem(at(r_weight.unwrap_or(a_row)), format!("The weight for {name} should be a number from 0 to 100; 1 was used."));
                    1.0
                }
            },
        };
        let category = r_cat.map(|rc| clip(text(r.get_value((rc, col))), 80)).unwrap_or_default();
        if !categories.is_empty() && !done {
            if category.is_empty() {
                problem(
                    at(r_cat.unwrap_or(a_row)),
                    format!("{name} has no category, so it won't count toward the mark until you choose one."),
                );
            } else if !categories.iter().any(|c| c.0.eq_ignore_ascii_case(&category)) {
                problem(
                    at(r_cat.unwrap_or(a_row)),
                    format!("\"{category}\" (for {name}) isn't one of the categories at the top of the sheet; it was added with weight 0."),
                );
            }
        }
        let kind = if done { Kind::Completion } else { Kind::Scored };
        let mut marks = Vec::new();
        for (i, &row) in student_rows.iter().enumerate() {
            let v = r.get_value((row, col));
            if is_date(v) {
                problem(at(row), "Excel changed this mark into a date. Retype it (for example 3/4 or 75%).".into());
                continue;
            }
            let input = text(v);
            match engine::parse_input(kind, &input, out_of) {
                Ok(CellInput::Clear) => {}
                Ok(CellInput::Set(m)) => marks.push((i, m)),
                Err(e) => problem(at(row), format!("\"{input}\": {e}")),
            }
        }
        assessments.push(AssessmentIn { name, cell: at(a_row), category, out_of, weight, done, marks });
    }

    if class.is_empty() {
        if !students.is_empty() || !assessments.is_empty() {
            problem(
                value(L_CLASS).map(|v| row_col_to_cell(v.0, v.1 as u16)).unwrap_or_default(),
                "This sheet has students or assessments but no class name; it was left out.".into(),
            );
        }
        return None;
    }
    Some(SheetIn { sheet: name.to_string(), class, subject, course_code, period, categories, assessments, students })
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
    pub problems: Vec<Problem>,
}

impl Outcome {
    pub fn changes(&self) -> usize {
        self.classes_new.len() + self.subjects_new + self.students_new + self.assessments_new + self.marks_changed
    }
}

/// Create or update everything in the file. Run inside a transaction; the
/// preview runs it the same way and rolls back.
pub fn apply(tx: &Transaction, user_id: i64, p: &Parsed) -> AppResult<Outcome> {
    let settings = model::settings(tx, user_id)?;
    let term = model::current_term(tx, user_id)?.ok_or_else(|| bad("Create a term first."))?;
    let mut out = Outcome { problems: p.problems.clone(), ..Default::default() };
    fn problem(out: &mut Outcome, sheet: &str, cell: &str, message: String) {
        out.problems.push(Problem { sheet: sheet.to_string(), cell: cell.to_string(), message })
    }

    // Sheets with the same class name are one class (one sheet per subject).
    let mut groups: Vec<(String, Vec<&SheetIn>)> = Vec::new();
    for s in &p.sheets {
        match groups.iter_mut().find(|g| g.0.eq_ignore_ascii_case(&s.class)) {
            Some(g) => g.1.push(s),
            None => groups.push((s.class.clone(), vec![s])),
        }
    }

    for (class_name, sheets) in groups {
        let classes = model::classes_in_term(tx, user_id, term.id)?;
        let first = sheets[0];
        let class_id = match classes.iter().find(|c| c.name.eq_ignore_ascii_case(&class_name)) {
            Some(c) => {
                out.classes_updated.push(c.name.clone());
                c.id
            }
            None => {
                if settings.mode == Mode::Secondary && classes.len() as i64 >= settings.active_periods {
                    problem(
                        &mut out,
                        &first.sheet,
                        "B3",
                        format!(
                            "{class_name} wasn't added: you're set up for {} teaching period(s) and they're all used. Change the number of periods in Settings, then upload again.",
                            settings.active_periods
                        ),
                    );
                    continue;
                }
                let wanted = sheets.iter().find_map(|s| s.period);
                let period = match wanted {
                    Some(pd) if settings.mode != Mode::Secondary => Some(pd),
                    Some(pd) if Some(pd) == settings.prep_period || classes.iter().any(|c| c.period == Some(pd)) => {
                        problem(
                            &mut out,
                            &first.sheet,
                            "B6",
                            format!("Period {pd} is your prep period or already has a class, so {class_name} was added without a period."),
                        );
                        None
                    }
                    pd => pd,
                };
                let code = sheets.iter().map(|s| s.course_code.as_str()).find(|c| !c.is_empty()).unwrap_or("");
                tx.execute(
                    "INSERT INTO classes (user_id, term_id, name, course_code, period, sort) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![user_id, term.id, class_name, code, period, classes.len() as i64],
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
            let subject_id = match subjects.iter().find(|x| x.name.eq_ignore_ascii_case(wanted)) {
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

            // Categories: add new ones, update weights.
            let mut cats: Vec<(i64, String, f64)> =
                model::categories(tx, subject_id)?.into_iter().map(|c| (c.id, c.name, c.weight)).collect();
            let wanted_cats = s.categories.iter().cloned().chain(
                s.assessments
                    .iter()
                    .filter(|a| !a.done && !a.category.is_empty() && !s.categories.iter().any(|c| c.0.eq_ignore_ascii_case(&a.category)))
                    .map(|a| (a.category.clone(), 0.0)),
            );
            for (name, weight) in wanted_cats {
                match cats.iter().find(|c| c.1.eq_ignore_ascii_case(&name)) {
                    Some(c) if (c.2 - weight).abs() > 1e-9 && s.categories.iter().any(|x| x.0.eq_ignore_ascii_case(&name)) => {
                        tx.execute("UPDATE categories SET weight = ?1 WHERE id = ?2", params![weight, c.0])?;
                    }
                    Some(_) => {}
                    None => {
                        tx.execute(
                            "INSERT INTO categories (subject_id, name, weight, sort) VALUES (?1, ?2, ?3, ?4)",
                            params![subject_id, name, weight, cats.len() as i64],
                        )?;
                        cats.push((tx.last_insert_rowid(), name, weight));
                    }
                }
            }
            let cat_id = |name: &str| cats.iter().find(|c| c.1.eq_ignore_ascii_case(name)).map(|c| c.0);

            // Students: match in the class by number, then by name; then the
            // teacher's other classes by number; otherwise add them.
            let mut ids = Vec::with_capacity(s.students.len());
            for st in &s.students {
                let found = roster
                    .iter()
                    .find(|x| !st.number.is_empty() && x.student_number == st.number)
                    .or_else(|| {
                        roster.iter().find(|x| x.first_name.eq_ignore_ascii_case(&st.first) && x.last_name.eq_ignore_ascii_case(&st.last))
                    })
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
                                    params![user_id, st.first, st.last, st.number, st.pronouns],
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
                if st.pronouns != "they" {
                    tx.execute("UPDATE students SET pronouns = ?1 WHERE id = ?2", params![st.pronouns, id])?;
                }
                if !st.number.is_empty() {
                    tx.execute("UPDATE students SET student_number = ?1 WHERE id = ?2 AND student_number = ''", params![st.number, id])?;
                }
                ids.push(id);
            }

            // Assessments and marks.
            let existing = model::assessments(tx, subject_id)?;
            let mut sort = existing.len() as i64;
            let mut changes = Vec::new();
            for a in &s.assessments {
                let kind = if a.done { Kind::Completion } else { Kind::Scored };
                let category = if a.category.is_empty() { None } else { cat_id(&a.category) };
                let aid = match existing.iter().find(|x| x.name.eq_ignore_ascii_case(&a.name)) {
                    Some(x) => {
                        if Kind::parse(&x.kind) != kind {
                            problem(
                                &mut out,
                                &s.sheet,
                                &a.cell,
                                format!("{} is already in Gradebook as a different type; its type wasn't changed.", a.name),
                            );
                        }
                        if kind == Kind::Scored && (x.out_of - a.out_of).abs() > 1e-9 {
                            problem(
                                &mut out,
                                &s.sheet,
                                &a.cell,
                                format!(
                                    "{} is out of {} in Gradebook but {} here. Marks were read as out of {}; change \"Out of\" on the grid if you meant to (it can rescale existing marks).",
                                    a.name,
                                    fmt_num(x.out_of),
                                    fmt_num(a.out_of),
                                    fmt_num(x.out_of)
                                ),
                            );
                        }
                        tx.execute(
                            "UPDATE assessments SET weight = ?1, category_id = COALESCE(?2, category_id) WHERE id = ?3",
                            params![a.weight, category, x.id],
                        )?;
                        // Marks were parsed against the sheet's "Out of"; put them on Gradebook's.
                        let scale = if kind == Kind::Scored { x.out_of / a.out_of } else { 1.0 };
                        for (i, m) in &a.marks {
                            let m = MarkEntry { value: m.value.map(|v| (v * scale * 1000.0).round() / 1000.0), status: m.status };
                            changes.push(marks::Change { assessment_id: x.id, student_id: ids[*i], new: Some(m) });
                        }
                        continue;
                    }
                    None => {
                        tx.execute(
                            "INSERT INTO assessments (subject_id, category_id, name, out_of, weight, kind, sort) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            params![subject_id, category, a.name, a.out_of, a.weight, kind.as_str(), sort],
                        )?;
                        sort += 1;
                        out.assessments_new += 1;
                        tx.last_insert_rowid()
                    }
                };
                for (i, m) in &a.marks {
                    changes.push(marks::Change { assessment_id: aid, student_id: ids[*i], new: Some(*m) });
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
            setup_done: true,
        }
    }

    #[test]
    fn blank_templates_parse_as_empty() {
        for mode in [Mode::Elementary, Mode::Secondary, Mode::University] {
            let t = blank(&settings(mode));
            let bytes = build(&t).unwrap();
            assert!(is_template(&bytes));
            assert!(!crate::workbook::is_workbook(&bytes));
            // Nothing filled in yet: a friendly error, not an empty import.
            let r = parse(&bytes);
            assert!(r.is_err(), "{mode:?}: {:?}", r.ok());
        }
        let t = blank(&settings(Mode::Secondary));
        let titles: Vec<_> = t.sheets.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["Example - Math", "Period 1", "Period 3", "Period 4"], "skips the prep period");
    }

    #[test]
    fn filled_sheet_round_trips_with_problems_located() {
        let marks = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let t = Template {
            missing_policy: MissingPolicy::Ignore,
            sheets: vec![SheetSpec {
                title: "Bio".into(),
                class: "Biology".into(),
                subject: String::new(),
                course_code: "SBI3U".into(),
                period: Some(1),
                categories: vec![("Knowledge".into(), 60.0), ("Application".into(), 40.0)],
                assessments: vec![
                    AssessmentSpec { name: "Quiz".into(), category: "Knowledge".into(), out_of: 20.0, weight: 1.0, done: false },
                    AssessmentSpec { name: "Lab".into(), category: "Nope".into(), out_of: 40.0, weight: 2.0, done: false },
                    AssessmentSpec { name: "Journal".into(), category: String::new(), out_of: 1.0, weight: 1.0, done: true },
                ],
                students: vec![
                    StudentSpec { number: "1".into(), last: "Lee".into(), first: "Ana".into(), pronouns: "she".into() },
                    StudentSpec { last: "Khan".into(), first: "Omar".into(), ..Default::default() },
                ],
                marks: vec![marks(&["17", "3/4", "Y"]), marks(&["abc", "85%L", "N"])],
                link_to: None,
            }],
        };
        let p = parse(&build(&t).unwrap()).unwrap();
        assert_eq!(p.sheets.len(), 1);
        let s = &p.sheets[0];
        assert_eq!((s.class.as_str(), s.course_code.as_str(), s.period), ("Biology", "SBI3U", Some(1)));
        assert_eq!(s.categories, vec![("Knowledge".to_string(), 60.0), ("Application".to_string(), 40.0)]);
        assert_eq!(s.students.len(), 2);
        assert_eq!(s.students[0].pronouns, "she");
        let quiz = &s.assessments[0];
        assert_eq!(quiz.marks, vec![(0, MarkEntry::marked(17.0))]);
        let lab = &s.assessments[1];
        assert_eq!(lab.weight, 2.0);
        assert_eq!(lab.marks[0], (0, MarkEntry::marked(30.0)));
        assert_eq!(lab.marks[1], (1, MarkEntry { value: Some(34.0), status: Status::Late }));
        assert_eq!(s.assessments[2].marks[1].1.status, Status::Missing);
        // The bad mark and the unknown category are reported where they are.
        assert!(p.problems.iter().any(|x| x.cell == "E15" && x.message.contains("abc")), "{:?}", p.problems);
        assert!(p.problems.iter().any(|x| x.cell == "F9" && x.message.contains("Nope")), "{:?}", p.problems);
    }
}
