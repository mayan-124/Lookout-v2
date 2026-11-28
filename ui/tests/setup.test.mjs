// First-run setup, help and the indexing status line.
import assert from "node:assert/strict";
import { test } from "node:test";
import { CLIPBOARD_ID, DAILY, SCREENSHOT_ID, sectionOffer, startTicks } from "../src/addons.ts";
import { indexLine } from "../src/indexstatus.ts";
import { DAILY_STEP, FOLDER_STEP, SHORTCUT_STEP, STEPS, THEME_STEP, TRY_EXAMPLES, chooseFoldersTarget, isHelpQuery, stepOnShow } from "../src/setupflow.ts";

test("setup opens only while it is pending", () => {
  // Pending: the backend says so only when it is not done and no folder is chosen.
  assert.equal(stepOnShow(null, true, false), 1);
  // Done, skipped or an install with folders: never.
  assert.equal(stepOnShow(null, false, false), null);
  assert.equal(stepOnShow(null, false, true), null);
});

test("a pending setup keeps its step across hide and show", () => {
  assert.equal(stepOnShow(3, true, false), 3);
  assert.equal(stepOnShow(3, false, true), 3);
});

test("run setup again stays open when the bar comes straight back, and ends otherwise", () => {
  assert.equal(stepOnShow(1, false, true), 1);
  assert.equal(stepOnShow(4, false, false), null);
});

test("choosing folders opens the setup while pending, else Settings", () => {
  assert.equal(chooseFoldersTarget(true), "setup");
  assert.equal(chooseFoldersTarget(false), "settings");
  assert.equal(STEPS[FOLDER_STEP - 1], "Folders");
  assert.equal(STEPS.length, 6);
  assert.equal(STEPS[DAILY_STEP - 1], "Daily features");
  assert.equal(DAILY_STEP, FOLDER_STEP + 1);
  assert.equal(STEPS[SHORTCUT_STEP - 1], "Shortcuts");
  assert.equal(STEPS[THEME_STEP - 1], "Theme");
  assert.deepEqual(TRY_EXAMPLES.map(([e]) => e), ["report", "timer 5 min", "pdf last week"]);
});

test("daily features are all ticked on a first run, and only the ones on when run again", () => {
  const on = (id) => id === CLIPBOARD_ID;
  assert.deepEqual(
    DAILY.map((o) => o.name),
    ["Screenshots", "Rename and shrink files", "Search Office files and PDFs", "Clipboard"],
  );
  assert.ok(Object.values(startTicks(false, () => false)).every(Boolean));
  assert.deepEqual(
    DAILY.filter((o) => startTicks(true, on)[o.id]).map((o) => o.id),
    [CLIPBOARD_ID],
  );
});

test("clipboard and capture searches know which add-on to offer", () => {
  assert.equal(sectionOffer(2).id, CLIPBOARD_ID);
  assert.equal(sectionOffer(3).id, SCREENSHOT_ID);
  for (const n of [0, 1, 4]) assert.equal(sectionOffer(n), undefined);
});

test("help opens for help or a question mark only", () => {
  for (const q of ["help", " Help ", "?", "? "]) assert.ok(isHelpQuery(q), q);
  for (const q of ["", "helpful", "help me", "??", "report?"]) assert.ok(!isHelpQuery(q), q);
});

const counts = (over = {}) => ({
  files: 1200,
  roots: 2,
  indexing: false,
  paused: false,
  read_done: 0,
  read_total: 0,
  ocr_done: 0,
  ocr_total: 0,
  waiting_for_idle: false,
  pauses_on_activity: true,
  ...over,
});

test("status while running shows only real counters", () => {
  // Listing names: the total is unknown, so only the running count.
  assert.deepEqual(indexLine(counts({ indexing: true })), { state: "running", text: "Preparing 1,200 files" });
  const reading = counts({ read_done: 100, read_total: 250, ocr_done: 20, ocr_total: 50 });
  assert.equal(indexLine(reading).text, "Preparing 120 of 300 files · pauses when you use the PC");
  // "Index now" or no idle wait: it does not claim to pause.
  assert.equal(indexLine({ ...reading, pauses_on_activity: false }).text, "Preparing 120 of 300 files");
  assert.doesNotMatch(indexLine(reading).text, /%/);
});

test("status says when indexing is paused", () => {
  assert.deepEqual(indexLine(counts({ paused: true, indexing: true })), {
    state: "paused",
    text: "Paused · 1,200 files ready",
  });
  const waiting = counts({ waiting_for_idle: true, read_done: 3, read_total: 10 });
  assert.deepEqual(indexLine(waiting), { state: "waiting", text: "Paused while you use the PC · 3 of 10 files read" });
  assert.equal(indexLine(counts({ waiting_for_idle: true })).text, "Paused while you use the PC · 1,200 files ready");
});

test("status when finished, empty or not known yet", () => {
  assert.deepEqual(indexLine(counts()), { state: "done", text: "1,200 files ready" });
  assert.equal(indexLine(counts({ files: 1, read_done: 5, read_total: 5 })).text, "1 file ready");
  assert.equal(indexLine(counts({ files: 0, roots: 0 })).state, "nofolders");
  assert.equal(indexLine(null).state, "loading");
});
