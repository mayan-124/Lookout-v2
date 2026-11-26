// Look and feel. Theme and accent are saved in the settings file; glass and
// reduced motion stay in this window's localStorage. Colors come from theme.ts;
// glass and motion are data attributes on <html> that tokens.css reads.
import { invoke } from "@tauri-apps/api/core";
import { applyTheme, DEFAULT_THEME, initialAppearance, normalizeHex, type Appearance } from "./theme";
import { plain } from "./errors";

const KEY = "lookout.look";
const motionQuery = window.matchMedia("(prefers-reduced-motion: reduce)");

/** Everything stored, including the accent earlier versions kept here. */
function stored(): Record<string, unknown> | null {
  try {
    const raw = JSON.parse(localStorage.getItem(KEY) ?? "null");
    return raw && typeof raw === "object" ? raw : null;
  } catch {
    return null; // storage unavailable: defaults
  }
}

const old = stored();
export const look = $state({
  theme: DEFAULT_THEME,
  /** Custom accent; null uses the theme's. */
  accent: null as string | null,
  /** True when the accent was too dark for the theme and is shown lighter. */
  lightened: false,
  saveError: "",
  glass: typeof old?.glass === "boolean" ? old.glass : true,
  reduceMotion: (typeof old?.reduceMotion === "boolean" ? old.reduceMotion : null) as boolean | null, // null: follow Windows
  /** Effective value, after the Windows preference. */
  reduced: false,
});

function persist() {
  try {
    localStorage.setItem(
      KEY,
      JSON.stringify({ ...stored(), glass: look.glass, reduceMotion: look.reduceMotion }),
    );
  } catch {
    /* not saved */
  }
}

function applyMotion() {
  look.reduced = look.reduceMotion ?? motionQuery.matches;
  document.documentElement.dataset.motion = look.reduced ? "reduced" : "full";
}

/** The glass look is CSS only. Off gives flat solid panels. */
function applyGlass() {
  document.documentElement.dataset.glass = look.glass ? "on" : "solid";
}

function paint() {
  const shown = applyTheme(document.documentElement, look.theme, look.accent);
  look.theme = shown.theme.id;
  look.lightened = shown.lightened;
}

async function saveAppearance() {
  const appearance: Appearance = { theme: look.theme, accent: look.accent };
  try {
    await invoke("set_appearance", { appearance });
    look.saveError = "";
  } catch (e) {
    look.saveError = plain(e);
  }
}

export function setTheme(id: string) {
  look.theme = id;
  paint();
  saveAppearance();
}
/** Shows an accent without saving it (while the color picker is open). */
export function previewAccent(value: string | null) {
  look.accent = normalizeHex(value);
  paint();
}
export function setAccent(value: string | null) {
  previewAccent(value);
  saveAppearance();
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

/** The recommended look: the default theme and accent, glass on, motion as Windows says. */
export function resetLook() {
  look.theme = DEFAULT_THEME;
  look.accent = null;
  look.glass = true;
  look.reduceMotion = null;
  persist();
  applyGlass();
  applyMotion();
  paint();
  saveAppearance();
}

/** Applies the saved theme; main.ts draws nothing until this is done. */
export async function loadAppearance() {
  try {
    const r = await invoke<{ appearance: Appearance | null }>("get_appearance");
    const a = r.appearance ?? initialAppearance(old);
    look.theme = a.theme;
    look.accent = normalizeHex(a.accent);
    paint();
    if (!r.appearance) saveAppearance();
  } catch {
    /* the default theme stays */
  }
}

paint();
applyMotion();
applyGlass();
motionQuery.addEventListener("change", applyMotion);
