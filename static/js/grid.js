// Mark grid: a small spreadsheet-like table backed by /api/subjects/{id}.
// The server does every calculation; this file only edits cells and redraws.
(function () {
  "use strict";
  const root = document.getElementById("grid");
  if (!root) return;
  const subjectId = root.dataset.subject;
  const readonly = root.dataset.readonly === "1";
  const csrf = document.querySelector('meta[name="csrf-token"]').content;
  const statusEl = document.getElementById("grid-status");
  const warningsEl = document.getElementById("grid-warnings");
  const undoBtn = document.querySelector('[data-action="undo"]');
  let data = null;
  let focus = null; // {s, a} ids of the focused cell
  let editing = null; // {td, input, original}
  let saving = 0;

  // ── helpers ────────────────────────────────────────────────────────────────
  const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
  // Same rounding as the server (halves up).
  const pct = (v) => (v === null || v === undefined ? "—" : (Math.round(v * 10) / 10).toFixed(1) + "%");
  const num = (v) => String(Math.round(v * 100) / 100);

  async function api(path, body) {
    const opts = body === undefined ? {} : { method: "POST", headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf }, body: JSON.stringify(body) };
    const r = await fetch(path, opts);
    if (r.status === 401) {
      location.href = "/login?next=" + encodeURIComponent(location.pathname + location.search);
      throw new Error("signed out");
    }
    const j = await r.json().catch(() => ({ error: "Unexpected response from the server." }));
    if (!r.ok) throw new Error(j.error || "Something went wrong.");
    if (body !== undefined) document.dispatchEvent(new Event("gb:changed"));
    return j;
  }

  function setStatus(msg, kind) {
    statusEl.textContent = msg;
    statusEl.className = "small ms-2 " + (kind === "error" ? "text-danger" : "text-muted");
  }

  // ── rendering ──────────────────────────────────────────────────────────────
  function catName(id) {
    const c = data.categories.find((c) => c.id === id);
    return c ? c.name : data.categories.length ? "No category (not counted)" : "";
  }

  function render() {
    const A = data.assessments;
    const cats = data.categories;
    // Header row 1: categories (consecutive runs merged).
    let groups = "";
    if (cats.length) {
      let i = 0;
      while (i < A.length) {
        let j = i;
        while (j < A.length && A[j].category_id === A[i].category_id) j++;
        const c = cats.find((c) => c.id === A[i].category_id);
        groups += `<th colspan="${j - i}" class="cat-head">${esc(catName(A[i].category_id))}${c ? ` <span class="text-muted">${num(c.weight)}%</span>` : ""}</th>`;
        i = j;
      }
    }
    const catCols = cats.map((c) => `<th class="calc" title="Category average">${esc(c.name)}<div class="sub">${num(c.weight)}%</div></th>`).join("");
    const heads = A.map((a, i) => {
      const times = a.weight !== 1 ? ` ×${num(a.weight)}` : "";
      const meta = a.kind === "completion" ? "done?" : a.kind === "level" ? `level 1–4${times}` : `/${num(a.out_of)}${times}`;
      return `<th class="a-head ${a.needs_review ? "needs-review" : ""} ${a.weight === 0 && a.kind !== "completion" ? "formative" : ""} ${a.include_in_report ? "" : "hidden-report"}" data-a="${a.id}" title="${esc(a.name)}${a.unit ? " · " + esc(a.unit) : ""}">
        <div class="d-flex align-items-start gap-1">
          <button class="a-name btn btn-link p-0 text-start" data-edit="${a.id}" ${readonly ? "disabled" : ""}>${esc(a.name)}</button>${a.needs_review ? ` <i class="bi bi-pencil-square review-dot" title="Details need checking" role="img" aria-label="Details need checking"></i>` : ""}
          ${readonly ? "" : `<div class="dropdown ms-auto"><button class="btn btn-link btn-sm p-0 a-menu" data-bs-toggle="dropdown" aria-label="Column menu"><i class="bi bi-three-dots-vertical"></i></button>
          <ul class="dropdown-menu dropdown-menu-end small">
            <li><button class="dropdown-item" data-fill="${a.id}" data-input="M">Mark blanks as missing</button></li>
            ${a.kind === "completion" ? `<li><button class="dropdown-item" data-fill="${a.id}" data-input="Y">Mark blanks as done</button></li>` : ""}
            <li><button class="dropdown-item" data-fill="${a.id}" data-input="?">Mark blanks as "to mark"</button></li>
            <li><hr class="dropdown-divider"></li>
            <li><button class="dropdown-item" data-move="${a.id}" data-dir="-1" ${i === 0 ? "disabled" : ""}>Move left</button></li>
            <li><button class="dropdown-item" data-move="${a.id}" data-dir="1" ${i === A.length - 1 ? "disabled" : ""}>Move right</button></li>
            <li><button class="dropdown-item" data-edit="${a.id}">Edit…</button></li>
          </ul></div>`}
        </div>
        <div class="sub">${esc(a.unit)}${a.unit ? " · " : ""}${meta}</div>
      </th>`;
    }).join("");

    const rows = data.students.map((s) => {
      const r = data.results[s.id] || {};
      const cells = data.cells[s.id] || {};
      const tds = A.map((a) => {
        const c = cells[a.id];
        const err = (data.errors || []).find((e) => e.student_id === s.id && e.assessment_id === a.id);
        const cls = ["cell", c ? "st-" + c.status : "", err ? "has-error" : "", a.kind === "completion" ? "completion" : ""].join(" ");
        let text = c ? c.text : "";
        if (a.kind === "completion" && c && c.status === "marked") text = "✓";
        if (err) text = err.input;
        return `<td class="${cls}" tabindex="-1" data-s="${s.id}" data-a="${a.id}"${err ? ` title="${esc(err.error)}"` : ""}>${esc(text)}</td>`;
      }).join("");
      const catTds = cats.map((c) => {
        const v = r.categories ? r.categories[c.id] : null;
        return `<td class="calc">${v === null || v === undefined ? "" : pct(v)}</td>`;
      }).join("");
      const overall = r.overall === null || r.overall === undefined
        ? `<span class="text-muted">—</span>`
        : `<span class="grade-pill" style="--c:${esc(r.color)}" title="${r.overridden ? "Teacher-set final grade" : "Calculated"}">${pct(r.overall)} · ${esc(r.label)}${r.overridden ? " ✎" : ""}</span>`;
      return `<tr data-s="${s.id}">
        <th class="name"><a href="/classes/${data.class_id}/students/${s.id}">${esc(s.name)}</a>${r.missing ? ` <span class="badge bg-danger-subtle text-danger-emphasis" title="Missing work">${r.missing}</span>` : ""}</th>
        <td class="overall">${overall}</td>${tds}${catTds}</tr>`;
    }).join("");

    const avgs = A.map((a) => `<td class="avg">${a.kind === "completion" ? "" : a.kind === "level" ? esc(a.avg_text || "") : pct(a.avg)}</td>`).join("");
    const classAvg = data.class_avg === null ? "—" : `<span class="grade-pill" style="--c:${esc(data.class_color)}">${pct(data.class_avg)} · ${esc(data.class_label)}</span>`;

    root.innerHTML = `<table class="gb-grid">
      <thead>
        ${cats.length && A.length ? `<tr class="groups"><th class="name"></th><th class="overall"></th>${groups}${cats.length ? `<th colspan="${cats.length}" class="cat-head">Category averages</th>` : ""}</tr>` : ""}
        <tr><th class="name">Student</th><th class="overall">Overall</th>${heads}${catCols}</tr>
      </thead>
      <tbody>${rows}</tbody>
      <tfoot><tr><th class="name">Class average</th><td class="overall">${classAvg}</td>${avgs}${cats.map(() => "<td></td>").join("")}</tr></tfoot>
    </table>
    ${A.length ? "" : `<div class="text-center py-5 px-3">
      <i class="bi bi-clipboard-plus fs-1 gb-empty-icon" aria-hidden="true"></i>
      <h2 class="h5 mt-3">No assessments yet</h2>
      <p class="text-muted mx-auto gb-empty-text">Each assessment is a column: a quiz, test, project or assignment.${readonly ? "" : " Add one, then type marks straight into the grid or paste a column from a spreadsheet."}</p>
      ${readonly ? "" : `<button type="button" class="btn btn-primary" data-empty-add><i class="bi bi-plus-lg"></i> Add your first assessment</button>`}
    </div>`}`;

    warningsEl.innerHTML = (data.warnings || []).map((w) => `<div class="alert alert-warning py-1 px-2 small mb-2"><i class="bi bi-exclamation-triangle"></i> ${esc(w)}</div>`).join("");
    if (undoBtn) {
      undoBtn.disabled = !data.undo;
      undoBtn.querySelector("span").textContent = data.undo ? `Undo (${data.undo.count} ${data.undo.source === "import" ? "imported " : ""}mark${data.undo.count === 1 ? "" : "s"})` : "Undo";
    }
    if (data.errors && data.errors.length) setStatus(`${data.errors.length} entr${data.errors.length === 1 ? "y wasn't" : "ies weren't"} understood. Point at a red cell to see why.`, "error");
    renderReviewBanner();
    restoreFocus();
  }

  function cellAt(s, a) {
    return root.querySelector(`td.cell[data-s="${s}"][data-a="${a}"]`);
  }

  function restoreFocus() {
    if (!focus) return;
    const td = cellAt(focus.s, focus.a);
    if (td) setFocus(td, false);
  }

  function setFocus(td, scroll = true) {
    root.querySelectorAll("td.cell.focused").forEach((x) => x.classList.remove("focused"));
    td.classList.add("focused");
    td.focus({ preventScroll: !scroll });
    if (scroll) td.scrollIntoView({ block: "nearest", inline: "nearest" });
    focus = { s: +td.dataset.s, a: +td.dataset.a };
  }

  function move(td, dr, dc) {
    const tr = td.parentElement;
    const cells = [...tr.querySelectorAll("td.cell")];
    const ci = cells.indexOf(td) + dc;
    const rows = [...root.querySelectorAll("tbody tr")];
    const ri = rows.indexOf(tr) + dr;
    if (ri < 0 || ri >= rows.length) return;
    const rowCells = [...rows[ri].querySelectorAll("td.cell")];
    const target = rowCells[Math.max(0, Math.min(rowCells.length - 1, ci))];
    if (target) setFocus(target);
  }

  // ── editing ────────────────────────────────────────────────────────────────
  function startEdit(td, initial) {
    if (readonly || editing) return;
    const original = td.textContent === "✓" ? "Y" : td.textContent;
    const input = document.createElement("input");
    input.className = "cell-input";
    input.value = initial !== undefined ? initial : original;
    td.textContent = "";
    td.appendChild(input);
    input.focus();
    if (initial === undefined) input.select();
    editing = { td, input, original };
  }

  function finishEdit(commit) {
    if (!editing) return null;
    const { td, input, original } = editing;
    editing = null;
    const value = input.value.trim();
    td.textContent = commit ? value : original;
    if (commit && value !== original) {
      save([{ student_id: +td.dataset.s, assessment_id: +td.dataset.a, input: value }], "grid");
    }
    return td;
  }

  async function save(changes, source) {
    if (!changes.length) return;
    saving++;
    setStatus("Saving…");
    try {
      data = await api(`/api/subjects/${subjectId}/marks`, { changes, source });
      render();
      if (!data.errors.length) {
        const hint = changes.length === 1 ? describe(changes[0].student_id, changes[0].assessment_id) : "";
        setStatus(hint ? `Saved ✓ ${hint}` : `Saved ${changes.length === 1 ? "" : changes.length + " cells "}✓`);
      }
    } catch (e) {
      setStatus(e.message + " Your last change was not saved.", "error");
    } finally {
      saving--;
    }
  }

  // How the server read what was typed, e.g. "Doe, John · Quiz 1: 17/20 = 85.0%".
  function describe(sid, aid) {
    const a = data.assessments.find((x) => x.id === aid);
    const st = data.students.find((x) => x.id === sid);
    if (!a || !st) return "";
    const c = (data.cells[sid] || {})[aid];
    const who = `${st.name} · ${a.name}: `;
    if (!c) return who + "cleared";
    const late = c.status === "late" ? "late, " : "";
    const counted = a.weight === 0 && a.kind !== "completion" ? " (formative, not counted)" : "";
    const value = c.text.replace(/L$/, "");
    switch (c.status) {
      case "missing": return who + (data.policy === "zero" ? "missing, counts as 0" : "missing, left out of the average and flagged on the report");
      case "excused": return who + "excused, not counted";
      case "pending": return who + "handed in, to mark (not counted yet)";
    }
    if (a.kind === "completion") return who + late + (c.pct ? "done" : "not done");
    if (a.kind === "level") return who + late + `Level ${value}` + (c.pct != null ? ` = ${pct(c.pct)}` : "") + counted;
    return who + late + `${value}/${num(a.out_of)}` + (c.pct != null ? ` = ${pct(c.pct)}` : "") + counted;
  }

  async function load() {
    try {
      data = await api(`/api/subjects/${subjectId}/grid`);
      render();
      // Arriving from the dashboard's "Review now": start going through them.
      if (!readonly && modal && flagged().length && new URLSearchParams(location.search).get("review")) startReview();
    } catch (e) {
      root.innerHTML = `<div class="alert alert-danger m-3">${esc(e.message)}</div>`;
    }
  }

  // ── events ─────────────────────────────────────────────────────────────────
  root.addEventListener("click", (e) => {
    const td = e.target.closest("td.cell");
    if (td && !td.contains(editing && editing.input)) {
      if (editing) finishEdit(true);
      setFocus(td);
    }
  });
  root.addEventListener("dblclick", (e) => {
    const td = e.target.closest("td.cell");
    if (td) startEdit(td);
  });

  // Keyboard shortcuts list: the button, or "?" when no cell is focused
  // (in a cell, "?" means "to mark").
  const keysEl = document.getElementById("shortcuts-modal");
  const showKeys = () => keysEl && bootstrap.Modal.getOrCreateInstance(keysEl).show();
  document.querySelector('[data-action="shortcuts"]')?.addEventListener("click", showKeys);
  document.addEventListener("keydown", (e) => {
    const t = e.target;
    const typing = t.closest && (t.closest("input, textarea, select, [contenteditable], td.cell") || t.closest(".modal"));
    if (e.key === "?" && !typing) { e.preventDefault(); showKeys(); }
    // Ctrl+Z / Cmd+Z: undo the last change (when not typing in a box).
    if ((e.ctrlKey || e.metaKey) && !e.shiftKey && e.key.toLowerCase() === "z" && !editing && !(t.closest && t.closest("input, textarea, .modal"))) {
      if (undoBtn && !undoBtn.disabled) { e.preventDefault(); undoBtn.click(); }
    }
  });

  root.addEventListener("keydown", (e) => {
    if (editing) {
      if (e.key === "Enter" || e.key === "Tab" || e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const td = finishEdit(true);
        if (e.key === "Tab") move(td, 0, e.shiftKey ? -1 : 1);
        else move(td, e.key === "ArrowUp" || (e.key === "Enter" && e.shiftKey) ? -1 : 1, 0);
      } else if (e.key === "Escape") {
        e.preventDefault();
        setFocus(finishEdit(false));
      }
      return;
    }
    const td = e.target.closest("td.cell");
    if (!td) return;
    const nav = { ArrowUp: [-1, 0], ArrowDown: [1, 0], ArrowLeft: [0, -1], ArrowRight: [0, 1] }[e.key];
    if (nav) {
      e.preventDefault();
      move(td, ...nav);
    } else if (e.key === "Tab") {
      e.preventDefault();
      move(td, 0, e.shiftKey ? -1 : 1);
    } else if (e.key === "Enter" || e.key === "F2") {
      e.preventDefault();
      startEdit(td);
    } else if ((e.key === "Delete" || e.key === "Backspace") && !readonly) {
      e.preventDefault();
      if (td.textContent !== "") save([{ student_id: +td.dataset.s, assessment_id: +td.dataset.a, input: "" }], "grid");
    } else if (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault();
      startEdit(td, e.key);
    }
  });

  // Paste a block from Excel / Google Sheets starting at the focused cell.
  root.addEventListener("paste", (e) => {
    if (editing || readonly || !focus) return;
    const text = (e.clipboardData || window.clipboardData).getData("text");
    if (!text) return;
    e.preventDefault();
    const lines = text.replace(/\r\n?/g, "\n").replace(/\n$/, "").split("\n").map((l) => l.split("\t"));
    const studentIds = data.students.map((s) => s.id);
    const aIds = data.assessments.map((a) => a.id);
    const r0 = studentIds.indexOf(focus.s);
    const c0 = aIds.indexOf(focus.a);
    const changes = [];
    lines.forEach((cols, dr) => cols.forEach((v, dc) => {
      const s = studentIds[r0 + dr];
      const a = aIds[c0 + dc];
      if (s !== undefined && a !== undefined) changes.push({ student_id: s, assessment_id: a, input: v.trim() });
    }));
    const skipped = lines.reduce((n, cols) => n + cols.length, 0) - changes.length;
    save(changes, "paste").then(() => {
      if (skipped > 0) setStatus(`Pasted ${changes.length} cells; ${skipped} fell outside the grid and were ignored.`);
    });
  });

  // The empty grid's button opens the same dialog as the toolbar's.
  root.addEventListener("click", (e) => {
    if (e.target.closest("[data-empty-add]")) document.querySelector('.grid-toolbar [data-action="add-assessment"]')?.click();
  });

  // Column menu actions and editing assessments.
  root.addEventListener("click", async (e) => {
    const fill = e.target.closest("[data-fill]");
    const mv = e.target.closest("[data-move]");
    const ed = e.target.closest("[data-edit]");
    try {
      if (fill) {
        setStatus("Saving…");
        data = await api(`/api/assessments/${fill.dataset.fill}/fill`, { input: fill.dataset.input, only_empty: true });
        render();
        setStatus("Saved ✓");
      } else if (mv) {
        data = await api(`/api/assessments/${mv.dataset.move}/move`, { dir: +mv.dataset.dir });
        render();
      } else if (ed) {
        openModal(data.assessments.find((a) => a.id === +ed.dataset.edit));
      }
    } catch (err) {
      setStatus(err.message, "error");
    }
  });

  if (undoBtn) {
    undoBtn.addEventListener("click", async () => {
      if (!data.undo) return;
      try {
        data = await api(`/api/subjects/${subjectId}/undo`, {});
        render();
        setStatus("Undone ✓");
      } catch (e) {
        setStatus(e.message, "error");
      }
    });
  }

  // ── assessment modal ───────────────────────────────────────────────────────
  const modalEl = document.getElementById("assessment-modal");
  const form = document.getElementById("assessment-form");
  const modal = modalEl ? new bootstrap.Modal(modalEl) : null;
  let editingAssessment = null;
  let review = null; // {skipped: Set, total, done} while stepping through assignments that need details
  let dirty = false;

  const flagged = () => data.assessments.filter((a) => a.needs_review > 0);
  function renderReviewBanner() {
    const el = document.getElementById("review-banner");
    if (!el) return;
    const n = readonly ? 0 : flagged().length;
    el.innerHTML = n
      ? `<div class="alert alert-warning d-flex flex-wrap align-items-center gap-2 py-2 mb-2" role="status"><i class="bi bi-pencil-square"></i>
          <div class="me-auto"><strong>${n} assignment${n === 1 ? " needs" : "s need"} a few details.</strong> Gradebook guessed the category, weight or what it's out of.</div>
          <button type="button" class="btn btn-warning btn-sm" data-action="start-review">Review ${n === 1 ? "it" : "them"}</button></div>`
      : "";
  }

  function nextReview() {
    const a = flagged().find((x) => !review.skipped.has(x.id));
    if (!a) {
      review = null;
      modal.hide();
      render();
      setStatus("All set ✓");
      return;
    }
    openModal(a, true);
  }

  function startReview() {
    review = { skipped: new Set(), total: flagged().length };
    nextReview();
  }

  function setSaveLabel() {
    const btn = document.getElementById("assessment-save");
    if (!review) return (btn.textContent = "Save assessment");
    btn.textContent = editingAssessment.needs_review === 2 || dirty ? "Update" : "Looks good";
  }

  function syncKind() {
    const completion = form.kind.value === "completion";
    const level = form.kind.value === "level";
    form.querySelectorAll("[data-scored]").forEach((el) => el.classList.toggle("d-none", completion));
    form.querySelectorAll("[data-out-of]").forEach((el) => el.classList.toggle("d-none", completion || level));
    form.querySelectorAll("[data-level-help]").forEach((el) => el.classList.toggle("d-none", !level));
  }

  function openModal(a, inReview) {
    if (!inReview) review = null;
    dirty = false;
    editingAssessment = a || null;
    form.reset();
    document.getElementById("assessment-error").textContent = "";
    form.category_id.innerHTML = (data.categories.length ? "" : '<option value="">—</option>') +
      data.categories.map((c) => `<option value="${c.id}">${esc(c.name)} (${num(c.weight)}%)</option>`).join("") +
      (data.categories.length ? '<option value="">No category (not counted)</option>' : "");
    const units = [...new Set(data.assessments.map((x) => x.unit).filter(Boolean))];
    document.getElementById("unit-list").innerHTML = units.map((u) => `<option value="${esc(u)}">`).join("");
    modalEl.querySelector(".modal-title").textContent = a ? "Edit assessment" : "New assessment";
    modalEl.querySelector('[data-action="delete-assessment"]').classList.toggle("d-none", !a);
    document.getElementById("rescale-row").classList.add("d-none");
    const help = document.getElementById("review-help");
    help.classList.toggle("d-none", !review);
    modalEl.querySelector('[data-action="review-skip"]').classList.toggle("d-none", !review);
    form.out_of.required = false;
    if (review) {
      const pos = Math.min(review.total, review.total - flagged().filter((x) => !review.skipped.has(x.id)).length + 1);
      modalEl.querySelector(".modal-title").textContent = `Check this assignment (${pos} of ${review.total})`;
      help.innerHTML = a.needs_review === 2
        ? `<div class="alert alert-warning small mb-0">Gradebook can't tell what <strong>${esc(a.name)}</strong> is out of. Enter it below. Marks stay as typed (17 with "out of 20" is 17/20). The other fields are best guesses, so change anything that's off.</div>`
        : `<div class="alert alert-info small mb-0"><strong>${esc(a.name)}</strong> was added outside Gradebook, so the category, weight and due date are best guesses. Change anything that's off, then confirm.</div>`;
    }
    if (a) {
      form.id.value = a.id;
      form.name.value = a.name;
      form.unit.value = a.unit;
      form.category_id.value = a.category_id ?? "";
      form.kind.value = a.kind;
      form.out_of.value = review && a.needs_review === 2 ? "" : num(a.out_of);
      form.out_of.required = !!review && a.needs_review === 2 && a.kind === "scored";
      form.weight.value = num(a.weight);
      form.due_date.value = a.due_date || "";
      form.include_in_report.checked = a.include_in_report;
    } else {
      const last = data.assessments[data.assessments.length - 1];
      if (last) {
        form.unit.value = last.unit;
        form.category_id.value = last.category_id ?? "";
      }
    }
    syncKind();
    setSaveLabel();
    modal.show();
    setTimeout(() => (review && a.needs_review === 2 ? form.out_of : form.name).focus(), 200);
  }

  if (form) {
    form.kind.addEventListener("change", () => {
      syncKind();
      form.out_of.required = !!review && editingAssessment.needs_review === 2 && form.kind.value === "scored";
    });
    form.addEventListener("input", () => {
      dirty = true;
      setSaveLabel();
    });
    form.out_of.addEventListener("input", () => {
      const show = !review && editingAssessment && editingAssessment.kind === "scored" && +form.out_of.value !== editingAssessment.out_of;
      document.getElementById("rescale-row").classList.toggle("d-none", !show);
    });
    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      const body = {
        name: form.name.value,
        unit: form.unit.value,
        category_id: form.category_id.value ? +form.category_id.value : null,
        kind: form.kind.value,
        out_of: parseFloat(form.out_of.value) || 0,
        weight: form.weight.value === "" ? 1 : parseFloat(form.weight.value),
        due_date: form.due_date.value || null,
        include_in_report: form.include_in_report.checked,
        rescale: !review && form.rescale.checked,
      };
      try {
        data = editingAssessment
          ? await api(`/api/assessments/${editingAssessment.id}`, body)
          : await api(`/api/subjects/${subjectId}/assessments`, body);
        if (review) {
          setStatus("Saved ✓");
          return nextReview();
        }
        modal.hide();
        render();
        setStatus("Saved ✓");
        if (!editingAssessment) {
          const a = data.assessments[data.assessments.length - 1];
          const first = data.students[0];
          if (a && first) {
            const td = cellAt(first.id, a.id);
            if (td) setFocus(td);
          }
        }
      } catch (err) {
        document.getElementById("assessment-error").textContent = err.message;
      }
    });
    modalEl.querySelector('[data-action="delete-assessment"]').addEventListener("click", async () => {
      if (!editingAssessment) return;
      const n = Object.values(data.cells).filter((row) => row[editingAssessment.id]).length;
      if (!confirm(`Delete "${editingAssessment.name}"${n ? ` and its ${n} mark${n === 1 ? "" : "s"}` : ""}? This can't be undone.`)) return;
      try {
        data = await api(`/api/assessments/${editingAssessment.id}/delete`, {});
        modal.hide();
        render();
      } catch (err) {
        document.getElementById("assessment-error").textContent = err.message;
      }
    });
    modalEl.querySelector('[data-action="review-skip"]').addEventListener("click", () => {
      review.skipped.add(editingAssessment.id);
      nextReview();
    });
    modalEl.addEventListener("hidden.bs.modal", () => {
      if (review) {
        review = null;
        render();
      }
    });
    document.addEventListener("click", (e) => {
      if (e.target.closest('[data-action="start-review"]')) startReview();
    });
    const addBtn = document.querySelector('[data-action="add-assessment"]');
    if (addBtn) addBtn.addEventListener("click", () => openModal(null));
  }

  window.addEventListener("beforeunload", (e) => {
    if (editing || saving) {
      if (editing) finishEdit(true);
      e.preventDefault();
    }
  });

  load();
})();
