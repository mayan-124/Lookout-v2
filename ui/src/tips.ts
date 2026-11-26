// Which tips Home shows and when one is swapped. No DOM, so node can test it.
// The pool is `tipsFor` (modes.ts): the help table, filtered by mode.
import type { HelpRow } from "./modes";

export const TIPS_SHOWN = 3;
export const TIP_MS = 8000;

export interface Deck {
  /** The examples on screen, one per row. */
  shown: string[];
  /** Where in the pool to look for the next tip. */
  cursor: number;
  /** The row replaced next. */
  slot: number;
}

export const EMPTY_DECK: Deck = { shown: [], cursor: 0, slot: 0 };

/** The first tip from `cursor` on that is not in `taken`. */
function nextTip(pool: HelpRow[], taken: string[], cursor: number) {
  for (let i = 0; i < pool.length; i++) {
    const at = (cursor + i) % pool.length;
    if (!taken.includes(pool[at].example)) return { example: pool[at].example, cursor: at + 1 };
  }
  return null;
}

/** Fits the deck to the pool: rows keep their tip while it is in it, gaps are filled. */
export function deal(pool: HelpRow[], deck: Deck): Deck {
  const inPool = new Set(pool.map((r) => r.example));
  const shown = deck.shown.map((e) => (inPool.has(e) ? e : ""));
  let cursor = deck.cursor;
  const size = Math.min(TIPS_SHOWN, inPool.size);
  shown.length = size;
  for (let i = 0; i < size; i++) {
    if (shown[i]) continue;
    const next = nextTip(pool, shown, cursor);
    if (!next) break;
    shown[i] = next.example;
    cursor = next.cursor;
  }
  return { shown, cursor, slot: size > 0 ? deck.slot % size : 0 };
}

/** Replaces one row with the next tip of the pool. */
export function turn(pool: HelpRow[], deck: Deck): Deck {
  const now = deal(pool, deck);
  const next = nextTip(pool, now.shown, now.cursor);
  if (!next) return now;
  const shown = [...now.shown];
  shown[now.slot] = next.example;
  return { shown, cursor: next.cursor, slot: (now.slot + 1) % shown.length };
}

/** Reduce motion: three fixed tips, the next three at each later open (0 is the first). */
export function fixedTips(pool: HelpRow[], opens: number): string[] {
  const size = Math.min(TIPS_SHOWN, pool.length);
  return Array.from({ length: size }, (_, i) => pool[(opens * TIPS_SHOWN + i) % pool.length].example);
}

export interface RotationState {
  visible: boolean;
  home: boolean;
  hidden: boolean;
  hovered: boolean;
  focused: boolean;
  reduced: boolean;
  poolSize: number;
}

export const mayRotate = (s: RotationState) =>
  s.visible && s.home && !s.hidden && !s.hovered && !s.focused && !s.reduced && s.poolSize > TIPS_SHOWN;

/** Starts the timer only when rotation is allowed; returns what stops it. */
export function startRotation(
  state: RotationState,
  onTurn: () => void,
  timers: { set: (f: () => void, ms: number) => unknown; clear: (id: any) => void } = {
    set: (f, ms) => setInterval(f, ms),
    clear: (id) => clearInterval(id),
  },
): (() => void) | undefined {
  if (!mayRotate(state)) return;
  const id = timers.set(onTurn, TIP_MS);
  return () => timers.clear(id);
}
