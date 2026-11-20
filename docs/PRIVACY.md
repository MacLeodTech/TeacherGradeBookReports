# Where student data lives: choosing a deployment

Gradebook holds student names, marks and comments. Many boards (in Ontario, MFIPPA) restrict where identifiable
student information may be stored. Before using this with real students, **ask your school board or privacy officer
which of these options is allowed.** This page lists what each one does, so you can ask a precise question.

## The three options

| | 1. Local | 2. Hosted, in memory (default) | 3. Hosted, saved |
|---|---|---|---|
| How to run | `gradebook local` (or the release binary) on the teacher's own computer | Docker image, `STORAGE=memory` | Docker image, `STORAGE=persistent` |
| Where student data is while working | The teacher's computer, in memory | Server memory, per teacher, while signed in | Server database |
| Where it is when not working | In the workbook `.xlsx` the teacher saves (`--keep-data` also keeps a database in the app's data folder) | In the workbook `.xlsx` the teacher saves to their own device | Server database and its backups |
| What the server stores | Nothing, unless `--keep-data` | Accounts and settings only | Everything |
| Accounts | None; loopback only | Passwords (optional 2-step) or board SSO | Passwords (optional 2-step) or board SSO |
| Backups, retention, audit | Teacher's own file | Teacher's own file | Nightly backups (`BACKUP_DIR`, `BACKUP_KEEP`), `RETENTION_DAYS`, audit log (`AUDIT_KEEP_DAYS`) |
| Who is the custodian of the data | The teacher | The teacher (the server has no student data to breach, back up or retain) | Whoever runs the server |

**Starting point for most teachers:** option 1 or 2. The server never keeps student data, so the board question
shrinks to "may a teacher keep a workbook file on this device or drive?" Option 3 is for a board that hosts the app
on its own infrastructure and accepts custody.

## Safeguards in every option

- Sign-in with passwords (hashed), optional 2-step codes (TOTP), or board single sign-on (`AUTH=oidc`).
- Idle sign-out (`IDLE_TIMEOUT_MINUTES`, default 120), CSRF protection, a login rate limit and a strict content security policy.
- In memory mode a write guard stops student tables being written to the server's database, a size cap
  (`WORKSPACE_MAX_MB`) applies, and idle workspaces are discarded.
- Local mode listens on the loopback address only and refuses requests aimed at other host names.
- Archived terms are read-only; **Purge** really deletes a term. In saved mode `RETENTION_DAYS` deletes old archived terms automatically.
- Every report download and sign-in is written to the audit log (saved mode and board deployments).
- Students can be entered with minimal identifiers: the student number is optional, and a first name with a last initial works.

## Questions to take to your board

1. May identifiable student marks be held on a teacher's own computer, a board-issued device, or a personal cloud drive?
2. If the app is hosted, may the host be inside the board's network only, or a third-party provider? In which country?
3. Is a workbook file (an `.xlsx` the teacher saves) acceptable as the record, and where may it be kept?
4. How long must records be kept, and who deletes them at the end?
5. Is single sign-on with board accounts required for any hosted option?
6. Who is the contact for a breach or an access request?

## Not covered yet

- The Windows and macOS release binaries are unsigned, so the OS shows a warning on first run (parked).
- The app doesn't encrypt the workbook file. Keep it on an encrypted drive, or use the board's file storage.
