// Look and feel preferences: accent color, glass effect, reduced motion.
// Stored only in this window's localStorage; they are not part of the saved
// settings file. Applied to <html> as a CSS variable and data attributes that
// tokens.css reads.
import { getCurrentWindow, Effect } from "@tauri-apps/api/window";

export const ACCENTS = [
  { name: "Mint", value: "#6EE7C8" },
  { name: "Blue", value: "#8AB4FF" },
  { name: "Amber", value: "#FFB86B" },
  { name: "Violet", value: "#C792FF" },
] as const;

const KEY = "lookout.look";
const motionQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
const transparencyQuery = window.matchMedia("(prefers-reduced-transparency: reduce)");

interface Stored {
  accent: string;
  glass: boolean;
  reduceMotion: boolean | null; // null: follow Windows
}

function load(): Stored {
  const base: Stored = { accent: ACCENTS[0].value, glass: true, reduceMotion: null };
  try {
    const raw = JSON.parse(localStorage.getItem(KEY) ?? "null");
    if (raw && typeof raw === "object") {
      if (ACCENTS.some((a) => a.value === raw.accent)) base.accent = raw.accent;
      if (typeof raw.glass === "boolean") base.glass = raw.glass;
      if (typeof raw.reduceMotion === "boolean") base.reduceMotion = raw.reduceMotion;
    }
  } catch {
    /* storage unavailable: defaults */
  }
  return base;
}

const saved = load();
export const look = $state({
  accent: saved.accent,
  glass: saved.glass,
  reduceMotion: saved.reduceMotion as boolean | null,
  /** Effective values, after Windows preferences and platform support. */
  reduced: false,
  glassActive: false,
});

function persist() {
  try {
    localStorage.setItem(
      KEY,
      JSON.stringify({ accent: look.accent, glass: look.glass, reduceMotion: look.reduceMotion }),
    );
  } catch {
    /* not saved */
  }
}

function applyMotion() {
  look.reduced = look.reduceMotion ?? motionQuery.matches;
  document.documentElement.dataset.motion = look.reduced ? "reduced" : "full";
}

let seq = 0;
/** Mica on Windows 11. Anything else (Windows 10, transparency off, error) is a solid panel. */
async function applyGlass() {
  const mine = ++seq;
  const want = look.glass && !transparencyQuery.matches;
  let on = false;
  try {
    const win = getCurrentWindow();
    if (want) {
      const light = document.documentElement.dataset.theme === "light";
      // Mica follows the window theme. An explicit Light/Dark choice is passed on;
      // Auto hands control back to Windows so prefers-color-scheme stays truthful.
      const auto = document.documentElement.dataset.themePref !== "light" && document.documentElement.dataset.themePref !== "dark";
      await win.setTheme(auto ? null : light ? "light" : "dark");
      await win.setEffects({ effects: [Effect.Mica] });
      on = true;
    } else {
      await win.clearEffects();
    }
  } catch {
    on = false;
  }
  if (mine !== seq) return;
  look.glassActive = on;
  document.documentElement.dataset.glass = on ? "on" : "solid";
}

function applyAccent() {
  document.documentElement.style.setProperty("--accent-pick", look.accent);
}

export function setAccent(value: string) {
  look.accent = value;
  applyAccent();
  persist();
}
export function setGlass(on: boolean) {
  look.glass = on;
  persist();
  applyGlass();
}
export function setReduceMotion(on: boolean) {
  look.reduceMotion = on;
  persist();
  applyMotion();
}

document.documentElement.dataset.glass = "solid";
applyAccent();
applyMotion();
applyGlass();
motionQuery.addEventListener("change", applyMotion);
transparencyQuery.addEventListener("change", applyGlass);
// The Mica variant follows the light/dark theme.
new MutationObserver(applyGlass).observe(document.documentElement, {
  attributes: true,
  attributeFilter: ["data-theme", "data-theme-pref"],
});
