// Reports page: before downloading, list selected students whose reports may
// not be ready (no comment, missing or unmarked work, few or no marks).
(() => {
  const form = document.querySelector("form[data-report-check]");
  const modalEl = document.getElementById("report-check");
  if (!form || !modalEl) return;
  const list = document.getElementById("report-check-list");
  let confirmed = false;
  let submitter = null;

  const checks = [
    ["No marks yet", (d) => +d.marked === 0],
    ["Grade based on only one or two marks", (d) => +d.marked > 0 && +d.marked <= 2],
    ["Missing work", (d) => +d.missing > 0, (d) => `${d.missing} missing`],
    ["Work not yet marked", (d) => +d.pending > 0, (d) => `${d.pending} to mark`],
    ["No teacher comment", (d) => d.noComment === "1" && form.querySelector("[name=show_comments]")?.checked],
  ];

  form.addEventListener("submit", (e) => {
    if (confirmed) { confirmed = false; return; }
    const chosen = [...form.querySelectorAll("input[name=student]:checked")].map((b) => b.dataset);
    if (!chosen.length) return; // the server says to pick someone
    list.replaceChildren();
    for (const [title, test, detail] of checks) {
      const hits = chosen.filter(test);
      if (!hits.length) continue;
      const h = document.createElement("h3");
      h.className = "h6 mt-3 mb-1";
      h.textContent = `${title} (${hits.length})`;
      const ul = document.createElement("ul");
      ul.className = "small mb-0";
      for (const d of hits) {
        const li = document.createElement("li");
        li.textContent = detail ? `${d.name}: ${detail(d)}` : d.name;
        ul.append(li);
      }
      list.append(h, ul);
    }
    if (!list.children.length) return;
    e.preventDefault();
    submitter = e.submitter;
    bootstrap.Modal.getOrCreateInstance(modalEl).show();
  });

  document.getElementById("report-check-go").addEventListener("click", () => {
    confirmed = true;
    bootstrap.Modal.getOrCreateInstance(modalEl).hide();
    form.requestSubmit(submitter);
  });
})();
