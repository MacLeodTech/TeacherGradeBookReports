//! End-to-end tests through the real router: every page renders, and the main
//! teacher workflows work (setup → class → students → marks → reports → import).

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::{
    AppState, build_state,
    config::{Config, Storage},
    routes,
};

struct Client {
    app: Router,
    cookie: Option<String>,
    csrf: String,
    host: Option<String>,
}

struct Resp {
    status: StatusCode,
    location: Option<String>,
    content_type: String,
    body: Vec<u8>,
}

impl Resp {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|_| panic!("not JSON: {}", self.text()))
    }
    #[track_caller]
    fn ok(&self) -> &Self {
        assert_eq!(self.status, StatusCode::OK, "body: {}", self.text());
        self
    }
    #[track_caller]
    fn redirects_to(&self, prefix: &str) -> &Self {
        assert!(self.status.is_redirection(), "expected redirect, got {} {}", self.status, self.text());
        let loc = self.location.as_deref().unwrap_or("");
        assert!(loc.starts_with(prefix), "redirected to {loc}, expected {prefix}");
        self
    }
}

fn enc(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

impl Client {
    fn new(app: Router) -> Self {
        Client { app, cookie: None, csrf: String::new(), host: None }
    }

    async fn send(&mut self, mut req: Request<Body>) -> Resp {
        if let Some(c) = &self.cookie {
            req.headers_mut().insert(header::COOKIE, c.parse().unwrap());
        }
        if let Some(h) = &self.host {
            req.headers_mut().insert(header::HOST, h.parse().unwrap());
        }
        let resp = self.app.clone().oneshot(req).await.unwrap();
        if let Some(sc) = resp.headers().get(header::SET_COOKIE) {
            let v = sc.to_str().unwrap();
            self.cookie = Some(v.split(';').next().unwrap().to_string());
        }
        let status = resp.status();
        let location = resp.headers().get(header::LOCATION).map(|v| v.to_str().unwrap().to_string());
        let content_type = resp.headers().get(header::CONTENT_TYPE).map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap().to_vec();
        let r = Resp { status, location, content_type, body };
        // Pick up the CSRF token from any page.
        if r.content_type.starts_with("text/html") {
            let t = r.text();
            let pat = r#"name="csrf-token" content=""#;
            if let Some(i) = t.find(pat) {
                let rest = &t[i + pat.len()..];
                self.csrf = rest[..rest.find('"').unwrap()].to_string();
            }
        }
        r
    }

    async fn get(&mut self, path: &str) -> Resp {
        self.send(Request::get(path).body(Body::empty()).unwrap()).await
    }

    async fn post(&mut self, path: &str, fields: &[(&str, &str)]) -> Resp {
        let mut body = format!("csrf_token={}", enc(&self.csrf));
        for (k, v) in fields {
            body.push_str(&format!("&{}={}", enc(k), enc(v)));
        }
        self.send(Request::post(path).header(header::CONTENT_TYPE, "application/x-www-form-urlencoded").body(Body::from(body)).unwrap())
            .await
    }

    async fn post_json(&mut self, path: &str, body: Value) -> Resp {
        self.send(
            Request::post(path)
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-csrf-token", &self.csrf)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    async fn upload(&mut self, path: &str, field: &str, filename: &str, bytes: &[u8]) -> Resp {
        let boundary = "XyZboundary123";
        let mut body = Vec::new();
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes());
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let sep = if path.contains('?') { '&' } else { '?' };
        let uri = format!("{path}{sep}csrf_token={}", enc(&self.csrf));
        self.send(
            Request::post(uri)
                .header(header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
    }

    async fn login(&mut self, user: &str, pass: &str) {
        self.get("/login").await.ok();
        self.post("/login", &[("username", user), ("password", pass)]).await.redirects_to("/");
    }
}

fn temp_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("gb-e2e-{}", crate::util::random_token(6)));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn app_with(storage: Storage, tweak: impl FnOnce(&mut Config)) -> (Router, std::path::PathBuf, AppState) {
    let dir = temp_dir();
    let mut config = Config::for_tests(dir.join("gradebook.db"));
    config.backup_dir = Some(dir.join("backups"));
    config.storage = storage;
    tweak(&mut config);
    let state = build_state(config).await.unwrap();
    crate::bootstrap(&state).unwrap();
    (routes::router(state.clone()), dir, state)
}

async fn app() -> (Router, std::path::PathBuf) {
    let (r, d, _) = app_with(Storage::Persistent, |_| {}).await;
    (r, d)
}

/// Each student's name and overall result, for comparing grids across ids.
fn results(g: &Value) -> Vec<(String, Value)> {
    let mut v: Vec<_> = g["students"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["name"].as_str().unwrap().to_string(), g["results"][s["id"].as_i64().unwrap().to_string()]["overall"].clone()))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

fn between<'a>(s: &'a str, start: &str, end: &str) -> &'a str {
    let i = s.find(start).unwrap_or_else(|| panic!("{start:?} not found")) + start.len();
    &s[i..i + s[i..].find(end).unwrap()]
}

#[tokio::test]
async fn full_teacher_workflow_saved_storage() {
    teacher_workflow(Storage::Persistent).await;
}

#[tokio::test]
async fn full_teacher_workflow_memory_storage() {
    teacher_workflow(Storage::Memory).await;
}

async fn teacher_workflow(storage: Storage) {
    let memory = storage == Storage::Memory;
    let (app, dir, state) = app_with(storage, |_| {}).await;
    let mut c = Client::new(app.clone());

    // Not signed in: pages redirect, the API says 401.
    c.get("/").await.redirects_to("/login");
    assert_eq!(c.get("/api/subjects/1/grid").await.status, StatusCode::UNAUTHORIZED);
    c.get("/healthz").await.ok();
    c.get("/static/app.css").await.ok();

    // Wrong password, then right.
    c.get("/login").await.ok();
    c.post("/login", &[("username", "admin"), ("password", "nope")]).await.redirects_to("/login");
    assert!(c.get("/login").await.text().contains("Wrong username or password"));
    c.login("admin", "Password1234").await;

    // First visit goes through setup.
    c.get("/").await.redirects_to("/setup");
    c.get("/setup").await.ok();
    c.post(
        "/settings",
        &[
            ("setup", "1"),
            ("mode", "secondary"),
            ("active_periods", "2"),
            ("prep_period", "4"),
            ("missing_policy", "ignore"),
            ("primary_color", "#003F5A"),
            ("accent_color", "#B1D2E1"),
            ("report_title", "Progress Report"),
            ("full_name", "Ms. Example"),
            ("school", "Example Secondary"),
            ("term_name", "Semester 1"),
        ],
    )
    .await
    .redirects_to("/");
    let home = c.get("/").await;
    assert!(home.ok().text().contains(if memory { "Open your workbook to get started" } else { "No classes yet" }));
    assert_eq!(home.text().contains("id=\"ws-bar\""), memory);

    // CSRF is enforced.
    let saved = std::mem::take(&mut c.csrf);
    assert_eq!(c.post("/classes/new", &[("name", "Sneaky")]).await.status, StatusCode::BAD_REQUEST);
    c.csrf = saved;

    // A class with the Ontario preset, and students.
    c.get("/classes/new").await.ok();
    let r = c
        .post("/classes/new", &[("name", "Grade 11 Biology"), ("course_code", "SBI3U"), ("period", "1"), ("preset", "ontario-secondary")])
        .await;
    r.redirects_to("/classes/1/students");
    c.post("/classes/1/students", &[("bulk", "Smith, Jane\nJohn Doe\nLefèvre, Zoë\t123456")]).await.redirects_to("/classes/1/students");
    let roster = c.get("/classes/1/students").await;
    assert!(roster.ok().text().contains("Lefèvre"), "{}", roster.text());

    // Grid page and API.
    let grid_page = c.get("/classes/1").await;
    let sid: i64 = between(&grid_page.ok().text(), "data-subject=\"", "\"").parse().unwrap();
    let g = c.get(&format!("/api/subjects/{sid}/grid")).await.json();
    assert_eq!(g["categories"].as_array().unwrap().len(), 5);
    assert_eq!(g["students"].as_array().unwrap().len(), 3);
    let ku = g["categories"][0]["id"].as_i64().unwrap();
    let students: Vec<i64> = g["students"].as_array().unwrap().iter().map(|s| s["id"].as_i64().unwrap()).collect();

    let g = c.post_json(&format!("/api/subjects/{sid}/assessments"), json!({ "name": "Quiz 1", "out_of": 20, "category_id": ku })).await;
    let g = g.json();
    let quiz = g["assessments"][0]["id"].as_i64().unwrap();
    let r = c
        .post_json(
            &format!("/api/subjects/{sid}/marks"),
            json!({ "changes": [
                { "student_id": students[0], "assessment_id": quiz, "input": "17" },
                { "student_id": students[1], "assessment_id": quiz, "input": "M" },
                { "student_id": students[2], "assessment_id": quiz, "input": "15/20L" },
                { "student_id": students[2], "assessment_id": 999_999, "input": "1" },
            ]}),
        )
        .await;
    let g = r.json();
    assert_eq!(g["results"][students[0].to_string()]["overall"].as_f64(), Some(85.0));
    assert_eq!(g["results"][students[0].to_string()]["label"], "A");
    assert_eq!(g["results"][students[1].to_string()]["missing"], 1);
    assert_eq!(g["cells"][students[2].to_string()][quiz.to_string()]["status"], "late");
    assert_eq!(g["undo"]["count"], 3);

    // Bad input is reported per cell and not saved.
    let g = c
        .post_json(
            &format!("/api/subjects/{sid}/marks"),
            json!({ "changes": [{ "student_id": students[0], "assessment_id": quiz, "input": "abc" }] }),
        )
        .await
        .json();
    assert_eq!(g["errors"].as_array().unwrap().len(), 1);
    assert_eq!(g["cells"][students[0].to_string()][quiz.to_string()]["text"], "17");

    // Fill blanks, rescale, move, undo.
    let g2 = c
        .post_json(&format!("/api/subjects/{sid}/assessments"), json!({ "name": "Journal 1", "kind": "completion", "unit": "Journals" }))
        .await
        .json();
    let journal = g2["assessments"][1]["id"].as_i64().unwrap();
    let g = c.post_json(&format!("/api/assessments/{journal}/fill"), json!({ "input": "Y", "only_empty": true })).await.json();
    assert_eq!(g["cells"][students[0].to_string()][journal.to_string()]["status"], "marked");
    let g = c.post_json(&format!("/api/subjects/{sid}/undo"), json!({})).await.json();
    assert!(g["cells"][students[0].to_string()].get(journal.to_string()).is_none());
    let g = c
        .post_json(&format!("/api/assessments/{quiz}"), json!({ "name": "Quiz 1", "out_of": 40, "category_id": ku, "rescale": true }))
        .await
        .json();
    assert_eq!(g["cells"][students[0].to_string()][quiz.to_string()]["text"], "34");
    assert_eq!(g["results"][students[0].to_string()]["overall"].as_f64(), Some(85.0));
    let g = c.post_json(&format!("/api/assessments/{journal}/move"), json!({ "dir": -1 })).await.json();
    assert_eq!(g["assessments"][0]["id"].as_i64(), Some(journal));

    // Student page, comment bank, comments, skills, override.
    c.post("/comments", &[("tag", "Strengths"), ("body", "{first} works hard and {their} effort shows.")]).await.redirects_to("/comments");
    c.get("/comments").await.ok();
    let st =
        g["students"].as_array().unwrap().iter().find(|s| s["name"].as_str().unwrap().contains("Jane")).unwrap()["id"].as_i64().unwrap();
    let page = c.get(&format!("/classes/1/students/{st}")).await;
    let t = page.ok().text();
    let k = t.find("dropdown-header").unwrap_or(0);
    assert!(t.contains("Jane works hard and their effort shows."), "{}", &t[k..(k + 600).min(t.len())]);
    c.post(
        &format!("/classes/1/students/{st}/comment"),
        &[("subject_id", &sid.to_string()), ("comment", "Great term, Jane! <b>#1</b> $5"), ("next_steps", "Keep going.")],
    )
    .await
    .redirects_to(&format!("/classes/1/students/{st}"));
    c.post(&format!("/classes/1/students/{st}/skills"), &[("skill_Responsibility", "E"), ("skill_Organization", "G")])
        .await
        .redirects_to("/classes/1/students");
    c.post(
        &format!("/classes/1/students/{st}/override"),
        &[("subject_id", &sid.to_string()), ("pct", "88"), ("reason", "recent evidence")],
    )
    .await;
    c.post(
        &format!("/classes/1/students/{st}/details"),
        &[("first_name", "Jane"), ("last_name", "Smith"), ("preferred_name", "Janie"), ("pronouns", "she")],
    )
    .await;
    let page = c.get(&format!("/classes/1/students/{st}")).await;
    let text = page.ok().text();
    assert!(text.contains("Set by you"));
    let k = text.find("Great term").expect("comment shown");
    assert!(!text.contains("<b>#1"), "comment must be escaped: {}", &text[k..k + 80]);

    // Reports: preview, download, class page, batch ZIP.
    let pdf = c.get(&format!("/classes/1/students/{st}/report.pdf")).await;
    assert!(pdf.ok().body.starts_with(b"%PDF"));
    let dl = c
        .post(
            &format!("/classes/1/students/{st}/report.pdf"),
            &[("report_type", "Midterm Report"), ("show_comments", "1"), ("show_assessments", "1")],
        )
        .await;
    assert!(dl.ok().body.starts_with(b"%PDF"));
    let reports = c.get("/classes/1/reports").await;
    assert!(reports.ok().text().contains("never"));
    let ids: Vec<String> = students.iter().map(|s| s.to_string()).collect();
    let mut fields: Vec<(&str, &str)> = ids.iter().map(|i| ("student", i.as_str())).collect();
    fields.push(("show_legend", "1"));
    let zip = c.post("/classes/1/reports", &fields).await;
    assert!(zip.ok().body.starts_with(b"PK"));
    assert_eq!(zip.content_type, "application/zip");
    let archive = zip::ZipArchive::new(std::io::Cursor::new(zip.body.clone())).unwrap();
    assert_eq!(archive.len(), 3);

    // Other class pages.
    c.get("/classes/1/insights").await.ok();
    c.get("/classes/1/settings").await.ok();
    let xlsx = c.get("/classes/1/export.xlsx").await;
    assert!(xlsx.ok().body.starts_with(b"PK"));
    c.post(
        &format!("/classes/1/subjects/{sid}/categories"),
        &[("cat_id", &ku.to_string()), ("cat_name", "Knowledge"), ("cat_weight", "60"), ("cat_name", "Other"), ("cat_weight", "40")],
    )
    .await
    .redirects_to("/classes/1/settings");
    let g = c.get(&format!("/api/subjects/{sid}/grid")).await.json();
    assert_eq!(g["categories"].as_array().unwrap().len(), 2);

    // Import the sample spreadsheet into a new class (the second and last period).
    let bytes = std::fs::read("Grades.xlsx").unwrap();
    c.get("/import").await.ok();
    let r = c.upload("/import", "file", "Grades.xlsx", &bytes).await;
    r.redirects_to("/import/");
    let map_url = r.location.clone().unwrap();
    let map = c.get(&map_url).await;
    map.ok();
    let grid = crate::import::parse_file(&bytes, "Grades.xlsx").unwrap();
    let roles = crate::import::guess_roles(&grid);
    let role_fields: Vec<(String, String)> = roles.iter().enumerate().map(|(i, r)| (format!("role_{i}"), r.as_str())).collect();
    let mut fields: Vec<(&str, &str)> = role_fields.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    fields.extend([
        ("target", "new"),
        ("new_class_name", "Imported Class"),
        ("new_class_period", "2"),
        ("blank_as", "skip"),
        ("zero_as", "missing"),
    ]);
    let preview = c.post(&map_url, &fields).await;
    let ptext = preview.ok().text();
    assert!(ptext.contains("Ready to import"), "{ptext}");
    // The preview must not have created the class.
    assert!(!c.get("/").await.text().contains("Imported Class"));
    let applied = c.post(&format!("{map_url}/apply"), &fields).await;
    applied.redirects_to("/classes/");
    let dash = c.get("/").await;
    let dtext = dash.ok().text();
    assert!(dtext.contains("Imported Class"));
    assert!(dtext.contains("Prep"));
    // Two periods are set up and both are used now.
    assert!(!dtext.contains("/classes/new\""));
    c.get("/classes/new").await.redirects_to("/");

    // Remaining pages.
    for p in ["/terms", "/settings", "/profile", "/admin/users", "/admin/backups"] {
        c.get(p).await.ok();
    }
    c.post("/admin/backups", &[]).await.redirects_to("/admin/backups");
    assert!(std::fs::read_dir(dir.join("backups")).unwrap().count() >= 1);
    let snap = c.get("/admin/backups/download").await;
    assert!(snap.ok().body.starts_with(b"SQLite format 3"));

    // A second teacher can't see the first one's class.
    c.post("/admin/users", &[("username", "teacher2"), ("password", "Another12345")]).await.redirects_to("/admin/users");
    let mut t2 = Client::new(app.clone());
    t2.login("teacher2", "Another12345").await;
    assert_eq!(t2.get("/classes/1").await.status, StatusCode::NOT_FOUND);
    assert_eq!(t2.get(&format!("/api/subjects/{sid}/grid")).await.status, StatusCode::NOT_FOUND);
    assert_eq!(t2.get(&format!("/classes/1/students/{st}/report.pdf")).await.status, StatusCode::NOT_FOUND);
    assert_eq!(t2.get("/admin/users").await.status, StatusCode::FORBIDDEN);

    // Save the workbook.
    let bio = c.get("/classes/1").await.text();
    let sid: i64 = between(&bio, "data-subject=\"", "\"").parse().unwrap();
    let before = results(&c.get(&format!("/api/subjects/{sid}/grid")).await.json());
    if memory {
        assert!(c.get("/").await.text().contains("Unsaved changes"));
    }
    let wb = c.get("/workbook.xlsx").await;
    assert!(wb.ok().body.starts_with(b"PK"));
    assert!(crate::workbook::is_workbook(&wb.body));
    if memory {
        assert!(c.get("/").await.text().contains("Saved to your workbook at"));
        // Nothing about students ever reached the server's file.
        assert_eq!(state.db.stored_student_rows().unwrap(), 0);
        assert_eq!(state.workspaces.len(), 2, "both teachers have a workspace open");
    }

    // Logout ends the session.
    c.post("/logout", &[]).await.redirects_to("/login");
    c.get("/").await.redirects_to("/login");

    c.login("admin", "Password1234").await;
    if memory {
        // Signing out cleared everything; the workbook brings it all back.
        assert!(c.get("/").await.text().contains("Open your workbook to get started"));
        c.get("/workbook").await.ok();
        let r = c.upload("/workbook", "file", "Gradebook.xlsx", &wb.body).await;
        r.redirects_to("/");
        let home = c.get("/").await.text();
        assert!(home.contains("Workbook opened: 2 classes, 8 students"), "{}", between(&home, "alert-", "</div>"));
        assert!(home.contains("Grade 11 Biology") && home.contains("Imported Class"));
        assert!(!home.contains("Unsaved changes"));
        let bio = c.get("/classes/1").await.text();
        assert!(bio.contains("Grade 11 Biology"));
        let sid: i64 = between(&bio, "data-subject=\"", "\"").parse().unwrap();
        assert_eq!(results(&c.get(&format!("/api/subjects/{sid}/grid")).await.json()), before);
        let roster = c.get("/classes/1/students").await.text();
        let st: i64 = between(&roster[roster.find("Smith").unwrap() - 200..], "/classes/1/students/", "\"").parse().unwrap();
        let page = c.get(&format!("/classes/1/students/{st}")).await.text();
        assert!(page.contains("Great term, Jane!") && page.contains("Set by you"));
        // Report history came back too.
        assert!(!c.get("/classes/1/reports").await.text().contains(">never<"));
        // Opening is the same as Import for a workbook.
        let r = c.upload("/import", "file", "Gradebook.xlsx", &wb.body).await;
        r.redirects_to("/");
        assert_eq!(state.db.stored_student_rows().unwrap(), 0);
        // Signing out drops the workspace.
        c.post("/logout", &[]).await.redirects_to("/login");
        assert!(state.workspaces.peek(1).is_none());
    } else {
        // Saved storage: opening a workbook adds a copy alongside.
        c.get("/workbook").await.ok();
        let r = c.upload("/workbook", "file", "Gradebook.xlsx", &wb.body).await;
        r.redirects_to("/");
        assert!(c.get("/terms").await.text().contains("(opened"));
        assert!(state.db.stored_student_rows().unwrap() > 0);
    }

    // Audit log.
    let mut a = Client::new(app.clone());
    a.login("admin", "Password1234").await;
    let audit = a.get("/admin/audit").await;
    let t = audit.ok().text();
    for action in ["login.failed", "login", "report.batch", "workbook.save", "workbook.open", "admin.user_add", "admin.backup_download"] {
        assert!(t.contains(&format!("<code>{action}</code>")), "audit is missing {action}");
    }
    assert!(!t.contains("Jane") && !t.contains("Smith"), "the audit log must not name students");

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn elementary_mode_has_many_subjects() {
    let (app, dir) = app().await;
    let mut c = Client::new(app);
    c.login("admin", "Password1234").await;
    c.get("/setup").await.ok();
    c.post(
        "/settings",
        &[("setup", "1"), ("mode", "elementary"), ("primary_color", "#003F5A"), ("accent_color", "#B1D2E1"), ("term_name", "2026–27")],
    )
    .await
    .redirects_to("/");
    c.get("/classes/new").await.ok();
    c.post("/classes/new", &[("name", "Grade 3"), ("subjects", "Language\nMath\nScience"), ("letter_grades", "1"), ("preset", "none")])
        .await
        .redirects_to("/classes/1/students");
    let page = c.get("/classes/1").await;
    let text = page.ok().text();
    assert!(text.contains(">Language<") && text.contains(">Math<") && text.contains(">Science<"));
    c.post("/classes/1/students", &[("first_name", "Ana"), ("last_name", "Lee")]).await;
    let st = c.get("/classes/1/students").await.text();
    let sid: i64 = between(&st, "/classes/1/students/", "\"").parse().unwrap();
    let pdf = c.get(&format!("/classes/1/students/{sid}/report.pdf")).await;
    assert!(pdf.ok().body.starts_with(b"%PDF"));
    // Elementary has no period limit.
    c.post("/classes/new", &[("name", "Grade 3 French"), ("preset", "none")]).await.redirects_to("/classes/2/students");
    let _ = std::fs::remove_dir_all(dir);
}

// ── Local mode ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn local_mode_launch_link_host_check_and_quit() {
    let dir = temp_dir();
    let config = Config::local(dir.clone(), 0, Storage::Memory);
    let token = config.launch_token.clone().unwrap();
    let state = build_state(config).await.unwrap();
    crate::bootstrap(&state).unwrap();
    let app = routes::router(state.clone());

    // Another site pointing its name at 127.0.0.1 (DNS rebinding) is refused.
    let mut evil = Client::new(app.clone());
    evil.host = Some("evil.example:8080".into());
    assert_eq!(evil.get(&format!("/launch?token={token}")).await.status, StatusCode::FORBIDDEN);

    let mut c = Client::new(app.clone());
    c.host = Some("127.0.0.1:8123".into());
    // No passwords: the login page points at the app instead.
    let login = c.get("/login").await;
    assert!(login.ok().text().contains("signs you in automatically"));
    assert!(!login.text().contains("name=\"password\""));
    c.post("/login", &[("username", "teacher"), ("password", "!local")]).await;
    c.get("/").await.redirects_to("/login");
    assert_eq!(c.get("/launch?token=wrong").await.status, StatusCode::BAD_REQUEST);
    c.get(&format!("/launch?token={token}")).await.redirects_to("/");
    c.get("/").await.redirects_to("/setup");
    c.get("/setup").await.ok();
    c.post(
        "/settings",
        &[("setup", "1"), ("mode", "elementary"), ("primary_color", "#003F5A"), ("accent_color", "#B1D2E1"), ("term_name", "2026–27")],
    )
    .await
    .redirects_to("/");
    let home = c.get("/").await.text();
    assert!(home.contains("on this computer") && home.contains("Quit Gradebook"));
    assert!(!home.contains("/admin/users") && !home.contains("Sign out"));
    // Password and 2-step settings don't apply.
    let profile = c.get("/profile").await.text();
    assert!(!profile.contains("Change password") && !profile.contains("Two-step"));
    assert_eq!(
        c.post("/profile/password", &[("current_password", "!local"), ("new_password", "x"), ("confirm_password", "x")]).await.status,
        StatusCode::BAD_REQUEST
    );

    c.post("/classes/new", &[("name", "Grade 3"), ("preset", "none")]).await.redirects_to("/classes/1/students");
    assert_eq!(state.db.stored_student_rows().unwrap(), 0);

    // Quit stops the server.
    let quit = c.post("/quit", &[]).await;
    assert!(quit.ok().text().contains("Gradebook has closed"));
    tokio::time::timeout(std::time::Duration::from_secs(1), state.shutdown.notified()).await.expect("shutdown was signalled");
    let _ = std::fs::remove_dir_all(dir);
}

// ── Board single sign-on, against a mock identity provider ──────────────────

mod idp {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use axum::{
        Form, Json, Router,
        extract::State,
        routing::{get, post},
    };
    use openidconnect::{
        Audience, EmptyAdditionalClaims, EndUserEmail, IssuerUrl, JsonWebKeySet, Nonce, PrivateSigningKey, StandardClaims,
        SubjectIdentifier,
        core::{CoreIdToken, CoreIdTokenClaims, CoreJwsSigningAlgorithm, CoreRsaPrivateSigningKey},
    };
    use serde_json::{Value, json};

    pub const CLIENT_ID: &str = "gradebook-test";

    /// What the provider will say about whoever signs in with a given code.
    #[derive(Clone)]
    pub struct Grant {
        pub nonce: String,
        pub subject: String,
        pub email: String,
        pub verified: bool,
    }

    #[derive(Clone)]
    pub struct Idp {
        pub issuer: String,
        pub grants: Arc<Mutex<HashMap<String, Grant>>>,
        key: Arc<CoreRsaPrivateSigningKey>,
    }

    pub async fn start() -> Idp {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let key = CoreRsaPrivateSigningKey::from_pem(
            include_str!("testdata/oidc-test-only-key.pem"),
            Some(openidconnect::JsonWebKeyId::new("k1".into())),
        )
        .unwrap();
        let idp = Idp { issuer, grants: Arc::default(), key: Arc::new(key) };
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .route("/token", post(token))
            .with_state(idp.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        idp
    }

    async fn discovery(State(idp): State<Idp>) -> Json<Value> {
        let i = &idp.issuer;
        Json(json!({
            "issuer": i,
            "authorization_endpoint": format!("{i}/authorize"),
            "token_endpoint": format!("{i}/token"),
            "jwks_uri": format!("{i}/jwks"),
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"],
        }))
    }

    async fn jwks(State(idp): State<Idp>) -> Json<Value> {
        Json(serde_json::to_value(JsonWebKeySet::new(vec![idp.key.as_verification_key()])).unwrap())
    }

    async fn token(State(idp): State<Idp>, Form(f): Form<HashMap<String, String>>) -> Json<Value> {
        let g = idp.grants.lock().unwrap().remove(&f["code"]).expect("unknown code");
        assert!(f.contains_key("code_verifier"), "PKCE verifier must be sent");
        let now = chrono::Utc::now();
        let claims = CoreIdTokenClaims::new(
            IssuerUrl::new(idp.issuer.clone()).unwrap(),
            vec![Audience::new(CLIENT_ID.into())],
            now + chrono::Duration::minutes(5),
            now,
            StandardClaims::new(SubjectIdentifier::new(g.subject))
                .set_email(Some(EndUserEmail::new(g.email)))
                .set_email_verified(Some(g.verified)),
            EmptyAdditionalClaims {},
        )
        .set_nonce(Some(Nonce::new(g.nonce)));
        let id = CoreIdToken::new(claims, idp.key.as_ref(), CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256, None, None).unwrap();
        Json(json!({ "access_token": "at", "token_type": "Bearer", "expires_in": 300, "id_token": id.to_string() }))
    }
}

fn query_param(url: &str, name: &str) -> String {
    openidconnect::url::Url::parse(url).unwrap().query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned()).unwrap_or_default()
}

/// Run the browser side of a sign-in: start, "authenticate" at the provider, come back.
async fn sso_sign_in(c: &mut Client, idp: &idp::Idp, subject: &str, email: &str, verified: bool) -> Resp {
    c.get("/login").await.ok();
    let r = c.get("/login/oidc?next=/terms").await;
    let auth_url = r.location.clone().expect("redirect to the provider");
    assert!(auth_url.starts_with(&format!("{}/authorize", idp.issuer)));
    assert_eq!(query_param(&auth_url, "code_challenge_method"), "S256");
    let code = crate::util::random_token(8);
    idp.grants.lock().unwrap().insert(
        code.clone(),
        idp::Grant { nonce: query_param(&auth_url, "nonce"), subject: subject.into(), email: email.into(), verified },
    );
    let state = query_param(&auth_url, "state");
    c.get(&format!("/login/oidc/callback?code={code}&state={}", enc(&state))).await
}

#[tokio::test]
async fn board_single_sign_on() {
    let idp = idp::start().await;
    let issuer = idp.issuer.clone();
    let (app, dir, state) = app_with(Storage::Memory, |c| {
        c.auth = crate::config::AuthMode::Oidc;
        c.admin_username = None;
        c.admin_password = None;
        c.oidc = Some(crate::config::Oidc {
            issuer,
            client_id: idp::CLIENT_ID.into(),
            client_secret: Some("secret".into()),
            public_url: "http://gradebook.test".into(),
            display_name: "Example Board".into(),
            allowed_domains: vec!["board.example".into()],
            admin_emails: vec!["head@board.example".into()],
            allow_password_login: false,
        });
    })
    .await;

    // The login page offers only single sign-on, and passwords are refused.
    let mut c = Client::new(app.clone());
    let login = c.get("/login").await;
    assert!(login.ok().text().contains("Sign in with Example Board"));
    assert!(!login.text().contains("name=\"password\""));
    assert_eq!(c.post("/login", &[("username", "x"), ("password", "y")]).await.status, StatusCode::NOT_FOUND);

    // A teacher signs in; their account is created on the way.
    sso_sign_in(&mut c, &idp, "sub-1", "Ms.Lee@Board.Example", true).await.redirects_to("/terms");
    let profile = c.get("/profile").await.text();
    assert!(profile.contains("ms.lee@board.example") && !profile.contains("Change password"));
    assert_eq!(c.get("/admin/users").await.status, StatusCode::FORBIDDEN);
    c.post("/logout", &[]).await.redirects_to("/login");
    // Next time is the same account, even if the email changed.
    sso_sign_in(&mut c, &idp, "sub-1", "lee.m@board.example", true).await.redirects_to("/terms");
    let n: i64 = state.db.call(|c| Ok(c.query_row("SELECT count(*) FROM users", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(n, 1);

    // Wrong domain, or an unverified email, is refused.
    let mut o = Client::new(app.clone());
    sso_sign_in(&mut o, &idp, "sub-2", "someone@gmail.com", true).await.redirects_to("/login");
    let l = o.get("/login").await.text();
    assert!(l.contains("from a school board that uses"), "{}", between(&l, "<main", "</main>"));
    sso_sign_in(&mut o, &idp, "sub-3", "fake@board.example", false).await.redirects_to("/login");
    o.get("/").await.redirects_to("/login");

    // A tampered state, or a replayed callback, goes nowhere.
    let mut t = Client::new(app.clone());
    t.get("/login").await.ok();
    let r = t.get("/login/oidc").await;
    let url = r.location.unwrap();
    let code = "c-tamper".to_string();
    idp.grants.lock().unwrap().insert(
        code.clone(),
        idp::Grant { nonce: query_param(&url, "nonce"), subject: "sub-1".into(), email: "x@board.example".into(), verified: true },
    );
    t.get(&format!("/login/oidc/callback?code={code}&state=forged")).await.redirects_to("/login");
    t.get(&format!("/login/oidc/callback?code={code}&state={}", enc(&query_param(&url, "state")))).await.redirects_to("/login");
    t.get("/").await.redirects_to("/login");

    // The listed head of department is an admin.
    let mut h = Client::new(app.clone());
    sso_sign_in(&mut h, &idp, "sub-9", "head@board.example", true).await.redirects_to("/terms");
    let users = h.get("/admin/users").await;
    assert!(users.ok().text().contains("SSO") && !users.text().contains("Create account"));
    let audit = h.get("/admin/audit").await.text();
    assert!(audit.contains("single sign-on") && audit.contains("domain not allowed"));
    let _ = std::fs::remove_dir_all(dir);
}

// ── Retention ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn retention_deletes_long_archived_terms() {
    let (app, dir, state) = app_with(Storage::Persistent, |c| c.retention_days = Some(30)).await;
    let mut c = Client::new(app);
    c.login("admin", "Password1234").await;
    c.get("/setup").await.ok();
    c.post(
        "/settings",
        &[("setup", "1"), ("mode", "elementary"), ("primary_color", "#003F5A"), ("accent_color", "#B1D2E1"), ("term_name", "Old term")],
    )
    .await
    .redirects_to("/");
    c.post("/classes/new", &[("name", "Grade 3"), ("preset", "none")]).await.redirects_to("/classes/1/students");
    c.post("/classes/1/students", &[("first_name", "Ana"), ("last_name", "Lee")]).await;
    // Archived 40 days ago, and one archived yesterday.
    state
        .db
        .call(|c| {
            c.execute("UPDATE terms SET archived = 1, archived_at = datetime('now', '-40 days')", [])?;
            c.execute("INSERT INTO terms (user_id, name, archived, archived_at) VALUES (1, 'Recent', 1, datetime('now', '-1 day'))", [])?;
            Ok(())
        })
        .await
        .unwrap();
    crate::audit::housekeeping(&state).await.unwrap();
    let (terms, students): (Vec<String>, i64) = state
        .db
        .call(|c| {
            let t = c.prepare("SELECT name FROM terms")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
            Ok((t, c.query_row("SELECT count(*) FROM students", [], |r| r.get(0))?))
        })
        .await
        .unwrap();
    assert_eq!(terms, vec!["Recent".to_string()]);
    assert_eq!(students, 0);
    let _ = std::fs::remove_dir_all(dir);
}

// ── Starter spreadsheet ─────────────────────────────────────────────────────

fn filled_template(extra_student: bool, quiz_for_ana: &str) -> Vec<u8> {
    use crate::starter::{AssessmentSpec as A, SheetSpec, StudentSpec as S, Template};
    let row = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut students = vec![
        S { number: "1001".into(), last: "Lee".into(), first: "Ana".into(), pronouns: "she".into() },
        S { last: "Khan".into(), first: "Omar".into(), ..Default::default() },
        S { last: "Ng".into(), first: "Zed".into(), ..Default::default() },
    ];
    let mut marks = vec![row(&[quiz_for_ana, "3/4", "Y"]), row(&["M", "85%L", "N"]), row(&["abc", "?", ""])];
    if extra_student {
        students.push(S { last: "Roy".into(), first: "Bea".into(), ..Default::default() });
        marks.push(row(&["X", "", "Y"]));
    }
    let bio = SheetSpec {
        title: "Period 1".into(),
        class: "Grade 11 Biology".into(),
        course_code: "SBI3U".into(),
        period: Some(1),
        categories: vec![("Knowledge".into(), 60.0), ("Application".into(), 40.0)],
        assessments: vec![
            A { name: "Quiz 1".into(), category: "Knowledge".into(), out_of: 20.0, weight: 1.0, done: false },
            A { name: "Lab 1".into(), category: "Application".into(), out_of: 40.0, weight: 2.0, done: false },
            A { name: "Journal 1".into(), category: String::new(), out_of: 1.0, weight: 1.0, done: true },
        ],
        students,
        marks,
        ..Default::default()
    };
    let chem = SheetSpec {
        title: "Period 3".into(),
        class: "Grade 12 Chemistry".into(),
        period: Some(3),
        assessments: vec![A { name: "Test".into(), category: String::new(), out_of: 50.0, weight: 1.0, done: false }],
        students: vec![S { number: "1001".into(), last: "Lee".into(), first: "Ana".into(), ..Default::default() }],
        marks: vec![row(&["45"])],
        ..Default::default()
    };
    // Over the two periods this teacher is set up for.
    let extra = SheetSpec { title: "Extra".into(), class: "Grade 9 Science".into(), ..Default::default() };
    let extra = SheetSpec { students: vec![S { last: "A".into(), first: "B".into(), ..Default::default() }], ..extra };
    crate::starter::build(&Template { missing_policy: crate::engine::MissingPolicy::Ignore, sheets: vec![bio, chem, extra] }).unwrap()
}

#[tokio::test]
async fn starter_spreadsheet_saved_storage() {
    starter_spreadsheet(Storage::Persistent).await;
}

#[tokio::test]
async fn starter_spreadsheet_memory_storage() {
    starter_spreadsheet(Storage::Memory).await;
}

async fn starter_spreadsheet(storage: Storage) {
    let (app, dir, state) = app_with(storage, |_| {}).await;
    let mut c = Client::new(app);
    c.login("admin", "Password1234").await;
    c.get("/setup").await.ok();
    c.post(
        "/settings",
        &[
            ("setup", "1"),
            ("mode", "secondary"),
            ("active_periods", "2"),
            ("prep_period", "2"),
            ("missing_policy", "ignore"),
            ("primary_color", "#003F5A"),
            ("accent_color", "#B1D2E1"),
            ("term_name", "Semester 1"),
        ],
    )
    .await
    .redirects_to("/");
    let page = c.get("/import").await.text();
    assert!(page.contains("/import/template.xlsx") && !page.contains("filled=1"));

    // The blank template: shaped for two periods around the prep.
    let blank = c.get("/import/template.xlsx").await;
    assert!(blank.ok().body.starts_with(b"PK"));
    assert!(crate::starter::is_template(&blank.body));
    let x = calamine::open_workbook_from_rs::<calamine::Xlsx<_>, _>(std::io::Cursor::new(blank.body.clone())).unwrap();
    use calamine::Reader;
    assert_eq!(x.sheet_names(), ["How to fill this in", "Example - Math", "Period 1", "Period 3", "_gradebook_template"]);
    // Uploading it unfilled says so.
    c.upload("/import", "file", "Gradebook starter spreadsheet.xlsx", &blank.body).await.redirects_to("/import");
    assert!(c.get("/import").await.text().contains("have any classes filled in yet"));

    // Filled in: the preview shows what will happen and where the problems are.
    let r = c.upload("/import", "file", "mine.xlsx", &filled_template(false, "17")).await;
    r.redirects_to("/import/sheet/");
    let url = r.location.clone().unwrap();
    let preview = c.get(&url).await;
    let t = preview.ok().text();
    assert!(
        t.contains("New class: <strong>Grade 11 Biology</strong>") && t.contains("New class: <strong>Grade 12 Chemistry</strong>"),
        "{t}"
    );
    assert!(t.contains("<code>E16</code>") && t.contains("abc"), "the bad mark is located");
    assert!(t.contains("Grade 9 Science wasn") && t.contains("2 teaching period"), "over the period limit");
    assert!(t.contains("3 new students"), "Ana is one student in two classes");
    // The preview saved nothing.
    assert!(c.get("/").await.text().contains("No classes yet") || storage == Storage::Memory);
    c.post(&url, &[]).await.redirects_to("/");
    let home = c.get("/").await.text();
    assert!(
        home.contains("Spreadsheet imported: 2 new classes, 3 new students, 4 new assessments"),
        "{}",
        between(&home, "alert-", "</div>")
    );
    assert!(home.contains("Grade 11 Biology") && home.contains("Grade 12 Chemistry") && !home.contains("Grade 9 Science"));

    // The marks are what the legend says.
    let bio = c.get("/classes/1").await.text();
    let sid: i64 = between(&bio, "data-subject=\"", "\"").parse().unwrap();
    let g = c.get(&format!("/api/subjects/{sid}/grid")).await.json();
    let id_of = |name: &str| {
        g["students"].as_array().unwrap().iter().find(|s| s["name"].as_str().unwrap().contains(name)).unwrap()["id"]
            .as_i64()
            .unwrap()
            .to_string()
    };
    let col =
        |name: &str| g["assessments"].as_array().unwrap().iter().find(|a| a["name"] == name).unwrap()["id"].as_i64().unwrap().to_string();
    let (ana, omar, zed) = (id_of("Ana"), id_of("Omar"), id_of("Zed"));
    let (quiz, lab, journal) = (col("Quiz 1"), col("Lab 1"), col("Journal 1"));
    assert_eq!(g["cells"][&ana][&quiz]["text"], "17");
    assert_eq!(g["cells"][&ana][&lab]["text"], "30", "3/4 of 40");
    assert_eq!(g["cells"][&omar][&quiz]["status"], "missing");
    assert_eq!(g["cells"][&omar][&lab]["status"], "late");
    assert_eq!(g["cells"][&omar][&lab]["text"], "34L");
    assert_eq!(g["cells"][&omar][&journal]["status"], "missing");
    assert_eq!(g["cells"][&zed][&lab]["status"], "pending");
    assert!(g["cells"][&zed].get(&quiz).is_none(), "the bad cell was skipped");
    assert_eq!(g["categories"].as_array().unwrap().len(), 2);
    // Missing work is left out (the teacher's setting): Omar's mark is just the late lab.
    assert_eq!(g["results"][&omar]["overall"].as_f64(), Some(85.0));

    // Download it filled in and upload it unchanged: nothing to do.
    let filled = c.get("/import/template.xlsx?filled=1").await;
    assert!(filled.ok().body.starts_with(b"PK"));
    let p = crate::starter::parse(&filled.body).unwrap();
    assert_eq!(p.sheets.len(), 2);
    let r = c.upload("/import", "file", "again.xlsx", &filled.body).await;
    let again = c.get(r.location.as_deref().unwrap()).await.text();
    assert!(again.contains("already in Gradebook"), "{}", between(&again, "<main", "</main>"));

    // Keep marking in the spreadsheet: a changed mark and a new student.
    let r = c.upload("/workbook", "file", "week2.xlsx", &filled_template(true, "19")).await;
    r.redirects_to("/import/sheet/");
    let url = r.location.clone().unwrap();
    let t = c.get(&url).await.text();
    assert!(t.contains("Update: <strong>Grade 11 Biology</strong>") && t.contains("1 new student"), "{t}");
    c.post(&url, &[]).await.redirects_to("/");
    let g = c.get(&format!("/api/subjects/{sid}/grid")).await.json();
    assert_eq!(g["cells"][&ana][&quiz]["text"], "19");
    assert_eq!(g["students"].as_array().unwrap().len(), 4);
    assert_eq!(g["assessments"].as_array().unwrap().len(), 3, "no duplicate columns");
    // And it can be undone from the grid.
    let g = c.post_json(&format!("/api/subjects/{sid}/undo"), json!({})).await.json();
    assert_eq!(g["cells"][&ana][&quiz]["text"], "17");

    if storage == Storage::Memory {
        assert_eq!(state.db.stored_student_rows().unwrap(), 0);
    }
    let _ = std::fs::remove_dir_all(dir);
}
