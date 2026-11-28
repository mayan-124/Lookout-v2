// Home and the footer: Recent, the searches in the same card, the button grid
// and the one quiet line under the dock. Layout is read from the markup.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { homeRows, recentRows } from "../src/home.ts";
import { placeWindows } from "../src/discover.ts";
import { changesList, changesPins, isQuiet, rowKeys } from "../src/rows.ts";
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

test("Recent is the newest six", () => {
  const rows = (n) => Array.from({ length: n }, (_, ts) => ({ ts }));
  assert.equal(recentRows(rows(2), 10_000).length, 2);
  assert.deepEqual(recentRows(rows(8), 10_000).map((r) => r.ts), [7, 6, 5, 4, 3, 2]);
  // The files come from the backend, which drops junk and skipped places.
  assert.ok(app.includes('invoke<HomeLists>("home_lists")') && !app.includes('"past week" })'));
});

test("Home runs from open windows through pinned and recent to apps, each item once", () => {
  const item = (id) => ({ id });
  const lists = {
    windows: [item("window:1")],
    pinned: [item("file:a"), item("app:p")],
    files: ["a", "b", "c", "d", "e", "f", "g"].map((n) => item(`file:${n}`)),
    apps: [item("app:p"), item("app:q")],
  };
  const clips = [1, 2, 3, 4].map((ts) => ({ item: item(`clip:${ts}`), ts }));
  const rows = homeRows(lists, clips, 10_000);
  assert.deepEqual([...new Set(rows.map((r) => r.section))], ["Open now", "Pinned", "Recent", "Apps"]);
  const ids = rows.map((r) => r.item.id);
  assert.equal(new Set(ids).size, ids.length);
  // Three clips and three files, whatever their ages.
  const recent = rows.filter((r) => r.section === "Recent");
  assert.deepEqual(recent.map((r) => r.item.id).sort(), ["clip:2", "clip:3", "clip:4", "file:b", "file:c", "file:d"]);
  assert.deepEqual(rows.filter((r) => r.section === "Apps").map((r) => r.item.id), ["app:q"]);
  assert.deepEqual(homeRows({ windows: [], pinned: [], files: [], apps: [] }, [], 0), []);
});

test("Down picks a Home row, Enter runs it; compact Home shows only the box", () => {
  const keys = app.slice(app.indexOf("function onKeydown"));
  assert.match(keys, /if \(home\) homeMove\(1\);/);
  assert.match(keys, /if \(!e\.repeat\) activate\(homeCur\);/);
  assert.match(app, /const compact = \$derived\(home && ui\.compact_home && earlier === null\);/);
  assert.match(markup, /\{#if !card && !compact\}/);
});

test("every file, folder, app, Settings page and web address can be pinned", () => {
  const row = (kind, uri, source = "files") => ({ kind, uri, source });
  const store = "shell:AppsFolder\\x!y";
  for (const r of [
    row("file", "C:\\a.txt"),
    row("folder", "C:\\a"),
    row("app", "C:\\a.lnk", "apps"),
    row("app", store, "apps"),
    row({ custom: "system" }, "wifi_page", "system"),
    row({ custom: "url" }, "https://a.com", "link"),
  ]) {
    assert.equal(rowKeys(r).pin.action, "saved_add", r.uri);
    assert.equal(rowKeys(r, true).pin.action, "saved_remove", r.uri);
    // Delete unpins only on the pinned list's own rows.
    assert.equal(rowKeys(r, true).remove, undefined);
    assert.equal(rowKeys({ ...r, source: "saved" }).remove.action, "saved_remove");
  }
  for (const r of [
    row({ custom: "window" }, "1:2:3", "windows"),
    row("clipboard", null, "plugin:com.lookout.clipboard"),
    row({ custom: "url" }, "https://a.com/?q=x", "websearch"),
  ]) {
    assert.equal(rowKeys(r).pin, undefined, r.source);
  }
  assert.equal(rowKeys(row("app", store, "apps")).reveal, undefined);
  assert.equal(rowKeys(row("app", "C:\\a.lnk", "apps")).openFolder, "open_folder");
  assert.deepEqual(rowKeys(row({ custom: "window" }, "1:2:3", "windows")).window.map((w) => w.label), ["Minimize", "Close"]);
  // Unpinning shows its card, with Undo; the rest just change the list.
  assert.ok(changesPins("saved_remove") && changesList("saved_remove") && !isQuiet("saved_remove"));
  for (const a of ["saved_add", "pin_up", "pin_down", "window_close"]) assert.ok(isQuiet(a), a);
});

test("Right arrow opens the actions list and Ctrl+D pins", () => {
  const keys = app.slice(app.indexOf("function rowKey("), app.indexOf("interface Entry"));
  assert.match(keys, /e\.key === "ArrowRight" && e\.target === input && caretIsAtEnd\(\)/);
  assert.match(keys, /letter === "d"/);
  const menu = app.slice(app.indexOf("function rowMenu"), app.indexOf("const searchMenu"));
  const order = ["keys.openFolder", "keys.copy?.action", "keys.copyFile", "keys.pin?.action", "keys.reveal"].map((s) => menu.indexOf(s));
  assert.ok(order.every((at) => at >= 0));
  assert.deepEqual([...order].sort((a, b) => a - b), order);
});

test("matched windows sit under the top result and never above an exact name", () => {
  const r = (id, source, kind, score) => ({ id, source, kind, score, title: id });
  const win = r("w", "windows", { custom: "window" }, 1.15);
  const ids = (rows, q = "notes") => placeWindows(rows, q).map((x) => x.id);
  assert.deepEqual(ids([win, r("a", "files", "file", 1)]), ["a", "w"]);
  assert.deepEqual(ids([r("a", "apps", "app", 1.1), r("b", "files", "file", 1), win]), ["a", "w", "b"]);
  assert.deepEqual(ids([r("a", "system", { custom: "system" }, 1.35), r("x", "apps", "app", 3.8), win]), ["a", "x", "w"]);
  assert.deepEqual(ids([win]), ["w"]);
  // The "win" list keeps its own order.
  assert.deepEqual(ids([win, r("a", "files", "file", 1)], "win notes"), ["w", "a"]);
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
    "Delete search history",
    "askClear = !s.remember_searches",
    "Also delete the searches already saved?",
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
