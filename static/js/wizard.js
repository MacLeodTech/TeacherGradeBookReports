// Guided setup. Everything the teacher types stays in this page's memory (nothing in
// localStorage) until the last step, when it is sent once to /api/start.
(function () {
  "use strict";
  const root = document.getElementById("wizard");
  if (!root) return;
  const cfg = JSON.parse(root.dataset.config);
  const memory = root.dataset.storage === "memory";
  const csrf = document.querySelector('meta[name="csrf-token"]').content;
  const mode = cfg.mode;
  const STEPS = ["Class", "Students", "Marking", "Assignments"];

  const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
  const presetOf = (v) => cfg.presets.find((p) => p.value === v) || cfg.presets[0];

  const state = { editing: null, step: root.dataset.firstRun === "1" ? "welcome" : "class", done: [], cur: null, result: null, busy: false, error: "" };
  let finished = false;

  function blankClass() {
    const used = state.done.map((c) => c.period).filter(Boolean);
    const free = cfg.freePeriods.filter((p) => !used.includes(p));
    return {
      name: "",
      course_code: "",
      period: free[0] ?? null,
      grade_level: "",
      room: "",
      preset: cfg.presets[0].value,
      subjects: "Language\nMathematics\nScience and Technology\nSocial Studies\nThe Arts\nHealth and Physical Education",
      letter_grades: false,
      studentsText: "",
      nameOrder: "first",
      assessments: [],
    };
  }

  // ── names ──────────────────────────────────────────────────────────────────
  function parseNames(text, order) {
    const out = [];
    for (let line of text.split(/\r?\n/)) {
      line = line.trim();
      if (!line) continue;
      let first = "", last = "";
      if (line.includes("\t")) {
        const cells = line.split("\t").map((c) => c.trim()).filter(Boolean);
        if (cells.length === 1) [first, last] = splitOne(cells[0], order);
        else [first, last] = order === "last" ? [cells[1], cells[0]] : [cells[0], cells[1]];
      } else if (line.includes(",")) {
        const i = line.indexOf(",");
        last = line.slice(0, i).trim();
        first = line.slice(i + 1).trim();
      } else [first, last] = splitOne(line, order);
      if (first || last) out.push({ first, last });
    }
    return out;
  }
  function splitOne(s, order) {
    const w = s.split(/\s+/);
    if (w.length === 1) return [w[0], ""];
    return order === "last" ? [w.slice(1).join(" "), w[0]] : [w.slice(0, -1).join(" "), w[w.length - 1]];
  }
  const subjectList = (c) => (mode === "elementary" ? c.subjects.split(/\r?\n/).map((s) => s.trim()).filter(Boolean) : []);
  const catNames = (c) => presetOf(c.preset).categories.map((x) => x[0]);

  // ── views ──────────────────────────────────────────────────────────────────
  function progress(n) {
    return `<ol class="wiz-steps list-unstyled d-flex gap-2 mb-4 flex-wrap">${STEPS.map(
      (s, i) => `<li class="wiz-step ${i + 1 === n ? "active" : i + 1 < n ? "past" : ""}"><span class="num">${i + 1 < n ? '<i class="bi bi-check-lg"></i>' : i + 1}</span> ${s}</li>`
    ).join("")}</ol>`;
  }

  function shell(n, title, lead, body, nav) {
    const label = state.done.length ? `<div class="small text-muted mb-1">Class ${state.done.length + 1}</div>` : "";
    return `${progress(n)}<div class="card shadow-sm"><div class="card-body p-4">
      ${label}<h1 class="h4">${title}</h1><p class="text-muted">${lead}</p>
      ${body}
      <div class="text-danger small mt-3" role="alert" id="wiz-error">${esc(state.error)}</div>
      <div class="d-flex gap-2 mt-4">${nav}</div></div></div>`;
  }

  const views = {
    welcome() {
      return `<div class="text-center mb-4"><i class="bi bi-journal-check display-4 text-primary"></i>
        <h1 class="h3 mt-2">Let's set up your gradebook</h1>
        <p class="text-muted mx-auto" style="max-width:34rem">We'll ask about your class, your students, how marks count, and any assignments you already have. Every step can be skipped and changed later. It takes about five minutes.</p></div>
        <div class="row g-3 justify-content-center">
          <div class="col-md-5"><button class="btn btn-primary btn-lg w-100 h-100 py-3" data-go="class"><i class="bi bi-stars"></i><br>Start fresh<br><small class="fw-normal">Walk me through it</small></button></div>
          <div class="col-md-5"><a class="btn btn-outline-secondary btn-lg w-100 h-100 py-3" href="/workbook"><i class="bi bi-folder2-open"></i><br>Open my saved workbook<br><small class="fw-normal">I've used Gradebook before</small></a></div>
          <div class="col-md-5"><a class="btn btn-outline-secondary w-100" href="/import"><i class="bi bi-upload"></i> I have a spreadsheet</a></div>
        </div>`;
    },

    class() {
      const c = state.cur;
      const el = mode === "elementary";
      const periods = cfg.freePeriods.filter((p) => !state.done.some((d) => d.period === p));
      const body = `<div class="row g-3">
        <div class="col-md-7"><label class="form-label" for="w-name">${el ? "Class name" : "Course name"}</label>
          <input class="form-control form-control-lg" id="w-name" data-f="name" value="${esc(c.name)}" placeholder="${el ? "Grade 3 – Room 12" : mode === "university" ? "Intro to Psychology" : "Grade 11 Biology"}" autocomplete="off"></div>
        ${el ? `<div class="col-md-2"><label class="form-label" for="w-grade">Grade</label><input class="form-control" id="w-grade" data-f="grade_level" value="${esc(c.grade_level)}" placeholder="3"></div>
                <div class="col-md-3"><label class="form-label" for="w-room">Room</label><input class="form-control" id="w-room" data-f="room" value="${esc(c.room)}"></div>`
              : `<div class="col-md-5"><label class="form-label" for="w-code">Course code <span class="text-muted">(optional)</span></label><input class="form-control" id="w-code" data-f="course_code" value="${esc(c.course_code)}" placeholder="${mode === "university" ? "PSYC 101" : "SBI3U"}"></div>`}
        ${mode === "secondary" ? `<div class="col-md-4"><label class="form-label" for="w-period">Which period?</label>
          <select class="form-select" id="w-period" data-f="period" data-num="1">${periods.map((p) => `<option value="${p}" ${c.period === p ? "selected" : ""}>Period ${p}</option>`).join("")}<option value="" ${c.period == null ? "selected" : ""}>Not sure yet</option></select></div>` : ""}
        ${el ? `<div class="col-12"><label class="form-label" for="w-subjects">Subjects you mark <span class="text-muted">(one per line)</span></label>
          <textarea class="form-control" id="w-subjects" data-f="subjects" rows="6">${esc(c.subjects)}</textarea>
          <div class="form-check mt-2"><input class="form-check-input" type="checkbox" id="w-letter" data-f="letter_grades" ${c.letter_grades ? "checked" : ""}><label class="form-check-label" for="w-letter">My reports show letter grades instead of percentages</label></div></div>` : ""}
      </div>`;
      const back = state.done.length ? `<button class="btn btn-link" data-go="more">Back</button>` : root.dataset.firstRun === "1" ? `<button class="btn btn-link" data-go="welcome">Back</button>` : `<a class="btn btn-link" href="/">Cancel</a>`;
      return shell(1, "Tell us about your class", mode === "elementary" ? "Name your class and the subjects you'll record marks for." : "Just the basics. You can change any of this later.", body, `${back}<button class="btn btn-primary ms-auto" data-next="students">Next: students <i class="bi bi-arrow-right"></i></button>`);
    },

    students() {
      const c = state.cur;
      const names = parseNames(c.studentsText, c.nameOrder);
      const body = `<label class="form-label" for="w-students">Paste or type your class list, one student per line</label>
        <textarea class="form-control" id="w-students" data-f="studentsText" rows="9" placeholder="Ana Lee&#10;Omar Khan&#10;Zed Ng">${esc(c.studentsText)}</textarea>
        <div class="d-flex flex-wrap gap-3 align-items-center mt-2 small">
          <span class="text-muted">Copying from Excel works: two columns are read as one name each way.</span>
          <label class="ms-auto">Names without a comma are written
            <select class="form-select form-select-sm d-inline-block w-auto ms-1" data-f="nameOrder"><option value="first" ${c.nameOrder === "first" ? "selected" : ""}>First Last</option><option value="last" ${c.nameOrder === "last" ? "selected" : ""}>Last First</option></select></label>
        </div>
        <div id="w-names" class="mt-3"></div>`;
      return shell(2, "Who's in this class?", "You can add or remove students at any time. Leave this empty to add them later.", body,
        `<button class="btn btn-link" data-go="class">Back</button><button class="btn btn-link ms-auto" data-next="marking">Skip for now</button><button class="btn btn-primary" data-next="marking">Next: marking <i class="bi bi-arrow-right"></i></button>`);
    },

    marking() {
      const c = state.cur;
      const body = `<div class="row g-3">${cfg.presets.map((p) => `<div class="col-12"><label class="card mode-card h-100 ${c.preset === p.value ? "border-primary" : ""}"><div class="card-body d-flex gap-3">
          <input class="form-check-input mt-1" type="radio" name="preset" data-f="preset" value="${p.value}" ${c.preset === p.value ? "checked" : ""}>
          <div><div class="fw-semibold">${esc(p.title)}</div><div class="small text-muted">${esc(p.detail)}</div>
          ${p.categories.length ? `<div class="small mt-1">${p.categories.map((x) => `${esc(x[0])} <strong>${+x[1]}%</strong>`).join(" · ")}</div>` : ""}</div></div></label></div>`).join("")}</div>
        <p class="small text-muted mt-3 mb-0">Not sure? Pick the first one. You can change the categories and their weights any time in the class setup.</p>`;
      return shell(3, "How do marks count toward the final grade?", mode === "elementary" ? "This applies to each subject." : "Pick the closest match.", body,
        `<button class="btn btn-link" data-go="students">Back</button><button class="btn btn-primary ms-auto" data-next="assignments">Next: assignments <i class="bi bi-arrow-right"></i></button>`);
    },

    assignments() {
      const body = `<div id="w-rows"></div>
        <button class="btn btn-outline-primary btn-sm mt-2" data-wiz-add><i class="bi bi-plus-lg"></i> Add an assignment</button>
        <p class="small text-muted mt-3 mb-0">Leave "out of" or the category as "decide later" if you're not sure. We'll remind you to fill them in after your gradebook is created.</p>`;
      return shell(4, "Any assignments to add now?", "Quizzes, tests, projects you've already given or have planned. You can type the marks in the grid next.", body,
        `<button class="btn btn-link" data-go="marking">Back</button><button class="btn btn-link ms-auto" data-next="more" data-skip-assignments>I'll add them later</button><button class="btn btn-primary" data-next="more">Done with this class <i class="bi bi-check-lg"></i></button>`);
    },

    more() {
      const used = state.done.map((c) => c.period).filter(Boolean);
      const periodsLeft = cfg.freePeriods.filter((p) => !used.includes(p));
      const room = state.done.length + cfg.existing.length < cfg.maxClasses && (mode !== "secondary" || periodsLeft.length > 0);
      const rows = state.done.map((c, i) => `<li class="list-group-item d-flex align-items-center gap-2"><i class="bi bi-journal-check text-success"></i>
        <div class="me-auto"><strong>${esc(c.name)}</strong>${c.period ? ` <span class="text-muted">· Period ${c.period}</span>` : ""}<div class="small text-muted">${summaryOf(c)}</div></div>
        <button class="btn btn-sm btn-outline-secondary" data-edit-class="${i}">Edit</button><button class="btn btn-sm btn-outline-danger" data-remove-class="${i}" aria-label="Remove ${esc(c.name)}"><i class="bi bi-x-lg"></i></button></li>`).join("");
      const body = `<ul class="list-group mb-3">${rows}</ul>
        ${room ? `<button class="btn btn-outline-primary" data-wiz-new><i class="bi bi-plus-lg"></i> Add another class</button>` : mode === "secondary" ? `<p class="small text-muted mb-0">All your teaching periods are filled. You can change how many periods you teach in Settings.</p>` : ""}`;
      return `<div class="card shadow-sm"><div class="card-body p-4"><h1 class="h4">${state.done.length === 1 ? "Your class is ready to create" : "Your classes are ready to create"}</h1>
        <p class="text-muted">Nothing is saved yet. Add more classes, or finish to create everything.</p>${body}
        <div class="text-danger small mt-3" role="alert" id="wiz-error">${esc(state.error)}</div>
        <div class="d-flex gap-2 mt-4"><button class="btn btn-primary btn-lg ms-auto" data-create ${state.busy ? "disabled" : ""}>${state.busy ? "Creating…" : "Create my gradebook"} <i class="bi bi-check-lg"></i></button></div></div></div>`;
    },

    finished() {
      const r = state.result;
      const n = (v, one, many) => `${v} ${v === 1 ? one : many}`;
      const save = memory
        ? `<div class="alert alert-info text-start"><i class="bi bi-download"></i> <strong>One more thing: save your workbook.</strong> Your gradebook is only kept while you're signed in. Download your workbook file now and keep it somewhere your school allows. Next time, open it to carry on.
            <div class="mt-2"><a class="btn btn-primary" href="/workbook.xlsx" data-save-workbook><i class="bi bi-download"></i> Save my workbook</a></div></div>`
        : "";
      const review = r.review ? `<p class="small text-muted">${n(r.review, "assignment has", "assignments have")} details to finish (category or what it's out of). You'll see a reminder on your classes page.</p>` : "";
      return `<div class="text-center mb-3"><i class="bi bi-check-circle-fill display-3 text-success"></i><h1 class="h3 mt-2">Your gradebook is ready</h1>
        <p class="text-muted">Created ${n(r.classes, "class", "classes")}, ${n(r.students, "student", "students")} and ${n(r.assessments, "assignment", "assignments")}.</p></div>
        ${save}${review}
        <div class="d-flex gap-2 justify-content-center mt-3">${r.first_class ? `<a class="btn btn-primary btn-lg" href="/classes/${r.first_class}${r.review ? "?review=1" : ""}">Open my class</a>` : ""}<a class="btn btn-outline-secondary btn-lg" href="/">My classes</a></div>`;
    },
  };

  function summaryOf(c) {
    const s = parseNames(c.studentsText, c.nameOrder).length;
    const a = c.assessments.filter((x) => x.name.trim()).length;
    return `${s} student${s === 1 ? "" : "s"} · ${a} assignment${a === 1 ? "" : "s"} · ${esc(presetOf(c.preset).title)}`;
  }

  function show(step) {
    state.step = step;
    state.error = "";
    if (["class", "students", "marking", "assignments"].includes(step) && !state.cur) state.cur = blankClass();
    root.innerHTML = views[step]();
    document.getElementById("wiz-foot")?.classList.toggle("d-none", step === "finished");
    if (step === "students") renderNames();
    if (step === "assignments") {
      if (!state.cur.assessments.length) state.cur.assessments.push(blankRow());
      renderRows();
    }
    window.scrollTo({ top: 0 });
    const first = root.querySelector("input[type=text], input:not([type]), textarea");
    if (first && step !== "assignments") first.focus({ preventScroll: true });
  }

  function renderNames() {
    const c = state.cur;
    const names = parseNames(c.studentsText, c.nameOrder);
    const el = document.getElementById("w-names");
    if (!el) return;
    if (!names.length) return (el.innerHTML = "");
    const shown = names.slice(0, 6);
    el.innerHTML = `<div class="small fw-semibold mb-1">${names.length} student${names.length === 1 ? "" : "s"}. This is how we read them:</div>
      <table class="table table-sm w-auto mb-0"><thead><tr><th>First name</th><th>Last name</th></tr></thead><tbody>${shown.map((n) => `<tr><td>${esc(n.first)}</td><td>${esc(n.last)}</td></tr>`).join("")}</tbody></table>
      ${names.length > shown.length ? `<div class="small text-muted">…and ${names.length - shown.length} more</div>` : ""}`;
  }

  const blankRow = () => ({ name: "", kind: "scored", out_of: "", category: "", subject: "" });

  function renderRows() {
    const c = state.cur;
    const cats = catNames(c);
    const subs = subjectList(c);
    document.getElementById("w-rows").innerHTML = c.assessments.map((a, i) => `<div class="row g-2 align-items-end mb-2" data-row="${i}">
      <div class="col-12 col-md-4"><label class="form-label small mb-0">Name</label><input class="form-control" data-r="name" value="${esc(a.name)}" placeholder="Quiz 1" autocomplete="off"></div>
      <div class="col-6 col-md-2"><label class="form-label small mb-0">Type</label><select class="form-select" data-r="kind">
        <option value="scored" ${a.kind === "scored" ? "selected" : ""}>Marked</option><option value="level" ${a.kind === "level" ? "selected" : ""}>Level 1–4</option><option value="completion" ${a.kind === "completion" ? "selected" : ""}>Done / not done</option></select></div>
      <div class="col-6 col-md-2"><label class="form-label small mb-0">Out of</label><input class="form-control" data-r="out_of" inputmode="decimal" value="${esc(a.out_of)}" placeholder="decide later" ${a.kind === "scored" ? "" : "disabled"}></div>
      ${cats.length ? `<div class="col-8 col-md-3"><label class="form-label small mb-0">Category</label><select class="form-select" data-r="category"><option value="">Decide later</option>${cats.map((n) => `<option ${a.category === n ? "selected" : ""}>${esc(n)}</option>`).join("")}</select></div>` : ""}
      ${subs.length > 1 ? `<div class="col-8 col-md-3"><label class="form-label small mb-0">Subject</label><select class="form-select" data-r="subject">${subs.map((n) => `<option ${a.subject === n || (!a.subject && n === subs[0]) ? "selected" : ""}>${esc(n)}</option>`).join("")}</select></div>` : ""}
      <div class="col-auto ms-auto"><button class="btn btn-outline-danger" data-wiz-del="${i}" aria-label="Remove this assignment"><i class="bi bi-x-lg"></i></button></div></div>`).join("");
  }

  // ── events ─────────────────────────────────────────────────────────────────
  function validate(step) {
    const c = state.cur;
    if (step === "students" && !c.name.trim()) return "Give the class a name first.";
    if (step === "students") {
      const n = c.name.trim().toLowerCase();
      if (state.done.some((d) => d.name.trim().toLowerCase() === n) || cfg.existing.some((e) => e.toLowerCase() === n)) return "You already have a class with that name.";
      if (mode === "elementary" && !subjectList(c).length) return "List at least one subject.";
    }
    return "";
  }

  function finishClass() {
    const c = state.cur;
    c.assessments = c.assessments.filter((a) => a.name.trim());
    if (state.editing != null) state.done[state.editing] = c;
    else state.done.push(c);
    state.editing = null;
    state.cur = null;
  }

  root.addEventListener("input", (e) => {
    const f = e.target.dataset.f;
    if (f && state.cur) {
      state.cur[f] = e.target.type === "checkbox" ? e.target.checked : e.target.dataset.num ? (e.target.value === "" ? null : +e.target.value) : e.target.value;
      if (f === "studentsText" || f === "nameOrder") renderNames();
      return;
    }
    const r = e.target.dataset.r;
    if (r) {
      const i = +e.target.closest("[data-row]").dataset.row;
      state.cur.assessments[i][r] = e.target.value;
      if (r === "kind") renderRows();
    }
  });
  root.addEventListener("change", (e) => {
    if (e.target.dataset.f === "preset") show("marking");
  });

  root.addEventListener("click", async (e) => {
    const t = e.target.closest("button, a");
    if (!t) return;
    if (t.dataset.go) {
      if (t.dataset.go === "more") {
        // Leaving a class being added or edited without finishing it.
        state.cur = null;
        state.editing = null;
        if (!state.done.length) return show(root.dataset.firstRun === "1" ? "welcome" : "class");
      }
      return show(t.dataset.go);
    }
    if (t.dataset.next) {
      const here = state.step;
      const err = validate(here === "class" ? "students" : "");
      if (here === "class" && err) {
        state.error = err;
        return (document.getElementById("wiz-error").textContent = err);
      }
      if (t.dataset.skipAssignments !== undefined) state.cur.assessments = [];
      if (t.dataset.next === "more") finishClass();
      return show(t.dataset.next);
    }
    if (t.dataset.wizAdd !== undefined) {
      state.cur.assessments.push(blankRow());
      renderRows();
      return root.querySelectorAll("[data-row] [data-r=name]")[state.cur.assessments.length - 1]?.focus();
    }
    if (t.dataset.wizDel) {
      state.cur.assessments.splice(+t.dataset.wizDel, 1);
      return renderRows();
    }
    if (t.dataset.wizNew !== undefined) {
      state.editing = null;
      state.cur = blankClass();
      return show("class");
    }
    if (t.dataset.editClass) {
      state.editing = +t.dataset.editClass;
      state.cur = state.done[state.editing];
      return show("class");
    }
    if (t.dataset.removeClass) {
      state.done.splice(+t.dataset.removeClass, 1);
      return state.done.length ? show("more") : show("class");
    }
    if (t.dataset.create !== undefined) return create();
  });

  async function create() {
    state.busy = true;
    show("more");
    const classes = state.done.map((c) => ({
      name: c.name,
      course_code: c.course_code,
      period: c.period,
      grade_level: c.grade_level,
      room: c.room,
      preset: c.preset,
      subjects: subjectList(c),
      letter_grades: c.letter_grades,
      students: parseNames(c.studentsText, c.nameOrder),
      assessments: c.assessments.map((a) => ({
        name: a.name,
        kind: a.kind,
        out_of: a.kind === "scored" && String(a.out_of).trim() !== "" ? parseFloat(a.out_of) || null : null,
        category: a.category,
        subject: a.subject,
      })),
    }));
    try {
      const r = await fetch("/api/start", { method: "POST", headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf }, body: JSON.stringify({ classes }) });
      const j = await r.json().catch(() => ({ error: "Unexpected response from the server." }));
      if (!r.ok) throw new Error(j.error || "Something went wrong.");
      finished = true;
      state.result = j;
      state.busy = false;
      show("finished");
    } catch (err) {
      state.busy = false;
      show("more");
      state.error = err.message;
      document.getElementById("wiz-error").textContent = err.message;
    }
  }

  window.addEventListener("beforeunload", (e) => {
    if (!finished && (state.done.length || (state.cur && (state.cur.name || state.cur.studentsText)))) e.preventDefault();
  });

  show(state.step);
})();
