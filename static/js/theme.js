// Light / dark / match-the-computer theme. Loaded in <head> (not deferred) so
// the page is drawn in the right theme from the start, without a flash.
// The choice is a per-browser preference, kept in localStorage.
(function () {
  "use strict";
  const KEY = "gb-theme";
  const ICONS = { light: "bi-sun", dark: "bi-moon-stars", auto: "bi-circle-half" };
  const dark = window.matchMedia("(prefers-color-scheme: dark)");
  // High contrast: on if chosen here, or if the computer asks for more contrast.
  const CONTRAST = "gb-contrast";
  const moreContrast = window.matchMedia("(prefers-contrast: more)");
  function contrastOn() {
    let v = null;
    try { v = localStorage.getItem(CONTRAST); } catch (_) { /* storage blocked */ }
    return v === "more" || (v !== "normal" && moreContrast.matches);
  }
  function applyContrast() {
    const on = contrastOn();
    if (on) document.documentElement.setAttribute("data-contrast", "more");
    else document.documentElement.removeAttribute("data-contrast");
    document.querySelectorAll("[data-contrast-toggle]").forEach((b) => {
      b.setAttribute("aria-pressed", on ? "true" : "false");
      b.classList.toggle("active", on);
    });
    document.querySelectorAll("[data-contrast-icon]").forEach((i) => { i.className = "bi me-2 " + (on ? "bi-check-square" : "bi-square"); });
  }

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
  applyContrast();
  moreContrast.addEventListener("change", applyContrast);
  // Always print in the light theme: dark pages waste ink and print badly.
  let beforePrint = null;
  window.addEventListener("beforeprint", () => {
    beforePrint = document.documentElement.getAttribute("data-bs-theme");
    document.documentElement.setAttribute("data-bs-theme", "light");
  });
  window.addEventListener("afterprint", () => {
    if (beforePrint) document.documentElement.setAttribute("data-bs-theme", beforePrint);
    beforePrint = null;
  });
  dark.addEventListener("change", () => { if (pref() === "auto") apply(); });
  // Other tabs follow a change made in this one.
  window.addEventListener("storage", (e) => { if (e.key === KEY) apply(); if (e.key === CONTRAST) applyContrast(); });
  document.addEventListener("DOMContentLoaded", () => { mark(pref()); tint(); applyContrast(); });
  document.addEventListener("click", (e) => {
    if (!e.target.closest("[data-contrast-toggle]")) return;
    const next = contrastOn() ? "normal" : "more";
    try {
      // Matching the computer's own setting needs no stored choice.
      if ((next === "more") === moreContrast.matches) localStorage.removeItem(CONTRAST);
      else localStorage.setItem(CONTRAST, next);
    } catch (_) { /* storage blocked */ }
    applyContrast();
  });
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
