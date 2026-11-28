// What the bar brings back when it is reopened. Pure, so node can test it as it
// is. The search lives in the window's memory only: nothing here stores it.

/** The last search comes back if the bar was hidden for less than this, unless Settings says otherwise. */
export const SEARCH_KEEP_SECS = 30;
/** Settings, Disk and the setup come back for longer. */
export const PAGE_KEEP_SECS = 5 * 60;

/** Whole seconds hidden, as the shell reports them; none if never hidden. */
export function keepsSearch(
  hiddenSecs: number | null,
  remember: boolean,
  query: string,
  keepSecs: number = SEARCH_KEEP_SECS,
): boolean {
  return remember && hiddenSecs !== null && hiddenSecs < keepSecs && query.trim() !== "";
}

/** `closed`: the user left with Esc or by opening something. */
export function keepsPage(hiddenSecs: number | null, open: boolean, closed: boolean): boolean {
  return open && !closed && hiddenSecs !== null && hiddenSecs < PAGE_KEEP_SECS;
}
