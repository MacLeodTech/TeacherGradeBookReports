# Roadmap: from "spreadsheet → PDF" to a persistent gradebook with report generation

_Drafted 2026-11-19 from a review of the codebase at `2f536c9`._

> **Status (2026-11-19):** rebuilt in Rust on the `rust-rewrite` branch, with every phase below implemented.
> Where the build differs from this plan: Rust/axum/SQLite (rusqlite) instead of Flask/SQLAlchemy; a small
> custom grid (`static/js/grid.js`) instead of Tabulator; Typst instead of ReportLab for PDFs. The roadmap
> text below is kept as the original analysis. See "Implementation status" at the end, and the open
> questions still to answer.

## 1. Where the app is today

**What it is:** a stateless Flask app. You upload an `.xlsx`, it parses it with fixed column positions,
renders one PDF per student with ReportLab, keeps the PDFs in `/tmp` until logout (or 24 h), then forgets
everything. The only persistent data is the `users` table (login, school name, teacher name, email).

| Area | Current state | Keep / change |
| --- | --- | --- |
| Stack | Flask 3, SQLite (raw `sqlite3`), Jinja + Bootstrap 5 (vendored), ReportLab, gunicorn, hardened Docker | **Keep.** Right size for a self-hosted, single-household app. |
| Security baseline | CSRF, rate-limited login, CSP, non-root read-only container, ownership checks on downloads | **Keep**, and extend to every new route. |
| Data model | None: a student is a dict built from one spreadsheet row | **Replace** with a real schema (section 3). |
| Grade maths | Overall = column D if present, else unweighted mean of every non-zero score; module avg = unweighted mean | **Replace** with a weighted category engine. |
| Spreadsheet import | Fixed layout: A=ID, B=Last, C=First, D=Mark, E+=`<module>.<n> Name`, `J<n>.<n>` = journal | **Keep as a "legacy" importer**, add a mapping wizard and *import into an existing class*. |
| PDF | Good-looking layout, but SCDSB colours, "Academic Year 2025–2026", grade scale and title are hard-coded | **Keep the look**, feed it a report view-model and per-teacher settings. |
| UI | 5 standalone templates, each with its own copy of the CSS (no base template) | Introduce `base.html` before adding pages. |
| Tests | `smoke_test.py` only: needs a live server and a hard-coded `D:\Desktop\...` path | Add pytest unit + Flask test-client tests. CI only builds the image today. |
| Schema changes | Ad-hoc `ALTER TABLE ... except OperationalError: pass` | Move to Alembic migrations. |

### Bugs found during the review (verified)

1. **Formulas in the spreadsheet are silently ignored.** `openpyxl.load_workbook()` is called without
   `data_only=True`, so a column-D "Mark" like `=AVERAGE(E2:O2)` comes back as the string `"=AVERAGE(...)"`,
   fails to parse, and the report quietly falls back to the unweighted mean. Wrong grade, no warning.
2. **One un-numbered assignment column breaks every report.** A header like `Final Exam` lands in module
   `"Other"`; sorting `["1", "Other"]` with `float(x) if ... else x` raises
   `TypeError: '<' not supported between 'str' and 'float'`, so each student's PDF fails.
3. **Accented names can't be viewed or downloaded.** `_safe_filename` keeps Unicode letters (`\w`), but
   `_validated_report_pdf_path` only allows `[A-Za-z0-9._ -]`, so `Lefèvre_Zoë.pdf` is generated and then 400s.
   (French names are common in Ontario.)
4. **`.xls` is accepted but can't be read:** openpyxl only reads `.xlsx`.
5. **A real 0 can't be recorded:** `0` and blank both mean "Not Submitted".
6. **Duplicate student names overwrite each other's PDF** (filename is `Last_First.pdf`).
7. **Compose defeats the `SECRET_KEY` guard:** `docker-compose.yml` defaults it to `change-me-in-production`,
   and `ADMIN_PASSWORD` defaults to `changeme` (which fails the app's own password policy).
8. Minor: the grade scale (A+ ≥ 95, A ≥ 87) doesn't match Ontario's conversion (A+ 90–100, A 85–89,
   A− 80–84, …, below 50 = **R** rather than F). "Download all" builds the whole ZIP in memory despite the
   "streaming" comment. The login rate limit is per gunicorn worker (2 workers = 20/min). Deleting a user
   leaves their temp sessions behind.

## 2. Target product

> A teacher sets up her year once (school, level, classes/periods, subjects, weighting), keeps marks in an
> Excel-like grid that saves as she types, clicks a student to see just their picture, and generates a
> professional PDF on demand for one student or the whole class. Spreadsheet upload stays available for
> bulk entry and for importing into existing classes.

### One model, three "teaching modes"

Each mode is a preset over the same data model, not a separate app:

| | Elementary | Secondary | University / ad-hoc |
| --- | --- | --- | --- |
| Classes | 1 homeroom | 1–3 active periods (setting; LTO = 1 or 2), plus prep | Any number of sections, created freely |
| Subjects per class | Many (Language, Math, Science, …) | One (the course, e.g. `ENG2D`) | One |
| Default categories | Per subject, often simple | Ontario: K/U, Thinking, Communication, Application = 70 %, Final/Culminating = 30 % | Free-form (Assignments 40 %, Midterm 20 %, Final 40 %) |
| Report | One PDF per student, a section per subject, learning skills, comments | One PDF per student per course | One PDF per student per section |
| Extras | Learning skills (E/G/S/N), letter grades for Gr 1–6 vs % for Gr 7–8 | Semester/quadmester terms, midterm vs final reports | Optional letter/GPA scale |

## 3. Data model

```
teacher (users) ──< school_year/term ──< class ──< subject ──< category ──< assessment ──< mark >── student
                                         │                                                    │
                                         └──────────────< enrolment >─────────────────────────┘
teacher_settings: mode, active_periods (1–3), grade_scale, branding, report defaults
report_comment (student × subject × term), learning_skill (student × term × skill), report_run (history)
```

Key tables:

- **term**: name ("2026–27 Semester 1"), start/end dates, report-period label (replaces the hard-coded
  `REPORT_PERIOD`), status (active / archived).
- **class**: term, name, period number (secondary), course code, room. Secondary validation:
  `count(active classes) <= teacher_settings.active_periods`.
- **subject**: elementary classes have many; secondary/university get one created automatically.
- **category**: name, weight. Weights are renormalised over categories that have marks, so an empty
  "Final" doesn't drag a midterm grade to 70 %.
- **assessment** (a grid column): name, category, unit/module (keeps today's "Module 1" grouping),
  out-of (points, or 100 for %), weight within category, date, `include_in_report`.
- **student**: first, last, preferred name, optional student number, pronouns (for comments).
  **enrolment** links students to classes, so one student can be in several classes and leaving a class
  keeps their history.
- **mark**: assessment × student → `value` (nullable) + `status` ∈ {marked, missing, excused, pending
  (today's `?`), late} + optional note. A real 0 is a distinct value from "missing", and excused marks are
  left out of the average.
- **mark_history** (append-only): who/when/old/new. Cheap, and it makes undo and "what changed since the
  last report" possible.
- **report_run**: when it was generated, for which students, and the grade at that moment. PDFs are
  regenerated on demand, not stored.

Every row carries `owner_id` (teacher) so the app stays multi-teacher-safe, and every query is scoped by it.

### Grade engine (pure Python, no Flask)

`gradebook/engine.py`: `compute(subject, marks) -> StudentResult` with per-category averages, overall %,
letter/level, counts (submitted / missing / pending / excused). Rules to pin down in tests:

- Mark % = value / out_of; assessment weight within its category (default: equal weight, or by points).
- Category average skips excused and pending, and counts missing as missing (a configurable policy:
  "missing counts as 0" vs "ignore until marked").
- Overall = Σ(category avg × weight) / Σ(weights of categories that have marks).
- The grade scale is data (bands + labels + colours), with an Ontario default and an editable copy per teacher.
- Later: Ontario *Growing Success* lets teachers weight "most consistent, more recent" evidence. Offer a
  manual override of the final grade (with a reason) rather than trying to automate that.

The **server is the single source of truth** for calculations. The grid saves a cell, the server returns
the recomputed row and column stats, so there's no second implementation of the maths in JavaScript.

## 4. UI plan

Stay server-rendered (Jinja + Bootstrap) and add **one** vendored grid library. Recommendation: **Tabulator**
(MIT, plain JS, no build step, works under the current `script-src 'self'` CSP). It handles inline editing,
frozen name columns, keyboard navigation, copy/paste ranges and column groups. AG Grid Community is the
heavier alternative. Avoid Handsontable: its licence is non-commercial only.

Pages:

1. **Setup wizard / Settings**: teaching mode, active periods (1–3) + prep slot, school/branding,
   grade scale, term dates. Replaces most of today's Profile page.
2. **Dashboard**: current term's classes as cards (period, course, # students, class average, # missing).
3. **Class gradebook (the grid)**:
   - rows = students (frozen name column), columns = assessments grouped by category/unit;
   - right-hand computed columns: category averages + overall + letter (read-only, colour-coded like the PDF);
   - add/edit an assessment column (name, category, out-of, date, weight) from a side panel;
   - cell input accepts `85`, `17/20`, `M` (missing), `X` (excused), `?` (pending), `L` (late);
   - autosave per cell (JSON `PATCH`, CSRF header), with a saved/failed indicator;
   - paste a column from Excel; bottom row with class average per assessment;
   - an elementary class gets a subject switcher (tabs) above the grid.
4. **Categories & weights** panel per subject: weights must sum to 100 %, with presets ("Ontario
   Secondary 70/30", "Equal").
5. **Student view**: click a name → their marks by category, overall trend, missing work, comment box,
   learning skills (elementary), and **Preview PDF** / **Generate PDF** buttons.
6. **Reports page** per class: choose students (all / changed since last run / selected), options (show
   individual assessments? include comments? include missing list?), generate → ZIP. Same results UI as
   today, but sourced from the DB.
7. **Import**: (a) today's legacy format, (b) a wizard: upload → preview the detected header row → map
   columns (name / ID / assessment / ignore) → choose target class (new or existing) → diff preview
   ("3 new assessments, 41 marks changed, 1 new student") → apply. Students are matched by ID, otherwise by
   normalised name. (c) **Export** class to `.xlsx` (backup, and round-trip editing in Excel).

## 5. Report (PDF) changes

- Refactor `build_student_report(student_dict, ...)` to take a **ReportModel** built from the engine
  result. Keep the current visual design as the default template.
- Pull hard-coded values into settings: school name (already done), board colours, logo upload, report
  title, period label, grade scale legend (generated from the scale, not hard-coded).
- New sections, each optional: category breakdown (bar per category), teacher comment, learning skills grid
  (elementary), missing/outstanding work list, next steps, signature line.
- Elementary: one PDF with a section per subject. Secondary: per course, with an option to combine a student's
  courses if she teaches the same student twice.
- Escape user text before passing it to `Paragraph` (it's a mini-markup language).
- Register a TTF font (e.g. DejaVu Sans) so names outside Latin-1 render properly.

## 6. Privacy and data safety (now that student data persists)

This is the biggest change in risk. Today nothing about students outlives a logout.

- **Check the school board's policy first.** Ontario boards (MFIPPA) often restrict storing identifiable
  student information on personal or non-board systems. Design so it can run with minimal identifiers:
  student number optional, first name + last initial allowed.
- Back up the SQLite DB nightly (`sqlite3 .backup` or Litestream) to the homelab backup target, and test
  a restore. Once the grid is the system of record, losing the DB means losing a term of marks.
- Year-end: archive a term (read-only), and a "purge term" action that really deletes the data.
- Keep the repo free of real data (the repo is **public**; the committed `Grades.xlsx` is synthetic,
  which is fine).
- Session hardening for a persistent app: idle timeout, "log out everywhere", optional TOTP for admin.

## 7. Phased plan

Each phase ships something usable. Effort is rough evenings-and-weekends sizing for one developer with
Claude Code.

### Phase 0: Stabilise the foundation (small)
- Fix bugs 1–7 above. Ontario grade-scale default.
- `templates/base.html`; move shared CSS into `static/app.css`.
- Split `app.py` into a package (`gradebook/`: `auth`, `admin`, `legacy_reports`, `db`), Flask app factory.
- SQLAlchemy 2.0 + Alembic; port `users`/`sessions` as the first migration.
- pytest: `load_grades` fixtures (formula, un-numbered columns, accents, `?`, journals), route tests via the
  Flask test client; run them in CI before the Docker build. Rewrite `smoke_test.py` to read paths from
  arguments or the environment.
- Remove the insecure compose defaults (fail fast if `SECRET_KEY` is unset).

### Phase 1: Persistent data + grade engine (medium)
- Schema from section 3 (terms, classes, subjects, categories, assessments, students, enrolments, marks,
  history, settings).
- `engine.py` with thorough unit tests (this is where correctness lives).
- **Legacy import into the DB**: the existing spreadsheet creates or updates a class. Generating reports
  then reads from the DB. Payoff: she uploads once, and later uploads *update* the class instead of starting over.
- Settings page: teaching mode, active periods 1–3, term.

### Phase 2: The gradebook grid (medium–large) ← the core experience
- Dashboard + class pages, Tabulator grid with autosave, assessment column editor, categories/weights panel,
  computed columns, status codes, paste from Excel, class averages.
- JSON API (`/api/classes/<id>/grid`, `PATCH /api/marks`, `POST /api/assessments`), with owner checks and
  CSRF on every endpoint.

### Phase 3: Student view + on-demand reports (medium)
- Student detail page, comments, preview/generate single PDF, class batch generate, report options,
  report history ("last generated Oct 12; 4 marks changed since").
- ReportModel refactor and configurable branding/scale legend.

### Phase 4: Level-specific features (medium)
- Elementary: multi-subject class, subject tabs, learning skills (E/G/S/N), combined report, Gr 1–6 letter vs
  Gr 7–8 % per subject.
- Secondary: period/prep layout, Ontario 70/30 preset, midterm vs final report type, semester rollover
  ("copy class setup to next term").
- University: free-form sections, custom scale.

### Phase 5: Quality of life (ongoing)
- Import wizard with column mapping + diff preview; `.xlsx` export.
- Comment bank with placeholders (`{first}`, `{pronoun}`), per subject.
- Class analytics: distribution, at-risk list (below 60 % or ≥ N missing), assessment averages.
- Undo via `mark_history`; tablet-friendly layout; backup/restore UI; term archive/purge.

### Suggested MVP for real use
Phases 0 → 1 → 2 → the single-student part of Phase 3, for **one** teaching mode (whichever she teaches
now). That gives "set up once, update marks in the grid, click a student, get a PDF". Everything else builds
on top without rework, because the data model already covers all three modes.

## 8. Open questions for the teacher

Answered 2026-11-20:

1. **Level:** Ontario secondary is the default focus. The other modes stay available.
2. **Mark format:** mixed. Points, percentages and levels can be used per assessment (already supported).
3. **Missing work before the deadline:** ignored until marked (the default). "Count as 0" stays a setting.
4. **Who gets the PDF:** a mix, so the current report defaults stay.
5. **Board policy on keeping student data:** still depends on the board. See `docs/PRIVACY.md` for the options and the questions to ask.

## 9. Implementation status (Rust rewrite)

| Roadmap item | Where |
|---|---|
| Bugs 1–7 (formulas, un-numbered columns, accents, `.xls`, 0 ≠ missing, duplicate names, secret key) | `src/import.rs`, `src/util.rs`, `src/routes/reports.rs`; no secret key needed (server-side sessions) |
| Ontario scale default, configurable scale | `src/engine.rs` (`Scale`), Settings → Grade scale |
| Schema, migrations, legacy account import | `migrations/001_init.sql`, `src/db.rs` |
| Grade engine (categories, weights, formative, missing policy, overrides) | `src/engine.rs` (unit tested) |
| Grid with autosave, keyboard, paste from Excel, status codes, undo, fill | `static/js/grid.js`, `src/routes/api.rs`, `src/marks.rs` |
| Student page, comments, comment bank, learning skills, final-grade override | `src/routes/students.rs` |
| PDF preview/download, batch ZIP, report history, "changed since" | `src/report.rs`, `report/report.typ`, `src/routes/reports.rs` |
| Elementary / high school (periods 1–3 + prep) / university modes | Settings, `src/routes/classes.rs` |
| Terms: create, switch, archive (read-only), copy setup, purge | `src/routes/terms.rs` |
| Import wizard with column mapping and preview; Excel export | `src/routes/imports.rs`, `src/import.rs`, `src/export.rs` |
| Insights (distribution, at-risk, assessment averages) | `/classes/{id}/insights` |
| Privacy: TOTP 2-step, idle timeout, CSRF, rate limit, CSP, nightly backups | `src/auth.rs`, `src/backup.rs` |
| Workbook file: save/open everything as one `.xlsx` (readable marks sheets + hidden data sheets) | `src/workbook.rs`, `src/routes/workbook.rs` |
| In-memory storage (default): per-teacher in-memory SQLite, write guard on the file, size cap, idle discard | `src/db.rs` (`Workspace`, `guard`) |
| Local mode: single binary on a teacher's computer, loopback only, launch link, Quit | `src/local.rs` |
| Board deployment: OIDC single sign-on, audit log, retention of archived terms | `src/oidc.rs`, `src/audit.rs`, `migrations/002_board.sql` |
| Guided setup (class → students → marking → assignments), draft kept in the browser, created in one step | `src/routes/wizard.rs`, `static/js/wizard.js` |
| Review flag for guessed assessment details (`needs_review`), popup that steps through them | `migrations/005_review.sql`, `static/js/grid.js` |
| CI on Linux/Windows/macOS; release binaries on tags | `.github/workflows/` |
| Combined PDF for a student in several classes | `combined` option in `src/routes/reports.rs`, `build_json` in `src/report.rs` |
| "Count recent work more" (oldest ×1 → newest ×2, per category) | `recent` in `src/engine.rs`, `migrations/006_recency.sql`, Settings |
| Tablet and phone layout: tap a cell to edit, bigger touch targets, narrower frozen columns, scrolling tables | `static/app.css`, `static/js/grid.js` |
| Privacy and deployment guide | `docs/PRIVACY.md` |

## 10. Deployment options

The same features in three deployments; only where student data lives between sessions differs:

1. **Local** (`gradebook local`): on the teacher's own computer, no server, no accounts. In memory by
   default; `--keep-data` keeps a database in the app's data folder.
2. **Hosted, in-memory** (`STORAGE=memory`, the default): the server holds accounts and settings only.
   Each teacher's data lives in memory while they're signed in and in their own workbook otherwise. This is
   the low-risk option for a board: the server has no student data to breach, back up or retain.
3. **Hosted, saved** (`STORAGE=persistent`): everything in the server's database, with backups, the audit
   log and `RETENTION_DAYS`. For a board that hosts it on its own infrastructure and accepts custody.

Either hosted option can use passwords (with optional 2-step) or board SSO (`AUTH=oidc`).

Still open: the board's answer on where teachers may keep the workbook (`docs/PRIVACY.md`). Parked:
signed installers for Windows/macOS (the release binaries are unsigned, so the OS will warn on first run) and an
ARM64 Docker image.
