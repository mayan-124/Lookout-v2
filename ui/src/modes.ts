// What each mode shows: dock, help, tips and footer hints. Visibility only;
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

export interface HelpRow {
  group: string;
  what: string;
  example: string;
  fill: string;
  feature: Feature | null;
  advanced: boolean;
}

/** A help row, or the line that folds the advanced groups. */
export type HelpEntry = { row: HelpRow } | { toggle: true; open: boolean };

const shown = (rows: HelpRow[], show: Show) => rows.filter((r) => !r.feature || show[r.feature]);

/** Full lists everything in table order; else the basic groups, then "Advanced". */
export function helpEntries(rows: HelpRow[], mode: Mode, show: Show, open: boolean): HelpEntry[] {
  const list = shown(rows, show);
  const entry = (row: HelpRow): HelpEntry => ({ row });
  if (mode === "full") return list.map(entry);
  const advanced = list.filter((r) => r.advanced);
  return [
    ...list.filter((r) => !r.advanced).map(entry),
    ...(advanced.length > 0 ? [{ toggle: true as const, open }] : []),
    ...(open ? advanced.map(entry) : []),
  ];
}

/** The rows Home may show as a tip. Advanced ones only in Full. */
export const tipsFor = (rows: HelpRow[], mode: Mode, show: Show) =>
  shown(rows, show).filter((r) => mode === "full" || !r.advanced);

export type View = "home" | "results" | "help" | "setup" | "settings" | "disk" | "confirm" | "done";

export const MAX_HINTS = 5;

/** The key hints under the dock: only what works in this view. */
export function footerHints(
  view: View,
  o: { mode: Mode; hotkey: string; enter?: string | null; path?: boolean; clears?: boolean },
): string[] {
  const jump = o.mode === "full" ? ["Ctrl+1–5 sections"] : [];
  const esc = o.clears ? "Esc clear" : "Esc hide";
  const hints: Record<View, string[]> = {
    home: ["Type ? for help", esc, ...(o.hotkey ? [`${o.hotkey} opens Lookout`] : []), ...jump],
    results: [
      ...(o.enter ? [`Enter ${o.enter.replace("…", "").toLowerCase()}`] : []),
      "↑↓ pick",
      o.path ? "Tab complete" : "Tab filter",
      esc,
      ...jump,
    ],
    help: ["↑↓ pick", "Enter try", esc],
    setup: ["Tab next", "Enter choose", "Esc skip setup"],
    settings: ["Tab next", "Esc close"],
    disk: ["↑↓ move", "→ expand", "Backspace up", "Esc close"],
    confirm: ["Tab move", "Enter choose", "Esc cancel"],
    done: ["Esc close"],
  };
  return hints[view].slice(0, MAX_HINTS);
}
