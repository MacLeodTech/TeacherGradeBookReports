//! Grade calculation engine.
//!
//! Pure functions only (no database, no HTTP), so every rule here is covered by
//! unit tests. The server is the single source of truth for these numbers: the
//! grid, the student page and the PDF all call [`compute`].
//!
//! All percentages are on a 0–100 scale.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ── Mark status ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// A value was entered.
    Marked,
    /// A value was entered, flagged late. Counts like `Marked`.
    Late,
    /// Not handed in. Counts as 0 or is ignored, depending on [`MissingPolicy`].
    Missing,
    /// Exempt. Never counts.
    Excused,
    /// Handed in, not marked yet (the old `?`). Never counts.
    Pending,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Marked => "marked",
            Status::Late => "late",
            Status::Missing => "missing",
            Status::Excused => "excused",
            Status::Pending => "pending",
        }
    }

    pub fn parse(s: &str) -> Option<Status> {
        Some(match s {
            "marked" => Status::Marked,
            "late" => Status::Late,
            "missing" => Status::Missing,
            "excused" => Status::Excused,
            "pending" => Status::Pending,
            _ => return None,
        })
    }

    /// Short code shown in the grid and written to exports.
    pub fn code(self) -> &'static str {
        match self {
            Status::Marked => "",
            Status::Late => "L",
            Status::Missing => "M",
            Status::Excused => "X",
            Status::Pending => "?",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarkEntry {
    /// Points, on the assessment's `out_of` scale.
    pub value: Option<f64>,
    pub status: Status,
}

impl MarkEntry {
    pub fn marked(value: f64) -> Self {
        MarkEntry { value: Some(value), status: Status::Marked }
    }
    pub fn status(status: Status) -> Self {
        MarkEntry { value: None, status }
    }
}

// ── Cell input parsing ───────────────────────────────────────────────────────

/// What a teacher typed into a grid cell, after parsing.
#[derive(Debug, PartialEq)]
pub enum CellInput {
    /// Empty cell: remove the mark (not assessed yet).
    Clear,
    Set(MarkEntry),
}

/// Parse grid input such as `85`, `85%`, `17/20`, `17/20L`, `M`, `X`, `?`.
///
/// `out_of` is the assessment's maximum. `17/20` is rescaled onto `out_of`, so
/// it works on a column marked out of 100 too.
pub fn parse_cell(input: &str, out_of: f64) -> Result<CellInput, String> {
    let s = input.trim();
    if s.is_empty() {
        return Ok(CellInput::Clear);
    }
    match s.to_ascii_uppercase().as_str() {
        "M" | "MISSING" | "NS" => return Ok(CellInput::Set(MarkEntry::status(Status::Missing))),
        "X" | "EX" | "EXC" | "EXEMPT" | "EXCUSED" => {
            return Ok(CellInput::Set(MarkEntry::status(Status::Excused)));
        }
        "?" | "?%" | "P" | "PENDING" => return Ok(CellInput::Set(MarkEntry::status(Status::Pending))),
        "L" | "LATE" => {
            return Err("Enter a late mark as the mark followed by L, e.g. 85L or 17/20L".into());
        }
        _ => {}
    }

    let (body, late) = match s.strip_suffix(['L', 'l']) {
        Some(rest) => (rest.trim(), true),
        None => (s, false),
    };

    let value = if let Some((num, den)) = body.split_once('/') {
        let n = parse_number(num)?;
        let d = parse_number(den)?;
        if d <= 0.0 {
            return Err("The number after / must be greater than 0".into());
        }
        n / d * out_of
    } else if let Some(pct) = body.strip_suffix('%') {
        parse_number(pct)? / 100.0 * out_of
    } else {
        parse_number(body)?
    };

    if value < 0.0 {
        return Err("Marks can't be negative".into());
    }
    if value > out_of * 2.0 {
        return Err(format!("{} is more than double the maximum ({})", fmt_num(value), fmt_num(out_of)));
    }
    let value = (value * 1000.0).round() / 1000.0;
    Ok(CellInput::Set(MarkEntry { value: Some(value), status: if late { Status::Late } else { Status::Marked } }))
}

/// Parse a cell for an assessment of the given kind. Done / not-done columns
/// take Y, N, X, ? or blank.
pub fn parse_input(kind: Kind, input: &str, out_of: f64) -> Result<CellInput, String> {
    if kind == Kind::Completion {
        let t = input.trim().to_lowercase();
        return match t.as_str() {
            "" => Ok(CellInput::Clear),
            "y" | "yes" | "1" | "done" | "d" | "✓" | "✔" | "true" => Ok(CellInput::Set(MarkEntry::marked(1.0))),
            "n" | "no" | "0" | "m" | "missing" | "false" => Ok(CellInput::Set(MarkEntry::status(Status::Missing))),
            "x" | "ex" | "excused" => Ok(CellInput::Set(MarkEntry::status(Status::Excused))),
            "?" => Ok(CellInput::Set(MarkEntry::status(Status::Pending))),
            _ => Err("For done/not-done columns use Y (done), N (not done), X (excused) or leave blank.".into()),
        };
    }
    parse_cell(input, out_of)
}

fn parse_number(s: &str) -> Result<f64, String> {
    let s = s.trim();
    s.parse::<f64>().ok().filter(|v| v.is_finite()).ok_or_else(|| format!("\"{s}\" isn't a number. Use a mark like 85, 17/20, or M, X, ?"))
}

pub fn fmt_num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        let s = format!("{v:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Text shown for a mark in the grid / export (inverse of [`parse_cell`]).
pub fn display_cell(m: &MarkEntry) -> String {
    match (m.value, m.status) {
        (Some(v), Status::Late) => format!("{}L", fmt_num(v)),
        (Some(v), _) => fmt_num(v),
        (None, s) => s.code().to_string(),
    }
}

// ── Grade scale ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Band {
    /// Lowest percentage (inclusive) that earns this band.
    pub min: f64,
    pub label: String,
    /// CSS hex colour, e.g. `#1A7340`.
    pub color: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Scale {
    /// Sorted highest `min` first.
    pub bands: Vec<Band>,
}

pub const GREEN: &str = "#1A7340";
pub const YELLOW: &str = "#9A7000";
pub const ORANGE: &str = "#C85A00";
pub const RED: &str = "#B22222";

impl Scale {
    pub fn new(mut bands: Vec<Band>) -> Self {
        bands.sort_by(|a, b| b.min.total_cmp(&a.min));
        Scale { bands }
    }

    /// Ontario letter-grade conversion (Growing Success). Below 50 is R.
    pub fn ontario_letters() -> Self {
        let b = |min: f64, label: &str, color: &str| Band { min, label: label.into(), color: color.into() };
        Scale::new(vec![
            b(90.0, "A+", GREEN),
            b(85.0, "A", GREEN),
            b(80.0, "A-", GREEN),
            b(77.0, "B+", GREEN),
            b(73.0, "B", GREEN),
            b(70.0, "B-", GREEN),
            b(67.0, "C+", YELLOW),
            b(63.0, "C", YELLOW),
            b(60.0, "C-", YELLOW),
            b(57.0, "D+", ORANGE),
            b(53.0, "D", ORANGE),
            b(50.0, "D-", ORANGE),
            b(0.0, "R", RED),
        ])
    }

    /// Ontario achievement levels.
    pub fn ontario_levels() -> Self {
        let b = |min: f64, label: &str, color: &str| Band { min, label: label.into(), color: color.into() };
        Scale::new(vec![
            b(80.0, "Level 4", GREEN),
            b(70.0, "Level 3", GREEN),
            b(60.0, "Level 2", YELLOW),
            b(50.0, "Level 1", ORANGE),
            b(0.0, "R", RED),
        ])
    }

    /// A common university-style scale.
    pub fn standard_letters() -> Self {
        let b = |min: f64, label: &str, color: &str| Band { min, label: label.into(), color: color.into() };
        Scale::new(vec![
            b(90.0, "A+", GREEN),
            b(85.0, "A", GREEN),
            b(80.0, "A-", GREEN),
            b(77.0, "B+", GREEN),
            b(73.0, "B", GREEN),
            b(70.0, "B-", GREEN),
            b(67.0, "C+", YELLOW),
            b(63.0, "C", YELLOW),
            b(60.0, "C-", YELLOW),
            b(50.0, "D", ORANGE),
            b(0.0, "F", RED),
        ])
    }

    pub fn band(&self, pct: f64) -> Option<&Band> {
        self.bands.iter().find(|b| pct + 1e-9 >= b.min).or(self.bands.last())
    }

    pub fn label(&self, pct: f64) -> String {
        self.band(pct).map(|b| b.label.clone()).unwrap_or_default()
    }

    pub fn color(&self, pct: f64) -> String {
        self.band(pct).map(|b| b.color.clone()).unwrap_or_else(|| "#333333".into())
    }

    /// Legend rows: (label, "lo–hi%", colour), highest first.
    pub fn legend(&self) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        let mut upper: Option<f64> = None;
        for b in &self.bands {
            let range = match upper {
                None => format!("{}–100%", fmt_num(b.min)),
                Some(u) if b.min <= 0.0 => format!("< {}%", fmt_num(u)),
                Some(u) => format!("{}–{}%", fmt_num(b.min), fmt_num(u - 1.0)),
            };
            out.push((b.label.clone(), range, b.color.clone()));
            upper = Some(b.min);
        }
        out
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.bands.is_empty() {
            return Err("The grade scale needs at least one band.".into());
        }
        if self.bands.iter().any(|b| b.label.trim().is_empty()) {
            return Err("Every band needs a label.".into());
        }
        if self.bands.iter().any(|b| !(0.0..=100.0).contains(&b.min)) {
            return Err("Band minimums must be between 0 and 100.".into());
        }
        if self.bands.last().map(|b| b.min) != Some(0.0) {
            return Err("The lowest band must start at 0.".into());
        }
        for w in self.bands.windows(2) {
            if (w[0].min - w[1].min).abs() < 1e-9 {
                return Err(format!("Two bands start at {}%.", fmt_num(w[0].min)));
            }
        }
        if self.bands.iter().any(|b| !is_hex_color(&b.color)) {
            return Err("Colours must look like #1A7340.".into());
        }
        Ok(())
    }
}

pub fn is_hex_color(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

// ── Subject setup ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MissingPolicy {
    /// Missing work is left out of the average until it's marked.
    #[default]
    Ignore,
    /// Missing work counts as 0.
    Zero,
}

impl MissingPolicy {
    pub fn parse(s: &str) -> MissingPolicy {
        if s == "zero" { MissingPolicy::Zero } else { MissingPolicy::Ignore }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            MissingPolicy::Ignore => "ignore",
            MissingPolicy::Zero => "zero",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A mark out of `out_of`.
    Scored,
    /// Done / not done (e.g. journals). Never affects the grade.
    Completion,
}

impl Kind {
    pub fn parse(s: &str) -> Kind {
        if s == "completion" { Kind::Completion } else { Kind::Scored }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Scored => "scored",
            Kind::Completion => "completion",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CategoryIn {
    pub id: i64,
    pub name: String,
    /// Relative weight; usually percentages that sum to 100.
    pub weight: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct AssessmentIn {
    pub id: i64,
    pub name: String,
    pub category_id: Option<i64>,
    /// Grouping label, e.g. "Unit 1". Empty = ungrouped.
    pub unit: String,
    pub out_of: f64,
    /// Weight inside its category. 0 = formative (recorded, not counted).
    pub weight: f64,
    pub kind: Kind,
}

#[derive(Clone, Debug)]
pub struct Setup {
    pub categories: Vec<CategoryIn>,
    pub assessments: Vec<AssessmentIn>,
    pub policy: MissingPolicy,
}

// ── Results ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct CategoryResult {
    pub id: Option<i64>,
    pub name: String,
    pub weight: f64,
    pub avg: Option<f64>,
    pub counted: usize,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct UnitResult {
    pub name: String,
    pub avg: Option<f64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Default)]
pub struct StudentResult {
    /// The grade to report: the override if set, else `calculated`.
    pub overall: Option<f64>,
    pub calculated: Option<f64>,
    pub overridden: bool,
    pub categories: Vec<CategoryResult>,
    pub units: Vec<UnitResult>,
    /// Scored assessments (excluding excused).
    pub expected: usize,
    /// Marked, late, or pending.
    pub submitted: usize,
    pub missing: usize,
    pub pending: usize,
    pub excused: usize,
    pub completion_done: usize,
    pub completion_total: usize,
    pub warnings: Vec<String>,
}

/// Percentage for one mark, if it has a value that counts.
pub fn mark_pct(a: &AssessmentIn, m: &MarkEntry) -> Option<f64> {
    match m.status {
        Status::Marked | Status::Late => m.value.map(|v| if a.out_of > 0.0 { v / a.out_of * 100.0 } else { 0.0 }),
        _ => None,
    }
}

/// Percentage that counts toward an average under the missing-work policy.
fn counted_pct(a: &AssessmentIn, m: Option<&MarkEntry>, policy: MissingPolicy) -> Option<f64> {
    let m = m?;
    match m.status {
        Status::Missing if policy == MissingPolicy::Zero => Some(0.0),
        _ => mark_pct(a, m),
    }
}

fn weighted_mean<'a>(
    items: impl Iterator<Item = &'a AssessmentIn>,
    marks: &HashMap<i64, MarkEntry>,
    policy: MissingPolicy,
) -> (Option<f64>, usize) {
    let (mut sum, mut wsum, mut n) = (0.0, 0.0, 0usize);
    for a in items.filter(|a| a.kind == Kind::Scored && a.weight > 0.0) {
        if let Some(p) = counted_pct(a, marks.get(&a.id), policy) {
            sum += p * a.weight;
            wsum += a.weight;
            n += 1;
        }
    }
    if wsum > 0.0 { (Some(sum / wsum), n) } else { (None, 0) }
}

/// Compute one student's results for one subject.
///
/// `marks` maps assessment id → mark. `override_pct` is the teacher's manual
/// final grade, if any.
pub fn compute(setup: &Setup, marks: &HashMap<i64, MarkEntry>, override_pct: Option<f64>) -> StudentResult {
    let mut r = StudentResult::default();
    let scored: Vec<&AssessmentIn> = setup.assessments.iter().filter(|a| a.kind == Kind::Scored).collect();

    // Counts.
    for a in &scored {
        match marks.get(&a.id) {
            Some(m) => match m.status {
                Status::Excused => r.excused += 1,
                Status::Missing => {
                    r.missing += 1;
                    r.expected += 1;
                }
                Status::Pending => {
                    r.pending += 1;
                    r.submitted += 1;
                    r.expected += 1;
                }
                Status::Marked | Status::Late => {
                    r.submitted += 1;
                    r.expected += 1;
                }
            },
            None => r.expected += 1,
        }
    }
    for a in setup.assessments.iter().filter(|a| a.kind == Kind::Completion) {
        r.completion_total += 1;
        if let Some(m) = marks.get(&a.id)
            && (m.value.is_some() || matches!(m.status, Status::Marked | Status::Late | Status::Pending))
        {
            r.completion_done += 1;
        }
    }

    // Category / overall.
    if setup.categories.is_empty() {
        let (avg, _) = weighted_mean(scored.iter().copied(), marks, setup.policy);
        r.calculated = avg;
    } else {
        let known: std::collections::HashSet<i64> = setup.categories.iter().map(|c| c.id).collect();
        let orphaned = scored.iter().filter(|a| a.category_id.is_none_or(|c| !known.contains(&c)) && a.weight > 0.0).count();
        if orphaned > 0 {
            r.warnings.push(format!("{orphaned} assessment(s) have no category and don't count toward the grade."));
        }
        for c in &setup.categories {
            let (avg, n) = weighted_mean(scored.iter().copied().filter(|a| a.category_id == Some(c.id)), marks, setup.policy);
            r.categories.push(CategoryResult { id: Some(c.id), name: c.name.clone(), weight: c.weight, avg, counted: n });
        }
        let with_marks: Vec<&CategoryResult> = r.categories.iter().filter(|c| c.avg.is_some()).collect();
        let wsum: f64 = with_marks.iter().map(|c| c.weight.max(0.0)).sum();
        r.calculated = if with_marks.is_empty() {
            None
        } else if wsum > 0.0 {
            Some(with_marks.iter().map(|c| c.avg.unwrap() * c.weight.max(0.0)).sum::<f64>() / wsum)
        } else {
            r.warnings.push("All category weights are 0, so categories are weighted equally.".into());
            Some(with_marks.iter().map(|c| c.avg.unwrap()).sum::<f64>() / with_marks.len() as f64)
        };
        let total_weight: f64 = setup.categories.iter().map(|c| c.weight).sum();
        if total_weight > 0.0 && (total_weight - 100.0).abs() > 0.01 {
            r.warnings.push(format!("Category weights add up to {}%, not 100%. They are scaled proportionally.", fmt_num(total_weight)));
        }
    }

    // Units, in first-appearance order.
    let mut unit_names: Vec<&str> = Vec::new();
    for a in &scored {
        if !unit_names.contains(&a.unit.as_str()) {
            unit_names.push(&a.unit);
        }
    }
    for u in unit_names {
        let (avg, _) = weighted_mean(scored.iter().copied().filter(|a| a.unit == u), marks, setup.policy);
        r.units.push(UnitResult { name: u.to_string(), avg });
    }

    r.overall = r.calculated;
    if let Some(o) = override_pct {
        r.overall = Some(o);
        r.overridden = true;
    }
    r
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn a(id: i64, cat: Option<i64>, out_of: f64) -> AssessmentIn {
        AssessmentIn { id, name: format!("A{id}"), category_id: cat, unit: String::new(), out_of, weight: 1.0, kind: Kind::Scored }
    }

    fn approx(x: Option<f64>, y: f64) {
        let x = x.expect("expected a value");
        assert!((x - y).abs() < 1e-6, "{x} != {y}");
    }

    #[test]
    fn parse_cell_variants() {
        assert_eq!(parse_cell("", 100.0), Ok(CellInput::Clear));
        assert_eq!(parse_cell("85", 100.0), Ok(CellInput::Set(MarkEntry::marked(85.0))));
        assert_eq!(parse_cell(" 17/20 ", 20.0), Ok(CellInput::Set(MarkEntry::marked(17.0))));
        assert_eq!(parse_cell("17/20", 100.0), Ok(CellInput::Set(MarkEntry::marked(85.0))));
        assert_eq!(parse_cell("85%", 20.0), Ok(CellInput::Set(MarkEntry::marked(17.0))));
        assert_eq!(parse_cell("17/20L", 20.0), Ok(CellInput::Set(MarkEntry { value: Some(17.0), status: Status::Late })));
        assert_eq!(parse_cell("m", 10.0), Ok(CellInput::Set(MarkEntry::status(Status::Missing))));
        assert_eq!(parse_cell("X", 10.0), Ok(CellInput::Set(MarkEntry::status(Status::Excused))));
        assert_eq!(parse_cell("?", 10.0), Ok(CellInput::Set(MarkEntry::status(Status::Pending))));
        assert_eq!(parse_cell("0", 10.0), Ok(CellInput::Set(MarkEntry::marked(0.0))));
        assert!(parse_cell("abc", 10.0).is_err());
        assert!(parse_cell("-1", 10.0).is_err());
        assert!(parse_cell("5/0", 10.0).is_err());
        assert!(parse_cell("L", 10.0).is_err());
        assert!(parse_cell("25", 10.0).is_err());
    }

    #[test]
    fn display_roundtrip() {
        for (s, out_of) in [("85", 100.0), ("17.5", 20.0), ("12L", 20.0), ("M", 5.0), ("X", 5.0), ("?", 5.0)] {
            let CellInput::Set(m) = parse_cell(s, out_of).unwrap() else { panic!() };
            assert_eq!(display_cell(&m), s);
        }
    }

    #[test]
    fn real_zero_is_not_missing() {
        let setup = Setup { categories: vec![], assessments: vec![a(1, None, 10.0), a(2, None, 10.0)], policy: MissingPolicy::Ignore };
        let marks = HashMap::from([(1, MarkEntry::marked(0.0)), (2, MarkEntry::marked(10.0))]);
        let r = compute(&setup, &marks, None);
        approx(r.overall, 50.0);
        assert_eq!(r.submitted, 2);
        assert_eq!(r.missing, 0);
    }

    #[test]
    fn statuses_and_missing_policy() {
        let asmts = vec![a(1, None, 10.0), a(2, None, 10.0), a(3, None, 10.0), a(4, None, 10.0), a(5, None, 10.0)];
        let marks = HashMap::from([
            (1, MarkEntry::marked(8.0)),
            (2, MarkEntry::status(Status::Missing)),
            (3, MarkEntry::status(Status::Excused)),
            (4, MarkEntry::status(Status::Pending)),
        ]);
        let ignore = compute(&Setup { categories: vec![], assessments: asmts.clone(), policy: MissingPolicy::Ignore }, &marks, None);
        approx(ignore.overall, 80.0);
        assert_eq!((ignore.submitted, ignore.missing, ignore.pending, ignore.excused, ignore.expected), (2, 1, 1, 1, 4));

        let zero = compute(&Setup { categories: vec![], assessments: asmts, policy: MissingPolicy::Zero }, &marks, None);
        approx(zero.overall, 40.0);
    }

    #[test]
    fn weights_within_and_across_categories() {
        // K/U 70 (two tests, second weighted double), Final 30.
        let mut t2 = a(2, Some(1), 50.0);
        t2.weight = 2.0;
        let setup = Setup {
            categories: vec![
                CategoryIn { id: 1, name: "Term".into(), weight: 70.0 },
                CategoryIn { id: 2, name: "Final".into(), weight: 30.0 },
            ],
            assessments: vec![a(1, Some(1), 100.0), t2, a(3, Some(2), 100.0)],
            policy: MissingPolicy::Ignore,
        };
        // Term: (60*1 + 90*2)/3 = 80. Final: 50. Overall: 80*.7 + 50*.3 = 71.
        let marks = HashMap::from([(1, MarkEntry::marked(60.0)), (2, MarkEntry::marked(45.0)), (3, MarkEntry::marked(50.0))]);
        let r = compute(&setup, &marks, None);
        approx(r.categories[0].avg, 80.0);
        approx(r.categories[1].avg, 50.0);
        approx(r.overall, 71.0);
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn empty_category_is_renormalised() {
        // Before the final exam exists, the term grade is the whole grade.
        let setup = Setup {
            categories: vec![
                CategoryIn { id: 1, name: "Term".into(), weight: 70.0 },
                CategoryIn { id: 2, name: "Final".into(), weight: 30.0 },
            ],
            assessments: vec![a(1, Some(1), 100.0), a(2, Some(2), 100.0)],
            policy: MissingPolicy::Ignore,
        };
        let r = compute(&setup, &HashMap::from([(1, MarkEntry::marked(82.0))]), None);
        approx(r.overall, 82.0);
        assert_eq!(r.categories[1].avg, None);
    }

    #[test]
    fn formative_and_completion_do_not_count() {
        let mut formative = a(2, None, 10.0);
        formative.weight = 0.0;
        let mut journal = a(3, None, 1.0);
        journal.kind = Kind::Completion;
        let setup = Setup { categories: vec![], assessments: vec![a(1, None, 10.0), formative, journal], policy: MissingPolicy::Zero };
        let marks = HashMap::from([(1, MarkEntry::marked(9.0)), (2, MarkEntry::marked(1.0)), (3, MarkEntry::marked(1.0))]);
        let r = compute(&setup, &marks, None);
        approx(r.overall, 90.0);
        assert_eq!((r.completion_done, r.completion_total), (1, 1));
    }

    #[test]
    fn uncategorised_warns_and_is_excluded() {
        let setup = Setup {
            categories: vec![CategoryIn { id: 1, name: "All".into(), weight: 100.0 }],
            assessments: vec![a(1, Some(1), 10.0), a(2, None, 10.0)],
            policy: MissingPolicy::Ignore,
        };
        let marks = HashMap::from([(1, MarkEntry::marked(10.0)), (2, MarkEntry::marked(0.0))]);
        let r = compute(&setup, &marks, None);
        approx(r.overall, 100.0);
        assert_eq!(r.warnings.len(), 1);
    }

    #[test]
    fn weights_not_summing_to_100_warn() {
        let setup = Setup {
            categories: vec![CategoryIn { id: 1, name: "A".into(), weight: 30.0 }, CategoryIn { id: 2, name: "B".into(), weight: 30.0 }],
            assessments: vec![a(1, Some(1), 10.0), a(2, Some(2), 10.0)],
            policy: MissingPolicy::Ignore,
        };
        let marks = HashMap::from([(1, MarkEntry::marked(10.0)), (2, MarkEntry::marked(5.0))]);
        let r = compute(&setup, &marks, None);
        approx(r.overall, 75.0);
        assert_eq!(r.warnings.len(), 1);
    }

    #[test]
    fn override_wins() {
        let setup = Setup { categories: vec![], assessments: vec![a(1, None, 10.0)], policy: MissingPolicy::Ignore };
        let r = compute(&setup, &HashMap::from([(1, MarkEntry::marked(5.0))]), Some(62.0));
        approx(r.calculated, 50.0);
        approx(r.overall, 62.0);
        assert!(r.overridden);
    }

    #[test]
    fn no_marks_means_no_grade() {
        let setup = Setup { categories: vec![], assessments: vec![a(1, None, 10.0)], policy: MissingPolicy::Ignore };
        let r = compute(&setup, &HashMap::new(), None);
        assert_eq!(r.overall, None);
        assert_eq!(r.expected, 1);
    }

    #[test]
    fn units_grouped_in_order() {
        let mut x = a(1, None, 10.0);
        x.unit = "Unit 2".into();
        let mut y = a(2, None, 10.0);
        y.unit = "Unit 1".into();
        let mut z = a(3, None, 10.0);
        z.unit = "Unit 2".into();
        let setup = Setup { categories: vec![], assessments: vec![x, y, z], policy: MissingPolicy::Ignore };
        let marks = HashMap::from([(1, MarkEntry::marked(10.0)), (2, MarkEntry::marked(5.0)), (3, MarkEntry::marked(6.0))]);
        let r = compute(&setup, &marks, None);
        assert_eq!(r.units.iter().map(|u| u.name.as_str()).collect::<Vec<_>>(), ["Unit 2", "Unit 1"]);
        approx(r.units[0].avg, 80.0);
    }

    #[test]
    fn ontario_scale_boundaries() {
        let s = Scale::ontario_letters();
        assert_eq!(s.label(100.0), "A+");
        assert_eq!(s.label(90.0), "A+");
        assert_eq!(s.label(89.99), "A");
        assert_eq!(s.label(85.0), "A");
        assert_eq!(s.label(80.0), "A-");
        assert_eq!(s.label(50.0), "D-");
        assert_eq!(s.label(49.9), "R");
        assert_eq!(s.label(0.0), "R");
        assert!(s.validate().is_ok());
        assert_eq!(s.legend()[0].1, "90–100%");
        assert_eq!(s.legend().last().unwrap().1, "< 50%");
    }

    #[test]
    fn scale_validation() {
        let mut s = Scale::ontario_levels();
        s.bands.pop();
        assert!(s.validate().is_err());
        assert!(Scale::new(vec![]).validate().is_err());
    }
}
