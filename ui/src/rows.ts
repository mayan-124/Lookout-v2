// What each kind of result does on each key, in one place: Enter, the menu,
// Ctrl+C, Ctrl+Shift+C, Ctrl+E and Delete. No DOM, so node can test it.
import { isFileRow } from "./dragout.ts";

export interface RowItem {
  kind: string | { custom: string };
  source: string;
  uri: string | null;
  metadata?: Record<string, unknown>;
}

export interface Keys {
  /** Ctrl+C */
  copy?: { action: string; label: string };
  /** Ctrl+Shift+C */
  copyFile?: string;
  /** Menu only: paste a clip into the app that was in front. */
  paste?: string;
  /** Ctrl+E */
  reveal?: string;
  /** Delete. Every one of these can be undone. */
  remove?: { action: string; label: string };
}

const kindOf = (i: RowItem) => (typeof i.kind === "string" ? i.kind : i.kind.custom);

/** Kinds whose copy is their own text. */
const ANSWERS = ["date_answer", "time_answer", "calculator", "conversion", "url"];

/** The full id of an action an add-on result offers, or null. */
export function addonAction(i: RowItem, name: string): string | null {
  const m = i.metadata;
  const also = Array.isArray(m?.plugin_also) ? (m.plugin_also as unknown[]) : [];
  return typeof m?.plugin === "string" && (m.plugin_action === name || also.includes(name))
    ? `plugin/${m.plugin}/${name}`
    : null;
}

/** "plugin/<add-on>/paste_clip" and "paste_clip" are the same verb. */
export const verbOf = (action: string) => action.slice(action.lastIndexOf("/") + 1);

export function rowKeys(i: RowItem): Keys {
  const kind = kindOf(i);
  if (isFileRow(kind, i.source, i.uri)) {
    return { copy: { action: "copy_path", label: "Copy path" }, copyFile: "copy_file", reveal: "reveal" };
  }
  if (kind === "note") {
    return { copy: { action: "note_copy", label: "Copy" }, remove: { action: "note_delete", label: "Delete" } };
  }
  if (kind === "timer") return { remove: { action: "timer_cancel", label: "Cancel timer" } };
  if (kind === "clipboard") {
    const copy = addonAction(i, "copy_clip");
    const remove = addonAction(i, "delete_clip");
    return {
      paste: addonAction(i, "paste_clip") ?? undefined,
      copy: copy ? { action: copy, label: "Copy" } : undefined,
      remove: remove ? { action: remove, label: "Delete" } : undefined,
    };
  }
  if (ANSWERS.includes(kind)) return { copy: { action: "answer_copy", label: "Copy" } };
  return {};
}

/** Actions after which the list on screen is out of date. */
export const changesList = (action: string) =>
  ["note_save", "note_delete", "timer_cancel", "delete_clip"].includes(verbOf(action));

const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/** How long ago, as the right side of a row says it. `now` and `ts` are seconds. */
export function ageLabel(ts: number, now: number): string {
  const secs = Math.max(0, now - ts);
  if (secs < 60) return "just now";
  if (secs < 3600) return `${Math.floor(secs / 60)} min ago`;
  const midnight = (s: number) => new Date(s * 1000).setHours(0, 0, 0, 0);
  const days = Math.round((midnight(now) - midnight(ts)) / 86_400_000);
  if (days <= 0) return `${Math.floor(secs / 3600)} h ago`;
  if (days === 1) return "Yesterday";
  const d = new Date(ts * 1000);
  if (days < 7) return WEEKDAYS[d.getDay()];
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}
