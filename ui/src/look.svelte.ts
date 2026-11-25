// Look and feel preferences: accent color, glass effect, reduced motion.
// Stored only in this window's localStorage; they are not part of the saved
// settings file. Applied to <html> as a CSS variable and data attributes that
// tokens.css reads.
import tokens from "./tokens.css?raw";

// The color values are defined in tokens.css only.
function pick(token: string): string {
  return tokens.match(new RegExp(`--pick-${token}:\\s*(#[0-9a-fA-F]{6})`))![1];
}

export const ACCENTS = [
  { name: "Yellow", value: pick("yellow") },
  { name: "Mint", value: pick("mint") },
  { name: "Blue", value: pick("blue") },
  { name: "Violet", value: pick("violet") },
];
/** The default before the Graphite palette. Saved without a choice, it moves to the new default. */
const OLD_DEFAULT = pick("mint");

const KEY = "lookout.look";
const motionQuery = window.matchMedia("(prefers-reduced-motion: reduce)");

interface Stored {
  accent: string;
  accentChosen: boolean; // picked in Settings
  glass: boolean;
  reduceMotion: boolean | null; // null: follow Windows
}

function load(): Stored {
  const base: Stored = { accent: ACCENTS[0].value, accentChosen: false, glass: true, reduceMotion: null };
  try {
    const raw = JSON.parse(localStorage.getItem(KEY) ?? "null");
    if (raw && typeof raw === "object") {
      base.accentChosen = raw.accentChosen === true;
      const kept = base.accentChosen || raw.accent !== OLD_DEFAULT;
      if (kept && ACCENTS.some((a) => a.value === raw.accent)) base.accent = raw.accent;
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
  accentChosen: saved.accentChosen,
  glass: saved.glass,
  reduceMotion: saved.reduceMotion as boolean | null,
  /** Effective value, after the Windows preference. */
  reduced: false,
});

function persist() {
  try {
    localStorage.setItem(
      KEY,
      JSON.stringify({
        accent: look.accent,
        accentChosen: look.accentChosen,
        glass: look.glass,
        reduceMotion: look.reduceMotion,
      }),
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

function applyAccent() {
  document.documentElement.style.setProperty("--accent-pick", look.accent);
}

export function setAccent(value: string) {
  look.accent = value;
  look.accentChosen = true;
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
