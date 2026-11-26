// The theme module: the only UI file that holds color values. Applying a theme
// sets CSS custom properties on <html>; tokens.css derives everything else.
// No imports and no DOM globals, so node can test it as it is.

export interface Theme {
  id: string;
  name: string;
  panel: string;
  text: string;
  muted: string;
  accent: string;
}

/** What is saved in the settings file. No accent: the theme's own. */
export interface Appearance {
  theme: string;
  accent: string | null;
}

const theme = (id: string, name: string, panel: string, text: string, muted: string, accent: string): Theme => ({
  id,
  name,
  panel,
  text,
  muted,
  accent,
});

export const THEMES: readonly Theme[] = [
  theme("carbon", "Carbon", "#1A1A1D", "#F2F2F3", "#A0A0A8", "#FF6363"),
  theme("slate", "Slate", "#1C1F26", "#E8EAED", "#9AA3AE", "#4C8DFF"),
  theme("ink", "Ink", "#16161D", "#EDEDF4", "#A3A3B5", "#8B7CF6"),
  theme("mint", "Mint", "#13201C", "#E8F3EF", "#98B3A9", "#3DD6A3"),
  theme("amber", "Amber", "#1E1A14", "#F4EEE4", "#B3A68F", "#F5A623"),
  theme("graphite", "Graphite", "#1E1E1E", "#F0F0F0", "#A0A0A0", "#0A84FF"),
];

/** New installs, and any saved theme that no longer exists. */
export const DEFAULT_THEME = "carbon";

export const ACCENT_PRESETS = [
  { name: "Yellow", value: "#FFD43B" },
  { name: "Mint", value: "#7FE9D0" },
  { name: "Blue", value: "#9DB8FF" },
  { name: "Violet", value: "#D2A8FF" },
];

const WHITE = "#FFFFFF";
export const ON_ACCENT = { dark: "#111214", light: WHITE };
const SHADE = "#000000";
/** Text on a chart fill: whichever of the two reads better. */
export const INK = { black: SHADE, white: WHITE };
const SEMANTIC = { danger: "#FF7A90", success: "#3FB950" };
/** Disk chart: color-blind-safe hues, one per folder, and grey for the rest. */
const CHART_HUES = ["#66AFD9", "#41BD98", "#DC98C1", "#A8A8F4", "#AABF4A", "#D58385", "#4AB9C6", "#BAA8D2"];
const CHART_GREY = "#7C8494";
/** --c8 on: orange, yellow and magenta, for file types only. */
const CHART_TYPE_HUES = ["#E8872E", "#F2E05A", "#E260E0"];
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
    const c = mix(color, WHITE, step / 50);
    if (ok(c)) return c;
  }
  return WHITE;
}

/** An accent needs 3:1 against the base and readable text on it; else it is lightened. */
export function usableAccent(accent: string, base: string): { accent: string; lightened: boolean } {
  const out = lightenUntil(accent, (c) => contrast(c, base) >= 3 && contrast(c, onAccent(c)) >= 4.5);
  return { accent: out, lightened: out !== accent };
}

/** The extremes of a panel: with glass on it is 96% opaque, so a white desktop
    behind it is the worst case for light text; off, it is the panel itself. */
export function panelColors(t: Theme) {
  return { glass: mix(t.panel, WHITE, 0.04), solid: t.panel };
}

/** The panels, and Settings' cards on them (two 4% layers of the text colour). */
export function textSurfaces(t: Theme): string[] {
  const panels = Object.values(panelColors(t));
  const card = (p: string) => mix(mix(p, t.text, 0.04), t.text, 0.04);
  return [...panels, ...panels.map(card)];
}

/** Hover and selection fills on those: --accent-soft and --btn-bg-hover. */
export function highlights(t: Theme, surfaces: string[], accent: string): string[] {
  return surfaces.flatMap((p) => [mix(p, accent, 0.13), mix(p, t.text, 0.13)]);
}

export const themeById = (id: unknown): Theme =>
  THEMES.find((t) => t.id === id) ?? THEMES.find((t) => t.id === DEFAULT_THEME)!;

const icon = (stroke: string, width: number, path: string) =>
  `url("data:image/svg+xml,${encodeURIComponent(
    `<svg xmlns='http://www.w3.org/2000/svg' width='12' height='12' viewBox='0 0 12 12' fill='none' stroke='${stroke}' stroke-width='${width}' stroke-linecap='round' stroke-linejoin='round'><path d='${path}'/></svg>`,
  )}")`;

export function themeVars(t: Theme, choice: string | null) {
  const { accent, lightened } = usableAccent(normalizeHex(choice) ?? t.accent, t.panel);
  const on = onAccent(accent);
  const panels = Object.values(panelColors(t));
  const visible = (hue: string) => lightenUntil(hue, (c) => panels.every((p) => contrast(c, p) >= 3));
  // All text reads on the panels, Settings' cards, and hover and selection fills.
  // Borders and fills keep the accent itself, which only needs 3:1.
  const surfaces = textSurfaces(t);
  const all = [...surfaces, ...highlights(t, surfaces, accent)];
  const readable = (color: string) => lightenUntil(color, (c) => all.every((p) => contrast(c, p) >= 4.5));
  const muted = readable(t.muted);
  const soft = readable(mix(t.text, muted, 0.35));
  // The faint suggestion in the search box: 3:1, dimmer than other text.
  let ghost = muted;
  for (let step = 0; step < 50; step++) {
    const c = mix(t.panel, muted, step / 50);
    if (all.every((p) => contrast(c, p) >= 3)) {
      ghost = c;
      break;
    }
  }
  const vars: Record<string, string> = {
    "--base": t.panel,
    "--accent": accent,
    "--accent-on": on,
    "--text": readable(t.text),
    "--text-soft": soft,
    "--muted": muted,
    "--ghost": ghost,
    "--accent-text": readable(accent),
    "--shade": SHADE,
    "--danger": readable(SEMANTIC.danger),
    "--success": readable(SEMANTIC.success),
    "--c-grey": visible(CHART_GREY),
    // data URIs cannot read variables, so the icons are built here.
    "--icon-chevron": icon(soft, 1.6, "M2.5 4.5 6 8l3.5-3.5"),
    "--icon-check": icon(on, 2, "M2.5 6.2 5 8.7l4.5-5"),
  };
  [...CHART_HUES, ...CHART_TYPE_HUES].forEach((hue, i) => (vars[`--c${i}`] = visible(hue)));
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
    window's storage. */
export function initialAppearance(old: unknown): Appearance {
  const o = (old && typeof old === "object" ? old : null) as { accent?: unknown; accentChosen?: unknown } | null;
  let accent = normalizeHex(o?.accent);
  // The old mint default is not a choice; nor is the later yellow default
  // that was written to storage without being picked.
  if (accent === "#7FE9D0" || (accent === "#FFD43B" && o?.accentChosen !== true)) accent = null;
  return { theme: DEFAULT_THEME, accent };
}
