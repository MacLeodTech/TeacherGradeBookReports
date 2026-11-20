//! Spreadsheet import: parse → map columns → preview the changes → apply.
//!
//! Handles the original layout (ID, Last, First, Mark, then `1.1 Name` columns
//! and `J1.1` journals) through auto-detection, as well as arbitrary sheets
//! via the column mapping step and this app's own `.xlsx` export.

use std::{collections::HashMap, io::Cursor};

use calamine::{Data, Reader, open_workbook_auto_from_rs};
use regex_lite::Regex;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::{
    engine::{self, CellInput, Kind, MarkEntry, Status, fmt_num},
    error::{AppResult, bad},
    marks::{self, Change},
    model::{self, Student},
    util::name_key,
};

// ── Parsing ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Cell {
    Num(f64),
    Text(String),
    Empty,
}

impl Cell {
    pub fn is_empty(&self) -> bool {
        match self {
            Cell::Empty => true,
            Cell::Text(t) => t.trim().is_empty(),
            Cell::Num(_) => false,
        }
    }
    pub fn text(&self) -> String {
        match self {
            Cell::Num(n) => fmt_num(*n),
            Cell::Text(t) => t.trim().to_string(),
            Cell::Empty => String::new(),
        }
    }
}

fn from_calamine(d: &Data) -> Cell {
    match d {
        Data::Int(i) => Cell::Num(*i as f64),
        Data::Float(f) => Cell::Num(*f),
        Data::String(s) => {
            let t = s.trim();
            if t.is_empty() { Cell::Empty } else { Cell::Text(t.to_string()) }
        }
        Data::Bool(b) => Cell::Text(if *b { "TRUE".into() } else { "FALSE".into() }),
        Data::DateTime(dt) => Cell::Num(dt.as_f64()),
        Data::DateTimeIso(s) | Data::DurationIso(s) => Cell::Text(s.clone()),
        Data::Error(_) | Data::Empty => Cell::Empty,
    }
}

pub type Grid = Vec<Vec<Cell>>;

/// Read the first non-empty sheet of an .xlsx/.xlsm/.xls/.ods file, or a CSV.
/// Formula cells give their last calculated value.
pub fn parse_file(bytes: &[u8], filename: &str) -> AppResult<Grid> {
    let lower = filename.to_ascii_lowercase();
    if lower.ends_with(".csv") || lower.ends_with(".txt") {
        return Ok(parse_csv(&String::from_utf8_lossy(bytes)));
    }
    let mut wb = open_workbook_auto_from_rs(Cursor::new(bytes.to_vec()))
        .map_err(|e| bad(format!("That file couldn't be read as a spreadsheet ({e}). Save it as .xlsx and try again.")))?;
    for name in wb.sheet_names().to_vec() {
        let Ok(range) = wb.worksheet_range(&name) else { continue };
        let grid: Grid = range.rows().map(|r| r.iter().map(from_calamine).collect()).collect();
        if grid.iter().any(|r| r.iter().any(|c| !c.is_empty())) {
            return Ok(trim_grid(grid));
        }
    }
    Err(bad("The spreadsheet is empty."))
}

fn trim_grid(mut g: Grid) -> Grid {
    while g.last().is_some_and(|r| r.iter().all(Cell::is_empty)) {
        g.pop();
    }
    let width = g.iter().map(|r| r.iter().rposition(|c| !c.is_empty()).map_or(0, |i| i + 1)).max().unwrap_or(0);
    for r in &mut g {
        r.resize(width, Cell::Empty);
    }
    g
}

pub fn parse_csv(text: &str) -> Grid {
    let text = text.trim_start_matches('\u{feff}');
    let delim = if text.lines().next().is_some_and(|l| l.matches('\t').count() > l.matches(',').count()) { '\t' } else { ',' };
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    let push_field = |row: &mut Vec<Cell>, field: &mut String| {
        let t = field.trim();
        let cell = if t.is_empty() {
            Cell::Empty
        } else if let Ok(n) = t.parse::<f64>() {
            Cell::Num(n)
        } else {
            Cell::Text(t.to_string())
        };
        row.push(cell);
        field.clear();
    };
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => in_quotes = !in_quotes,
            c if c == delim && !in_quotes => push_field(&mut row, &mut field),
            '\r' if !in_quotes => {}
            '\n' if !in_quotes => {
                push_field(&mut row, &mut field);
                rows.push(std::mem::take(&mut row));
            }
            c => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        push_field(&mut row, &mut field);
        rows.push(row);
    }
    trim_grid(rows)
}

// ── Column mapping ───────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Ignore,
    StudentNumber,
    LastName,
    FirstName,
    FullName,
    FinalMark,
    Assessment,
    Completion,
}

impl Role {
    pub const ALL: [(Role, &'static str); 8] = [
        (Role::Assessment, "Assessment (mark)"),
        (Role::Completion, "Done / not done (e.g. journal)"),
        (Role::FirstName, "First name"),
        (Role::LastName, "Last name"),
        (Role::FullName, "Full name"),
        (Role::StudentNumber, "Student number / ID"),
        (Role::FinalMark, "Final mark (from the spreadsheet)"),
        (Role::Ignore, "Ignore this column"),
    ];

    pub fn parse(s: &str) -> Role {
        serde_json::from_value(serde_json::Value::String(s.into())).unwrap_or(Role::Ignore)
    }

    pub fn as_str(self) -> String {
        serde_json::to_value(self).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
    }
}

/// Index of the header row: the first of the top 10 rows with 2+ text cells.
pub fn header_row(grid: &Grid) -> usize {
    grid.iter().take(10).position(|r| r.iter().filter(|c| matches!(c, Cell::Text(t) if !t.trim().is_empty())).count() >= 2).unwrap_or(0)
}

fn re(p: &str) -> Regex {
    Regex::new(p).expect("valid regex")
}

pub fn guess_role(header: &str) -> Role {
    let h = header.trim().to_lowercase();
    let h = h.trim_end_matches(':');
    if h.is_empty() {
        return Role::Ignore;
    }
    match h {
        "id" | "student id" | "studentid" | "student number" | "student #" | "student no" | "student no." | "number" | "#" | "oen"
        | "student_id" => return Role::StudentNumber,
        "last" | "last name" | "lastname" | "surname" | "family name" | "last_name" => return Role::LastName,
        "first" | "first name" | "firstname" | "given name" | "first_name" | "preferred name" => return Role::FirstName,
        "name" | "student" | "student name" | "full name" | "fullname" => return Role::FullName,
        "mark" | "final mark" | "overall" | "overall mark" | "average" | "term mark" | "weighted grade" | "current grade"
        | "weighted mark" | "grade" => return Role::FinalMark,
        _ => {}
    }
    if h.starts_with("overall (calculated)") || h == "letter" || h == "level" {
        return Role::Ignore;
    }
    if re(r"^j\d+\.").is_match(h) || h.ends_with("[done]") {
        return Role::Completion;
    }
    Role::Assessment
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColumnSpec {
    pub name: String,
    pub unit: String,
    pub out_of: Option<f64>,
}

/// `"1.2 Quiz (/20)"` → name `1.2 Quiz`, unit `Module 1`, out of 20.
/// `"J1.1"` → unit `Journals`.
/// `"Essay [Unit 3] (/40)"` (this app's export) → unit `Unit 3`.
pub fn parse_header(header: &str, role: Role) -> ColumnSpec {
    let mut name = header.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out_of = None;
    if let Some(c) = re(r"\s*[\(\[]?\s*/\s*(\d+(?:\.\d+)?)\s*[\)\]]?$").captures(&name) {
        out_of = c[1].parse().ok().filter(|v: &f64| *v > 0.0);
        let full = c.get(0).map(|m| m.start()).unwrap_or(name.len());
        name.truncate(full);
    }
    let name = name.trim().trim_end_matches(" [done]").to_string();
    let mut unit = String::new();
    let mut clean = name.clone();
    if let Some(c) = re(r"^(.*?)\s*\[([^\]]+)\]$").captures(&name) {
        clean = c[1].trim().to_string();
        unit = c[2].trim().to_string();
    } else if role == Role::Completion {
        unit = "Journals".into();
    } else if let Some(c) = re(r"^(\d+)\.\d*").captures(&name) {
        unit = format!("Module {}", &c[1]);
    }
    ColumnSpec { name: if clean.is_empty() { name } else { clean }, unit, out_of }
}

// ── Options + plan ───────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportOptions {
    pub roles: Vec<Role>,
    /// "missing" or "skip".
    pub blank_as: String,
    /// "missing" (the original app's rule) or "zero".
    pub zero_as: String,
    pub final_as_override: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct Plan {
    pub new_students: Vec<String>,
    pub matched_students: usize,
    pub new_assessments: Vec<String>,
    pub existing_assessments: usize,
    pub changed_marks: usize,
    pub unchanged_marks: usize,
    pub overrides: usize,
    pub skipped_rows: usize,
    pub warnings: Vec<String>,
}

struct RowStudent {
    existing: Option<i64>,
    first: String,
    last: String,
    number: String,
    in_class: bool,
}

struct Col {
    idx: usize,
    existing: Option<i64>,
    spec: ColumnSpec,
    kind: Kind,
    /// Multiply values by this to get points on `out_of`.
    scale: f64,
    out_of: f64,
}

struct Prepared {
    students: Vec<RowStudent>,
    cols: Vec<Col>,
    /// (student row index, col index, mark). `None` mark = leave alone.
    cells: Vec<(usize, usize, MarkEntry)>,
    finals: Vec<(usize, f64)>,
    plan: Plan,
}

fn split_full_name(s: &str) -> (String, String) {
    let s = s.trim();
    if let Some((last, first)) = s.split_once(',') {
        return (first.trim().to_string(), last.trim().to_string());
    }
    match s.rsplit_once(' ') {
        Some((first, last)) => (first.trim().to_string(), last.trim().to_string()),
        None => (s.to_string(), String::new()),
    }
}

fn parse_final(c: &Cell) -> Option<f64> {
    let v = match c {
        Cell::Num(n) => *n,
        Cell::Text(t) => t.trim().trim_end_matches('%').trim().parse().ok()?,
        Cell::Empty => return None,
    };
    let pct = if v <= 1.0 { v * 100.0 } else { v };
    (0.0..=150.0).contains(&pct).then_some(pct)
}

fn prepare(c: &Connection, user_id: i64, class_id: i64, subject_id: i64, grid: &Grid, opts: &ImportOptions) -> AppResult<Prepared> {
    let hdr = header_row(grid);
    let headers: Vec<String> = grid.get(hdr).map(|r| r.iter().map(Cell::text).collect()).unwrap_or_default();
    let role = |i: usize| opts.roles.get(i).copied().unwrap_or(Role::Ignore);
    let find = |r: Role| (0..headers.len()).find(|&i| role(i) == r);
    let (num_col, first_col, last_col, full_col, final_col) =
        (find(Role::StudentNumber), find(Role::FirstName), find(Role::LastName), find(Role::FullName), find(Role::FinalMark));
    if first_col.is_none() && last_col.is_none() && full_col.is_none() {
        return Err(bad("Choose which column holds the student names (first/last name or full name)."));
    }
    let mut plan = Plan::default();

    // Students.
    let roster = model::roster(c, class_id)?;
    let everyone = model::all_students(c, user_id)?;
    let by_number: HashMap<&str, &Student> =
        everyone.iter().filter(|s| !s.student_number.is_empty()).map(|s| (s.student_number.as_str(), s)).collect();
    let key = |s: &Student| name_key(&format!("{} {}", s.first_name, s.last_name));
    let key_pref = |s: &Student| name_key(&format!("{} {}", s.first(), s.last_name));
    let mut used: Vec<i64> = Vec::new();
    let mut students = Vec::new();
    let mut data_rows = Vec::new();
    for (ri, row) in grid.iter().enumerate().skip(hdr + 1) {
        let get = |col: Option<usize>| col.and_then(|i| row.get(i)).map(Cell::text).unwrap_or_default();
        let (mut first, mut last) = (get(first_col), get(last_col));
        if first.is_empty() && last.is_empty() {
            (first, last) = split_full_name(&get(full_col));
        }
        if first.is_empty() && last.is_empty() {
            if row.iter().any(|c| !c.is_empty()) {
                plan.skipped_rows += 1;
            }
            continue;
        }
        let number = get(num_col);
        let k = name_key(&format!("{first} {last}"));
        let found = (!number.is_empty())
            .then(|| by_number.get(number.as_str()).map(|s| s.id))
            .flatten()
            .or_else(|| roster.iter().find(|s| (key(s) == k || key_pref(s) == k) && !used.contains(&s.id)).map(|s| s.id))
            .or_else(|| everyone.iter().find(|s| (key(s) == k || key_pref(s) == k) && !used.contains(&s.id)).map(|s| s.id))
            .filter(|id| !used.contains(id));
        if let Some(id) = found {
            used.push(id);
            plan.matched_students += 1;
        } else {
            plan.new_students.push(format!("{first} {last}").trim().to_string());
        }
        students.push(RowStudent {
            in_class: found.is_some_and(|id| roster.iter().any(|s| s.id == id)),
            existing: found,
            first,
            last,
            number,
        });
        data_rows.push(ri);
    }
    if students.is_empty() {
        return Err(bad("No student rows were found under the header row."));
    }

    // Assessment columns.
    let existing = model::assessments(c, subject_id)?;
    let mut cols = Vec::new();
    for (i, h) in headers.iter().enumerate() {
        let r = role(i);
        if !matches!(r, Role::Assessment | Role::Completion) {
            continue;
        }
        let spec = parse_header(h, r);
        if spec.name.is_empty() {
            continue;
        }
        let found = existing.iter().find(|a| name_key(&a.name) == name_key(&spec.name));
        let kind = match found {
            // Levels stay levels.
            Some(a) if Kind::parse(&a.kind) == Kind::Level => Kind::Level,
            _ if r == Role::Completion => Kind::Completion,
            _ => Kind::Scored,
        };
        let values: Vec<f64> = data_rows
            .iter()
            .filter_map(|&ri| match grid[ri].get(i) {
                Some(Cell::Num(n)) => Some(*n),
                _ => None,
            })
            .collect();
        // Work out what the numbers mean.
        let (out_of, scale) = match (found, spec.out_of, kind) {
            (_, _, Kind::Completion) => (1.0, 1.0),
            (_, _, Kind::Level) => (engine::MAX_LEVEL, 1.0),
            (Some(a), Some(o), _) => (a.out_of, a.out_of / o),
            (Some(a), None, _) if values.iter().all(|v| *v <= 1.0) && a.out_of > 1.0 && !values.is_empty() => (a.out_of, a.out_of),
            (Some(a), None, _) => (a.out_of, 1.0),
            (None, Some(o), _) => (o, 1.0),
            // The original app's format: decimals 0–1 meaning a percentage.
            (None, None, _) if !values.is_empty() && values.iter().all(|v| *v <= 1.0) => (100.0, 100.0),
            (None, None, _) => (100.0, 1.0),
        };
        match found {
            Some(_) => plan.existing_assessments += 1,
            None => plan.new_assessments.push(spec.name.clone()),
        }
        cols.push(Col { idx: i, existing: found.map(|a| a.id), spec, kind, scale, out_of });
    }
    if cols.is_empty() {
        plan.warnings.push("No assessment columns are selected, so no marks will be imported.".into());
    }

    // Cells.
    let current = model::marks_for_subject(c, subject_id)?;
    let mut cells = Vec::new();
    let mut bad_cells = Vec::new();
    for (si, &ri) in data_rows.iter().enumerate() {
        for (ci, col) in cols.iter().enumerate() {
            let cell = grid[ri].get(col.idx).cloned().unwrap_or(Cell::Empty);
            let mark = match (&cell, col.kind) {
                (c, Kind::Completion) if c.is_empty() => {
                    if opts.blank_as == "missing" {
                        Some(MarkEntry::status(Status::Missing))
                    } else {
                        None
                    }
                }
                (c, Kind::Completion) => {
                    let t = c.text().to_lowercase();
                    if matches!(t.as_str(), "0" | "n" | "no" | "false" | "m" | "missing" | "x") {
                        Some(MarkEntry::status(Status::Missing))
                    } else {
                        Some(MarkEntry::marked(1.0))
                    }
                }
                (c, Kind::Level) if !c.is_empty() => match engine::parse_input(Kind::Level, &c.text(), col.out_of) {
                    Ok(CellInput::Set(m)) => Some(m),
                    Ok(CellInput::Clear) => None,
                    Err(_) => {
                        bad_cells.push(format!("{} / {}: \"{}\"", students[si].first, col.spec.name, c.text()));
                        None
                    }
                },
                (c, _) if c.is_empty() => {
                    if opts.blank_as == "missing" {
                        Some(MarkEntry::status(Status::Missing))
                    } else {
                        None
                    }
                }
                (Cell::Num(n), _) if *n == 0.0 && opts.zero_as == "missing" => Some(MarkEntry::status(Status::Missing)),
                (Cell::Num(n), _) => {
                    let v = (n * col.scale * 1000.0).round() / 1000.0;
                    if v < 0.0 || v > col.out_of * 2.0 {
                        bad_cells.push(format!("{} / {}: {}", students[si].first, col.spec.name, cell.text()));
                        None
                    } else {
                        Some(MarkEntry::marked(v))
                    }
                }
                (Cell::Text(t), _) => match engine::parse_cell(t, col.out_of) {
                    Ok(CellInput::Set(m)) => {
                        if m.value == Some(0.0) && opts.zero_as == "missing" {
                            Some(MarkEntry::status(Status::Missing))
                        } else {
                            Some(m)
                        }
                    }
                    Ok(CellInput::Clear) => None,
                    Err(_) => {
                        bad_cells.push(format!("{} / {}: \"{}\"", students[si].first, col.spec.name, t));
                        None
                    }
                },
                (Cell::Empty, _) => None,
            };
            if let Some(m) = mark {
                let old = students[si].existing.zip(col.existing).and_then(|(s, a)| current.get(&s).and_then(|x| x.get(&a)));
                if old == Some(&m) {
                    plan.unchanged_marks += 1;
                } else {
                    plan.changed_marks += 1;
                }
                cells.push((si, ci, m));
            }
        }
    }
    if !bad_cells.is_empty() {
        let n = bad_cells.len();
        let sample = bad_cells.into_iter().take(5).collect::<Vec<_>>().join("; ");
        plan.warnings.push(format!("{n} cell(s) couldn't be read and will be skipped: {sample}{}", if n > 5 { "; …" } else { "" }));
    }

    let mut finals = Vec::new();
    if let (Some(fc), true) = (final_col, opts.final_as_override) {
        for (si, &ri) in data_rows.iter().enumerate() {
            if let Some(p) = grid[ri].get(fc).and_then(parse_final) {
                finals.push((si, p));
            }
        }
        plan.overrides = finals.len();
    }

    let dup_names = {
        let mut seen = HashMap::new();
        for s in &students {
            *seen.entry(name_key(&format!("{} {}", s.first, s.last))).or_insert(0) += 1;
        }
        seen.values().filter(|n| **n > 1).count()
    };
    if dup_names > 0 && num_col.is_none() {
        plan.warnings.push("Some students share a name. Add a student number column so they're matched correctly next time.".into());
    }

    Ok(Prepared { students, cols, cells, finals, plan })
}

pub fn preview(c: &Connection, user_id: i64, class_id: i64, subject_id: i64, grid: &Grid, opts: &ImportOptions) -> AppResult<Plan> {
    Ok(prepare(c, user_id, class_id, subject_id, grid, opts)?.plan)
}

pub fn apply(tx: &Transaction, user_id: i64, class_id: i64, subject_id: i64, grid: &Grid, opts: &ImportOptions) -> AppResult<Plan> {
    let p = prepare(tx, user_id, class_id, subject_id, grid, opts)?;

    let mut student_ids = Vec::with_capacity(p.students.len());
    for s in &p.students {
        let id = match s.existing {
            Some(id) => {
                if !s.number.is_empty() {
                    tx.execute("UPDATE students SET student_number = ?1 WHERE id = ?2 AND student_number = ''", params![s.number, id])?;
                }
                id
            }
            None => {
                tx.execute(
                    "INSERT INTO students (user_id, first_name, last_name, student_number) VALUES (?1, ?2, ?3, ?4)",
                    params![user_id, s.first, s.last, s.number],
                )?;
                tx.last_insert_rowid()
            }
        };
        if !s.in_class {
            tx.execute(
                "INSERT INTO enrolments (class_id, student_id, active) VALUES (?1, ?2, 1)
                 ON CONFLICT(class_id, student_id) DO UPDATE SET active = 1",
                params![class_id, id],
            )?;
        }
        student_ids.push(id);
    }

    let mut sort: i64 = tx
        .query_row("SELECT COALESCE(MAX(sort), 0) FROM assessments WHERE subject_id = ?1", [subject_id], |r| r.get(0))
        .optional()?
        .unwrap_or(0);
    let default_cat: Option<i64> =
        tx.query_row("SELECT id FROM categories WHERE subject_id = ?1 ORDER BY sort, id LIMIT 1", [subject_id], |r| r.get(0)).optional()?;
    let mut col_ids = Vec::with_capacity(p.cols.len());
    for col in &p.cols {
        let id = match col.existing {
            Some(id) => id,
            None => {
                sort += 1;
                tx.execute(
                    "INSERT INTO assessments (subject_id, category_id, name, unit, out_of, kind, sort) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        subject_id,
                        if col.kind == Kind::Scored { default_cat } else { None },
                        col.spec.name,
                        col.spec.unit,
                        col.out_of,
                        col.kind.as_str(),
                        sort
                    ],
                )?;
                tx.last_insert_rowid()
            }
        };
        col_ids.push(id);
    }

    let changes: Vec<Change> =
        p.cells.iter().map(|&(si, ci, m)| Change { assessment_id: col_ids[ci], student_id: student_ids[si], new: Some(m) }).collect();
    marks::apply(tx, class_id, &changes, "import")?;

    for &(si, pct) in &p.finals {
        tx.execute(
            "INSERT INTO grade_overrides (student_id, subject_id, pct, reason) VALUES (?1, ?2, ?3, 'Imported from the spreadsheet''s final mark column')
             ON CONFLICT(student_id, subject_id) DO UPDATE SET pct = excluded.pct, reason = excluded.reason",
            params![student_ids[si], subject_id, pct],
        )?;
    }
    Ok(p.plan)
}

/// Default roles for a freshly uploaded grid.
pub fn guess_roles(grid: &Grid) -> Vec<Role> {
    let hdr = header_row(grid);
    let headers: Vec<String> = grid.get(hdr).map(|r| r.iter().map(Cell::text).collect()).unwrap_or_default();
    let mut roles: Vec<Role> = headers.iter().map(|h| guess_role(h)).collect();
    // Only one column per identity role.
    for r in [Role::StudentNumber, Role::FirstName, Role::LastName, Role::FullName, Role::FinalMark] {
        let mut seen = false;
        for x in roles.iter_mut().filter(|x| **x == r) {
            if seen {
                *x = Role::Ignore;
            }
            seen = true;
        }
    }
    // Columns with no numbers at all aren't marks (e.g. "Notes").
    for (i, r) in roles.iter_mut().enumerate() {
        if *r == Role::Assessment {
            let any_mark = grid.iter().skip(hdr + 1).any(|row| match row.get(i) {
                Some(Cell::Num(_)) => true,
                Some(Cell::Text(t)) => !t.trim().is_empty() && engine::parse_cell(t, 100.0).is_ok(),
                _ => false,
            });
            let all_empty = grid.iter().skip(hdr + 1).all(|row| row.get(i).is_none_or(Cell::is_empty));
            if !any_mark && !all_empty {
                *r = Role::Ignore;
            }
        }
    }
    roles
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Cell {
        Cell::Text(s.into())
    }
    fn n(v: f64) -> Cell {
        Cell::Num(v)
    }

    fn legacy_grid() -> Grid {
        vec![
            vec![
                t("StudentID"),
                t("Last Name"),
                t("First Name"),
                t("Mark"),
                t("1.1 Discussion"),
                t("1.2 Poster"),
                t("Final Exam"),
                t("J1.1"),
            ],
            vec![n(1.0), t("Smith"), t("John"), n(0.82), n(0.9), n(0.0), n(0.7), t("x")],
            vec![n(2.0), t("Lefèvre"), t("Zoë"), n(0.91), t("?%"), Cell::Empty, n(0.95), Cell::Empty],
        ]
    }

    #[test]
    fn header_parsing() {
        assert_eq!(
            parse_header("1.2 Quiz (/20)", Role::Assessment),
            ColumnSpec { name: "1.2 Quiz".into(), unit: "Module 1".into(), out_of: Some(20.0) }
        );
        assert_eq!(parse_header("J2.1", Role::Completion).unit, "Journals");
        assert_eq!(
            parse_header("Essay [Unit 3] (/40)", Role::Assessment),
            ColumnSpec { name: "Essay".into(), unit: "Unit 3".into(), out_of: Some(40.0) }
        );
        assert_eq!(parse_header("Final Exam", Role::Assessment), ColumnSpec { name: "Final Exam".into(), unit: "".into(), out_of: None });
        assert_eq!(parse_header("1.22 Writeup on the   dichotomy", Role::Assessment).name, "1.22 Writeup on the dichotomy");
    }

    #[test]
    fn roles_detected_for_legacy_layout() {
        let roles = guess_roles(&legacy_grid());
        assert_eq!(
            roles,
            vec![
                Role::StudentNumber,
                Role::LastName,
                Role::FirstName,
                Role::FinalMark,
                Role::Assessment,
                Role::Assessment,
                Role::Assessment,
                Role::Completion
            ]
        );
    }

    #[test]
    fn csv_parsing() {
        let g = parse_csv("\u{feff}Name,\"Quiz, part 1\"\r\n\"O'Brien, Pat\",17\r\nSam Lee,\n");
        assert_eq!(g[0], vec![t("Name"), t("Quiz, part 1")]);
        assert_eq!(g[1], vec![t("O'Brien, Pat"), n(17.0)]);
        assert_eq!(g[2], vec![t("Sam Lee"), Cell::Empty]);
        assert_eq!(split_full_name("O'Brien, Pat"), ("Pat".into(), "O'Brien".into()));
    }

    fn setup_db() -> (Connection, i64, i64, i64) {
        let path = std::env::temp_dir().join(format!("gb-import-{}.db", crate::util::random_token(6)));
        crate::db::Db::new(path.clone()).migrate().unwrap();
        let c = crate::db::open(&path).unwrap();
        c.execute("INSERT INTO users (username, password_hash) VALUES ('t', 'x')", []).unwrap();
        c.execute("INSERT INTO terms (user_id, name) VALUES (1, 'T1')", []).unwrap();
        c.execute("INSERT INTO classes (user_id, term_id, name) VALUES (1, 1, 'Science')", []).unwrap();
        c.execute("INSERT INTO subjects (class_id, name) VALUES (1, 'Science')", []).unwrap();
        (c, 1, 1, 1)
    }

    fn opts(grid: &Grid) -> ImportOptions {
        ImportOptions { roles: guess_roles(grid), blank_as: "missing".into(), zero_as: "missing".into(), final_as_override: false }
    }

    #[test]
    fn legacy_import_then_update() {
        let (mut c, user, class, subject) = setup_db();
        let grid = legacy_grid();
        let o = opts(&grid);

        let plan = preview(&c, user, class, subject, &grid, &o).unwrap();
        assert_eq!(plan.new_students.len(), 2);
        assert_eq!(plan.new_assessments, vec!["1.1 Discussion", "1.2 Poster", "Final Exam", "J1.1"]);
        assert_eq!(plan.changed_marks, 8);

        let tx = c.transaction().unwrap();
        apply(&tx, user, class, subject, &grid, &o).unwrap();
        tx.commit().unwrap();

        let asm = model::assessments(&c, subject).unwrap();
        assert_eq!(asm[0].unit, "Module 1");
        assert_eq!(asm[2].unit, "");
        assert_eq!(asm[3].kind, "completion");
        let roster = model::roster(&c, class).unwrap();
        let zoe = roster.iter().find(|s| s.first_name == "Zoë").unwrap();
        let john = roster.iter().find(|s| s.first_name == "John").unwrap();
        let marks = model::marks_for_subject(&c, subject).unwrap();
        assert_eq!(marks[&john.id][&asm[0].id], MarkEntry::marked(90.0));
        // 0 in the original format meant "not submitted".
        assert_eq!(marks[&john.id][&asm[1].id].status, Status::Missing);
        assert_eq!(marks[&zoe.id][&asm[0].id].status, Status::Pending);
        assert_eq!(marks[&zoe.id][&asm[3].id].status, Status::Missing);

        // Second upload: one new mark, one new column, one new student → only those change.
        let mut g2 = legacy_grid();
        g2[0].push(t("2.1 Lab"));
        g2[1].push(n(0.5));
        g2[2].push(n(0.6));
        g2[2][5] = n(0.8);
        g2.push(vec![n(3.0), t("Parker"), t("Peter"), Cell::Empty, n(0.7), n(0.7), n(0.7), t("x"), n(0.7)]);
        let o2 = opts(&g2);
        let plan = preview(&c, user, class, subject, &g2, &o2).unwrap();
        assert_eq!(plan.matched_students, 2);
        assert_eq!(plan.new_students, vec!["Peter Parker"]);
        assert_eq!(plan.new_assessments, vec!["2.1 Lab"]);
        assert_eq!(plan.changed_marks, 2 + 1 + 5);
        let tx = c.transaction().unwrap();
        apply(&tx, user, class, subject, &g2, &o2).unwrap();
        tx.commit().unwrap();
        assert_eq!(model::roster(&c, class).unwrap().len(), 3);

        // Re-importing the same file changes nothing.
        let plan = preview(&c, user, class, subject, &g2, &o2).unwrap();
        assert_eq!(plan.changed_marks, 0);
        assert!(plan.new_students.is_empty() && plan.new_assessments.is_empty());
    }

    #[test]
    fn final_mark_override() {
        let (mut c, user, class, subject) = setup_db();
        let grid = legacy_grid();
        let mut o = opts(&grid);
        o.final_as_override = true;
        let tx = c.transaction().unwrap();
        let plan = apply(&tx, user, class, subject, &grid, &o).unwrap();
        tx.commit().unwrap();
        assert_eq!(plan.overrides, 2);
        let ov = model::overrides_for_subject(&c, subject).unwrap();
        assert!(ov.values().any(|(p, _)| (*p - 82.0).abs() < 1e-9));
    }

    #[test]
    fn points_columns_and_text_cells() {
        let (c, user, class, subject) = setup_db();
        let grid = vec![
            vec![t("Name"), t("Quiz (/20)"), t("Notes")],
            vec![t("Ann Lee"), n(17.0), t("great")],
            vec![t("Bo Chan"), t("15L"), t("")],
            vec![t("Cy Diaz"), t("banana"), t("")],
        ];
        let o = opts(&grid);
        assert_eq!(o.roles, vec![Role::FullName, Role::Assessment, Role::Ignore]);
        let plan = preview(&c, user, class, subject, &grid, &o).unwrap();
        assert_eq!(plan.new_students.len(), 3);
        assert_eq!(plan.changed_marks, 2);
        assert_eq!(plan.warnings.len(), 1);
    }
}
