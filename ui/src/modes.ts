// What each mode shows: dock, help, tips, actions and footer hints. Visibility only;
// no imports and no DOM, so node can test it as it is.

export type Mode = "simple" | "standard" | "full";
export type Feature = "disk" | "clipboard" | "capture" | "timers" | "quick_keys" | "filters";
export type Show = Record<Feature, boolean>;

export interface UiState {
  mode: Mode;
  /** The switches differ from the mode's. */
  custom: boolean;
  show: Show;
  hide_tips: boolean;
  advanced_open: boolean;
  disk_notice: boolean;
}

export const MODES: readonly { id: Mode; name: string; what: string }[] = [
  { id: "simple", name: "Simple", what: "Just search and settings." },
  { id: "standard", name: "Standard", what: "Adds clipboard and capture." },
  { id: "full", name: "Full", what: "Adds disk and every tip." },
];

export const modeLabel = (ui: Pick<UiState, "mode" | "custom">) =>
  ui.custom ? "Custom" : MODES.find((m) => m.id === ui.mode)!.name;

/** The sections, in Ctrl+1 to Ctrl+5 order. */
export const SECTIONS = ["Search", "Disk", "Clipboard", "Capture", "Settings"] as const;
const SECTION_FEATURE: (Feature | null)[] = [null, "disk", "clipboard", "capture", null];

/** The dock buttons. `n` is the section number, which hiding never changes. */
export const dockItems = (show: Show) =>
  SECTIONS.map((name, n) => ({ name, n })).filter(({ n }) => {
    const f = SECTION_FEATURE[n];
    return f === null || show[f];
  });

/** Typed text that opens the disk analyzer, whatever the mode. */
export const isDiskCommand = (q: string) => /^disk( usage| analy[sz]er)?$/i.test(q.trim());

/** The section typed text belongs to: clipboard and capture searches, else search. */
export const typedSection = (q: string) =>
  /^\s*(clip|clipboard|clips)(\s|$)/i.test(q) ? 2 : /^\s*(shot|screenshot)(\s|$)/i.test(q) ? 3 : 0;

/** One entry of the registry (registry.json). */
export interface Action {
  id: string;
  name: string;
  /** The help heading and the icon. */
  category: string;
  /** The Settings > Features switch that shows it, if any. */
  feature?: Feature;
  /** The smallest mode that advertises it. */
  mode: Mode;
  /** Folded under "Advanced" in Simple and Standard. */
  advanced?: boolean;
  /** False keeps it out of the Home tips. */
  tip?: boolean;
  /** Plain words and common typos that suggest it. */
  synonyms: string[];
  /** Words that are the command itself: its suggestion goes first. */
  commands?: string[];
  example: string;
  description: string;
  /** What choosing it does. "fill" needs more typing; the rest run. */
  run: "fill" | "show" | "disk" | "settings" | "appearance" | "privacy" | "setup" | "theme" | "capture";
  /** The text put in the search box by "fill" and "show". */
  stem?: string;
  /** The add-on action "capture" starts. */
  target?: string;
  /** Commands completed by the faint text, and how much must be typed first. */
  ghost?: string[];
  ghostMin?: number;
  /** A result source, id or add-on that makes the suggestion needless. */
  covered?: string;
  /** What a help row or tip types instead of the example. */
  try?: string;
  /** Place in the Home quick-action grid. */
  home?: number;
  /** The grid leaves it out below this mode. */
  homeMin?: Mode;
  /** A shorter name for the grid button. */
  label?: string;
}

export const RANK: Record<Mode, number> = { simple: 0, standard: 1, full: 2 };
/** The switches Simple turns off (core/src/uimode.rs). */
const SIMPLE_OFF: Feature[] = ["disk", "clipboard", "capture", "quick_keys"];

/**
 * Whether help, tips, the quick grid and ghost text list an action. A switch that
 * is on always lists it. One that is off hides it, except where Simple itself
 * turned it off and the action is meant for every mode (capture).
 */
export function advertised(a: Action, mode: Mode, show: Show): boolean {
  if (a.feature && show[a.feature]) return true;
  if (RANK[mode] < RANK[a.mode]) return false;
  return !a.feature || (mode === "simple" && SIMPLE_OFF.includes(a.feature));
}

/** A help row, or the line that folds the advanced groups. */
export type HelpEntry = { row: Action } | { toggle: true; open: boolean };

const shown = (rows: Action[], mode: Mode, show: Show) => rows.filter((r) => advertised(r, mode, show));

/** Full lists everything in table order; else the basic groups, then "Advanced". */
export function helpEntries(rows: Action[], mode: Mode, show: Show, open: boolean): HelpEntry[] {
  const list = shown(rows, mode, show);
  const entry = (row: Action): HelpEntry => ({ row });
  if (mode === "full") return list.map(entry);
  const advanced = list.filter((r) => r.advanced);
  return [
    ...list.filter((r) => !r.advanced).map(entry),
    ...(advanced.length > 0 ? [{ toggle: true as const, open }] : []),
    ...(open ? advanced.map(entry) : []),
  ];
}

/** The rows Home may show as a tip. Advanced ones only in Full. */
export const tipsFor = (rows: Action[], mode: Mode, show: Show) =>
  shown(rows, mode, show).filter((r) => r.tip !== false && (mode === "full" || !r.advanced));

export type View = "home" | "history" | "results" | "help" | "setup" | "settings" | "disk" | "confirm" | "done";

export const MAX_HINTS = 3;

/** The key hints under the dock: only what works in this view. */
export function footerHints(
  view: View,
  o: { mode: Mode; hotkey: string; enter?: string | null; path?: boolean; clears?: boolean },
): string[] {
  const jump = o.mode === "full" ? ["Ctrl+1–5 sections"] : o.hotkey ? [`${o.hotkey} opens Lookout`] : [];
  const esc = o.clears ? "Esc clear" : "Esc hide";
  const enter = o.enter ? `Enter ${o.enter.replace("…", "").toLowerCase()}` : "↑↓ pick";
  const hints: Record<View, string[]> = {
    home: ["Type ? for help", esc, ...jump],
    history: ["Enter use", "Del remove", "Esc back"],
    results: [enter, o.path ? "Tab complete" : "Tab filter", esc],
    help: ["↑↓ pick", "Enter try", esc],
    setup: ["Tab next", "Enter choose", "Esc skip setup"],
    settings: ["Tab next", "Esc close"],
    disk: ["↑↓ move", "→ expand", "Esc close"],
    confirm: ["Tab move", "Enter choose", "Esc cancel"],
    done: ["Esc close"],
  };
  return hints[view].slice(0, MAX_HINTS);
}
