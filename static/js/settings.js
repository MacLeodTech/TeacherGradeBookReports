// Settings page: school colour swatches, swap, and a live report preview.
// The colours themselves come from the server (/settings/palette), so the
// preview always matches what the PDF will use.
(() => {
  const main = document.getElementById("primary_color");
  const second = document.getElementById("accent_color");
  const mock = document.getElementById("report_mock");
  if (!main || !second || !mock) return;
  const note = document.getElementById("swap_note");

  const sample = document.getElementById("sample_report");

  let seq = 0;
  async function refresh() {
    const n = ++seq;
    const q = new URLSearchParams({ main: main.value, second: second.value });
    if (sample) sample.href = "/settings/sample-report.pdf?" + q;
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
  // Colours from the school logo: added to the front of each swatch row, and
  // a button that uses the two strongest (the deeper one for the banners).
  const useLogo = document.getElementById("use_logo_colors");
  if (useLogo) {
    fetch("/settings/logo-colors", { headers: { Accept: "application/json" } })
      .then((r) => (r.ok ? r.json() : []))
      .then((colors) => {
        if (!colors.length) return;
        for (const box of document.querySelectorAll(".swatches")) {
          const which = box.dataset.for === "primary_color" ? "Main colour" : "Second colour";
          const group = document.createElement("span");
          group.className = "swatch-group";
          for (const c of colors) {
            const b = document.createElement("button");
            b.type = "button";
            b.className = "swatch";
            b.style.setProperty("--c", c);
            b.dataset.color = c;
            b.title = "From your logo";
            b.setAttribute("aria-label", which + ": " + c + " from your logo");
            group.append(b);
          }
          box.prepend(group);
        }
        const lum = (h) => {
          const v = [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16) / 255)
            .map((x) => (x <= 0.04045 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4));
          return 0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2];
        };
        const [a, b = "#FFFFFF"] = colors;
        const [deep, light] = lum(a) <= lum(b) ? [a, b] : [b, a];
        useLogo.hidden = false;
        useLogo.addEventListener("click", () => {
          main.value = deep.toLowerCase();
          second.value = light.toLowerCase();
          mark();
          refresh();
        });
        mark();
      })
      .catch(() => {});
  }

  mark();
  refresh();
})();
