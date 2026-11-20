// Small page behaviours. No inline scripts (the CSP forbids them).
(function () {
  "use strict";

  // Highlight the menu entry for the page you're on.
  const here = location.pathname;
  document.querySelectorAll(".gb-nav .navbar-nav > .nav-item > a.nav-link[href]").forEach((a) => {
    const href = a.getAttribute("href");
    if (href !== "/" && href !== "#" && (here === href || here.startsWith(href + "/"))) a.setAttribute("aria-current", "page");
  });
  if (here.startsWith("/classes/")) {
    const nav = document.querySelector(".gb-nav [data-nav-classes]");
    const id = here.split("/")[2];
    const item = document.querySelector(`.gb-nav .dropdown-menu a[href="/classes/${id}"]`);
    if (item) { item.classList.add("active"); if (nav) nav.setAttribute("aria-current", "page"); }
  }

  // Confirm before submitting dangerous forms: <button data-confirm="…">.
  document.addEventListener("click", (e) => {
    const el = e.target.closest("[data-confirm]");
    if (el && !confirm(el.dataset.confirm)) {
      e.preventDefault();
      e.stopImmediatePropagation();
    }
  }, true);

  // Remove a table row: <button data-remove-row>.
  document.addEventListener("click", (e) => {
    const btn = e.target.closest("[data-remove-row]");
    if (!btn) return;
    const table = btn.closest("table");
    btn.closest("tr").remove();
    updateWeights(table);
  });

  // Add a row by cloning the last one (or a blank category row): <button data-add-row="#table">.
  document.addEventListener("click", (e) => {
    const btn = e.target.closest("[data-add-row]");
    if (!btn) return;
    const table = document.querySelector(btn.dataset.addRow);
    const tbody = table.querySelector("tbody");
    let row;
    if (btn.dataset.template === "cat") {
      row = document.createElement("tr");
      row.innerHTML = '<td><input type="hidden" name="cat_id" value=""><input class="form-control form-control-sm" name="cat_name" placeholder="Category name"></td>' +
        '<td><input class="form-control form-control-sm" name="cat_weight" inputmode="decimal" data-weight value="0"></td>' +
        '<td><button type="button" class="btn btn-sm btn-link text-danger" data-remove-row title="Remove"><i class="bi bi-x-lg"></i></button></td>';
    } else {
      const last = tbody.querySelector("tr:last-child");
      if (!last) return;
      row = last.cloneNode(true);
      row.querySelectorAll("input:not([type=color])").forEach((i) => (i.value = ""));
    }
    tbody.appendChild(row);
    const first = row.querySelector("input:not([type=hidden])");
    if (first) first.focus();
    updateWeights(table);
  });

  // Live total for category weights.
  function updateWeights(table) {
    if (!table) return;
    const total = table.querySelector("[data-weight-total]");
    if (!total) return;
    let sum = 0;
    table.querySelectorAll("[data-weight]").forEach((i) => (sum += parseFloat(i.value) || 0));
    total.textContent = Math.round(sum * 100) / 100 + "%";
    const empty = !table.querySelector("[data-weight]");
    total.classList.toggle("text-danger", !empty && Math.abs(sum - 100) > 0.01);
  }
  document.addEventListener("input", (e) => {
    if (e.target.matches("[data-weight]")) updateWeights(e.target.closest("table"));
  });

  // Show settings that only apply to one teaching mode.
  const modeInputs = document.querySelectorAll('input[name="mode"]');
  function syncMode() {
    const checked = document.querySelector('input[name="mode"]:checked');
    document.querySelectorAll("[data-show-mode]").forEach((el) => {
      el.classList.toggle("d-none", !checked || el.dataset.showMode !== checked.value);
    });
    document.querySelectorAll(".mode-card").forEach((c) => c.classList.toggle("border-primary", c.contains(checked)));
  }
  modeInputs.forEach((i) => i.addEventListener("change", syncMode));
  if (modeInputs.length) syncMode();

  // Insert a comment-bank entry into the target textarea: <button data-insert="#id" data-text="…">.
  document.addEventListener("click", (e) => {
    const btn = e.target.closest("[data-insert]");
    if (!btn) return;
    e.preventDefault();
    const ta = document.querySelector(btn.dataset.insert);
    if (!ta) return;
    const text = btn.dataset.text;
    const start = ta.selectionStart ?? ta.value.length;
    const before = ta.value.slice(0, start);
    const sep = before && !/\s$/.test(before) ? " " : "";
    ta.value = before + sep + text + ta.value.slice(ta.selectionEnd ?? start);
    ta.focus();
    ta.dispatchEvent(new Event("input"));
  });

  // Character counters: <textarea data-count>.
  document.querySelectorAll("textarea[data-count]").forEach((ta) => {
    const out = document.createElement("div");
    out.className = "form-text text-end d-print-none";
    ta.after(out);
    const sync = () => (out.textContent = ta.value.length + " characters");
    ta.addEventListener("input", sync);
    sync();
  });

  // Report options → refresh the inline preview.
  const reportForm = document.getElementById("report-form");
  const frame = document.getElementById("report-preview");
  if (reportForm && frame) {
    const refresh = () => {
      const params = new URLSearchParams(new FormData(reportForm));
      params.delete("csrf_token");
      params.set("_", "1");
      frame.src = reportForm.dataset.preview + "?" + params.toString() + "#view=FitH";
    };
    reportForm.addEventListener("change", refresh);
    document.querySelectorAll("[data-refresh-preview]").forEach((b) => b.addEventListener("click", refresh));
  }

  // Select all / none checkboxes: <input type=checkbox data-check-all="name">.
  document.querySelectorAll("[data-check-all]").forEach((box) => {
    box.addEventListener("change", () => {
      document.querySelectorAll(`input[name="${box.dataset.checkAll}"]`).forEach((c) => (c.checked = box.checked));
    });
  });
  document.querySelectorAll("[data-check-where]").forEach((btn) => {
    btn.addEventListener("click", () => {
      document.querySelectorAll('input[name="student"]').forEach((c) => (c.checked = c.dataset[btn.dataset.checkWhere] === "1"));
    });
  });

  // Import mapping: show the new-class fields only when needed.
  const target = document.querySelector('select[name="target"]');
  if (target) {
    const sync = () => document.querySelectorAll("[data-new-class]").forEach((el) => el.classList.toggle("d-none", target.value !== "new"));
    target.addEventListener("change", sync);
    sync();
  }
})();

// Memory storage: keep the "unsaved changes" state in the privacy bar current.
(() => {
  "use strict";
  const bar = document.getElementById("ws-bar");
  if (!bar) return;
  const state = document.getElementById("ws-state");
  const leave = document.querySelectorAll("[data-leave]");
  function setUnsaved(unsaved, text) {
    bar.classList.toggle("unsaved", unsaved);
    state.innerHTML = "";
    const el = document.createElement(unsaved ? "strong" : "span");
    el.textContent = text;
    state.appendChild(el);
    leave.forEach((b) => {
      if (unsaved) b.dataset.confirm = b.dataset.leaveWarning;
      else delete b.dataset.confirm;
    });
  }
  // The grid (and anything else that saves in the background) fires this.
  document.addEventListener("gb:changed", () => setUnsaved(true, "Unsaved changes"));
  document.querySelectorAll("[data-save-workbook]").forEach((a) =>
    a.addEventListener("click", () => {
      const t = new Date().toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
      setTimeout(() => setUnsaved(false, "Saved to your workbook at " + t), 800);
    })
  );

  // Print buttons, and the date in the printed footer.
  document.addEventListener("click", (e) => {
    if (e.target.closest("[data-print]")) window.print();
  });
  // Collapsed sections print open (except ones hidden from print).
  let opened = [];
  window.addEventListener("afterprint", () => { opened.forEach((d) => { d.open = false; }); opened = []; });
  window.addEventListener("beforeprint", () => {
    opened = [...document.querySelectorAll("details:not([open])")].filter((d) => !d.closest(".d-print-none"));
    opened.forEach((d) => { d.open = true; });
    const d = new Date().toLocaleDateString(undefined, { year: "numeric", month: "long", day: "numeric" });
    document.querySelectorAll("[data-print-date]").forEach((el) => { el.textContent = d; });
  });
})();
