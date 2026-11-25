// Fill colours of the disk charts. Every colour is derived from the theme's
// variables (see theme.ts), so each theme base gets readable fills.
import { OTHER_TYPE, type CNode } from "./chartlayout.ts";
import { contrast, INK, mix, panelColors, themeById, themeVars } from "./theme.ts";

export type ColourMode = "folder" | "type" | "age";
export const COLOUR_MODES: { id: ColourMode; label: string }[] = [
  { id: "folder", label: "Folder" },
  { id: "type", label: "Type" },
  { id: "age", label: "Age" },
];

/** Same order as `file_type` in core/src/diskusage.rs. */
export const TYPES = [
  "Documents",
  "Images",
  "Video",
  "Audio",
  "Archives",
  "Code",
  "Apps",
  "Other",
  "System and libraries",
  "Disk images and VMs",
  "Data and models",
];

/** Upper age, in days, of every bucket but the last. */
export const AGE_DAYS = [30, 182, 365, 730];

export interface Palette {
  base: string;
  surface: string;
  text: string;
  soft: string;
  accent: string;
  grey: string;
  folder: string[];
  type: string[];
  age: string[]; // recent first
}

const WHITE = INK.white;

/** Black or white, whichever reads better on the fill (always at least 4.5:1). */
export function textOn(fill: string): string {
  return contrast(fill, INK.black) >= contrast(fill, INK.white) ? INK.black : INK.white;
}

export function palette(themeId: string, accentChoice: string | null): Palette {
  const theme = themeById(themeId);
  const { vars, accent } = themeVars(theme, accentChoice);
  const hues = [0, 1, 2, 3, 4, 5, 6, 7].map((i) => vars[`--c${i}`]);
  const grey = vars["--c-grey"];
  const grounds = [theme.base, ...Object.values(panelColors(theme))];
  const visible = (c: string) => grounds.every((g) => contrast(c, g) >= 3);
  // One hue from nearly white (recent) down to the darkest shade still visible.
  const hue = hues[0];
  let sink = 0;
  while (sink < 0.9 && visible(mix(hue, theme.base, sink + 0.02))) sink += 0.02;
  const light = mix(hue, WHITE, 0.82);
  const dark = mix(hue, theme.base, sink);
  const age = [light, mix(light, hue, 0.55), hue, mix(hue, dark, 0.55), dark];
  return {
    base: theme.base,
    surface: mix(theme.base, WHITE, 0.05),
    text: vars["--text"],
    soft: vars["--text-soft"],
    accent,
    grey,
    folder: hues,
    type: [...hues.slice(0, OTHER_TYPE), grey, ...[8, 9, 10].map((i) => vars[`--c${i}`])],
    age,
  };
}

/** Bucket of a modified date, 0 = most recent; -1 when the date is unknown. */
export function ageBucket(modified: number, now: number): number {
  if (!modified) return -1;
  const days = (now - modified) / 86_400;
  const i = AGE_DAYS.findIndex((d) => days < d);
  return i < 0 ? AGE_DAYS.length : i;
}

export function fillFor(node: CNode, mode: ColourMode, branch: number, depth: number, p: Palette, now: number): string {
  // By type, a group has a type like anything else: grey means unknown types only.
  if (mode === "type") return p.type[node.cat] ?? p.grey;
  if (node.kind === "other" || node.kind === "unlisted") return p.grey;
  if (mode === "age") return p.age[ageBucket(node.modified, now)] ?? p.grey;
  // Deeper levels are lighter shades: never closer to the dark base.
  return mix(p.folder[branch % p.folder.length], WHITE, Math.min(depth, 3) * 0.15);
}

/** Legend entries for the current mode; folder colours need none. `split` gives the type entries their totals. */
export function legend(mode: ColourMode, p: Palette, now: number, split?: number[]): { colour: string; label: string; size?: number }[] {
  if (mode === "type") {
    const keys = TYPES.map((label, i) => ({ colour: p.type[i], label, size: split?.[i] }));
    // The types after "Other" are listed only when the scan has some (an older
    // saved scan has none), and "Other" goes last.
    const added = keys.slice(OTHER_TYPE + 1).filter((k) => !split || k.size);
    return [...keys.slice(0, OTHER_TYPE), ...added, keys[OTHER_TYPE]];
  }
  if (mode !== "age") return [];
  const date = (days: number) =>
    new Date((now - days * 86_400) * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
  return p.age.map((colour, i) => ({
    colour,
    label:
      i === 0
        ? `Since ${date(AGE_DAYS[0])}`
        : i === AGE_DAYS.length
          ? `Before ${date(AGE_DAYS[i - 1])}`
          : `${date(AGE_DAYS[i])} – ${date(AGE_DAYS[i - 1])}`,
  }));
}
