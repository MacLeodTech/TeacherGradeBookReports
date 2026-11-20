//! The workbook file: a teacher's classes, students, marks, comments and
//! report history in one `.xlsx` they keep themselves.
//!
//! With memory storage this file *is* the gradebook: open it to start, save
//! it to keep your work. It opens in Excel too:
//!
//! - The same tabs as the starter spreadsheet (`crate::starter`): **Start
//!   here**, a plain grid of marks per class and subject in the current term,
//!   and the grey **Setup** tabs. These are editable: the teacher can add
//!   marks and assessments in Excel, and opening the file applies them on
//!   top of the data (a cleared cell clears the mark).
//! - Hidden `_…` sheets with the data itself, one per table, which is what
//!   the app reads back first. Editing those by hand isn't supported.
//!
//! Opening maps every id to a fresh one, so a workbook can be opened into any
//! account, and opening the same file twice in "add" mode makes two copies.

use std::{collections::HashMap, io::Cursor};

use calamine::{Data, Reader, Xlsx};
use rusqlite::{Connection, Transaction, types::Value as Sql};
use rust_xlsxwriter::Workbook;

use crate::{
    error::{AppResult, bad},
    export::err,
    model, starter,
};

const FORMAT: &str = "gradebook-workbook";
/// 2: the visible tabs are the editable spreadsheet (`crate::starter`).
const VERSION: i64 = 2;
const META_SHEET: &str = "_gradebook";
/// Cap on rows per table when reading (a term is a few thousand).
const MAX_ROWS: usize = 200_000;
const MAX_TEXT: usize = 20_000;

/// A table in the file: which columns, and how to find this teacher's rows.
struct Table {
    name: &'static str,
    /// Has its own `id` (remapped on open).
    has_id: bool,
    cols: &'static [&'static str],
    /// `WHERE` clause selecting the teacher's rows (`?1` = user id).
    owner: &'static str,
    order: &'static str,
}

const IN_MY_CLASSES: &str = "class_id IN (SELECT id FROM classes WHERE user_id = ?1)";
const IN_MY_SUBJECTS: &str = "subject_id IN (SELECT s.id FROM subjects s JOIN classes c ON c.id = s.class_id WHERE c.user_id = ?1)";
const IN_MY_ASSESSMENTS: &str = "assessment_id IN (SELECT a.id FROM assessments a JOIN subjects s ON s.id = a.subject_id \
     JOIN classes c ON c.id = s.class_id WHERE c.user_id = ?1)";

/// In dependency order: every reference points at a table earlier in the list.
const TABLES: &[Table] = &[
    Table {
        name: "terms",
        has_id: true,
        cols: &["id", "name", "period_label", "start_date", "end_date", "is_current", "archived", "archived_at", "created_at"],
        owner: "user_id = ?1",
        order: "id",
    },
    Table {
        name: "classes",
        has_id: true,
        cols: &["id", "term_id", "name", "course_code", "period", "grade_level", "room", "sort", "created_at"],
        owner: "user_id = ?1",
        order: "id",
    },
    Table { name: "subjects", has_id: true, cols: &["id", "class_id", "name", "report_style", "sort"], owner: IN_MY_CLASSES, order: "id" },
    Table { name: "categories", has_id: true, cols: &["id", "subject_id", "name", "weight", "sort"], owner: IN_MY_SUBJECTS, order: "id" },
    Table {
        name: "assessments",
        has_id: true,
        cols: &["id", "subject_id", "category_id", "name", "unit", "out_of", "weight", "kind", "due_date", "include_in_report", "sort"],
        owner: IN_MY_SUBJECTS,
        order: "id",
    },
    Table {
        name: "students",
        has_id: true,
        cols: &["id", "first_name", "last_name", "preferred_name", "student_number", "pronouns", "created_at"],
        owner: "user_id = ?1",
        order: "id",
    },
    Table {
        name: "enrolments",
        has_id: false,
        cols: &["class_id", "student_id", "active"],
        owner: IN_MY_CLASSES,
        order: "class_id, student_id",
    },
    Table {
        name: "marks",
        has_id: false,
        cols: &["assessment_id", "student_id", "value", "status", "updated_at"],
        owner: IN_MY_ASSESSMENTS,
        order: "assessment_id, student_id",
    },
    Table {
        name: "grade_overrides",
        has_id: false,
        cols: &["student_id", "subject_id", "pct", "reason"],
        owner: IN_MY_SUBJECTS,
        order: "subject_id, student_id",
    },
    Table {
        name: "report_comments",
        has_id: false,
        cols: &["student_id", "subject_id", "comment", "next_steps", "updated_at"],
        owner: IN_MY_SUBJECTS,
        order: "subject_id, student_id",
    },
    Table {
        name: "learning_skills",
        has_id: false,
        cols: &["student_id", "class_id", "skill", "rating"],
        owner: IN_MY_CLASSES,
        order: "class_id, student_id, skill",
    },
    Table {
        name: "report_runs",
        has_id: false,
        cols: &["class_id", "student_id", "report_type", "overall", "created_at"],
        owner: IN_MY_CLASSES,
        order: "id",
    },
    Table {
        name: "mark_history",
        has_id: false,
        cols: &[
            "batch",
            "class_id",
            "assessment_id",
            "student_id",
            "old_value",
            "old_status",
            "new_value",
            "new_status",
            "source",
            "undone",
            "changed_at",
        ],
        owner: IN_MY_CLASSES,
        order: "id",
    },
];

/// Columns that point at another table's `id`.
fn ref_table(col: &str) -> Option<&'static str> {
    Some(match col {
        "term_id" => "terms",
        "class_id" => "classes",
        "subject_id" => "subjects",
        "category_id" => "categories",
        "assessment_id" => "assessments",
        "student_id" => "students",
        _ => return None,
    })
}

/// Allowed values for enumerated columns.
fn allowed(table: &str, col: &str) -> Option<&'static [&'static str]> {
    Some(match (table, col) {
        ("marks", "status") => &["marked", "late", "missing", "excused", "pending"],
        ("assessments", "kind") => &["scored", "completion", "level"],
        ("subjects", "report_style") => &["percent", "letter"],
        ("students", "pronouns") => &["she", "he", "they"],
        ("learning_skills", "rating") => &["E", "G", "S", "N"],
        _ => return None,
    })
}

// ── Saving ───────────────────────────────────────────────────────────────────

/// Who the file belongs to, for the "Start here" tab.
pub struct Owner<'a> {
    pub teacher: &'a str,
    pub school: &'a str,
}

pub fn save(c: &Connection, user_id: i64, owner: &Owner) -> AppResult<Vec<u8>> {
    let mut wb = Workbook::new();
    let settings = model::settings(c, user_id)?;
    let terms = model::terms(c, user_id)?;

    // What the teacher works with: Start here, a tab per class, setup tabs.
    let mut counts = Vec::new();
    for t in ["classes", "students", "marks"] {
        let n: i64 = c.query_row(
            &format!("SELECT count(*) FROM {t} WHERE {}", TABLES.iter().find(|x| x.name == t).map(|x| x.owner).unwrap_or("0")),
            [user_id],
            |r| r.get(0),
        )?;
        counts.push(n);
    }
    let current = terms.iter().find(|t| t.is_current).or(terms.first());
    let others: Vec<&str> = terms.iter().filter(|t| Some(t.id) != current.map(|c| c.id)).map(|t| t.name.as_str()).collect();
    let mut t = if current.is_some() { starter::from_term(c, user_id, &settings)? } else { starter::blank(&settings) };
    t.example = None;
    t.saved = Some(starter::SavedInfo {
        teacher: owner.teacher.to_string(),
        school: owner.school.to_string(),
        term: current.map_or_else(|| "this term".to_string(), |t| t.name.clone()),
        contents: format!(
            "{} classes, {} students, {} marks{}",
            counts[0],
            counts[1],
            counts[2],
            if others.is_empty() { String::new() } else { format!(". Other terms (saved, not shown): {}", others.join(", ")) }
        ),
    });
    starter::write_visible(&mut wb, &t)?;

    // The data.
    let ws = wb.add_worksheet();
    ws.set_name(META_SHEET).map_err(err)?;
    for (i, (k, v)) in [("format", FORMAT.to_string()), ("version", VERSION.to_string()), ("app", env!("CARGO_PKG_VERSION").to_string())]
        .iter()
        .enumerate()
    {
        ws.write(i as u32, 0, *k).map_err(err)?;
        ws.write(i as u32, 1, v.as_str()).map_err(err)?;
    }
    ws.set_hidden(true);
    ws.protect();
    for t in TABLES {
        let ws = wb.add_worksheet();
        ws.set_name(format!("_{}", t.name)).map_err(err)?;
        for (j, col) in t.cols.iter().enumerate() {
            ws.write(0, j as u16, *col).map_err(err)?;
        }
        let sql = format!("SELECT {} FROM {} WHERE {} ORDER BY {}", t.cols.join(", "), t.name, t.owner, t.order);
        let mut st = c.prepare(&sql)?;
        let mut rows = st.query([user_id])?;
        let mut r = 1u32;
        while let Some(row) = rows.next()? {
            for j in 0..t.cols.len() {
                let col = j as u16;
                match row.get::<_, Sql>(j)? {
                    Sql::Null => {}
                    Sql::Integer(v) => {
                        ws.write_number(r, col, v as f64).map_err(err)?;
                    }
                    Sql::Real(v) => {
                        ws.write_number(r, col, v).map_err(err)?;
                    }
                    Sql::Text(v) => {
                        ws.write_string(r, col, &v).map_err(err)?;
                    }
                    Sql::Blob(_) => {}
                }
            }
            r += 1;
        }
        ws.set_hidden(true);
        ws.protect();
    }
    wb.save_to_buffer().map_err(err)
}

// ── Opening ──────────────────────────────────────────────────────────────────

/// Whether these bytes are a Gradebook workbook (vs. any other spreadsheet).
pub fn is_workbook(bytes: &[u8]) -> bool {
    let Ok(mut x) = calamine::open_workbook_from_rs::<Xlsx<_>, _>(Cursor::new(bytes)) else { return false };
    x.worksheet_range(META_SHEET).ok().is_some_and(|r| cell_text(r.get_value((0, 1))) == FORMAT)
}

/// The tables read from a workbook, not yet saved anywhere.
pub struct Parsed {
    tables: HashMap<&'static str, (Vec<String>, Vec<Vec<Data>>)>,
    /// The class tabs, as the teacher may have edited them in Excel (from
    /// version 2 on; earlier workbooks had a read-only view).
    tabs: Option<starter::Parsed>,
}

pub fn parse(bytes: &[u8]) -> AppResult<Parsed> {
    let mut x =
        calamine::open_workbook_from_rs::<Xlsx<_>, _>(Cursor::new(bytes)).map_err(|_| bad("That file isn't an Excel workbook (.xlsx)."))?;
    let meta = x
        .worksheet_range(META_SHEET)
        .map_err(|_| bad("That spreadsheet isn't a Gradebook workbook. To bring in marks from another spreadsheet, use Import."))?;
    if cell_text(meta.get_value((0, 1))) != FORMAT {
        return Err(bad("That spreadsheet isn't a Gradebook workbook."));
    }
    let version: i64 = cell_text(meta.get_value((1, 1))).parse().unwrap_or(0);
    if version > VERSION {
        return Err(bad("This workbook was saved by a newer version of Gradebook. Ask your administrator to update it."));
    }
    let mut tables = HashMap::new();
    for t in TABLES {
        let Ok(range) = x.worksheet_range(&format!("_{}", t.name)) else { continue };
        let mut rows = range.rows();
        let header: Vec<String> = rows.next().map(|r| r.iter().map(|c| cell_text(Some(c))).collect()).unwrap_or_default();
        let data: Vec<Vec<Data>> = rows.filter(|r| r.iter().any(|c| !matches!(c, Data::Empty))).map(|r| r.to_vec()).collect();
        if data.len() > MAX_ROWS {
            return Err(bad(format!("This workbook has too many {} to open.", t.name.replace('_', " "))));
        }
        tables.insert(t.name, (header, data));
    }
    if !tables.contains_key("terms") || !tables.contains_key("classes") {
        return Err(bad("This workbook is damaged: its data sheets are missing."));
    }
    let tabs = if version >= 2 { Some(starter::parse_tabs(bytes)?) } else { None };
    Ok(Parsed { tables, tabs })
}

fn cell_text(c: Option<&Data>) -> String {
    match c {
        Some(Data::String(s)) => s.trim().to_string(),
        Some(Data::Float(f)) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", *f as i64),
        Some(Data::Float(f)) => f.to_string(),
        Some(Data::Int(i)) => i.to_string(),
        Some(Data::Bool(b)) => (if *b { "1" } else { "0" }).into(),
        // Dates are saved as text; a serial number only appears if someone edited the sheet.
        Some(Data::DateTime(d)) => d.as_f64().to_string(),
        Some(Data::DateTimeIso(s)) | Some(Data::DurationIso(s)) => s.clone(),
        _ => String::new(),
    }
}

/// Declared column type (INTEGER / REAL / TEXT) and whether NULL is allowed.
fn column_types(c: &Connection, table: &str) -> AppResult<HashMap<String, (String, bool)>> {
    let mut st = c.prepare(&format!("SELECT name, upper(type), \"notnull\" FROM pragma_table_info('{table}')"))?;
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, String>(1)?, r.get::<_, i64>(2)? == 0))))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn to_sql(cell: &Data, ty: &str) -> Option<Sql> {
    if matches!(cell, Data::Empty) {
        return None;
    }
    let text = cell_text(Some(cell));
    match ty {
        "INTEGER" => match cell {
            Data::Float(f) if f.fract() == 0.0 => Some(Sql::Integer(*f as i64)),
            Data::Int(i) => Some(Sql::Integer(*i)),
            Data::Bool(b) => Some(Sql::Integer(*b as i64)),
            _ => text.parse().ok().map(Sql::Integer),
        },
        "REAL" => match cell {
            Data::Float(f) if f.is_finite() => Some(Sql::Real(*f)),
            Data::Int(i) => Some(Sql::Real(*i as f64)),
            _ => text.parse::<f64>().ok().filter(|f| f.is_finite()).map(Sql::Real),
        },
        _ => Some(Sql::Text(text.chars().take(MAX_TEXT).collect())),
    }
}

/// How opening treats what's already there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Remove the teacher's classes, students and terms first.
    Replace,
    /// Keep them and add the workbook's alongside.
    Add,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct Opened {
    pub terms: usize,
    pub classes: usize,
    pub students: usize,
    pub marks: usize,
    pub skipped: usize,
    /// What the teacher changed in Excel, applied on top.
    pub edits: Option<starter::Outcome>,
}

/// Load a parsed workbook into the teacher's data, all or nothing.
pub fn open(tx: &Transaction, user_id: i64, parsed: &Parsed, mode: Mode) -> AppResult<Opened> {
    if mode == Mode::Replace {
        tx.execute("DELETE FROM terms WHERE user_id = ?1", [user_id])?;
        tx.execute("DELETE FROM students WHERE user_id = ?1", [user_id])?;
        tx.execute("DELETE FROM import_staging WHERE user_id = ?1", [user_id])?;
    }
    let existing_terms: Vec<String> = model::terms(tx, user_id)?.into_iter().map(|t| t.name.to_lowercase()).collect();
    // old id → new id, per table
    let mut ids: HashMap<&'static str, HashMap<i64, i64>> = HashMap::new();
    let mut out = Opened::default();
    let mut new_current: Option<i64> = None;

    for t in TABLES {
        let Some((header, rows)) = parsed.tables.get(t.name) else { continue };
        let types = column_types(tx, t.name)?;
        // Columns we know, at their position in this file.
        let cols: Vec<(usize, &str)> =
            header.iter().enumerate().filter_map(|(i, h)| t.cols.iter().find(|c| **c == h.as_str()).map(|c| (i, *c))).collect();
        'row: for row in rows {
            let mut names: Vec<&str> = Vec::new();
            let mut vals: Vec<Sql> = Vec::new();
            let mut old_id = None;
            for (i, col) in &cols {
                let Some((ty, _)) = types.get(*col) else { continue };
                let cell = row.get(*i).unwrap_or(&Data::Empty);
                let Some(mut v) = to_sql(cell, ty) else {
                    if *col == "id" || (ref_table(col).is_some() && *col != "category_id") {
                        out.skipped += 1;
                        continue 'row;
                    }
                    continue;
                };
                if *col == "id" {
                    if let Sql::Integer(i) = v {
                        old_id = Some(i);
                    }
                    continue;
                }
                if let Some(target) = ref_table(col) {
                    let Sql::Integer(old) = v else {
                        out.skipped += 1;
                        continue 'row;
                    };
                    match ids.get(target).and_then(|m| m.get(&old)) {
                        Some(new) => v = Sql::Integer(*new),
                        // A deleted category just leaves the assessment uncategorised.
                        None if *col == "category_id" => continue,
                        None => {
                            out.skipped += 1;
                            continue 'row;
                        }
                    }
                }
                if let (Some(ok), Sql::Text(s)) = (allowed(t.name, col), &v)
                    && !ok.contains(&s.as_str())
                {
                    out.skipped += 1;
                    continue 'row;
                }
                if t.name == "terms"
                    && *col == "name"
                    && let Sql::Text(s) = &v
                    && existing_terms.contains(&s.to_lowercase())
                {
                    v = Sql::Text(format!("{s} (opened {})", chrono::Local::now().format("%b %-d")));
                }
                names.push(col);
                vals.push(v);
            }
            if t.has_id && old_id.is_none() {
                out.skipped += 1;
                continue;
            }
            if matches!(t.name, "terms" | "classes" | "students") {
                names.push("user_id");
                vals.push(Sql::Integer(user_id));
            }
            let placeholders: Vec<String> = (1..=vals.len()).map(|i| format!("?{i}")).collect();
            let sql = format!("INSERT OR REPLACE INTO {} ({}) VALUES ({})", t.name, names.join(", "), placeholders.join(", "));
            tx.prepare_cached(&sql)?.execute(rusqlite::params_from_iter(vals.iter())).map_err(|e| {
                bad(format!("This workbook couldn't be opened: a row in its {} is damaged ({e}).", t.name.replace('_', " ")))
            })?;
            if let Some(old) = old_id {
                let new = tx.last_insert_rowid();
                ids.entry(t.name).or_default().insert(old, new);
                if t.name == "terms" {
                    let cur: i64 = tx.query_row("SELECT is_current FROM terms WHERE id = ?1", [new], |r| r.get(0))?;
                    if cur != 0 {
                        new_current = Some(new);
                    }
                }
            }
            match t.name {
                "terms" => out.terms += 1,
                "classes" => out.classes += 1,
                "students" => out.students += 1,
                "marks" => out.marks += 1,
                _ => {}
            }
        }
    }
    if out.terms == 0 {
        return Err(bad("This workbook has no terms in it."));
    }
    // The workbook's current term becomes the current one.
    if let Some(cur) = new_current.or_else(|| ids.get("terms").and_then(|m| m.values().max().copied())) {
        tx.execute("UPDATE terms SET is_current = (id = ?1) WHERE user_id = ?2", [cur, user_id])?;
    }
    // Then what was changed in Excel: the class tabs are the current term's
    // marks, so a cleared cell clears.
    if let Some(tabs) = &parsed.tabs
        && !tabs.sheets.is_empty()
    {
        out.edits = Some(starter::apply(tx, user_id, tabs, true)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ownership_filters_cover_every_table() {
        for t in TABLES {
            assert!(!t.owner.is_empty(), "{}", t.name);
            for c in t.cols {
                if let Some(target) = ref_table(c) {
                    let pos = TABLES.iter().position(|x| x.name == target).unwrap();
                    let me = TABLES.iter().position(|x| x.name == t.name).unwrap();
                    assert!(pos < me || (t.name == target), "{}.{c} refers to a later table", t.name);
                }
            }
        }
        assert_eq!(
            TABLES.len() + 1,
            crate::db::WORKSPACE_TABLES.len(),
            "a student table is missing from the workbook (only import_staging is left out)"
        );
    }

    #[test]
    fn coercion() {
        assert_eq!(to_sql(&Data::Float(3.0), "INTEGER"), Some(Sql::Integer(3)));
        assert_eq!(to_sql(&Data::Float(3.5), "REAL"), Some(Sql::Real(3.5)));
        assert_eq!(to_sql(&Data::Float(123456.0), "TEXT"), Some(Sql::Text("123456".into())));
        assert_eq!(to_sql(&Data::String("00123".into()), "TEXT"), Some(Sql::Text("00123".into())));
        assert_eq!(to_sql(&Data::Empty, "TEXT"), None);
        assert_eq!(to_sql(&Data::String("abc".into()), "INTEGER"), None);
    }
}
