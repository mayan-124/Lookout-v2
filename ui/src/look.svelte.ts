// Look and feel preferences: accent color, glass effect, reduced motion.
// Stored only in this window's localStorage; they are not part of the saved
// settings file. Applied to <html> as a CSS variable and data attributes that
// tokens.css reads.
export const ACCENTS = [
  { name: "Mint", value: "#7FE9D0" },
  { name: "Blue", value: "#9DB8FF" },
  { name: "Amber", value: "#FFC27A" },
  { name: "Violet", value: "#D2A8FF" },
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

/** The glass look is CSS only. Off, or backdrop-filter unsupported, gives flat solid panels. */
const blurSupported =
  typeof CSS !== "undefined" &&
  (CSS.supports("backdrop-filter", "blur(1px)") || CSS.supports("-webkit-backdrop-filter", "blur(1px)"));
function applyGlass() {
  const on = look.glass && blurSupported && !transparencyQuery.matches;
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

applyAccent();
applyMotion();
applyGlass();
motionQuery.addEventListener("change", applyMotion);
transparencyQuery.addEventListener("change", applyGlass);
