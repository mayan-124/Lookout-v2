// The Home "Recent" list. Pure, so node can test it as it is.

export const MAX_RECENT = 6;
/** With fewer rows than this the card shows its empty-state text instead. */
export const MIN_RECENT = 3;

/** Newest first, at most six; none when there is too little to be a list. */
export function recentRows<T extends { ts: number }>(rows: T[]): T[] {
  const newest = [...rows].sort((a, b) => b.ts - a.ts).slice(0, MAX_RECENT);
  return newest.length < MIN_RECENT ? [] : newest;
}
