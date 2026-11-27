// Dragging a result out of the bar. This only decides when a press on a row
// becomes a drag; the backend decides what may leave.

/** Pointer travel, in CSS pixels, before a press is a drag and not a click. */
export const DRAG_THRESHOLD = 6;

export function movedEnough(dx: number, dy: number): boolean {
  return Math.hypot(dx, dy) >= DRAG_THRESHOLD;
}

/** Rows that stand for a file or folder: not apps, actions, text or plugin commands. */
export function isFileRow(kind: string, source: string, uri: string | null): boolean {
  return (kind === "file" || kind === "folder") && !source.startsWith("plugin:") && !!uri;
}

/** Rows that stand for one stored clip. */
export function isClipRow(kind: string, source: string): boolean {
  return kind === "clipboard" && source.startsWith("plugin:");
}
