// Comfort: the last search for 30 seconds, and when a press becomes a drag.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { DRAG_THRESHOLD, isClipRow, isFileRow, movedEnough } from "../src/dragout.ts";
import { PAGE_KEEP_SECS, SEARCH_KEEP_SECS, keepsPage, keepsSearch } from "../src/lastsearch.ts";

const text = (path) => readFileSync(new URL(path, import.meta.url), "utf8").replaceAll("\r\n", "\n");
const app = text("../src/App.svelte");

/** A fake clock that reports whole seconds hidden, as the shell does. */
function clock() {
  let now = 0;
  let hidden = null;
  return {
    hide: () => (hidden = now),
    wait: (ms) => (now += ms),
    hiddenSecs: () => (hidden === null ? null : Math.floor((now - hidden) / 1000)),
  };
}

test("the search comes back within 30 seconds and not after", () => {
  assert.equal(SEARCH_KEEP_SECS, 30);
  const c = clock();
  assert.equal(keepsSearch(c.hiddenSecs(), true, "report"), false, "never hidden");
  c.hide();
  for (const [ms, kept] of [[0, true], [29_999, true], [1, false], [600_000, false]]) {
    c.wait(ms);
    assert.equal(keepsSearch(c.hiddenSecs(), true, "report"), kept, `after ${c.hiddenSecs()} s`);
  }
  c.hide();
  assert.equal(keepsSearch(c.hiddenSecs(), true, "report"), true, "each hide starts the 30 seconds again");
  assert.equal(keepsSearch(c.hiddenSecs(), false, "report"), false, "switched off");
  assert.equal(keepsSearch(c.hiddenSecs(), true, "  "), false, "nothing was typed");
  // The time is a setting.
  assert.equal(keepsSearch(45, true, "report", 60), true);
  assert.equal(keepsSearch(11, true, "report", 10), false);
});

test("open pages keep their five minutes unless the user closed the bar", () => {
  assert.equal(keepsPage(PAGE_KEEP_SECS - 1, true, false), true);
  assert.equal(keepsPage(PAGE_KEEP_SECS, true, false), false);
  assert.equal(keepsPage(1, true, true), false);
  assert.equal(keepsPage(1, false, false), false);
  assert.equal(keepsPage(null, true, false), false);
});

test("the last search is never persisted", () => {
  for (const source of [text("../src/lastsearch.ts"), app]) {
    assert.ok(!/localStorage|sessionStorage|indexedDB|document\.cookie/.test(source));
  }
  assert.ok(!/\bimport\b|invoke/.test(text("../src/lastsearch.ts")));
  // The only thing that stores typed text is the search history.
  assert.equal(app.split('invoke("record_search"').length, 2);
});

test("a restored search is selected, quiet and not saved twice", () => {
  assert.match(app, /if \(search\) input\?\.select\(\);/);
  assert.match(app, /caretAtEnd && !quiet && !composing/);
  assert.match(app, /quiet \? \[\] : suggest\(/);
  assert.match(app, /const meant = \$derived\(quiet \?/);
  assert.match(app, /if \(text === savedSearch \|\| !worthRemembering\(text\)\) return;/);
  // Typing (every run) ends the quiet and lets the new text be saved.
  assert.match(app, /function run\(text: string\) \{\n\s+cleared = false;\n\s+quiet = false;\n\s+savedSearch = null;/);
  // Bringing it back asks for nothing: the results are the ones on screen.
  const back = app.slice(app.indexOf("function bringBackSearch"), app.indexOf("const nextFrame"));
  assert.ok(back.length > 0 && !back.includes("invoke") && !/\brun\(/.test(back));
});

test("a drag needs movement, and only file and folder rows have one", () => {
  assert.equal(movedEnough(0, 0), false);
  assert.equal(movedEnough(DRAG_THRESHOLD - 1, 0), false);
  assert.equal(movedEnough(3, 3), false);
  assert.equal(movedEnough(0, -DRAG_THRESHOLD), true);
  assert.equal(movedEnough(5, 5), true);
  assert.equal(isFileRow("file", "files", "C:\\a.txt"), true);
  assert.equal(isFileRow("folder", "quickkey", "C:\\a"), true);
  for (const kind of ["app", "action", "clipboard", "screenshot", "snippet", "port", "url"]) {
    assert.equal(isFileRow(kind, "files", "C:\\a"), false, kind);
  }
  assert.equal(isFileRow("file", "plugin:files-tools", "C:\\a.txt"), false);
  assert.equal(isFileRow("file", "files", null), false);
});

test("stored clips can be dragged, clipboard commands cannot", () => {
  assert.ok(isClipRow("clipboard", "plugin:com.lookout.clipboard"));
  assert.ok(!isClipRow("clipboard_action", "plugin:com.lookout.clipboard"));
  assert.ok(!isClipRow("clipboard_clear", "plugin:com.lookout.clipboard"));
  assert.ok(!isClipRow("file", "files"));
});
