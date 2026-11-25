// First-run setup, help and the indexing status line.
import assert from "node:assert/strict";
import { test } from "node:test";
import { indexLine } from "../src/indexstatus.ts";
import { FOLDER_STEP, STEPS, TRY_EXAMPLES, chooseFoldersTarget, isHelpQuery, stepOnShow } from "../src/setupflow.ts";

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
  assert.equal(STEPS.length, 5);
  assert.deepEqual(TRY_EXAMPLES.map(([e]) => e), ["report", "timer 5 min", "pdf last week"]);
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
  assert.deepEqual(indexLine(counts({ indexing: true })), { state: "running", text: "Indexing 1,200 files" });
  const reading = counts({ read_done: 100, read_total: 250, ocr_done: 20, ocr_total: 50 });
  assert.equal(indexLine(reading).text, "Indexing 120 of 300 files · pauses when you use the PC");
  // "Index now" or no idle wait: it does not claim to pause.
  assert.equal(indexLine({ ...reading, pauses_on_activity: false }).text, "Indexing 120 of 300 files");
  assert.doesNotMatch(indexLine(reading).text, /%/);
});

test("status says when indexing is paused", () => {
  assert.deepEqual(indexLine(counts({ paused: true, indexing: true })), {
    state: "paused",
    text: "Indexing paused · 1,200 files ready",
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
