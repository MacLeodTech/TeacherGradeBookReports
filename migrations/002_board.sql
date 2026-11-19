-- Schema v2: board deployments (single sign-on, audit log, retention).

ALTER TABLE users ADD COLUMN oidc_issuer TEXT;
ALTER TABLE users ADD COLUMN oidc_subject TEXT;
CREATE UNIQUE INDEX users_oidc ON users(oidc_issuer, oidc_subject) WHERE oidc_subject IS NOT NULL;

-- PKCE verifier, nonce and state while a single sign-on login is in progress.
ALTER TABLE sessions ADD COLUMN oidc_pending TEXT;

-- When a term was archived, for automatic deletion after RETENTION_DAYS.
ALTER TABLE terms ADD COLUMN archived_at TEXT;

-- Who did what, without student details (no names, marks or comments).
-- Not tied to users(id) so entries outlive a deleted account.
CREATE TABLE audit_log (
    id        INTEGER PRIMARY KEY,
    at        TEXT NOT NULL DEFAULT (datetime('now')),
    user_id   INTEGER,
    username  TEXT NOT NULL DEFAULT '',
    ip        TEXT NOT NULL DEFAULT '',
    action    TEXT NOT NULL,
    detail    TEXT NOT NULL DEFAULT ''
);
CREATE INDEX audit_log_at ON audit_log(at);
