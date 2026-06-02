// Theme toggle: cycles auto → light → dark → auto.
// "auto" means follow the OS via prefers-color-scheme (no data-theme attribute).
// An explicit choice sets data-theme on <html> and is persisted in localStorage.
(function () {
  "use strict";

  var KEY = "horndb-theme";
  var ORDER = ["auto", "light", "dark"];
  var LABEL = { auto: "Auto", light: "Light", dark: "Dark" };

  var root = document.documentElement;
  var btn = document.getElementById("theme-toggle");
  if (!btn) return;
  var label = btn.querySelector("[data-theme-label]");

  function current() {
    var t = root.getAttribute("data-theme");
    return t === "light" || t === "dark" ? t : "auto";
  }

  function apply(mode) {
    if (mode === "auto") {
      root.removeAttribute("data-theme");
      try { localStorage.removeItem(KEY); } catch (e) {}
    } else {
      root.setAttribute("data-theme", mode);
      try { localStorage.setItem(KEY, mode); } catch (e) {}
    }
    if (label) label.textContent = LABEL[mode];
    btn.title = "Theme: " + LABEL[mode];
  }

  // Sync the label with whatever the pre-paint script established.
  apply(current());

  btn.addEventListener("click", function () {
    var next = ORDER[(ORDER.indexOf(current()) + 1) % ORDER.length];
    apply(next);
  });
})();
