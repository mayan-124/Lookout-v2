// Settings: what the General page holds, read from the component's markup.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const src = readFileSync(new URL("../src/Privacy.svelte", import.meta.url), "utf8").replaceAll("\r\n", "\n");
const markup = src.slice(src.indexOf('<section class="settings"'), src.indexOf("<style>"));
const general = markup.slice(markup.indexOf('{#if section === "general"}'), markup.indexOf('{:else if section === "features"}'));
const rest = markup.replace(general, "");
const cards = Object.fromEntries(
  general
    .split('<div class="scard"')
    .slice(1)
    .map((c) => [c.match(/<h3 class="stitle">([^<]+)</)?.[1] ?? "", c]),
);

const WANTED = {
  Folders: ['add("roots")'],
  Shortcuts: ['"set_open_hotkey"', '"set_grab_hotkey"', "info.pause_hotkey}<"],
  Appearance: ['<ThemePicker part="theme"', '<ThemePicker part="accent"', "setGlass(", "setReduceMotion(", "setHideTips("],
  Mode: ["<ModePicker"],
  "Start with Windows": ["setStartWithWindows,"],
  Setup: ["onclick={onsetup}"],
  Updates: ["onclick={checkUpdates}", "onclick={installUpdate}", "{#if updMsg}"],
  Status: ["<Health"],
};

test("General shows the headed cards, in order, each with its controls", () => {
  assert.deepEqual(Object.keys(cards).filter(Boolean), Object.keys(WANTED));
  for (const [card, controls] of Object.entries(WANTED)) {
    for (const c of controls) assert.ok(cards[card].includes(c), `${card}: ${c}`);
  }
});

test("none of those controls is repeated or left under Advanced", () => {
  for (const c of Object.values(WANTED).flat()) {
    assert.ok(!rest.includes(c), c);
    assert.equal(general.split(c).length - 1, 1, c);
  }
});

test("Privacy has the history switch and Clear history", () => {
  const privacy = rest.slice(rest.indexOf('section === "privacy"}'), rest.indexOf('section === "speed"}'));
  for (const c of ['"Remember my searches"', "onclick={clearHistory}"]) assert.ok(privacy.includes(c), c);
});

test("the Advanced menu lists only what stayed there, each with a page", () => {
  const menu = src.slice(src.indexOf("const ADVANCED"), src.indexOf("// [switch, name"));
  const ids = [...menu.matchAll(/\["(\w+)", "/g)].map((m) => m[1]);
  assert.deepEqual(ids, ["features", "folders", "privacy", "speed", "addons", "quickkeys", "repair", "delete"]);
  // "delete" is the last, unnamed branch.
  for (const id of ids.slice(0, -1)) assert.ok(rest.includes(`section === "${id}"}`), id);
});
