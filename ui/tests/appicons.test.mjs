// App icons: only rows on screen are asked for, and a failure keeps the generic one.
import assert from "node:assert/strict";
import { test } from "node:test";
import { KEPT, iconLoader, iconTarget, rowWatcher } from "../src/appicons.ts";

const settle = () => new Promise((done) => setTimeout(done, 0));
function setup(fetch = async (p) => `icon:${p}`) {
  const asked = [];
  const kept = new Map();
  const loader = iconLoader(
    (p) => (asked.push(p), fetch(p)),
    (p, src) => (src ? kept.set(p, src) : kept.delete(p)),
  );
  return { asked, kept, loader };
}

test("apps, Store apps and programs have icons; other rows do not", () => {
  assert.ok(iconTarget("app", "C:\\Start\\Editor.lnk"));
  assert.ok(iconTarget("app", "shell:AppsFolder\\Vendor.Calc_abc!App"));
  assert.ok(iconTarget("file", "D:\\tools\\tool.EXE"));
  assert.equal(iconTarget("file", "\\\\server\\share\\tool.exe"), null);
  assert.equal(iconTarget("file", "C:\\a.pdf"), null);
  assert.equal(iconTarget("folder", "C:\\x.exe"), null);
  assert.equal(iconTarget("app", null), null);
});

test("a row off screen causes no icon work", async () => {
  const { asked, loader } = setup();
  let changed;
  const watch = rowWatcher(loader, (c) => ((changed = c), { observe() {}, unobserve() {} }));
  const [a, b, c] = [{}, {}, {}];
  watch(a, "a");
  watch(b, "b");
  const third = watch(c, "c");
  await settle();
  assert.deepEqual(asked, []);
  // "c" leaves the screen while "a" is still being made: it is never asked for.
  changed([{ target: a, isIntersecting: true }, { target: c, isIntersecting: true }]);
  changed([{ target: c, isIntersecting: false }]);
  third.destroy();
  await settle();
  assert.deepEqual(asked, ["a"]);
});

test("a hidden bar asks for nothing and keeps nothing", async () => {
  const { asked, kept, loader } = setup();
  loader.show("a");
  await settle();
  loader.setActive(false);
  loader.show("b");
  await settle();
  assert.deepEqual(asked, ["a"]);
  assert.equal(kept.size, 0);
  loader.setActive(true);
  await settle();
  assert.deepEqual([...kept.keys()], ["a", "b"]);
});

test("a failed icon leaves the generic one and the rest still arrive", async () => {
  const { asked, kept, loader } = setup(async (p) => {
    if (p === "bad") throw new Error("no");
    return p === "none" ? null : "x";
  });
  for (const p of ["bad", "none", "good"]) loader.show(p);
  await settle();
  loader.show("bad");
  await settle();
  assert.deepEqual(asked, ["bad", "none", "good"]);
  assert.deepEqual([...kept.keys()], ["good"]);
});

test("the page keeps a limited number of icons", async () => {
  const { kept, loader } = setup();
  for (let n = 0; n <= KEPT; n++) {
    loader.show(`${n}`);
    await settle();
    loader.hide(`${n}`);
  }
  assert.equal(kept.size, KEPT);
  assert.ok(!kept.has("0"));
});
