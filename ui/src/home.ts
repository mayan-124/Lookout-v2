// The Home lists: open windows, pinned items, recent files and clips, and the
// most used apps. Pure, so node can test it as it is.

export const MAX_RECENT = 6;
export const MAX_CLIPS = 3;

/** A clip copied within this long comes first. */
export const FRESH_CLIP_SECS = 600;

export function isFreshClip(row: { ts: number; clip?: boolean }, now: number): boolean {
  return !!row.clip && now - row.ts < FRESH_CLIP_SECS;
}

/** Just-copied clips, then newest first, at most six. */
export function recentRows<T extends { ts: number; clip?: boolean }>(rows: T[], now: number): T[] {
  const fresh = (r: T) => Number(isFreshClip(r, now));
  return [...rows].sort((a, b) => fresh(b) - fresh(a) || b.ts - a.ts).slice(0, MAX_RECENT);
}

export interface HomeRow<I> {
  item: I;
  section: "Open now" | "Pinned" | "Recent" | "Apps";
  ts: number;
  clip: boolean;
  /** Copied in the last few minutes. */
  fresh: boolean;
}

/**
 * Home from top to bottom. Recent holds the last three clips and then files; a
 * pinned item is listed once, under Pinned.
 */
export function homeRows<I extends { id: string; modified_at?: number | null }>(
  lists: { windows: I[]; pinned: I[]; files: I[]; apps: I[] },
  clips: { item: I; ts: number }[],
  now: number,
): HomeRow<I>[] {
  const pinned = new Set(lists.pinned.map((i) => i.id));
  const plain = (section: HomeRow<I>["section"]) => (item: I) => ({ item, section, ts: 0, clip: false, fresh: false });
  const newest = [...clips].sort((a, b) => b.ts - a.ts).slice(0, MAX_CLIPS);
  const files = lists.files
    .filter((i) => !pinned.has(i.id))
    .slice(0, MAX_RECENT - newest.length)
    .map((item) => ({ item, ts: item.modified_at ?? 0, clip: false }));
  const recent = recentRows([...newest.map((c) => ({ ...c, clip: true })), ...files], now).map((r) => ({
    ...r,
    section: "Recent" as const,
    fresh: isFreshClip(r, now),
  }));
  return [
    ...lists.windows.map(plain("Open now")),
    ...lists.pinned.map(plain("Pinned")),
    ...recent,
    ...lists.apps.filter((i) => !pinned.has(i.id)).map(plain("Apps")),
  ];
}
