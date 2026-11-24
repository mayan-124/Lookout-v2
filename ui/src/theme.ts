// Applies the Appearance setting. The page always carries data-theme="light" or
// "dark": Auto is resolved here from Windows (prefers-color-scheme) and follows
// Windows changes while the app runs. Tokens live in app.css.
export type ThemePref = "auto" | "light" | "dark";

const windowsLight = window.matchMedia("(prefers-color-scheme: light)");
let pref: ThemePref = "auto";

function resolve() {
  const light = pref === "light" || (pref === "auto" && windowsLight.matches);
  document.documentElement.dataset.theme = light ? "light" : "dark";
  document.documentElement.dataset.themePref = pref;
}

export function applyTheme(next: ThemePref) {
  pref = next;
  resolve();
}

windowsLight.addEventListener("change", resolve);
resolve();
