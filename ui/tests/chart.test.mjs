// Disk charts: layout, hit-testing, colours and selection sync.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import {
  CENTRE,
  MAX_SHAPES,
  TAU,
  backing,
  dominant,
  filterShapes,
  hitBlocks,
  hitRing,
  mergeSmall,
  prune,
  ringArcs,
  ringGeom,
  squarify,
  treemapBlocks,
  zoomArc,
  zoomPlan,
} from "../src/chartlayout.ts";
import { AGE_DAYS, TYPES, ageBucket, fillFor, legend, palette, textOn } from "../src/chartcolors.ts";
import { listPath, selectionShape, shapePath, stepShape } from "../src/chartsync.ts";
import { drawArc, drawBlock, drawCentre, drawRingZoom, fitText, ringCache } from "../src/chartdraw.ts";
import { THEMES, contrast, panelColors } from "../src/theme.ts";

let ids = 0;
/** All of `size` in one type, unless the children say otherwise. */
const node = (name, size, children = [], more = {}) => {
  const cat = more.cat ?? 7;
  const split = TYPES.map((_, i) =>
    children.length ? children.reduce((s, c) => s + c.split[i], 0) : i === cat ? size : 0,
  );
  return {
    id: ++ids,
    name,
    kind: children.length ? "folder" : "file",
    size,
    items: children.length || 1,
    modified: 0,
    cat: dominant(split),
    split,
    children,
    ...more,
  };
};
const folder = (name, children, more = {}) =>
  node(name, children.reduce((s, c) => s + c.size, 0), children, { kind: "folder", ...more });

const sum = (list, f) => list.reduce((s, x) => s + f(x), 0);
const close = (a, b, eps = 1e-6) => assert.ok(Math.abs(a - b) <= eps * Math.max(1, Math.abs(b)), `${a} ≈ ${b}`);
const overlap = (a, b) =>
  Math.max(0, Math.min(a.x + a.w, b.x + b.w) - Math.max(a.x, b.x)) *
  Math.max(0, Math.min(a.y + a.h, b.y + b.h) - Math.max(a.y, b.y));

const sample = () => {
  const photos = folder("Photos", [node("a.jpg", 300), node("b.jpg", 200)]);
  const work = folder("Work", [folder("Old", [node("deep.zip", 150)]), node("plan.docx", 100)]);
  return folder("root", [photos, work, node("film.mkv", 250)]);
};

test("items under 0.5% of their parent are merged into one Other", () => {
  const kids = [node("big", 10_000), node("edge", 51), node("s1", 20), node("s2", 10), node("zero", 0)];
  const out = mergeSmall(kids, 10_081);
  assert.deepEqual(
    out.map((n) => n.name),
    ["big", "edge", "Other (2 items)"],
  );
  assert.equal(out[2].size, 30);
  assert.equal(out[2].id, null);
  assert.equal(sum(out, (n) => n.size), 10_081, "nothing is lost");
  // A single small item keeps its name; an "Other" from the scan joins new small ones.
  assert.equal(mergeSmall([node("big", 1000), node("one", 1)], 1001)[1].name, "one");
  const group = node("Other (7 items)", 40, [], { id: null, kind: "other", items: 7 });
  const joined = mergeSmall([node("big", 100_000), group, node("s", 5)], 100_045);
  assert.equal(joined[1].name, "Other (8 items)");
  assert.equal(joined.length, 2);
});

test("squarified areas are proportional, sum to the parent and do not overlap", () => {
  const sizes = [600, 600, 400, 300, 200, 200, 100, 0, 1];
  const rects = squarify(sizes, 10, 20, 600, 400);
  const total = sum(sizes, (s) => s);
  rects.forEach((r, i) => {
    close(r.w * r.h, (sizes[i] / total) * 600 * 400);
    assert.ok(r.x >= 10 - 1e-6 && r.y >= 20 - 1e-6 && r.x + r.w <= 610 + 1e-6 && r.y + r.h <= 420 + 1e-6);
  });
  close(sum(rects, (r) => r.w * r.h), 240_000);
  for (let i = 0; i < rects.length; i++)
    for (let j = i + 1; j < rects.length; j++) assert.ok(overlap(rects[i], rects[j]) < 0.01, `${i} ${j}`);
  // The classic example stays close to square.
  assert.ok(squarify([6, 6, 4, 3, 2, 2, 1], 0, 0, 6, 4).every((r) => Math.max(r.w / r.h, r.h / r.w) < 3));
});

test("treemap and ring handle nothing, zero sizes and a single item", () => {
  assert.deepEqual(squarify([], 0, 0, 10, 10), []);
  assert.deepEqual(squarify([0, 0], 0, 0, 10, 10).map((r) => r.w * r.h), [0, 0]);
  assert.deepEqual(squarify([5], 0, 0, 0, 10).map((r) => r.w), [0]);
  assert.deepEqual(squarify([5], 1, 2, 30, 40), [{ x: 1, y: 2, w: 30, h: 40 }]);
  const empty = folder("empty", []);
  assert.deepEqual(ringArcs(empty), []);
  assert.deepEqual(treemapBlocks(empty, 300, 300), []);
  const one = folder("one", [node("only", 5)]);
  const [arc] = ringArcs(one);
  assert.equal(arc.a0, 0);
  close(arc.a1, TAU);
  const [block] = treemapBlocks(one, 300, 200);
  assert.deepEqual([block.x, block.y, block.w, block.h], [0, 0, 300, 200]);
});

test("ring arcs fill their parent without overlap", () => {
  const root = sample();
  const arcs = ringArcs(root);
  const top = arcs.filter((a) => a.depth === 0);
  close(sum(top, (a) => a.a1 - a.a0), TAU);
  for (const parent of arcs.filter((a) => a.node.children.length)) {
    const kids = arcs.filter((a) => a.parent === parent.node);
    close(sum(kids, (a) => a.a1 - a.a0), parent.a1 - parent.a0);
    close(kids[0].a0, parent.a0);
    kids.forEach((k, i) => i && close(k.a0, kids[i - 1].a1));
    close((kids[0].a1 - kids[0].a0) / (parent.a1 - parent.a0), kids[0].node.size / parent.node.size);
    assert.ok(kids.every((k) => k.branch === parent.branch && k.depth === parent.depth + 1));
  }
});

test("treemap blocks nest one level, inside their folder and under its header", () => {
  const blocks = treemapBlocks(sample(), 600, 400, 20);
  assert.equal(Math.max(...blocks.map((b) => b.depth)), 1);
  close(sum(blocks.filter((b) => b.depth === 0), (b) => b.w * b.h), 240_000);
  for (const kid of blocks.filter((b) => b.depth === 1)) {
    const parent = blocks.find((b) => b.node === kid.parent);
    assert.equal(parent.head, 20);
    assert.ok(kid.y >= parent.y + 20 - 1e-6 && kid.x >= parent.x && kid.x + kid.w <= parent.x + parent.w);
    assert.ok(kid.y + kid.h <= parent.y + parent.h + 1e-6);
  }
});

test("very wide and very deep trees stay capped", () => {
  const wide = folder(
    "wide",
    Array.from({ length: 150 }, (_, i) =>
      folder(`d${i}`, Array.from({ length: 150 }, (_, j) => node(`f${i}-${j}`, 10))),
    ),
  );
  assert.ok(ringArcs(wide, 4, MAX_SHAPES, 0).length <= MAX_SHAPES);
  assert.ok(treemapBlocks(wide, 2000, 1500).length <= MAX_SHAPES);
  let deep = node("leaf", 1);
  for (let i = 0; i < 5000; i++) deep = folder(`n${i}`, [deep]);
  assert.equal(ringArcs(deep).length, 4);
  assert.equal(treemapBlocks(deep, 600, 600).length, 2);
});

test("ring hit-testing finds the arc under a point", () => {
  const arcs = ringArcs(sample());
  const g = ringGeom(400, 400);
  assert.equal(hitRing(arcs, g, 200, 200), CENTRE);
  assert.equal(hitRing(arcs, g, 0, 0), -1, "outside the ring");
  arcs.forEach((a, i) => {
    const mid = (a.a0 + a.a1) / 2;
    for (const at of [0.05, 0.5, 0.95]) {
      const r = g.hole + (a.depth + at) * g.ring;
      assert.equal(hitRing(arcs, g, g.cx + r * Math.sin(mid), g.cy - r * Math.cos(mid)), i, a.node.name);
    }
  });
  // Just either side of the top: the first and the last slice of the inner ring.
  const r = g.hole + g.ring / 2;
  assert.equal(arcs[hitRing(arcs, g, g.cx + 0.01, g.cy - r)].a0, 0);
  close(arcs[hitRing(arcs, g, g.cx - 0.01, g.cy - r)].a1, TAU);
});

test("treemap hit-testing finds the innermost block", () => {
  const blocks = treemapBlocks(sample(), 600, 400, 20);
  blocks.forEach((b, i) => {
    // A folder is hit on its header, anything else in its middle.
    const y = b.head ? b.y + 5 : b.y + b.h / 2;
    assert.equal(hitBlocks(blocks, b.x + b.w / 2, y), i, b.node.name);
  });
  assert.equal(hitBlocks(blocks, -1, 5), -1);
  assert.equal(hitBlocks(blocks, 600, 400), -1);
});

test("zooming turns the picked folder's slice into the whole ring", () => {
  const arcs = ringArcs(sample());
  const work = arcs.find((a) => a.node.name === "Work");
  const old = arcs.find((a) => a.node.name === "Old");
  assert.deepEqual(zoomArc(old, work, 0), { a0: old.a0, a1: old.a1, depth: 1 });
  const end = zoomArc(old, work, 1);
  const fresh = ringArcs(work.node).find((a) => a.node.name === "Old");
  close(end.a0, fresh.a0);
  close(end.a1, fresh.a1);
  assert.equal(end.depth, fresh.depth);
  assert.deepEqual(zoomArc(work, work, 1), { a0: 0, a1: TAU, depth: -1 });
});

test("deleted items leave the chart and their folders shrink", () => {
  const root = sample();
  const zip = root.children[1].children[0].children[0];
  const out = prune(root, new Set([zip.id]));
  assert.equal(out.size, root.size - 150);
  assert.equal(out.children[1].children[0].size, 0);
  assert.equal(out.children[0], root.children[0], "untouched branches are reused");
  assert.equal(prune(root, new Set()), root);
});

test("the name filter keeps matches and their contents bright", () => {
  const blocks = treemapBlocks(sample(), 600, 400, 20);
  const bright = (q) => {
    const f = filterShapes(blocks, q);
    return [f.count, blocks.filter((_, i) => f.bright[i]).map((b) => b.node.name).sort()];
  };
  assert.deepEqual(bright("JPG"), [2, ["a.jpg", "b.jpg"]]);
  assert.deepEqual(bright("photos"), [1, ["Photos", "a.jpg", "b.jpg"]]);
  assert.deepEqual(bright("nothing"), [0, []]);
  assert.equal(bright("  ")[1].length, blocks.length);
});

test("colour modes map folders, types and ages to fills", () => {
  const p = palette("neon-grid", null);
  const now = 1_800_000_000;
  const file = node("x", 1, [], { cat: 2, modified: now - 400 * 86_400 });
  assert.equal(fillFor(file, "folder", 1, 0, p, now), p.folder[1]);
  assert.equal(fillFor(file, "folder", 9, 0, p, now), p.folder[1], "hues repeat");
  assert.notEqual(fillFor(file, "folder", 1, 1, p, now), p.folder[1], "deeper is another shade");
  assert.equal(fillFor(file, "type", 1, 3, p, now), p.type[2]);
  assert.equal(fillFor(file, "age", 1, 0, p, now), p.age[3]);
  assert.equal(fillFor({ ...file, modified: 0 }, "age", 0, 0, p, now), p.grey);
  for (const kind of ["other", "unlisted"]) {
    for (const mode of ["folder", "age"]) assert.equal(fillFor({ ...file, kind }, mode, 0, 0, p, now), p.grey);
    assert.equal(fillFor({ ...file, kind }, "type", 0, 0, p, now), p.type[2], "a group shows its largest type");
  }
  assert.deepEqual(
    [1, 31, 183, 366, 731].map((d) => ageBucket(now - d * 86_400, now)),
    [0, 1, 2, 3, 4],
  );
  assert.equal(ageBucket(now + 500, now), 0);
  assert.equal(AGE_DAYS.length + 1, p.age.length);
  assert.equal(legend("type", p, now).length, TYPES.length);
  assert.equal(legend("age", p, now).length, p.age.length);
  assert.deepEqual(legend("folder", p, now), []);
});

test("every fill reaches 3:1 on the panel and its text 4.5:1, on every theme", () => {
  for (const t of THEMES) {
    for (const accent of [null, "#7A7A7A"]) {
      const p = palette(t.id, accent);
      assert.equal(new Set(p.type).size, TYPES.length, t.id);
      assert.equal(new Set(p.age).size, p.age.length, t.id);
      const fills = [...p.type, ...p.age, p.grey];
      for (let branch = 0; branch < 8; branch++)
        for (let depth = 0; depth < 4; depth++)
          fills.push(fillFor(node("x", 1), "folder", branch, depth, p, 0));
      for (const fill of fills) {
        for (const ground of [t.base, ...Object.values(panelColors(t))])
          assert.ok(contrast(fill, ground) >= 3, `${t.id} ${fill} on ${ground}`);
        assert.ok(contrast(fill, textOn(fill)) >= 4.5, `${t.id} text on ${fill}`);
      }
    }
  }
});

test("text on a fill is the better of black and white", () => {
  assert.equal(textOn("#FFFFFF"), "#000000");
  assert.equal(textOn("#000000"), "#FFFFFF");
  // The hardest greys still reach 4.5:1 with one of the two.
  for (let v = 0; v < 256; v++) {
    const grey = `#${v.toString(16).padStart(2, "0").repeat(3)}`;
    assert.ok(contrast(grey, textOn(grey)) >= 4.5, grey);
  }
});

test("labels are cut with an ellipsis or dropped", () => {
  const ctx = { measureText: (t) => ({ width: t.length * 10 }) };
  assert.equal(fitText(ctx, "short", 60), "short");
  assert.equal(fitText(ctx, "a long file name", 60), "a lon…");
  assert.equal(fitText(ctx, "a long file name", 20), "");
  assert.equal(fitText(ctx, "x".repeat(5000), 50), "xxxx…");
});

test("a list row and a chart shape name the same item", () => {
  const rows = [
    { depth: 0, row: { id: 1 } },
    { depth: 1, row: { id: 2 } },
    { depth: 2, row: { id: 3 } },
    { depth: 1, row: { id: 4 } },
    { depth: 0, row: { id: 5 } },
  ];
  assert.deepEqual(listPath(rows, 2), [1, 2, 3]);
  assert.deepEqual(listPath(rows, 3), [1, 4]);
  assert.deepEqual(listPath(rows, 4), [5]);
  assert.equal(listPath(rows, -1), null);

  const root = sample();
  const [ring, map] = [ringArcs(root), treemapBlocks(root, 600, 400, 20)];
  const work = root.children[1];
  const zip = work.children[0].children[0];
  for (const shapes of [ring, map]) {
    for (const [i, s] of shapes.entries()) {
      assert.deepEqual(selectionShape(shapes, shapePath(s)), { index: i, exact: true });
    }
  }
  // The treemap does not draw the third level: its nearest folder is marked.
  const path = [work.id, work.children[0].id, zip.id];
  assert.equal(ring[selectionShape(ring, path).index].node, zip);
  const near = selectionShape(map, path);
  assert.equal(near.exact, false);
  assert.equal(map[near.index].node, work.children[0]);
  assert.deepEqual(selectionShape(map, [999_999]), { index: -1, exact: false });
  assert.deepEqual(selectionShape(map, null), { index: -1, exact: false });
});

test("an item grouped into Other marks the Other block", () => {
  const tiny = [node("t1", 1), node("t2", 1)];
  const inner = folder("inner", [node("big", 5000), ...tiny]);
  const root = folder("r", [inner, node("t3", 2), node("t4", 2)]);
  const arcs = ringArcs(root, 4, MAX_SHAPES, 0);
  const at = selectionShape(arcs, [inner.id, tiny[0].id]);
  assert.equal(at.exact, false);
  assert.equal(arcs[at.index].node.kind, "other");
  assert.equal(arcs[at.index].parent, inner);
  const outer = selectionShape(arcs, [root.children[1].id]);
  assert.equal(arcs[outer.index].node.kind, "other");
  assert.equal(arcs[outer.index].depth, 0);
  assert.equal(shapePath(arcs[outer.index]), null);
});

test("arrow keys move between siblings, into folders and out", () => {
  const root = sample();
  const arcs = ringArcs(root);
  const name = (i) => arcs[i].node.name;
  const first = stepShape(arcs, -1, "next");
  assert.equal(name(first), "Photos");
  const second = stepShape(arcs, first, "next");
  assert.equal(name(second), "Work");
  assert.equal(stepShape(arcs, first, "prev"), first, "stops at the end");
  assert.equal(name(stepShape(arcs, second, "in")), "Old");
  assert.equal(stepShape(arcs, stepShape(arcs, second, "in"), "out"), second);
  assert.equal(stepShape(arcs, second, "out"), second);
  assert.equal(stepShape([], -1, "next"), -1);
});

const types = (more) => TYPES.map((_, i) => more[i] ?? 0);

test("groups and folders carry their type totals, and deleting takes them away", () => {
  const tiny = [node("a.mp3", 30, [], { cat: 3 }), node("b.mp3", 10, [], { cat: 3 }), node("c.txt", 20, [], { cat: 0 })];
  const root = folder("r", [node("big.zip", 100_000, [], { cat: 4 }), folder("f", tiny)]);
  const group = mergeSmall(tiny, 100_000).at(-1);
  assert.deepEqual([group.kind, group.cat, group.split], ["other", 3, types({ 0: 20, 3: 40 })]);
  const p = palette("neon-grid", null);
  assert.deepEqual(
    legend("type", p, 0, root.split).map((k) => k.size),
    types({ 0: 20, 3: 40, 4: 100_000 }),
  );
  const out = prune(root, new Set([root.children[0].id, tiny[0].id]));
  assert.deepEqual([out.cat, out.split], [0, types({ 0: 20, 3: 10 })]);
  assert.equal(sum(out.split, (v) => v), out.size);
  assert.equal(dominant(types({})), 7);
  assert.equal(dominant(types({ 1: 5, 2: 5 })), 1);
});

test("drawn shapes and hit-testing agree at 125%, 150% and 200% scaling", () => {
  for (const dpr of [1, 1.25, 1.5, 2]) {
    for (const [w, h] of [[848, 671], [331, 331], [1203, 457]]) {
      const { cw, ch } = backing(w, h, dpr);
      assert.ok(Math.abs(cw - w * dpr) <= 0.5 && Math.abs(ch - h * dpr) <= 0.5);
      // A layout point is drawn at (x*dpr, y*dpr) in canvas pixels; the browser
      // stretches the canvas over w by h CSS pixels, where the pointer reports.
      const onScreen = (x, y) => [(x * dpr * w) / cw, (y * dpr * h) / ch];
      const arcs = ringArcs(sample());
      const g = ringGeom(w, h);
      arcs.forEach((a, i) => {
        // The middle of the slice, and a device pixel inside each of its edges.
        for (const at of [0.5, 1.5 / (g.ring * dpr), 1 - 1.5 / (g.ring * dpr)]) {
          const r = g.hole + (a.depth + at) * g.ring;
          const mid = (a.a0 + a.a1) / 2;
          const [x, y] = onScreen(g.cx + r * Math.sin(mid), g.cy - r * Math.cos(mid));
          assert.equal(hitRing(arcs, g, x, y), i, `${dpr} ${w}x${h} ${a.node.name}`);
        }
      });
      assert.equal(hitRing(arcs, g, ...onScreen(g.cx, g.cy)), CENTRE);
      const blocks = treemapBlocks(sample(), w, h, 20);
      blocks.forEach((b, i) => {
        const y = b.head ? b.y + 5 : b.y + b.h / 2;
        assert.equal(hitBlocks(blocks, ...onScreen(b.x + b.w / 2, y)), i, `${dpr} ${b.node.name}`);
      });
    }
  }
});

/** A canvas that only checks that every number it is given is usable. */
function checkedCanvas() {
  const calls = [];
  const check = (name, args) => {
    for (const a of args) if (typeof a === "number") assert.ok(Number.isFinite(a), `${name}(${args})`);
    if (name === "arc") assert.ok(args[2] >= 0, `radius ${args[2]}`);
    calls.push(name);
  };
  const target = { measureText: (t) => ({ width: t.length * 7 }) };
  const ctx = new Proxy(target, {
    get: (t, k) => t[k] ?? ((...args) => check(k, args)),
    set: (t, k, v) => ((t[k] = v), true),
  });
  globalThis.Path2D = class {
    arc = (...args) => check("arc", args);
    closePath = () => {};
  };
  return { ctx, calls };
}

test("empty, single-file, all-tiny and very deep folders lay out and draw", () => {
  let deep = node("leaf", 4096);
  for (let i = 0; i < 400; i++) deep = folder(`a rather long folder name ${i}`, [deep]);
  const tiny = folder("tiny", Array.from({ length: 1000 }, (_, i) => node(`t${i}`, 4096)));
  const zero = folder("zero", [node("nothing", 0)]);
  const cases = { empty: folder("empty", []), zero, one: folder("one", [node("only", 1)]), tiny, deep };
  const p = palette("neon-grid", null);
  const st = { mode: "type", palette: p, now: 0, px: 12, font: "sans-serif", code: "monospace" };
  for (const [name, root] of Object.entries(cases)) {
    for (const [w, h] of [[330, 330], [848, 671], [1, 1]]) {
      const { ctx } = checkedCanvas();
      const g = ringGeom(w, h);
      const cache = ringCache();
      const arcs = ringArcs(root);
      for (const a of arcs) drawArc(ctx, g, a, st, cache);
      if (arcs.length) drawRingZoom(ctx, g, arcs, arcs[0], 0.5, 4, st);
      drawCentre(ctx, g, [{ text: root.name, strong: true }, { text: "0 B", code: true }], true, st, cache.fits);
      const blocks = treemapBlocks(root, w, h, 20);
      for (const b of blocks) drawBlock(ctx, b, st);
      hitRing(arcs, g, w / 3, h / 3);
      hitBlocks(blocks, w / 3, h / 3);
      assert.equal(arcs.length > 0, root.size > 0, name);
    }
  }
  // Everything under the threshold becomes one full ring.
  const [only, ...rest] = ringArcs(tiny);
  assert.deepEqual([only.node.name, rest.length], ["Other (1,000 items)", 0]);
  close(only.a1, TAU);
});

test("a slice drawn again reuses its path and label", () => {
  const { ctx, calls } = checkedCanvas();
  const g = ringGeom(600, 600);
  const [arc] = ringArcs(sample());
  const st = { mode: "folder", palette: palette("neon-grid", null), now: 0, px: 12, font: "sans-serif", code: "monospace" };
  const cache = ringCache();
  drawArc(ctx, g, arc, st, cache);
  const first = cache.slices.get(arc);
  calls.length = 0;
  drawArc(ctx, g, arc, st, cache);
  assert.equal(cache.slices.get(arc), first);
  assert.ok(!calls.includes("arc") && calls.includes("fill") && calls.includes("fillText"));
});

test("zooming is planned only when motion is allowed", () => {
  const root = sample();
  const work = root.children[1];
  const scene = (r, crumbs) => ({ mode: "ring", root: r, crumbs, shapes: ringArcs(r) });
  const top = scene(root, [{ id: root.id }]);
  const inside = scene(work, [{ id: root.id }, { id: work.id }]);
  const into = zoomPlan(top, inside, true);
  assert.deepEqual([into.from, into.to, top.shapes[into.focus].node], [0, 1, work]);
  const back = zoomPlan(inside, top, true);
  assert.deepEqual([back.from, back.to, top.shapes[back.focus].node], [1, 0, work]);
  // Reduce motion (or a hidden view) passes false at the moment of the change.
  assert.equal(zoomPlan(top, inside, false), null);
  assert.equal(zoomPlan(inside, top, false), null);
  assert.equal(zoomPlan(null, top, true), null);
  assert.equal(zoomPlan(top, top, true), null);
});

test("arrow keys at the first and last sibling still lead everywhere else", () => {
  const root = folder("r", [
    folder("first", [node("f1", 400), node("f2", 300)]),
    node("middle", 500),
    folder("last", [node("l1", 200), node("l2", 100)]),
    node("s1", 1),
    node("s2", 1),
  ]);
  for (const shapes of [ringArcs(root, 4, MAX_SHAPES, 0), treemapBlocks(root, 900, 700, 20)]) {
    const at = (name) => shapes.findIndex((s) => s.node.name === name);
    for (const [end, past, away, child] of [
      ["first", "prev", "next", "f1"],
      ["last", "next", "prev", "l1"],
    ]) {
      const i = at(end);
      // The ends stop (the grouped "Other" after the last one is skipped).
      assert.equal(stepShape(shapes, i, past), i, end);
      assert.equal(stepShape(shapes, i, away), at("middle"), end);
      assert.equal(stepShape(shapes, i, "in"), at(child), end);
      assert.equal(stepShape(shapes, at(child), "out"), i, end);
    }
    // From the group itself, which a selection can mark, the arrows leave it.
    const group = shapes.findIndex((s) => s.node.kind === "other");
    assert.equal(stepShape(shapes, group, "prev"), at("last"));
  }
});

test("the chart cannot act on files: it only asks the view for a menu", () => {
  // Delete goes through the view: menu, disk_prepare, preview, confirm (Disk.svelte).
  for (const file of ["Chart.svelte", "chartdraw.ts", "chartlayout.ts", "chartsync.ts", "chartcolors.ts"]) {
    const source = readFileSync(new URL(`../src/${file}`, import.meta.url), "utf8");
    assert.doesNotMatch(source, /invoke|@tauri-apps\/api\/core/, file);
  }
});
