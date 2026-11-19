-- Gradebook schema v1.
-- Every teaching row traces back to users.id; deleting a user deletes their data.

CREATE TABLE users (
    id             INTEGER PRIMARY KEY,
    username       TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash  TEXT NOT NULL,
    full_name      TEXT NOT NULL DEFAULT '',
    email          TEXT NOT NULL DEFAULT '',
    school         TEXT NOT NULL DEFAULT '',
    is_admin       INTEGER NOT NULL DEFAULT 0,
    totp_secret    TEXT,
    created_at     TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE sessions (
    token_hash   TEXT PRIMARY KEY,
    user_id      INTEGER REFERENCES users(id) ON DELETE CASCADE,
    -- Set while a password is verified but the TOTP code isn't yet.
    pending_user INTEGER REFERENCES users(id) ON DELETE CASCADE,
    csrf         TEXT NOT NULL,
    flash        TEXT NOT NULL DEFAULT '[]',
    created_at   INTEGER NOT NULL,
    last_seen    INTEGER NOT NULL
);
CREATE INDEX sessions_user ON sessions(user_id);

CREATE TABLE teacher_settings (
    user_id          INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    mode             TEXT NOT NULL DEFAULT 'secondary',  -- elementary | secondary | university
    active_periods   INTEGER NOT NULL DEFAULT 3,         -- secondary: 1..3 teaching periods
    prep_period      INTEGER,                            -- secondary: 1..4, NULL = not shown
    missing_policy   TEXT NOT NULL DEFAULT 'ignore',     -- ignore | zero
    board_name       TEXT NOT NULL DEFAULT '',
    report_title     TEXT NOT NULL DEFAULT 'Student Progress Report',
    primary_color    TEXT NOT NULL DEFAULT '#003F5A',
    accent_color     TEXT NOT NULL DEFAULT '#B1D2E1',
    logo             BLOB,
    logo_mime        TEXT,
    scale_json       TEXT,                               -- NULL = Ontario letter grades
    setup_done       INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE terms (
    id           INTEGER PRIMARY KEY,
    user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    period_label TEXT NOT NULL DEFAULT '',   -- printed on reports, e.g. "Semester 1, 2026–2027"
    start_date   TEXT,
    end_date     TEXT,
    is_current   INTEGER NOT NULL DEFAULT 0,
    archived     INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX terms_user ON terms(user_id);

CREATE TABLE classes (
    id           INTEGER PRIMARY KEY,
    user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    term_id      INTEGER NOT NULL REFERENCES terms(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    course_code  TEXT NOT NULL DEFAULT '',
    period       INTEGER,
    grade_level  TEXT NOT NULL DEFAULT '',
    room         TEXT NOT NULL DEFAULT '',
    sort         INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX classes_term ON classes(term_id);
CREATE INDEX classes_user ON classes(user_id);

CREATE TABLE subjects (
    id           INTEGER PRIMARY KEY,
    class_id     INTEGER NOT NULL REFERENCES classes(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    report_style TEXT NOT NULL DEFAULT 'percent',  -- percent | letter (elementary Gr 1–6)
    sort         INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX subjects_class ON subjects(class_id);

CREATE TABLE categories (
    id          INTEGER PRIMARY KEY,
    subject_id  INTEGER NOT NULL REFERENCES subjects(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    weight      REAL NOT NULL DEFAULT 0,
    sort        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX categories_subject ON categories(subject_id);

CREATE TABLE assessments (
    id                 INTEGER PRIMARY KEY,
    subject_id         INTEGER NOT NULL REFERENCES subjects(id) ON DELETE CASCADE,
    category_id        INTEGER REFERENCES categories(id) ON DELETE SET NULL,
    name               TEXT NOT NULL,
    unit               TEXT NOT NULL DEFAULT '',
    out_of             REAL NOT NULL DEFAULT 100,
    weight             REAL NOT NULL DEFAULT 1,
    kind               TEXT NOT NULL DEFAULT 'scored',  -- scored | completion
    due_date           TEXT,
    include_in_report  INTEGER NOT NULL DEFAULT 1,
    sort               INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX assessments_subject ON assessments(subject_id);

CREATE TABLE students (
    id              INTEGER PRIMARY KEY,
    user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    first_name      TEXT NOT NULL,
    last_name       TEXT NOT NULL DEFAULT '',
    preferred_name  TEXT NOT NULL DEFAULT '',
    student_number  TEXT NOT NULL DEFAULT '',
    pronouns        TEXT NOT NULL DEFAULT 'they',     -- she | he | they
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX students_user ON students(user_id);

CREATE TABLE enrolments (
    class_id    INTEGER NOT NULL REFERENCES classes(id) ON DELETE CASCADE,
    student_id  INTEGER NOT NULL REFERENCES students(id) ON DELETE CASCADE,
    active      INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (class_id, student_id)
);
CREATE INDEX enrolments_student ON enrolments(student_id);

CREATE TABLE marks (
    assessment_id  INTEGER NOT NULL REFERENCES assessments(id) ON DELETE CASCADE,
    student_id     INTEGER NOT NULL REFERENCES students(id) ON DELETE CASCADE,
    value          REAL,
    status         TEXT NOT NULL,   -- marked | late | missing | excused | pending
    updated_at     TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (assessment_id, student_id)
);
CREATE INDEX marks_student ON marks(student_id);

-- Append-only log of every mark change; powers undo and "changed since last report".
CREATE TABLE mark_history (
    id             INTEGER PRIMARY KEY,
    batch          TEXT NOT NULL,
    class_id       INTEGER NOT NULL REFERENCES classes(id) ON DELETE CASCADE,
    assessment_id  INTEGER NOT NULL REFERENCES assessments(id) ON DELETE CASCADE,
    student_id     INTEGER NOT NULL REFERENCES students(id) ON DELETE CASCADE,
    old_value      REAL,
    old_status     TEXT,            -- NULL = there was no mark
    new_value      REAL,
    new_status     TEXT,            -- NULL = mark cleared
    source         TEXT NOT NULL,   -- grid | paste | import | undo
    undone         INTEGER NOT NULL DEFAULT 0,
    changed_at     TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX mark_history_class ON mark_history(class_id, id);

CREATE TABLE grade_overrides (
    student_id  INTEGER NOT NULL REFERENCES students(id) ON DELETE CASCADE,
    subject_id  INTEGER NOT NULL REFERENCES subjects(id) ON DELETE CASCADE,
    pct         REAL NOT NULL,
    reason      TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (student_id, subject_id)
);

CREATE TABLE report_comments (
    student_id  INTEGER NOT NULL REFERENCES students(id) ON DELETE CASCADE,
    subject_id  INTEGER NOT NULL REFERENCES subjects(id) ON DELETE CASCADE,
    comment     TEXT NOT NULL DEFAULT '',
    next_steps  TEXT NOT NULL DEFAULT '',
    updated_at  TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (student_id, subject_id)
);

CREATE TABLE learning_skills (
    student_id  INTEGER NOT NULL REFERENCES students(id) ON DELETE CASCADE,
    class_id    INTEGER NOT NULL REFERENCES classes(id) ON DELETE CASCADE,
    skill       TEXT NOT NULL,
    rating      TEXT NOT NULL,    -- E | G | S | N
    PRIMARY KEY (student_id, class_id, skill)
);

CREATE TABLE report_runs (
    id           INTEGER PRIMARY KEY,
    class_id     INTEGER NOT NULL REFERENCES classes(id) ON DELETE CASCADE,
    student_id   INTEGER NOT NULL REFERENCES students(id) ON DELETE CASCADE,
    report_type  TEXT NOT NULL,
    overall      REAL,
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX report_runs_student ON report_runs(class_id, student_id, id);

CREATE TABLE comment_bank (
    id        INTEGER PRIMARY KEY,
    user_id   INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    tag       TEXT NOT NULL DEFAULT '',   -- free text: subject, "strength", "next step", …
    body      TEXT NOT NULL
);
CREATE INDEX comment_bank_user ON comment_bank(user_id);

-- Uploaded spreadsheets waiting for the teacher to map columns and confirm.
CREATE TABLE import_staging (
    id          TEXT PRIMARY KEY,
    user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    filename    TEXT NOT NULL,
    grid_json   TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);
