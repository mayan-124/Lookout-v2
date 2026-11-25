// Run with `npm test`: node strips the types from theme.ts itself.
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  ACCENT_PRESETS,
  DEFAULT_THEME,
  THEMES,
  applyTheme,
  contrast,
  initialAppearance,
  normalizeHex,
  onAccent,
  themeById,
  themeVars,
  usableAccent,
} from "../src/theme.ts";
import { readFileSync, readdirSync } from "node:fs";

const HEX = /^#[0-9A-F]{6}$/;
const src = new URL("../src/", import.meta.url);
const files = readdirSync(src).filter((f) => /\.(css|svelte)$/.test(f));
const source = files.map((f) => readFileSync(new URL(f, src), "utf8")).join("\n");

test("every theme has a unique id and valid hex colors; the default exists", () => {
  assert.equal(THEMES.length, 8);
  assert.equal(new Set(THEMES.map((t) => t.id)).size, THEMES.length);
  for (const t of THEMES) {
    assert.ok(t.name, t.id);
    for (const key of ["base", "tint1", "tint2", "accent"]) assert.match(t[key], HEX, `${t.id} ${key}`);
    // A theme's own accent is never altered.
    assert.deepEqual(usableAccent(t.accent, t.base), { accent: t.accent, lightened: false }, t.id);
  }
  assert.equal(themeById(DEFAULT_THEME).id, "neon-grid");
  assert.equal(themeById("no-such-theme").id, DEFAULT_THEME);
});

test("applying a theme sets every variable the styles use", () => {
  for (const t of THEMES) {
    const set = new Map();
    applyTheme({ style: { setProperty: (k, v) => set.set(k, v) } }, t.id, null);
    assert.deepEqual([...set.keys()].sort(), Object.keys(themeVars(t, null).vars).sort());
    assert.equal(set.get("--base"), t.base);
    assert.equal(set.get("--tint1"), t.tint1);
    assert.equal(set.get("--tint2"), t.tint2);
    assert.equal(set.get("--accent"), t.accent);
    for (const value of set.values()) assert.ok(value);
    // Every var() without a fallback is defined by the theme, a stylesheet or a component.
    for (const [, name] of source.matchAll(/var\((--[a-z0-9-]+)\)/g)) {
      assert.ok(set.has(name) || new RegExp(`${name}\\s*:`).test(source), `${name} is never set`);
    }
  }
});

test("no color is hard-coded in a stylesheet or component", () => {
  for (const f of files) {
    const text = readFileSync(new URL(f, src), "utf8");
    assert.doesNotMatch(text, /#[0-9a-fA-F]{6}\b|\b(rgba?|hsla?)\(|backdrop-filter/, f);
  }
});

test("text on the accent reaches 4.5:1", () => {
  assert.equal(onAccent("#FFFFFF"), "#111214");
  assert.equal(onAccent("#000000"), "#FFFFFF");
  const accents = [...THEMES.map((t) => t.accent), ...ACCENT_PRESETS.map((a) => a.value), "#FFFFFF", "#000000"];
  for (const a of accents) assert.ok(contrast(a, onAccent(a)) >= 4.5, a);
});

test("a custom accent is lightened until it is readable on the base", () => {
  // Includes a mid grey that neither white nor dark text reads on.
  const hard = ["#000000", "#0000FF", "#1A1A1A", "#7A7A7A", "#FF0000", "#FFFFFF"];
  for (const t of THEMES) {
    for (const c of hard) {
      const { accent } = usableAccent(c, t.base);
      assert.ok(contrast(accent, t.base) >= 3, `${t.id} ${c}`);
      assert.ok(contrast(accent, onAccent(accent)) >= 4.5, `${t.id} ${c}`);
    }
  }
  const base = themeById("neon-grid").base;
  assert.equal(usableAccent("#000000", base).lightened, true);
  assert.deepEqual(usableAccent("#FFFFFF", base), { accent: "#FFFFFF", lightened: false });
  assert.equal(themeVars(themeById("matrix"), "#000000").lightened, true);
  assert.equal(themeVars(themeById("matrix"), "not a color").vars["--accent"], "#3DFF8A");
});

test("hex input is validated", () => {
  assert.equal(normalizeHex(" 7fe9d0 "), "#7FE9D0");
  assert.equal(normalizeHex("#abc"), "#AABBCC");
  for (const bad of ["", "#12", "#12345", "#1234567", "red", "#GGGGGG", null, 5]) {
    assert.equal(normalizeHex(bad), null, String(bad));
  }
});

test("disk chart hues stay distinct and visible on every theme", () => {
  for (const t of THEMES) {
    const { vars } = themeVars(t, null);
    const hues = [0, 1, 2, 3, 4, 5, 6, 7].map((i) => vars[`--c${i}`]);
    assert.equal(new Set(hues).size, 8, t.id);
    for (const h of hues) assert.ok(contrast(h, t.base) >= 3, `${t.id} ${h}`);
  }
});

test("the first appearance keeps a chosen accent and drops old defaults", () => {
  assert.deepEqual(initialAppearance(null, false), { theme: "neon-grid", accent: null });
  assert.deepEqual(initialAppearance(null, true), { theme: "graphite", accent: null });
  const from = (accent, accentChosen) => initialAppearance({ accent, accentChosen }, true).accent;
  assert.equal(from("#7FE9D0", false), null);
  assert.equal(from("#7fe9d0", true), null);
  assert.equal(from("#9DB8FF", false), "#9DB8FF");
  assert.equal(from("#D2A8FF", true), "#D2A8FF");
  assert.equal(from("#FFD43B", true), "#FFD43B");
  assert.equal(from("#FFD43B", false), null);
  assert.equal(from("junk", true), null);
});
