"use strict";
// Loaded synchronously in <head> so the chosen theme applies before first paint.
(function () {
  const KEY = "fsend.theme";
  const MODES = ["auto", "light", "dark"];
  const ICONS = {
    auto: '<svg class="i" viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="8"/><path d="M12 4a8 8 0 010 16z" fill="currentColor"/></svg>',
    light: '<svg class="i" viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/></svg>',
    dark: '<svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path d="M20 14.5A8 8 0 019.5 4a8 8 0 1010.5 10.5z"/></svg>',
  };
  const LABELS = { auto: "Match system theme", light: "Light theme", dark: "Dark theme" };

  let mode = "auto";
  try { mode = localStorage.getItem(KEY) || "auto"; } catch {}
  if (!MODES.includes(mode)) mode = "auto";

  function apply() {
    if (mode === "auto") document.documentElement.removeAttribute("data-theme");
    else document.documentElement.setAttribute("data-theme", mode);
    document.querySelectorAll("[data-theme-switch] button").forEach((b) =>
      b.setAttribute("aria-pressed", String(b.dataset.mode === mode))
    );
  }
  apply();

  document.addEventListener("DOMContentLoaded", () => {
    document.querySelectorAll("[data-theme-switch]").forEach((box) => {
      box.setAttribute("role", "group");
      box.setAttribute("aria-label", "Theme");
      box.innerHTML = MODES.map(
        (m) => `<button type="button" data-mode="${m}" title="${LABELS[m]}" aria-label="${LABELS[m]}">${ICONS[m]}</button>`
      ).join("");
      box.addEventListener("click", (e) => {
        const b = e.target.closest("button[data-mode]");
        if (!b) return;
        mode = b.dataset.mode;
        try { localStorage.setItem(KEY, mode); } catch {}
        apply();
      });
    });
    apply();
  });
})();
