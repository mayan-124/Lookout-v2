// Modes: the dock, help, tips and footer hints.
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  MAX_HINTS,
  dockItems,
  footerHints,
  helpEntries,
  isDiskCommand,
  modeLabel,
  tipsFor,
  typedSection,
} from "../src/modes.ts";

// The switches each mode sets (core/src/uimode.rs).
const SHOW = {
  simple: { disk: false, clipboard: false, capture: false, timers: true, quick_keys: false, filters: true, notes: false },
  standard: { disk: true, clipboard: true, capture: true, timers: true, quick_keys: true, filters: true, notes: true },
  full: { disk: true, clipboard: true, capture: true, timers: true, quick_keys: true, filters: true, notes: true },
};

const row = (category, feature = null, advanced = false, mode = "simple") => ({
  id: category,
  category,
  example: category,
  feature,
  advanced,
  mode,
});
const ROWS = [
  row("Search"),
  row("Timers", "timers"),
  row("Quick keys", "quick_keys", true, "standard"),
  row("System", null, true),
  row("Disk", "disk", true, "standard"),
  row("Clipboard", "clipboard", false, "standard"),
];
const names = (entries) => entries.map((e) => ("row" in e ? e.row.category : "advanced"));

test("the dock lists what each mode shows and keeps section numbers", () => {
  const dock = (mode) => dockItems(SHOW[mode]).map((d) => d.name);
  assert.deepEqual(dock("simple"), ["Search", "Settings"]);
  assert.deepEqual(dock("standard"), ["Search", "Disk", "Clipboard", "Capture", "Settings"]);
  assert.deepEqual(dock("full"), ["Search", "Disk", "Clipboard", "Capture", "Settings"]);
  assert.deepEqual(dockItems({ ...SHOW.standard, disk: false }).map((d) => d.n), [0, 2, 3, 4]);
});

test("changed switches read as Custom", () => {
  assert.equal(modeLabel({ mode: "simple", custom: false }), "Simple");
  assert.equal(modeLabel({ mode: "standard", custom: true }), "Custom");
});

test("help folds advanced groups outside Full", () => {
  assert.deepEqual(names(helpEntries(ROWS, "standard", SHOW.standard, false)), ["Search", "Timers", "Clipboard", "advanced"]);
  assert.deepEqual(names(helpEntries(ROWS, "standard", SHOW.standard, true)), [
    "Search",
    "Timers",
    "Clipboard",
    "advanced",
    "Quick keys",
    "System",
    "Disk",
  ]);
  assert.deepEqual(names(helpEntries(ROWS, "simple", SHOW.simple, true)), ["Search", "Timers", "advanced", "System"]);
  assert.deepEqual(names(helpEntries(ROWS, "full", SHOW.full, false)), ROWS.map((r) => r.category));
});

test("tips follow the mode and the switches", () => {
  const tips = (mode, show = SHOW[mode]) => tipsFor(ROWS, mode, show).map((r) => r.category);
  assert.deepEqual(tips("simple"), ["Search", "Timers"]);
  assert.deepEqual(tips("standard"), ["Search", "Timers", "Clipboard"]);
  assert.deepEqual(tips("full"), ROWS.map((r) => r.category));
  // A switch turned on shows its basic tips; advanced ones still wait for Full.
  assert.deepEqual(tips("standard", { ...SHOW.standard, disk: true, timers: false }), ["Search", "Clipboard"]);
});

test("hidden features still answer to typed commands", () => {
  // Nothing here takes the mode: typing works the same in all of them.
  for (const q of ["disk", "Disk usage", " disk analyzer "]) assert.ok(isDiskCommand(q), q);
  assert.ok(!isDiskCommand("disk image"));
  for (const q of ["clipboard", "clip ", "clips x"]) assert.equal(typedSection(q), 2, q);
  assert.equal(typedSection("shot"), 3);
  assert.equal(typedSection("timer 5 min"), 0);
});

test("the footer shows at most three hints for the view", () => {
  const o = { mode: "full", hotkey: "Alt+Space", enter: "Stop…" };
  for (const view of ["home", "results", "help", "setup", "settings", "disk", "confirm", "done"]) {
    const hints = footerHints(view, o);
    assert.ok(hints.length > 0 && hints.length <= MAX_HINTS, view);
  }
  assert.deepEqual(footerHints("results", o), ["Enter stop", "→ actions", "Esc hide"]);
  assert.deepEqual(footerHints("results", { mode: "simple", hotkey: "", path: true, clears: true }), [
    "↑↓ pick",
    "Tab complete",
    "Esc clear",
  ]);
  assert.ok(!footerHints("home", { ...o, mode: "standard" }).join().includes("Ctrl+1"));
});
