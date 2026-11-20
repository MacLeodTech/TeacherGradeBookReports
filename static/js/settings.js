// Settings page: school colour swatches, swap, and a live report preview.
// The colours themselves come from the server (/settings/palette), so the
// preview always matches what the PDF will use.
(() => {
  const main = document.getElementById("primary_color");
  const second = document.getElementById("accent_color");
  const mock = document.getElementById("report_mock");
  if (!main || !second || !mock) return;
  const note = document.getElementById("swap_note");

  let seq = 0;
  async function refresh() {
    const n = ++seq;
    const q = new URLSearchParams({ main: main.value, second: second.value });
    try {
      const r = await fetch("/settings/palette?" + q, { headers: { Accept: "application/json" } });
      if (!r.ok || n !== seq) return;
      const p = await r.json();
      for (const k of ["banner", "on_banner", "trim", "rule", "heading", "muted", "ink", "head_fill", "group_fill", "pale", "line"]) {
        mock.style.setProperty("--rm-" + k.replace("_", "-"), p[k]);
      }
      if (note) note.hidden = !p.swapped;
    } catch (_) { /* offline: keep the last preview */ }
  }

  function mark() {
    for (const box of document.querySelectorAll(".swatches")) {
      const v = document.getElementById(box.dataset.for).value.toUpperCase();
      for (const b of box.querySelectorAll(".swatch")) b.setAttribute("aria-pressed", String(b.dataset.color === v));
    }
  }

  document.querySelectorAll(".swatches").forEach((box) => {
    box.addEventListener("click", (e) => {
      const b = e.target.closest(".swatch");
      if (!b) return;
      document.getElementById(box.dataset.for).value = b.dataset.color.toLowerCase();
      mark();
      refresh();
    });
  });
  document.getElementById("swap_colors")?.addEventListener("click", () => {
    [main.value, second.value] = [second.value, main.value];
    mark();
    refresh();
  });
  for (const el of [main, second]) el.addEventListener("input", () => { mark(); refresh(); });
  mark();
  refresh();
})();
