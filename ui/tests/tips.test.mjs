// Home tips: three at a time, one swapped by a timer that only runs when wanted.
import assert from "node:assert/strict";
import { test } from "node:test";
import { tipsFor } from "../src/modes.ts";
import { EMPTY_DECK, TIP_MS, deal, fixedTips, startRotation, turn } from "../src/tips.ts";

const row = (example, advanced = false) => ({ id: example, category: "Search", example, mode: "simple", advanced });
const POOL = ["a", "b", "c", "d", "e"].map((e) => row(e));

const ON = { visible: true, home: true, hidden: false, hovered: false, focused: false, reduced: false, poolSize: 5 };
function fakeTimers() {
  const live = new Map();
  let n = 0;
  return {
    live,
    set: (f, ms) => (live.set(++n, { f, ms }), n),
    clear: (id) => live.delete(id),
  };
}

test("the timer runs only on a visible Home and is cleared on stop", () => {
  const t = fakeTimers();
  let turns = 0;
  const stop = startRotation(ON, () => turns++, t);
  assert.equal(t.live.size, 1);
  const [{ f, ms }] = t.live.values();
  assert.equal(ms, TIP_MS);
  assert.equal(TIP_MS, 8000);
  f();
  assert.equal(turns, 1);
  stop();
  assert.equal(t.live.size, 0);
});

test("no timer when hidden, elsewhere, hovered, focused, reduced or nothing to swap", () => {
  const off = [
    { visible: false },
    { home: false },
    { hidden: true },
    { hovered: true },
    { focused: true },
    { reduced: true },
    { poolSize: 3 },
  ];
  for (const change of off) {
    const t = fakeTimers();
    assert.equal(startRotation({ ...ON, ...change }, () => {}, t), undefined, JSON.stringify(change));
    assert.equal(t.live.size, 0);
  }
});

test("one row changes per turn, the whole pool comes round, never twice at once", () => {
  let deck = deal(POOL, EMPTY_DECK);
  assert.deepEqual(deck.shown, ["a", "b", "c"]);
  const seen = new Set(deck.shown);
  for (let i = 0; i < 30; i++) {
    const next = turn(POOL, deck);
    assert.equal(next.shown.filter((e, n) => e !== deck.shown[n]).length, 1);
    assert.equal(new Set(next.shown).size, 3, next.shown.join());
    next.shown.forEach((e) => seen.add(e));
    deck = next;
  }
  assert.equal(seen.size, POOL.length);
  assert.deepEqual(turn(POOL, deal(POOL, EMPTY_DECK)).shown, ["d", "b", "c"]);
  // A small pool is shown whole and stays put.
  const two = POOL.slice(0, 2);
  assert.deepEqual(turn(two, deal(two, EMPTY_DECK)).shown, ["a", "b"]);
  assert.deepEqual(deal([], deck).shown, []);
});

test("rows follow the mode filter", () => {
  const rows = [...POOL, row("x", true), row("y", true)];
  const show = {};
  let deck = EMPTY_DECK;
  const full = tipsFor(rows, "full", show);
  for (let i = 0; i < 6; i++) deck = turn(full, deck);
  assert.ok(deck.shown.some((e) => e === "x" || e === "y"));
  const simple = tipsFor(rows, "simple", show);
  for (let i = 0; i < 12; i++) {
    deck = turn(simple, deck);
    assert.ok(deck.shown.every((e) => e !== "x" && e !== "y"));
    assert.equal(new Set(deck.shown).size, 3);
    assert.ok(fixedTips(simple, i).every((e) => e !== "x" && e !== "y"));
  }
});

test("with Reduce motion the first three, then others only at a new open", () => {
  assert.deepEqual(fixedTips(POOL, 0), ["a", "b", "c"]);
  assert.deepEqual(fixedTips(POOL, 0), ["a", "b", "c"]);
  assert.deepEqual(fixedTips(POOL, 1), ["d", "e", "a"]);
  assert.deepEqual(fixedTips(POOL.slice(0, 2), 4), ["a", "b"]);
});
