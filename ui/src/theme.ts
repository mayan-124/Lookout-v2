// The theme module: the only UI file that holds color values. Applying a theme
// sets CSS custom properties on <html>; tokens.css derives everything else.
// No imports and no DOM globals, so node can test it as it is.

export interface Theme {
  id: string;
  name: string;
  base: string;
  tint1: string;
  tint2: string;
  accent: string;
}

/** What is saved in the settings file. No accent: the theme's own. */
export interface Appearance {
  theme: string;
  accent: string | null;
}

const theme = (id: string, name: string, base: string, tint1: string, tint2: string, accent: string): Theme => ({
  id,
  name,
  base,
  tint1,
  tint2,
  accent,
});

export const THEMES: readonly Theme[] = [
  theme("aurora", "Aurora", "#0D1020", "#3B5BDB", "#C2417A", "#7FE9D0"),
  theme("graphite", "Graphite", "#111214", "#495057", "#868E96", "#FFD43B"),
  theme("neon-grid", "Neon Grid", "#04080F", "#0B3D5C", "#0A2A44", "#00E5FF"),
  theme("synthwave", "Synthwave", "#0B0618", "#7B2FF7", "#F107A3", "#FF4FD8"),
  theme("plasma", "Plasma", "#060B10", "#00B87A", "#0077FF", "#B6FF3B"),
  theme("ice-chrome", "Ice Chrome", "#0A0E14", "#5D7A99", "#B8C7D9", "#A5F3FC"),
  theme("solar-flare", "Solar Flare", "#0F0806", "#FF5A1F", "#B8860B", "#FFC400"),
  theme("matrix", "Matrix", "#020A06", "#00502B", "#0A5C36", "#3DFF8A"),
];

/** New installs. */
export const DEFAULT_THEME = "neon-grid";
/** Installs from before themes keep the palette they had. */
export const UPGRADE_THEME = "graphite";

export const ACCENT_PRESETS = [
  { name: "Yellow", value: "#FFD43B" },
  { name: "Mint", value: "#7FE9D0" },
  { name: "Blue", value: "#9DB8FF" },
  { name: "Violet", value: "#D2A8FF" },
];

export const TEXT_COLORS = { title: "#FFFFFF", soft: "#E3E8F5", muted: "#9AA3BD" };
export const ON_ACCENT = { dark: "#111214", light: "#FFFFFF" };
const SHADE = "#000000";
/** Text on a chart fill: whichever of the two reads better. */
export const INK = { black: SHADE, white: TEXT_COLORS.title };
const SEMANTIC = { danger: "#FF6B63", success: "#3FB950" };
/** Disk chart: color-blind-safe hues, one per folder, and grey for the rest. */
const CHART_HUES = ["#66AFD9", "#41BD98", "#DC98C1", "#A8A8F4", "#AABF4A", "#D58385", "#4AB9C6", "#BAA8D2"];
const CHART_GREY = "#7C8494";
/** These read better with a neutral border than with an accent-tinted one. */
const NEUTRAL_BORDER = ["graphite", "aurora"];

/** `#RRGGBB` (upper case) from `#rgb`, `#rrggbb`, with or without `#`; else null. */
export function normalizeHex(input: unknown): string | null {
  if (typeof input !== "string") return null;
  const m = input.trim().match(/^#?([0-9a-f]{3}|[0-9a-f]{6})$/i);
  if (!m) return null;
  const h = m[1].length === 3 ? [...m[1]].map((c) => c + c).join("") : m[1];
  return `#${h.toUpperCase()}`;
}

const channels = (hex: string) => [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));

/** `amount` (0 to 1) of `over` on top of `under`, in sRGB like CSS color-mix. */
export function mix(under: string, over: string, amount: number): string {
  const u = channels(under);
  const o = channels(over);
  return `#${u
    .map((c, i) =>
      Math.round(c + (o[i] - c) * amount)
        .toString(16)
        .padStart(2, "0"),
    )
    .join("")
    .toUpperCase()}`;
}

function luminance(hex: string): number {
  const [r, g, b] = channels(hex).map((c) => {
    const v = c / 255;
    return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

/** WCAG contrast ratio, 1 to 21. */
export function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

/** Text drawn on the accent: whichever of the two reads better. */
export function onAccent(accent: string): string {
  return contrast(accent, ON_ACCENT.dark) >= contrast(accent, ON_ACCENT.light) ? ON_ACCENT.dark : ON_ACCENT.light;
}

function lightenUntil(color: string, ok: (c: string) => boolean): string {
  for (let step = 0; step < 50; step++) {
    const c = mix(color, TEXT_COLORS.title, step / 50);
    if (ok(c)) return c;
  }
  return TEXT_COLORS.title;
}

/** An accent needs 3:1 against the base and readable text on it; else it is lightened. */
export function usableAccent(accent: string, base: string): { accent: string; lightened: boolean } {
  const out = lightenUntil(accent, (c) => contrast(c, base) >= 3 && contrast(c, onAccent(c)) >= 4.5);
  return { accent: out, lightened: out !== accent };
}

/** The extremes of a panel: the three gradient stops over a white desktop (the
    worst case for light text; the fill is 96% opaque), and the flat panel. */
export function panelColors(t: Theme) {
  const under = mix(t.base, TEXT_COLORS.title, 0.04);
  return {
    "top-left": mix(under, t.tint1, 0.38),
    center: mix(under, t.base, 0.96),
    "bottom-right": mix(under, t.tint2, 0.26),
    flat: mix(t.base, TEXT_COLORS.title, 0.05),
  };
}

export const themeById = (id: unknown): Theme =>
  THEMES.find((t) => t.id === id) ?? THEMES.find((t) => t.id === DEFAULT_THEME)!;

const icon = (stroke: string, width: number, path: string) =>
  `url("data:image/svg+xml,${encodeURIComponent(
    `<svg xmlns='http://www.w3.org/2000/svg' width='12' height='12' viewBox='0 0 12 12' fill='none' stroke='${stroke}' stroke-width='${width}' stroke-linecap='round' stroke-linejoin='round'><path d='${path}'/></svg>`,
  )}")`;

export function themeVars(t: Theme, choice: string | null) {
  const { accent, lightened } = usableAccent(normalizeHex(choice) ?? t.accent, t.base);
  const on = onAccent(accent);
  const panels = Object.values(panelColors(t));
  const visible = (hue: string) => lightenUntil(hue, (c) => panels.every((p) => contrast(c, p) >= 3));
  const vars: Record<string, string> = {
    "--base": t.base,
    "--tint1": t.tint1,
    "--tint2": t.tint2,
    "--accent": accent,
    "--accent-on": on,
    "--glass-border": NEUTRAL_BORDER.includes(t.id)
      ? `color-mix(in srgb, ${TEXT_COLORS.title} 22%, transparent)`
      : mix(t.base, accent, 0.35),
    "--text": TEXT_COLORS.title,
    "--text-soft": TEXT_COLORS.soft,
    "--muted": TEXT_COLORS.muted,
    "--shade": SHADE,
    "--danger": SEMANTIC.danger,
    "--success": SEMANTIC.success,
    "--c-grey": visible(CHART_GREY),
    // data URIs cannot read variables, so the icons are built here.
    "--icon-chevron": icon(TEXT_COLORS.soft, 1.6, "M2.5 4.5 6 8l3.5-3.5"),
    "--icon-check": icon(on, 2, "M2.5 6.2 5 8.7l4.5-5"),
  };
  CHART_HUES.forEach((hue, i) => (vars[`--c${i}`] = visible(hue)));
  return { vars, accent, lightened };
}

interface StyleRoot {
  style: { setProperty(name: string, value: string): void };
}

export function applyTheme(root: StyleRoot, id: string, choice: string | null) {
  const t = themeById(id);
  const { vars, accent, lightened } = themeVars(t, choice);
  for (const [name, value] of Object.entries(vars)) root.style.setProperty(name, value);
  return { theme: t, accent, lightened };
}

/** The first saved appearance. `old` is what earlier versions kept in this
    window's storage; `existing` is an install that was set up before. */
export function initialAppearance(old: unknown, existing: boolean): Appearance {
  const o = (old && typeof old === "object" ? old : null) as { accent?: unknown; accentChosen?: unknown } | null;
  let accent = normalizeHex(o?.accent);
  // The old mint default is not a choice; nor is the later yellow default
  // that was written to storage without being picked.
  if (accent === "#7FE9D0" || (accent === "#FFD43B" && o?.accentChosen !== true)) accent = null;
  return { theme: existing || o ? UPGRADE_THEME : DEFAULT_THEME, accent };
}
