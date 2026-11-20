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

/// Blank, or a status code (M, X, ?) rather than a mark.
fn status_code(s: &str) -> Option<Result<CellInput, String>> {
    Some(Ok(match s.to_ascii_uppercase().as_str() {
        "" => CellInput::Clear,
        "M" | "MISSING" | "NS" => CellInput::Set(MarkEntry::status(Status::Missing)),
        "X" | "EX" | "EXC" | "EXEMPT" | "EXCUSED" => CellInput::Set(MarkEntry::status(Status::Excused)),
        "?" | "?%" | "P" | "PENDING" => CellInput::Set(MarkEntry::status(Status::Pending)),
        "L" | "LATE" => return Some(Err("Enter a late mark as the mark followed by L, e.g. 85L or 17/20L".into())),
        _ => return None,
    }))
}

/// Parse grid input such as `85`, `85%`, `17/20`, `17/20L`, `M`, `X`, `?`.
///
/// `out_of` is the assessment's maximum. `17/20` is rescaled onto `out_of`, so
/// it works on a column marked out of 100 too.
pub fn parse_cell(input: &str, out_of: f64) -> Result<CellInput, String> {
    let s = input.trim();
    if let Some(c) = status_code(s) {
        return c;
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
    if kind == Kind::Level {
        return parse_level(input);
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

/// Text for a mark of the given kind (inverse of [`parse_input`]).
pub fn display_mark(kind: Kind, m: &MarkEntry) -> String {
    let num = |v: f64| if kind == Kind::Level { level_text(v) } else { fmt_num(v) };
    match (m.value, m.status) {
        (Some(v), Status::Late) => format!("{}L", num(v)),
        (Some(v), _) => num(v),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A mark out of `out_of`.
    Scored,
    /// Done / not done (e.g. journals). Never affects the grade.
    Completion,
    /// A level from 1 to 4 (with + and −), e.g. weekly participation. Counts
    /// toward the grade through the teacher's level → percentage table.
    Level,
}

impl Kind {
    pub fn parse(s: &str) -> Kind {
        match s {
            "completion" => Kind::Completion,
            "level" => Kind::Level,
            _ => Kind::Scored,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Scored => "scored",
            Kind::Completion => "completion",
            Kind::Level => "level",
        }
    }
    /// Counts toward the grade (marks and levels; not done / not done).
    pub fn graded(self) -> bool {
        self != Kind::Completion
    }
}

// ── Levels ───────────────────────────────────────────────────────────────────

/// The highest level. Level assessments are stored "out of" this.
pub const MAX_LEVEL: f64 = 4.0;
/// What + and − add or take away from a level.
const LEVEL_STEP: f64 = 1.0 / 3.0;

/// What each level is worth as a percentage: R (below level 1), 1, 2, 3, 4.
/// Between levels (3+, 4−) the percentage is in proportion.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Levels {
    pub pct: [f64; 5],
}

impl Default for Levels {
    /// The middle of each Ontario achievement level's range.
    fn default() -> Self {
        Levels { pct: [40.0, 55.0, 65.0, 75.0, 90.0] }
    }
}

impl Levels {
    /// Valid when each level is worth more than the one below it.
    pub fn new(pct: [f64; 5]) -> Option<Levels> {
        let ok = pct.iter().all(|p| (0.0..=100.0).contains(p)) && pct.windows(2).all(|w| w[0] < w[1]);
        ok.then_some(Levels { pct })
    }

    /// Percentage for a level (0 = R … 4, 4+ ≈ 4.33).
    pub fn to_pct(self, level: f64) -> f64 {
        let p = &self.pct;
        let l = level.clamp(0.0, MAX_LEVEL + 0.5);
        let i = (l.floor() as usize).min(3);
        (p[i] + (p[i + 1] - p[i]) * (l - i as f64)).clamp(0.0, 100.0)
    }

    /// The level a percentage works out to (the inverse of [`Levels::to_pct`]).
    pub fn level_of(&self, pct: f64) -> f64 {
        let p = &self.pct;
        if pct <= p[0] {
            return 0.0;
        }
        for i in 0..4 {
            if pct <= p[i + 1] || i == 3 {
                return (i as f64 + (pct - p[i]) / (p[i + 1] - p[i])).min(MAX_LEVEL + LEVEL_STEP);
            }
        }
        MAX_LEVEL
    }
}

/// A level as a teacher writes it: R, 1 … 4, with + or − for a third of a
/// level either way (3+, 4−). Anything else to two decimals.
pub fn level_text(level: f64) -> String {
    let n = level.round();
    let d = level - n;
    let base = if n == 0.0 { "R".to_string() } else { fmt_num(n) };
    if d.abs() < 0.01 {
        base
    } else if (d - LEVEL_STEP).abs() < 0.01 {
        format!("{base}+")
    } else if (d + LEVEL_STEP).abs() < 0.01 {
        format!("{base}-")
    } else {
        fmt_num(level)
    }
}

/// The nearest whole level, + or − for an average (e.g. 3.2 → "3", 3.4 → "3+").
pub fn level_label(level: f64) -> String {
    level_text((level / LEVEL_STEP).round() * LEVEL_STEP)
}

fn parse_level(input: &str) -> Result<CellInput, String> {
    const HELP: &str = "Type a level from 1 to 4 (3+ and 4- are fine), R for below level 1, or M, X, ?";
    let s = input.trim();
    if let Some(c) = status_code(s) {
        return c;
    }
    let (body, late) = match s.strip_suffix(['L', 'l']).filter(|b| !b.trim().is_empty()) {
        Some(rest) => (rest.trim(), true),
        None => (s, false),
    };
    let lower = body.to_ascii_lowercase();
    let body = lower.strip_prefix("level").or_else(|| lower.strip_prefix('l')).unwrap_or(&lower).trim();
    let (num, adj) = match body.strip_suffix('+') {
        Some(b) => (b.trim(), LEVEL_STEP),
        None => match body.strip_suffix(['-', '−']) {
            Some(b) => (b.trim(), -LEVEL_STEP),
            None => (body, 0.0),
        },
    };
    let base = if num == "r" { 0.0 } else { num.parse::<f64>().ok().filter(|v| v.is_finite()).ok_or_else(|| HELP.to_string())? };
    let level = base + adj;
    if !(0.0..=MAX_LEVEL + 0.5).contains(&level) || (base == 0.0 && adj < 0.0) {
        return Err(HELP.into());
    }
    let level = (level * 1000.0).round() / 1000.0;
    Ok(CellInput::Set(MarkEntry { value: Some(level), status: if late { Status::Late } else { Status::Marked } }))
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
    /// Within each group, work further along the columns counts up to twice as much as the earliest.
    pub recent: bool,
    pub levels: Levels,
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
pub fn mark_pct(a: &AssessmentIn, m: &MarkEntry, levels: &Levels) -> Option<f64> {
    match m.status {
        Status::Marked | Status::Late => m.value.map(|v| match a.kind {
            Kind::Level => levels.to_pct(v),
            _ if a.out_of > 0.0 => v / a.out_of * 100.0,
            _ => 0.0,
        }),
        _ => None,
    }
}

/// Percentage that counts toward an average under the missing-work policy.
fn counted_pct(a: &AssessmentIn, m: Option<&MarkEntry>, setup: &Setup) -> Option<f64> {
    let m = m?;
    match m.status {
        Status::Missing if setup.policy == MissingPolicy::Zero => Some(0.0),
        _ => mark_pct(a, m, &setup.levels),
    }
}

fn weighted_mean<'a>(
    items: impl Iterator<Item = &'a AssessmentIn>,
    marks: &HashMap<i64, MarkEntry>,
    setup: &Setup,
) -> (Option<f64>, usize) {
    let counted: Vec<(&AssessmentIn, f64)> = items
        .filter(|a| a.kind.graded() && a.weight > 0.0)
        .filter_map(|a| counted_pct(a, marks.get(&a.id), setup).map(|p| (a, p)))
        .collect();
    let last = counted.len().saturating_sub(1).max(1) as f64;
    let (mut sum, mut wsum) = (0.0, 0.0);
    for (i, (a, p)) in counted.iter().enumerate() {
        // Oldest = x1, newest = x2, evenly in between (by column order).
        let w = if setup.recent { a.weight * (1.0 + i as f64 / last) } else { a.weight };
        sum += p * w;
        wsum += w;
    }
    if wsum > 0.0 { (Some(sum / wsum), counted.len()) } else { (None, 0) }
}

/// Compute one student's results for one subject.
///
/// `marks` maps assessment id → mark. `override_pct` is the teacher's manual
/// final grade, if any.
pub fn compute(setup: &Setup, marks: &HashMap<i64, MarkEntry>, override_pct: Option<f64>) -> StudentResult {
    let mut r = StudentResult::default();
    let scored: Vec<&AssessmentIn> = setup.assessments.iter().filter(|a| a.kind.graded()).collect();

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
        let (avg, _) = weighted_mean(scored.iter().copied(), marks, setup);
        r.calculated = avg;
    } else {
        let known: std::collections::HashSet<i64> = setup.categories.iter().map(|c| c.id).collect();
        let orphaned = scored.iter().filter(|a| a.category_id.is_none_or(|c| !known.contains(&c)) && a.weight > 0.0).count();
        if orphaned > 0 {
            r.warnings.push(format!("{orphaned} assessment(s) have no category and don't count toward the grade."));
        }
        for c in &setup.categories {
            let (avg, n) = weighted_mean(scored.iter().copied().filter(|a| a.category_id == Some(c.id)), marks, setup);
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
        let (avg, _) = weighted_mean(scored.iter().copied().filter(|a| a.unit == u), marks, setup);
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
            assert_eq!(display_mark(Kind::Scored, &m), s);
        }
    }

    #[test]
    fn recent_work_counts_more() {
        let asmts = vec![a(1, None, 100.0), a(2, None, 100.0), a(3, None, 100.0)];
        let marks: HashMap<i64, MarkEntry> =
            HashMap::from([(1, MarkEntry::marked(60.0)), (2, MarkEntry::marked(70.0)), (3, MarkEntry::marked(100.0))]);
        let mk = |recent| Setup {
            categories: vec![],
            assessments: asmts.clone(),
            policy: MissingPolicy::Ignore,
            recent,
            levels: Levels::default(),
        };
        approx(compute(&mk(false), &marks, None).overall, 76.666_666_666_666_67);
        // weights 1, 1.5, 2 → (60 + 105 + 200) / 4.5
        approx(compute(&mk(true), &marks, None).overall, 365.0 / 4.5);
        // A single mark is unchanged.
        let one: HashMap<i64, MarkEntry> = HashMap::from([(2, MarkEntry::marked(80.0))]);
        approx(compute(&mk(true), &one, None).overall, 80.0);
    }

    #[test]
    fn real_zero_is_not_missing() {
        let setup = Setup {
            categories: vec![],
            assessments: vec![a(1, None, 10.0), a(2, None, 10.0)],
            policy: MissingPolicy::Ignore,
            recent: false,
            levels: Levels::default(),
        };
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
        let ignore = compute(
            &Setup {
                categories: vec![],
                assessments: asmts.clone(),
                policy: MissingPolicy::Ignore,
                recent: false,
                levels: Levels::default(),
            },
            &marks,
            None,
        );
        approx(ignore.overall, 80.0);
        assert_eq!((ignore.submitted, ignore.missing, ignore.pending, ignore.excused, ignore.expected), (2, 1, 1, 1, 4));

        let zero = compute(
            &Setup { categories: vec![], assessments: asmts, policy: MissingPolicy::Zero, recent: false, levels: Levels::default() },
            &marks,
            None,
        );
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
            recent: false,
            levels: Levels::default(),
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
            recent: false,
            levels: Levels::default(),
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
        let setup = Setup {
            categories: vec![],
            assessments: vec![a(1, None, 10.0), formative, journal],
            policy: MissingPolicy::Zero,
            recent: false,
            levels: Levels::default(),
        };
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
            recent: false,
            levels: Levels::default(),
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
            recent: false,
            levels: Levels::default(),
        };
        let marks = HashMap::from([(1, MarkEntry::marked(10.0)), (2, MarkEntry::marked(5.0))]);
        let r = compute(&setup, &marks, None);
        approx(r.overall, 75.0);
        assert_eq!(r.warnings.len(), 1);
    }

    #[test]
    fn override_wins() {
        let setup = Setup {
            categories: vec![],
            assessments: vec![a(1, None, 10.0)],
            policy: MissingPolicy::Ignore,
            recent: false,
            levels: Levels::default(),
        };
        let r = compute(&setup, &HashMap::from([(1, MarkEntry::marked(5.0))]), Some(62.0));
        approx(r.calculated, 50.0);
        approx(r.overall, 62.0);
        assert!(r.overridden);
    }

    #[test]
    fn no_marks_means_no_grade() {
        let setup = Setup {
            categories: vec![],
            assessments: vec![a(1, None, 10.0)],
            policy: MissingPolicy::Ignore,
            recent: false,
            levels: Levels::default(),
        };
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
        let setup = Setup {
            categories: vec![],
            assessments: vec![x, y, z],
            policy: MissingPolicy::Ignore,
            recent: false,
            levels: Levels::default(),
        };
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

    #[test]
    fn levels_parse_and_show() {
        let lv = |s: &str| match parse_input(Kind::Level, s, MAX_LEVEL) {
            Ok(CellInput::Set(m)) => m,
            other => panic!("{s}: {other:?}"),
        };
        for (typed, shown) in [
            ("3", "3"),
            ("3+", "3+"),
            ("4-", "4-"),
            ("4−", "4-"),
            ("L2", "2"),
            ("level 1+", "1+"),
            ("R", "R"),
            ("3+L", "3+L"),
            ("4L", "4L"),
            ("M", "M"),
            ("x", "X"),
            ("?", "?"),
        ] {
            assert_eq!(display_mark(Kind::Level, &lv(typed)), shown, "{typed}");
        }
        assert_eq!(lv("3L").status, Status::Late);
        assert_eq!(parse_input(Kind::Level, "", MAX_LEVEL), Ok(CellInput::Clear));
        for bad in ["5", "abc", "R-", "-1", "L"] {
            assert!(parse_input(Kind::Level, bad, MAX_LEVEL).is_err(), "{bad}");
        }
    }

    #[test]
    fn levels_convert_to_percentages() {
        let l = Levels::default();
        approx(Some(l.to_pct(3.0)), 75.0);
        approx(Some(l.to_pct(4.0)), 90.0);
        approx(Some(l.to_pct(0.0)), 40.0);
        approx(Some(l.to_pct(3.0 + LEVEL_STEP)), 80.0);
        approx(Some(l.to_pct(4.0 + LEVEL_STEP)), 95.0);
        for v in [0.0, 1.0, 2.5, 3.0, 4.0, 4.0 + LEVEL_STEP] {
            approx(Some(l.level_of(l.to_pct(v))), v);
        }
        assert_eq!(level_label(l.level_of(78.0)), "3+");
        assert_eq!(level_label(3.1), "3");
        assert!(Levels::new([40.0, 55.0, 55.0, 75.0, 90.0]).is_none());
    }

    #[test]
    fn levels_count_toward_their_category() {
        let mut part = a(2, Some(2), MAX_LEVEL);
        part.kind = Kind::Level;
        let mut part2 = a(3, Some(2), MAX_LEVEL);
        part2.kind = Kind::Level;
        let setup = Setup {
            categories: vec![
                CategoryIn { id: 1, name: "Work".into(), weight: 85.0 },
                CategoryIn { id: 2, name: "Participation".into(), weight: 15.0 },
            ],
            assessments: vec![a(1, Some(1), 100.0), part, part2],
            policy: MissingPolicy::Ignore,
            recent: false,
            levels: Levels::default(),
        };
        let marks = HashMap::from([(1, MarkEntry::marked(80.0)), (2, MarkEntry::marked(4.0)), (3, MarkEntry::marked(3.0))]);
        let r = compute(&setup, &marks, None);
        approx(r.categories[1].avg, 82.5);
        approx(r.overall, 80.0 * 0.85 + 82.5 * 0.15);
        assert_eq!(r.submitted, 3);
    }
}
