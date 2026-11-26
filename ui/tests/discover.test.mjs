// Plain words to features: the registry, suggestions, ghost text and history keys.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { ghostFor, opensHistory, placeSuggestions, readLine, suggest, within1, worthRemembering } from "../src/discover.ts";
import { helpEntries, tipsFor } from "../src/modes.ts";
import { ACTIONS, ICONS, actionById, homeActions } from "../src/registry.ts";

// The switches each mode sets (core/src/uimode.rs).
const SHOW = {
  simple: { disk: false, clipboard: false, capture: false, timers: true, quick_keys: false, filters: true },
  standard: { disk: false, clipboard: true, capture: true, timers: true, quick_keys: true, filters: true },
  full: { disk: true, clipboard: true, capture: true, timers: true, quick_keys: true, filters: true },
};
const ids = (q, mode = "full") => suggest(q, ACTIONS, mode, SHOW[mode]).map((s) => s.action.id);
const ghost = (q, mode = "full") => ghostFor(q, ACTIONS, mode, SHOW[mode]);
const file = (title, source = "files") => ({ id: `${source}:${title}`, kind: "file", title, source });
const place = (q, rows) =>
  placeSuggestions(rows, suggest(q, ACTIONS, "full", SHOW.full), q, (a) => ({
    id: `action:${a.id}`,
    kind: "action",
    title: a.name,
    source: "action",
  })).map((r) => r.id);

test("every action is complete and has an icon", () => {
  for (const a of ACTIONS) {
    assert.ok(a.id && a.name && a.example && a.description && a.synonyms.length >= 5, a.id);
    assert.ok(ICONS[a.category], a.category);
    // Those that need more typing say what to put in the box.
    if (a.run === "fill") assert.equal(typeof a.stem, "string", a.id);
    if (a.run === "capture") assert.ok(a.target, a.id);
  }
  assert.equal(new Set(ACTIONS.map((a) => a.id)).size, ACTIONS.length);
});

test("synonyms, beginnings and typos find the action, whatever the case or accents", () => {
  for (const [q, id] of [
    ["countdown", "timer"],
    ["  Count  Down ", "timer"],
    ["tiemr", "timer"],
    ["timre", "timer"],
    ["remind", "timer"],
    ["screnshot", "screenshot"],
    ["snipping tool", "screenshot"],
    ["clipbrd", "clipboard"],
    ["DÁRK MÓDE", "darkmode"],
    ["preferences", "settings"],
    ["prefrences", "settings"],
    ["apearance", "appearance"],
    ["disk analyzer", "disk"],
    ["command line help", "commandline"],
    ["run setup again", "setup"],
    ["quick keys", "quickkey"],
    ["last week", "bydate"],
    ["network drive", "netpath"],
    ["website", "url"],
    ["convert", "units"],
    ["calculator", "calculator"],
  ]) {
    assert.ok(ids(q).includes(id), `${q} -> ${ids(q)}`);
  }
});

test("words under five letters are not matched by typo", () => {
  assert.ok(within1("hlep", "help") && within1("snp", "snip"));
  for (const q of ["snop", "clpi", "wbe", "mtah"]) assert.deepEqual(ids(q), [], q);
  assert.ok(!within1("timer", "tamar"));
});

test("short text and ordinary searches suggest nothing, and never more than two", () => {
  for (const q of ["", "t", "ti", "se", "?", "invoice", "report final", "timer notes", "timer 10 min", "budget 2025"]) {
    assert.deepEqual(ids(q), [], q);
  }
  for (const q of ["scr", "set", "tim", "screen", "dar"]) assert.ok(ids(q).length <= 2, q);
  assert.equal(ids("scr").length, 2);
});

test("an exact command word shows its action first, above a file of that name", () => {
  // "Running timers" only begins that way, so it waits below the file.
  assert.deepEqual(place("timer", [file("timer notes.txt")]), ["action:timer", "files:timer notes.txt", "action:timers"]);
  assert.equal(place("settings", [file("settings.json")])[0], "action:settings");
});

test("anything else stays below strong file, app and System rows", () => {
  const strong = place("tim", [file("timer notes.txt"), file("Time Sheet.lnk", "apps")]);
  assert.deepEqual(strong.slice(0, 2), ["files:timer notes.txt", "apps:Time Sheet.lnk"]);
  assert.ok(strong[2].startsWith("action:"));
  // A synonym is not a command word.
  assert.deepEqual(place("countdown", [file("countdown.mp4")]), ["files:countdown.mp4", "action:timer"]);
  // Weak matches (inside a word) come after the suggestion, in their own order.
  assert.deepEqual(place("countdown", [file("mycountdowns"), file("xcountdown")]), [
    "action:timer",
    "files:mycountdowns",
    "files:xcountdown",
  ]);
  const system = { id: "system:sound", kind: { custom: "system" }, title: "Open Sound settings", source: "system" };
  assert.equal(place("sound settings", [system])[0], "system:sound");
});

test("a result that already does it needs no suggestion", () => {
  const dark = { id: "system:dark_mode", kind: { custom: "system" }, title: "Switch to dark mode", source: "system" };
  assert.deepEqual(place("dark mode", [dark]), ["system:dark_mode"]);
  const running = { id: "timer:1", kind: { custom: "timer" }, title: "tea", source: "timer" };
  assert.deepEqual(place("timers", [running]), ["timer:1"]);
  const clip = { id: "c1", kind: "clipboard", title: "x", source: "plugin:com.lookout.clipboard", metadata: { plugin: "com.lookout.clipboard" } };
  assert.deepEqual(place("clipboard", [clip]), ["c1"]);
});

test("an action that needs more typing fills its stem instead of running", () => {
  for (const [id, stem] of [["timer", "timer "], ["quickkey", "notepad"], ["bydate", "pdf "], ["photos", "photos "], ["killport", "kill port "]]) {
    const a = actionById(id);
    assert.equal(a.run, "fill", id);
    assert.equal(a.stem, stem, id);
  }
  // With the stem in the box the same suggestion is not offered again.
  assert.ok(!ids("timer ").includes("timer"));
  for (const [id, run] of [["disk", "disk"], ["appearance", "appearance"], ["darkmode", "theme"], ["screenshot", "capture"]]) {
    assert.equal(actionById(id).run, run);
  }
});

test("suggestions follow the mode, but a whole word always finds its action", () => {
  assert.deepEqual(ids("clipb", "simple"), []);
  assert.deepEqual(ids("clipboard", "simple"), ["clipboard"]);
  assert.deepEqual(ids("disk analy", "standard"), []);
  assert.ok(ids("disk analy", "full").includes("disk"));
  assert.ok(ids("screens", "simple").includes("screenshot"));
});

test("the quick actions follow the mode", () => {
  const row = (mode, show = SHOW[mode]) => homeActions(mode, show).map((a) => a.id);
  assert.deepEqual(row("simple"), ["timer", "screenshot", "settings", "help"]);
  assert.deepEqual(row("standard"), ["timer", "screenshot", "clipboard", "settings", "help"]);
  assert.deepEqual(row("full"), ["timer", "screenshot", "clipboard", "disk", "settings", "help"]);
  // The user's own switches count.
  assert.deepEqual(row("standard", { ...SHOW.standard, capture: false, disk: true }), ["timer", "clipboard", "disk", "settings", "help"]);
  assert.ok(!row("simple", { ...SHOW.simple, timers: false }).includes("timer"));
});

test("every registry action appears in help", () => {
  const listed = helpEntries(ACTIONS, "full", SHOW.full, false).map((e) => e.row.id);
  assert.deepEqual(listed, ACTIONS.map((a) => a.id));
  // Outside Full the advanced ones are one click away; Disk waits for its switch.
  const open = helpEntries(ACTIONS, "standard", { ...SHOW.standard, disk: true }, true).flatMap((e) => ("row" in e ? [e.row.id] : []));
  assert.deepEqual([...open].sort(), ACTIONS.map((a) => a.id).sort());
  // Each category stays together, so it is headed once.
  const groups = ACTIONS.map((a) => a.category).filter((c, i, all) => c !== all[i - 1]);
  assert.equal(new Set(groups).size, groups.length);
  assert.ok(tipsFor(ACTIONS, "full", SHOW.full).length === ACTIONS.length);
});

test("help, tips and the quick actions read the one registry", () => {
  const app = readFileSync(new URL("../src/App.svelte", import.meta.url), "utf8");
  for (const use of ["helpEntries(ACTIONS", "tipsFor(ACTIONS", "homeActions(ui.mode", "suggest(query, ACTIONS", "ghostFor(query, ACTIONS"]) {
    assert.ok(app.includes(use), use);
  }
  assert.ok(!app.includes("help_entries") && !app.includes("TIP_ICONS"));
});

test("faint text completes commands only", () => {
  assert.equal(ghost("tim"), "er 5 min");
  assert.equal(ghost("Timer "), "5 min");
  assert.equal(ghost("dis"), "k usage");
  assert.equal(ghost("pdf "), "last week");
  assert.equal(ghost("kill p"), "ort 3000");
  for (const q of ["ti", " tim", "timer 5 min", "report", "rep", "invoice", "rece", "pdf", "notepad", "the", "10 km", "128 *", "git", "holiday", "timex"]) {
    assert.equal(ghost(q), "", q);
  }
  assert.equal(ghost("clipb", "simple"), "");
  assert.equal(ghost("dis", "standard"), "");
});

test("Tab takes the faint text; Esc dismisses it; neither acts during composition", () => {
  assert.equal("tim" + ghost("tim"), "timer 5 min");
  const app = readFileSync(new URL("../src/App.svelte", import.meta.url), "utf8").replaceAll("\r\n", "\n");
  const keys = app.slice(app.indexOf("function onKeydown"));
  assert.ok(keys.indexOf("if (e.isComposing) return;") < keys.indexOf("if (ghost && plain"));
  assert.match(keys, /if \(e\.key === "Tab"\) \{\n\s+e\.preventDefault\(\);\n\s+setInput\(query \+ ghost\);/);
  assert.match(keys, /ghostOff = query;/);
  assert.match(app, /<span class="ghost" aria-hidden="true">/);
  assert.match(app, /aria-live="polite">\{ghost \?/);
});

test("Up opens the history in an empty box or at its start", () => {
  assert.ok(opensHistory("", 0, 0, 0));
  assert.ok(opensHistory("report", 0, 0, 0));
  assert.ok(!opensHistory("report", 6, 6, 0));
  assert.ok(!opensHistory("report", 0, 6, 0));
  assert.ok(!opensHistory("report", 0, 0, 2));
  for (const q of ["report", "clip invoices"]) assert.ok(worthRemembering(q), q);
  for (const q of ["", "  ", "help", "?", "clip ", "shot "]) assert.ok(!worthRemembering(q), q);
});

test("the understood line says how a command was read, once", () => {
  const custom = (name, source, extra = {}) => ({ id: source, kind: { custom: name }, title: "", source, ...extra });
  assert.equal(readLine([custom("timer_new", "timer", { metadata: { secs: 600 } })], "timer 10 min"), "Timer · 10 minutes");
  assert.equal(readLine([custom("timer_new", "timer", { metadata: { secs: 5400 } })], "timer 1h30m"), "Timer · 1 hour 30 minutes");
  assert.equal(readLine([custom("url", "link", { uri: "https://a.com" })], "a.com"), "Web address · https://a.com");
  assert.equal(readLine([{ id: "c", kind: "calculator", title: "", source: "calculator" }], " 2+2 "), "Calculation · 2+2");
  assert.equal(readLine([custom("path", "path", { uri: "\\\\srv\\share" })], "\\\\srv\\share"), "Network path · \\\\srv\\share");
  assert.equal(readLine([{ ...file("np"), source: "quickkey", metadata: { quick_key: "np" } }], "np "), "Quick key · np");
  // Date and kind words have their own labels.
  assert.equal(readLine([file("invoice.pdf")], "pdf last week"), "");
});
