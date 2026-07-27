// Light/dark/auto toggle for every page on horndb.io.
// Cycles auto -> light -> dark -> auto. "auto" follows the OS via
// prefers-color-scheme (no stored choice). An explicit choice is persisted in
// localStorage under "horndb-theme" and applied through the public
// `window.quartoToggleColorScheme` hook, so the swap is Quarto's own compiled
// light/dark stylesheet swap rather than a bespoke one. theme/pretheme.html
// pre-seeds Quarto's own sentinel before its init script runs, so there is no
// flash of the wrong theme.
//
// The landing page and the docs are one Quarto project (`_quarto.yml` at the
// repo root), so this directory is the single copy of the shared theme — no
// duplication to keep in step. It was two projects and two theme/ directories
// until 2026-07; the split existed only to give the landing page a smaller
// navbar, which theme/landing.css now does with page-scoped CSS.
(function () {
  "use strict";

  var KEY = "horndb-theme";
  var ORDER = ["auto", "light", "dark"];
  var LABEL = { auto: "Auto", light: "Light", dark: "Dark" };
  var media = window.matchMedia("(prefers-color-scheme: dark)");
  // Half-filled circle — same icon on both engines. CSS rotates it 180deg for
  // "dark" and dashes the button border for "auto" (see theme-toggle.css);
  // the markup itself never changes, only the wrapping button's attributes.
  var ICON_SVG =
    '<svg viewBox="0 0 16 16" width="16" height="16" focusable="false">' +
    '<circle cx="8" cy="8" r="6.3" fill="none" stroke="currentColor" stroke-width="1.3"></circle>' +
    '<path d="M8 1.7a6.3 6.3 0 0 1 0 12.6z" fill="currentColor"></path>' +
    "</svg>";

  function current() {
    var v;
    try {
      v = localStorage.getItem(KEY);
    } catch (e) {
      v = null;
    }
    return v === "light" || v === "dark" ? v : "auto";
  }

  function persist(mode) {
    try {
      if (mode === "auto") localStorage.removeItem(KEY);
      else localStorage.setItem(KEY, mode);
    } catch (e) {}
  }

  function wantsDark(mode) {
    return mode === "dark" || (mode !== "light" && media.matches);
  }

  function applySite(mode) {
    var root = document.documentElement;
    if (mode === "auto") root.removeAttribute("data-theme");
    else root.setAttribute("data-theme", mode);
  }

  function applyQuarto(mode) {
    if (typeof window.quartoToggleColorScheme !== "function") return;
    var isDark = document.body.classList.contains("quarto-dark");
    if (isDark !== wantsDark(mode)) window.quartoToggleColorScheme();
  }

  function findButton() {
    return (
      document.getElementById("theme-toggle") ||
      document.querySelector(".quarto-color-scheme-toggle")
    );
  }

  function render(mode, btn) {
    if (!btn) return;
    btn.onclick = null;
    btn.removeAttribute("onclick");
    btn.classList.add("theme-toggle");
    btn.setAttribute("data-theme-mode", mode);
    if (!btn.querySelector(".theme-toggle-icon")) {
      btn.innerHTML = '<span class="theme-toggle-icon" aria-hidden="true">' + ICON_SVG + "</span>";
    }
    btn.title = "Theme: " + LABEL[mode];
    btn.setAttribute("aria-label", "Switch colour theme (currently " + LABEL[mode].toLowerCase() + ")");
  }

  function apply(mode, btn) {
    persist(mode);
    applySite(mode);
    applyQuarto(mode);
    render(mode, btn || findButton());
  }

  var btn = findButton();
  apply(current(), btn);

  if (btn) {
    btn.addEventListener("click", function (e) {
      e.preventDefault();
      var next = ORDER[(ORDER.indexOf(current()) + 1) % ORDER.length];
      apply(next, btn);
    });
  }

  media.addEventListener("change", function () {
    if (current() === "auto") apply("auto", findButton());
  });
})();
