// Plain words to features: suggestions, ghost text, the "what I understood"
// line. Pure functions over the registry; no DOM and no calls to the backend.
import { advertised, type Action, type Mode, type Show } from "./modes.ts";

export const MAX_SUGGESTIONS = 2;
const MIN_QUERY = 3;
const FUZZY_WORD = 5;

/** Lower case, no accents, single spaces. */
export const fold = (s: string) =>
  s.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase().replace(/\s+/g, " ").trim();

/** At most one letter changed, added, dropped or swapped with its neighbour. */
export function within1(a: string, b: string): boolean {
  if (Math.abs(a.length - b.length) > 1) return false;
  let i = 0;
  while (i < a.length && i < b.length && a[i] === b[i]) i++;
  if (a.length === b.length) {
    if (a.slice(i + 1) === b.slice(i + 1)) return true;
    return a[i] === b[i + 1] && a[i + 1] === b[i] && a.slice(i + 2) === b.slice(i + 2);
  }
  const [short, long] = a.length < b.length ? [a, b] : [b, a];
  return short.slice(i) === long.slice(i + 1);
}

interface Keys {
  all: { text: string; words: string[] }[];
  commands: string[];
}
// Folded once per action, not per keystroke.
const cache = new WeakMap<Action, Keys>();
function keysOf(a: Action): Keys {
  let k = cache.get(a);
  if (!k) {
    const commands = (a.commands ?? []).map(fold);
    const all = [a.name, ...a.synonyms, ...commands].map((s) => {
      const text = fold(s);
      return { text, words: text.split(" ") };
    });
    cache.set(a, (k = { all, commands }));
  }
  return k;
}

/** 3 the whole name or synonym, 2 its beginning, 1 one typo away, 0 no match. */
function tier(q: string, words: string[], keys: Keys): number {
  let best = 0;
  for (const key of keys.all) {
    if (key.text === q) return 3;
    if (key.text.startsWith(q)) best = 2;
    else if (best === 0 && key.words.length === words.length) {
      const near = words.every(
        (w, i) => w === key.words[i] || (w.length >= FUZZY_WORD && within1(w, key.words[i])),
      );
      if (near) best = 1;
    }
  }
  return best;
}

export interface Suggestion {
  action: Action;
  /** The text is exactly a command word or stem: the row goes first. */
  command: boolean;
}

/**
 * Up to two actions for the typed text. A whole word always finds its action;
 * a beginning or a typo only one the mode advertises.
 */
export function suggest(query: string, actions: Action[], mode: Mode, show: Show): Suggestion[] {
  const q = fold(query);
  if (q.length < MIN_QUERY) return [];
  const words = q.split(" ");
  const found: { action: Action; command: boolean; tier: number; rank: number }[] = [];
  for (const action of actions) {
    // The stem is already in the box: the user is typing the rest.
    if (action.stem && query === action.stem && query.endsWith(" ")) continue;
    const keys = keysOf(action);
    const t = tier(q, words, keys);
    if (t === 0) continue;
    const listed = advertised(action, mode, show);
    if (t < 3 && !listed) continue;
    const command = keys.commands.includes(q);
    found.push({ action, command, tier: t, rank: t * 4 + (command ? 2 : 0) + (listed ? 1 : 0) });
  }
  // A whole word was typed: nothing is guessed from it.
  const whole = found.some((f) => f.tier === 3);
  return found
    .filter((f) => !whole || f.tier > 1)
    .sort((a, b) => b.rank - a.rank)
    .slice(0, MAX_SUGGESTIONS)
    .map(({ action, command }) => ({ action, command }));
}

export interface Row {
  id: string;
  kind: unknown;
  title: string;
  source: string;
  uri?: string | null;
  metadata?: Record<string, unknown>;
}

/** Results that already do what the action offers. */
const covers = (a: Action, r: Row) =>
  !!a.covered &&
  (r.id === a.covered || r.source === a.covered || r.metadata?.plugin === a.covered);

/** Answers, System rows, add-on rows, and files or apps whose name starts as typed. */
function outranks(r: Row, q: string): boolean {
  if (r.kind !== "file" && r.kind !== "folder" && r.kind !== "app") {
    return r.source !== "content" && r.source !== "semantic";
  }
  const title = fold(r.title);
  if (title.startsWith(q)) return true;
  const parts = title.split(/[ ._()\[\]+,-]+/);
  return q.split(" ").every((w) => parts.some((p) => p.startsWith(w)));
}

/**
 * Puts the suggestion rows among the results without reordering those: an exact
 * command word first, anything else below the last row that outranks it.
 */
export function placeSuggestions<T extends Row>(rows: T[], found: Suggestion[], query: string, make: (a: Action) => T): T[] {
  const open = found.filter((s) => !rows.some((r) => covers(s.action, r)));
  if (open.length === 0) return rows;
  const q = fold(query);
  const first = open.filter((s) => s.command).map((s) => make(s.action));
  const later = open.filter((s) => !s.command).map((s) => make(s.action));
  let after = rows.length;
  while (after > 0 && !outranks(rows[after - 1], q)) after--;
  return [...first, ...rows.slice(0, after), ...later, ...rows.slice(after)];
}

/** The faint rest of a command, or "". Only listed commands, never plain searches. */
export function ghostFor(query: string, actions: Action[], mode: Mode, show: Show): string {
  const typed = query.toLowerCase();
  if (typed.length < MIN_QUERY || typed.length !== query.length) return "";
  for (const a of actions) {
    if (typed.length < (a.ghostMin ?? MIN_QUERY) || !a.ghost || !advertised(a, mode, show)) continue;
    const full = a.ghost.find((g) => g.length > typed.length && g.startsWith(typed));
    if (full) return full.slice(typed.length);
  }
  return "";
}

function duration(secs: number): string {
  const parts: [number, string][] = [
    [Math.floor(secs / 3600), "hour"],
    [Math.floor((secs % 3600) / 60), "minute"],
    [secs % 60, "second"],
  ];
  return parts
    .filter(([n]) => n > 0)
    .map(([n, unit]) => `${n} ${unit}${n === 1 ? "" : "s"}`)
    .join(" ");
}

/**
 * One short line saying how a typed command was read, or "". Date and kind words
 * have their own removable labels, so they are not repeated here.
 */
export function readLine(rows: Row[], query: string): string {
  const typed = query.trim();
  for (const r of rows) {
    if (r.kind && typeof r.kind === "object" && "custom" in r.kind) {
      const name = (r.kind as { custom: string }).custom;
      if (name === "timer_new" && typeof r.metadata?.secs === "number") return `Timer · ${duration(r.metadata.secs)}`;
    }
    if (r.source === "quickkey" && typeof r.metadata?.quick_key === "string") return `Quick key · ${r.metadata.quick_key}`;
    if (r.source === "link" && r.uri) return `Web address · ${r.uri}`;
    if (r.source === "calculator") return `Calculation · ${typed}`;
    if (r.source === "units") return `Unit conversion · ${typed}`;
    if (r.source === "path" && r.uri) return `${r.uri.startsWith("\\\\") ? "Network path" : "Path"} · ${r.uri}`;
  }
  return "";
}

/** Typed text worth keeping in the history: not help and not a section button's. */
export const worthRemembering = (query: string) => {
  const q = fold(query);
  return q !== "" && !["help", "?", "clip", "shot"].includes(q);
};

/** Up opens the history: an empty box, or the cursor at its start on the first row. */
export const opensHistory = (query: string, start: number | null, end: number | null, selected: number) =>
  query === "" || (start === 0 && end === 0 && selected === 0);
