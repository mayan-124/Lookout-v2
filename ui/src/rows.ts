// What each kind of result does on each key, in one place: Enter, Shift+Enter,
// the actions list (Right arrow or right-click), Ctrl+C, Ctrl+Shift+C, Ctrl+E,
// Ctrl+D and Delete. No DOM, so node can test it.
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
  /** Shift+Enter: paste a clip into the app that was in front. */
  paste?: string;
  /** Ctrl+Enter or Ctrl+E */
  reveal?: string;
  /** "Rename…" on a folder, which asks for a pattern (actions list only). */
  rename?: boolean;
  /** The folder it sits in (actions list only). */
  openFolder?: string;
  /** Ctrl+D: pins to Home, or takes it off again. */
  pin?: { action: string; label: string };
  /** An open window's other actions (actions list only). */
  window?: { action: string; label: string }[];
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

const ON_DISK = /^([a-z]:[\\/]|\\\\)/i;
/** What can be pinned: never an add-on's result, a web search or a window. */
const PINNABLE = ["file", "folder", "app", "url", "system"];

export const UNPIN = { action: "saved_remove", label: "★ Unpin" };
const PIN = { action: "saved_add", label: "☆ Pin to Home" };

/** `pinned`: the result is on the pinned list already. */
export function rowKeys(i: RowItem, pinned = false): Keys {
  const kind = kindOf(i);
  // A row of the pinned list itself, where Delete unpins.
  const listed = i.source === "saved";
  const pinnable = PINNABLE.includes(kind) && !i.source.startsWith("plugin:") && i.source !== "websearch" && !!i.uri;
  const pin: Keys = pinnable ? { pin: pinned || listed ? UNPIN : PIN, ...(listed ? { remove: UNPIN } : {}) } : {};
  const copyPath = { action: "copy_path", label: "Copy path" };
  if (isFileRow(kind, i.source, i.uri)) {
    const folder: Keys = kind === "folder" ? { rename: true } : {};
    return { openFolder: "open_folder", copy: copyPath, copyFile: "copy_file", reveal: "reveal", ...folder, ...pin };
  }
  if (kind === "app" && pinnable) {
    // A Store app has no file to point at.
    return ON_DISK.test(i.uri!) ? { openFolder: "open_folder", copy: copyPath, reveal: "reveal", ...pin } : pin;
  }
  if (kind === "window") {
    return {
      window: [
        { action: "window_minimize", label: "Minimize" },
        { action: "window_close", label: "Close" },
      ],
    };
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
  if (ANSWERS.includes(kind)) return { copy: { action: "answer_copy", label: "Copy" }, ...pin };
  return pin;
}

/** The clipboard add-on's answer when a clip was copied but could not be pasted. */
export const PASTE_FAILED = "Copied. Press Ctrl+V";

/**
 * Whether a finished action puts the bar away: a paste that worked always, a
 * copy or a saved note while "Hide the bar after copy or save" is on.
 */
export function hidesBar(action: string, message: string, hideAfterCopy: boolean): boolean {
  const verb = verbOf(action);
  if (verb === "paste_clip" || verb === "paste_plain") return message !== PASTE_FAILED;
  // Locked, asleep, leaving or in another window: the next show starts clean.
  const leaves = ["system_lock", "system_sleep", "system_restart", "system_sign_out", "window_focus"];
  if (leaves.includes(verb)) return true;
  const done = ["copy_path", "copy_file", "copy_clip", "copy_pretty", "answer_copy", "note_copy", "note_save"];
  return hideAfterCopy && done.includes(verb);
}

/** Actions after which the list on screen is out of date. */
export const changesList = (action: string) =>
  ["note_save", "note_delete", "timer_cancel", "delete_clip", "saved_remove", "window_minimize", "window_close"].includes(
    verbOf(action),
  );

/** Actions that change the pinned list, so Home's lists are asked for again. */
export const changesPins = (action: string) => ["saved_add", "saved_remove", "pin_up", "pin_down"].includes(action);

/** Actions that say nothing when they work: what changed is on screen. */
export const isQuiet = (action: string) =>
  ["saved_add", "pin_up", "pin_down", "window_minimize", "window_close"].includes(action);

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
