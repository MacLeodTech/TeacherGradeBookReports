// Light / dark / match-the-computer theme. Loaded in <head> (not deferred) so
// the page is drawn in the right theme from the start, without a flash.
// The choice is a per-browser preference, kept in localStorage.
(function () {
  "use strict";
  const KEY = "gb-theme";
  const ICONS = { light: "bi-sun", dark: "bi-moon-stars", auto: "bi-circle-half" };
  const dark = window.matchMedia("(prefers-color-scheme: dark)");

  function pref() {
    let v = null;
    try { v = localStorage.getItem(KEY); } catch (_) { /* storage blocked */ }
    return v === "light" || v === "dark" ? v : "auto";
  }

  function mark(p) {
    document.querySelectorAll("[data-theme-value]").forEach((b) => {
      const on = b.dataset.themeValue === p;
      b.classList.toggle("active", on);
      b.setAttribute("aria-pressed", on ? "true" : "false");
    });
    document.querySelectorAll("[data-theme-icon]").forEach((i) => { i.className = "bi " + ICONS[p]; });
  }

  // The browser's own bar (mobile browsers, installed apps) takes the menu bar's colour,
  // or the page background where there's no menu bar.
  function tint() {
    const meta = document.querySelector('meta[name="theme-color"]');
    const el = document.querySelector(".gb-nav") || document.body;
    if (meta && el) meta.setAttribute("content", getComputedStyle(el).backgroundColor);
  }

  function apply() {
    const p = pref();
    const theme = p === "auto" ? (dark.matches ? "dark" : "light") : p;
    document.documentElement.setAttribute("data-bs-theme", theme);
    mark(p);
    tint();
  }

  apply();
  dark.addEventListener("change", () => { if (pref() === "auto") apply(); });
  // Other tabs follow a change made in this one.
  window.addEventListener("storage", (e) => { if (e.key === KEY) apply(); });
  document.addEventListener("DOMContentLoaded", () => { mark(pref()); tint(); });
  document.addEventListener("click", (e) => {
    const b = e.target.closest("[data-theme-value]");
    if (!b) return;
    try {
      if (b.dataset.themeValue === "auto") localStorage.removeItem(KEY);
      else localStorage.setItem(KEY, b.dataset.themeValue);
    } catch (_) { /* storage blocked: still switch for this page */ }
    apply();
    if (pref() !== b.dataset.themeValue) {
      // Couldn't save: switch this page anyway.
      const v = b.dataset.themeValue;
      document.documentElement.setAttribute("data-bs-theme", v === "auto" ? (dark.matches ? "dark" : "light") : v);
      mark(v);
    }
  });
})();
