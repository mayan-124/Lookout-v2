// The Home "Recent" list. Pure, so node can test it as it is.

export const MAX_RECENT = 6;
/** With fewer rows than this the card shows its empty-state text instead. */
export const MIN_RECENT = 3;

/** A clip copied within this long comes first. */
export const FRESH_CLIP_SECS = 600;

export function isFreshClip(row: { ts: number; clip?: boolean }, now: number): boolean {
  return !!row.clip && now - row.ts < FRESH_CLIP_SECS;
}

/**
 * Just-copied clips, then newest first, at most six; none when there is too
 * little to be a list and nothing was just copied.
 */
export function recentRows<T extends { ts: number; clip?: boolean }>(rows: T[], now: number): T[] {
  const fresh = (r: T) => Number(isFreshClip(r, now));
  const top = [...rows].sort((a, b) => fresh(b) - fresh(a) || b.ts - a.ts).slice(0, MAX_RECENT);
  return top.length < MIN_RECENT && !top.some(fresh) ? [] : top;
}
