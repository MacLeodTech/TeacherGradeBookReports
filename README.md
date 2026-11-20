# Gradebook & Progress Reports

A self-hosted gradebook for teachers that produces professional PDF progress reports.

- **Your workbook is yours.** All of a teacher's classes, students, marks, comments and report history save
  to one Excel workbook that opens in Excel as a plain grid of marks per class, ready to keep marking in.
  Gradebook opens it again exactly as it was, plus any marks or assessments added in Excel. By default the server keeps **no student data at all**: it lives in memory only while the teacher
  is signed in.
- **A spreadsheet-like mark grid** per class: add assessment columns, type or paste marks straight from
  Excel, and mark work as missing (`M`), excused (`X`), late (`17L`) or still to mark (`?`). Every change
  saves immediately and can be undone.
- **Levels 1–4** (like weekly participation) alongside marks: type `3`, `3+`, `4-` or `R`. Reports show
  "Level 3+", and each level converts to a percentage for the final grade (75% for level 3 by default; set
  the table in **Settings**).
- **Weights like report-card software:** categories such as Knowledge/Thinking/Communication/Application
  plus a 30% final, with relative weights per assessment and formative (weight 0) work.
- **Click a student** to see just their marks, write comments (with a reusable comment bank), rate
  learning skills, and preview or download their report. Or download a whole class as a ZIP.
- **Elementary, high school or university:** one class with many subjects; 1–3 periods plus a prep (for
  LTO and part-time teachers too); or any number of courses.
- **A starter spreadsheet** to get going: download it from **Import**. Each class is a plain grid (class name
  at the top, students down the side, one assessment per column like `Quiz 1 /20`), with a **Start here** tab
  explaining the mark codes and a worked example (never imported). Categories, weights, course codes and
  pronouns are on optional grey **Setup** tabs at the back. Upload it and you see what it will create, and
  any cells to fix, before anything is saved. Upload it again later to update marks, or download it with
  your current classes filled in.
- **Still imports spreadsheets.** Upload an Excel/CSV file, check how each column is read, and preview what
  will change before anything is saved. Re-uploading an updated sheet only changes what's different.

## Three ways to run it

| | **On a teacher's own computer** | **Hosted, in-memory** (default) | **Hosted, saved** |
|---|---|---|---|
| Who | One teacher, no server | A household, school or board | A board that wants data kept centrally |
| Student data | In memory while the app is open | In memory while the teacher is signed in | In the server's database (daily backups) |
| Between sessions | Teacher saves their workbook | Teacher saves their workbook | Kept on the server (workbook still available) |
| Sign-in | None (only reachable from that computer) | Passwords + 2-step, or board SSO | Passwords + 2-step, or board SSO |
| Setting | `gradebook local` | `STORAGE=memory` | `STORAGE=persistent` |

Every option has the same features. The only difference is where student information lives between sessions.

### The workbook

With in-memory storage, each teacher opens their workbook when they start and saves it when they finish. A
bar under the menu shows whether there are unsaved changes, and signing out with unsaved changes asks first.
Save it somewhere your board allows student information (for example a board OneDrive or Google Drive).
Opening a workbook replaces what's open; with saved storage it adds the workbook's terms and classes
alongside instead. Importing a workbook on the **Import** page opens it too.

The workbook has the same layout as the starter spreadsheet, for the current term, and nothing is locked:
a late assignment or a new quiz can be added in Excel between sessions. Opening the file brings those
changes in (a cleared cell clears that mark), lists them, and points out anything to check, such as which
category a new assessment was put in. Other terms are kept in the file but not shown as tabs.

The server's database still holds accounts, settings, comment banks, logos and the audit log. These contain
no student information. A database guard refuses any write of student data to the file in memory mode, and
the tests check that the file's student tables stay empty.

### 1. On a teacher's own computer

Download the file for your computer from the
[latest release](https://github.com/MacLeodTech/TeacherGradeBookReports/releases/latest):

| Computer | Download |
|---|---|
| Windows | `gradebook-windows-x64.zip` |
| Mac with an Apple chip (M1 or later) | `gradebook-macos-arm64.tar.gz` |
| Mac with an Intel chip | `gradebook-macos-x64.tar.gz` |
| Linux | `gradebook-linux-x64.tar.gz` |

Not sure which Mac you have? Apple menu → **About This Mac**: "Chip" means Apple, "Processor" means Intel.

Unzip it (double-click the file) and double-click `gradebook` (`gradebook.exe` on Windows). It starts on
`127.0.0.1`, opens the browser and signs in through a one-time link. **Quit Gradebook** (in the menu) stops it.

The downloads aren't code-signed yet, so the first time you open Gradebook your computer warns you:

- **Windows:** "Windows protected your PC". Click **More info**, then **Run anyway**.
- **macOS:** "gradebook" can't be opened / "Apple could not verify…". Click **Done** (not Move to Trash), open
  **System Settings → Privacy & Security**, scroll down to the message about `gradebook` and click **Open Anyway**,
  then confirm with your password. Or, in Terminal: `xattr -d com.apple.quarantine ~/Downloads/gradebook`.
- **Linux:** no warning. If it won't run, make it executable with `chmod +x gradebook`.

You only need to do this once per download.

```bash
gradebook local                 # in-memory (default): save your workbook before quitting
gradebook local --keep-data     # keep everything in the app's data folder instead
gradebook local --port 8765 --no-browser --data-dir ~/Gradebook
```

It only answers requests addressed to `127.0.0.1`/`localhost`, so other computers and web pages can't reach it.
Running it again while it's open just reopens the browser.

### 2. and 3. Hosted (Docker)

```bash
cp .env.template .env      # set ADMIN_PASSWORD (10+ characters, with a letter and a number), STORAGE, AUTH
docker compose up -d --build
```

Open http://localhost:8000 and sign in as the admin. The first sign-in walks through setup (teaching level,
periods, school colours, first term). The main school colour fills the report banners, the second is
used for trim, and tables use soft tints of the main colour, so any pair of colours stays readable. Add more teacher accounts under **Accounts**.

Put it behind HTTPS (a reverse proxy such as Caddy, Traefik or nginx) before using real student data, and
set `TRUST_PROXY=true` when you do.

#### Board single sign-on

Set `AUTH=oidc` to have teachers sign in with their board account (Microsoft Entra ID, Google Workspace, or
any OpenID Connect provider). Register a web application with the provider, using the redirect URI
`https://<your host>/login/oidc/callback`, then set `OIDC_ISSUER`, `OIDC_CLIENT_ID`, `OIDC_CLIENT_SECRET` and
`PUBLIC_URL`. Accounts are created the first time someone signs in. Limit sign-in to your domains with
`OIDC_ALLOWED_DOMAINS`, and name admins with `OIDC_ADMIN_EMAILS`. The sign-in uses the authorization code flow
with PKCE and a nonce. The ID token's signature, issuer, audience and expiry are verified, and unverified
emails are ignored.

#### Audit and retention

Admins see an **Audit log** with sign-ins (including failures and refusals), workbook saves and opens, report
downloads, exports and account changes. It records who, when and from where, never student names or marks.
Entries are kept for `AUDIT_KEEP_DAYS`. With saved storage, `RETENTION_DAYS` deletes terms that number of
days after they're archived.

### Configuration

| Variable | Default | Meaning |
|---|---|---|
| `STORAGE` | `memory` | `memory`: student data only in memory, teachers keep workbooks. `persistent`: stored in the database |
| `WORKSPACE_MAX_MB` | `256` | Memory storage: the most one teacher's open work may use |
| `AUTH` | `password` | `password` or `oidc` (board single sign-on) |
| `OIDC_ISSUER` / `OIDC_CLIENT_ID` / `OIDC_CLIENT_SECRET` | — | Single sign-on provider and app registration |
| `PUBLIC_URL` | — | The address teachers use, e.g. `https://gradebook.example.ca` (for the redirect URI) |
| `OIDC_DISPLAY_NAME` | `your school board` | Shown as "Sign in with …" |
| `OIDC_ALLOWED_DOMAINS` | — | Comma-separated email domains allowed to sign in |
| `OIDC_ADMIN_EMAILS` | — | Comma-separated emails that are admins |
| `ALLOW_PASSWORD_LOGIN` | `false` | With `AUTH=oidc`, also allow password accounts |
| `ADMIN_USERNAME` / `ADMIN_PASSWORD` | — | First admin account, created only when there are no accounts yet |
| `ADMIN_SCHOOL` | — | School name for that account |
| `DB_PATH` | `/data/gradebook.db` | The database |
| `BACKUP_DIR` | `<data dir>/backups` | Daily database copies; `off` disables |
| `BACKUP_KEEP` | `14` | How many daily backups to keep |
| `RETENTION_DAYS` | — | Saved storage: delete archived terms after this many days |
| `AUDIT_KEEP_DAYS` | `365` | How long to keep the audit log |
| `COOKIE_SECURE` | `true` | Set `false` only for plain-http local testing |
| `TRUST_PROXY` | `false` | Use `X-Forwarded-For` for the client address (behind a proxy) |
| `IDLE_TIMEOUT_MINUTES` | `120` | Sign out after inactivity (memory storage: unsaved work is cleared then) |
| `BIND` | `0.0.0.0:8000` | Listen address |
| `TZ` | — | Time zone for report dates, e.g. `America/Toronto` |

### Upgrading from the Python version

Keep the same `/data` volume. On first start the app copies the accounts from the old `users.db` into
`gradebook.db` (existing passwords keep working). Classes are new: import last term's spreadsheet from the
**Import** page to create a class with all its students and marks.

### Backups and restore

A copy of the database (with memory storage: accounts and settings only) is written to `/data/backups` every day, and admins can download a fresh copy from
**Backups**. To restore, stop the container, replace `/data/gradebook.db` with a backup file, and start it
again. Keep a copy somewhere other than the server itself.

## Privacy

Student marks and comments are personal information. Check your board's policy on storing them outside
board systems. The default in-memory storage means the server never stores them: they live in the
teacher's own workbook. Beyond that, accounts can turn on two-step verification (authenticator app), sessions
expire after inactivity, logins are rate-limited, and nothing is sent to any outside service. PDFs are
generated in memory and never written to disk.

## Development

```bash
cargo test            # unit + end-to-end tests: both storage modes, local mode, SSO (mock provider), real PDFs
COOKIE_SECURE=false DB_PATH=./dev.db ADMIN_USERNAME=admin ADMIN_PASSWORD=Password1234 cargo run
```

| Path | What |
|---|---|
| `src/engine.rs` | Grade calculation (categories, weights, statuses, scale). Pure functions, unit tested |
| `src/model.rs` | Database loaders with ownership checks |
| `src/routes/` | Pages (`minijinja` templates in `templates/`) and the grid JSON API (`api.rs`) |
| `src/import.rs` / `src/export.rs` | Spreadsheet import (calamine) and Excel export |
| `src/workbook.rs` | Save/open the whole gradebook as one `.xlsx` workbook |
| `src/starter.rs` | The starter spreadsheet: generate, read back, import |
| `src/db.rs` | Database, migrations, in-memory teacher workspaces and the write guard |
| `src/local.rs`, `src/oidc.rs`, `src/audit.rs` | Local mode, board single sign-on, audit and retention |
| `src/report.rs`, `report/report.typ` | PDF reports (Typst, fonts embedded) |
| `src/color.rs` | Report colours: the school's two colours turned into a readable palette |
| `static/js/grid.js` | The mark grid |
| `migrations/` | SQL schema |
| `docs/ROADMAP.md` | Design notes and status |

Fonts: Source Sans 3, SIL Open Font License (`assets/fonts/OFL.txt`), used in the PDFs and, as a Latin subset in
`static/vendor/fonts/`, on the site.
