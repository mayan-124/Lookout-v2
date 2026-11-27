// Home and the footer: Recent, the searches in the same card, the button grid
// and the one quiet line under the dock. Layout is read from the markup.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { MIN_RECENT, recentRows } from "../src/home.ts";
import { footerStatus } from "../src/indexstatus.ts";
import { MAX_HINTS, footerHints } from "../src/modes.ts";

const text = (path) => readFileSync(new URL(path, import.meta.url), "utf8").replaceAll("\r\n", "\n");
const app = text("../src/App.svelte");
const markup = app.slice(app.indexOf("<main"), app.indexOf("<style>"));
const style = app.slice(app.indexOf("<style>"));
const rule = (selector) => {
  const at = style.indexOf(`\n  ${selector} {`);
  assert.ok(at >= 0, selector);
  return style.slice(at, style.indexOf("}", at));
};

test("Recent shows its empty text below three rows, else the newest six", () => {
  const rows = (n) => Array.from({ length: n }, (_, ts) => ({ ts }));
  assert.equal(MIN_RECENT, 3);
  for (const n of [0, 1, 2]) assert.deepEqual(recentRows(rows(n), 10_000), []);
  assert.deepEqual(recentRows(rows(8), 10_000).map((r) => r.ts), [7, 6, 5, 4, 3, 2]);
  // The files come from the backend, which drops junk and skipped places.
  assert.ok(app.includes('invoke<Item[]>("recent_files")') && !app.includes('"past week" })'));
});

test("clips from the last ten minutes lead Recent, then take their turn by time", () => {
  const now = 10_000;
  const rows = [
    { ts: now - 5, clip: false },
    { ts: now - 599, clip: true },
    { ts: now - 60, clip: true },
    { ts: now - 600, clip: true },
    { ts: now - 700, clip: false },
  ];
  assert.deepEqual(recentRows(rows, now).map((r) => now - r.ts), [60, 599, 5, 600, 700]);
  assert.deepEqual(recentRows(rows, now + 600).map((r) => now - r.ts), [5, 60, 599, 600, 700]);
  // One just-copied clip is shown even when there is nothing else.
  assert.equal(recentRows([rows[2]], now).length, 1);
  assert.ok(app.includes('"Clip · just copied"') || app.includes('"Clip · just copied"'));
});

test("the recent searches take over the Recent card without changing its height", () => {
  assert.ok(!markup.includes('class="history'), "no separate panel");
  const card = markup.slice(markup.indexOf('<section class="card glass pick"'), markup.indexOf('<section class="card glass side"'));
  assert.ok(card.includes("Recent searches") && card.includes('role="listbox"'));
  // Recent stays in the layout, hidden; the searches lie on top of it.
  assert.ok(card.includes("class:away={earlier !== null}"));
  assert.match(rule(".face.away"), /visibility: hidden/);
  assert.match(rule(".face.over"), /position: absolute/);
  assert.match(rule(".recent"), /overflow-y: auto/);
});

test("Down, Esc and typing return; Delete removes one entry", () => {
  const keys = app.slice(app.indexOf("function onKeydown"));
  assert.match(keys, /e\.key === "Delete"\) \{\n\s+e\.preventDefault\(\);\n\s+forgetSearch\(\);/);
  assert.match(keys, /e\.key === "Escape" \|\| \(plain && e\.key === "ArrowDown"\)\) \{\n\s+e\.preventDefault\(\);\n\s+earlier = null;/);
  assert.match(keys, /setInput\(earlier\[earlierAt\]\)/);
  assert.match(app, /oninput=\{\(\) => \{\n\s+skip = \[\];\n\s+earlier = null;/);
  assert.ok(app.includes('invoke("remove_search", { text })'));
});

test("turning the history off asks once about the saved searches", () => {
  const settings = text("../src/Privacy.svelte");
  for (const s of [
    '"Press the Up arrow in an empty box to see your last searches."',
    "Clear history now",
    "askClear = !s.remember_searches",
    "Also clear the searches already saved?",
  ]) {
    assert.ok(settings.includes(s), s);
  }
});

test("the quick actions are labelled buttons inside the Tips card, above the index", () => {
  assert.ok(!markup.includes('class="quick"'), "no separate row");
  const side = markup.slice(markup.indexOf('<section class="card glass side"'), markup.indexOf("{:else if isDiskCommand(query)}"));
  const [tips, grid, index] = ['class="tips"', 'class="qgrid"', 'class="idx"'].map((s) => side.indexOf(s));
  assert.ok(tips >= 0 && tips < grid && grid < index);
  assert.ok(side.includes("<span>{a.label ?? a.name}</span>"));
});

const counts = {
  files: 12,
  roots: 1,
  indexing: false,
  paused: false,
  read_done: 0,
  read_total: 0,
  ocr_done: 0,
  ocr_total: 0,
  waiting_for_idle: false,
  pauses_on_activity: false,
};

test("the footer speaks of the index only while something happens", () => {
  assert.equal(footerStatus(null), "");
  assert.equal(footerStatus(counts), "");
  assert.equal(footerStatus({ ...counts, roots: 0, files: 0 }), "");
  assert.equal(footerStatus({ ...counts, indexing: true }), "Preparing 12 files");
  assert.equal(footerStatus({ ...counts, read_done: 2, read_total: 9 }), "Preparing 2 of 9 files");
  assert.equal(footerStatus({ ...counts, paused: true }), "Background work paused");
  assert.equal(footerStatus({ ...counts, waiting_for_idle: true }), "Paused while you use the PC");
  // An error takes the line instead.
  assert.match(markup, /\{#if notice\}<span class="warn" role="alert">\{notice\}<\/span>\{:else\}\{footText\}\{\/if\}/);
});

test("the footer is one unboxed line of at most three hints", () => {
  assert.equal(MAX_HINTS, 3);
  const o = { mode: "full", hotkey: "Alt+Space", enter: "Open" };
  for (const view of ["home", "history", "results", "help", "setup", "settings", "disk", "confirm", "done"]) {
    assert.ok(footerHints(view, o).length <= 3, view);
  }
  assert.equal(footerHints("home", o)[0], "Type ? for help");
  const css = rule("footer");
  assert.ok(!/^\s*(border|background)[-a-z]*:/m.test(css));
});

test("nothing in the footer or the grid can overlap when narrow", () => {
  for (const s of ["max-width: 100%", "overflow: hidden", "white-space: nowrap", "text-overflow: ellipsis"]) {
    assert.ok(rule("footer").includes(s), s);
  }
  assert.match(rule(".qgrid"), /repeat\(2, minmax\(0, 1fr\)\)/);
  assert.match(rule(".qa"), /min-width: 0/);
  assert.match(rule(".qa span"), /text-overflow: ellipsis/);
});
