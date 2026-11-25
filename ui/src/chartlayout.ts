// Layout and hit-testing for the disk charts: pure functions, no DOM, no scan
// access. Everything here works on the capped tree `disk_chart` sends.

export interface CNode {
  id: number | null; // null: grouped "Other" and "not listed"
  name: string;
  kind: "folder" | "file" | "files" | "other" | "unlisted";
  size: number;
  /** Items inside a folder, 1 for a file, how many were grouped. */
  items: number;
  /** Unix seconds (a folder: the newest inside); 0 when unknown. */
  modified: number;
  /** File type index (see TYPES in chartcolors.ts) holding the most bytes. */
  cat: number;
  /** Bytes per file type, in the order of TYPES; adds up to `size`. */
  split: number[];
  children: CNode[];
}

export interface Shape {
  node: CNode;
  parent: CNode;
  depth: number; // 0 = a child of the chart's folder
  branch: number; // colour group: index of the depth-0 ancestor
  /** Nodes from the chart's folder (excluded) down to this one (included). */
  trail: CNode[];
}
export interface Arc extends Shape {
  a0: number; // radians, clockwise from the top
  a1: number;
}
export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}
export interface Block extends Shape, Rect {
  /** Height of the header strip; 0 when nothing is drawn inside. */
  head: number;
}

export const TAU = Math.PI * 2;
export const MAX_SHAPES = 1500;
export const RING_LEVELS = 4;
/** Items under this share of their parent are grouped into "Other". */
export const MIN_SHARE = 0.005;

const total = (nodes: CNode[]) => nodes.reduce((s, n) => s + n.size, 0);
export const OTHER_TYPE = 7;

/** The type holding the most bytes (the first of equals); "other" for none. */
export function dominant(split: number[]): number {
  let best = OTHER_TYPE;
  let most = 0;
  split.forEach((bytes, i) => {
    if (bytes > most) [best, most] = [i, bytes];
  });
  return best;
}

/** Largest first, empty items dropped, small ones merged into one "Other (N items)" at the end. */
export function mergeSmall(children: CNode[], parentSize: number, share = MIN_SHARE): CNode[] {
  const live = children.filter((c) => c.size > 0).sort((a, b) => b.size - a.size);
  const limit = Math.max(parentSize, total(live)) * share;
  const keep: CNode[] = [];
  const small: CNode[] = [];
  for (const c of live) (c.kind === "other" || c.size < limit ? small : keep).push(c);
  // One small item is not worth hiding behind "Other".
  if (small.length < 2) return keep.concat(small);
  const items = small.reduce((s, c) => s + (c.kind === "other" || c.kind === "files" ? c.items : 1), 0);
  const split = small.reduce((sum, c) => sum.map((v, i) => v + c.split[i]), small[0].split.map(() => 0));
  keep.push({
    id: null,
    name: `Other (${items.toLocaleString("en-US")} items)`,
    kind: "other",
    size: total(small),
    items,
    modified: small.reduce((m, c) => Math.max(m, c.modified), 0),
    cat: dominant(split),
    split,
    children: [],
  });
  return keep;
}

/** The tree without the ids in `gone` (moved to the Recycle Bin since the scan). */
export function prune(node: CNode, gone: ReadonlySet<number>): CNode {
  if (!gone.size) return node;
  let size = node.size;
  let items = node.items;
  const split = node.split.slice();
  let changed = false;
  const children: CNode[] = [];
  for (const c of node.children) {
    if (c.id !== null && gone.has(c.id)) {
      changed = true;
      size -= c.size;
      items -= c.kind === "folder" ? c.items + 1 : 1;
      c.split.forEach((v, i) => (split[i] -= v));
      continue;
    }
    const p = prune(c, gone);
    if (p !== c) {
      changed = true;
      size -= c.size - p.size;
      items -= c.items - p.items;
      c.split.forEach((v, i) => (split[i] -= v - p.split[i]));
    }
    children.push(p);
  }
  if (!changed) return node;
  const left = split.map((v) => Math.max(0, v));
  return { ...node, children, size: Math.max(0, size), items: Math.max(0, items), split: left, cat: dominant(left) };
}

// ------------------------------------------------------------ ring

/**
 * Sunburst arcs, level by level (so the shape cap drops outer rings first).
 * Slices narrower than `minSpan` radians are not drawn.
 */
export function ringArcs(root: CNode, levels = RING_LEVELS, cap = MAX_SHAPES, minSpan = 0.004): Arc[] {
  const out: Arc[] = [];
  let frontier: { node: CNode; a0: number; a1: number; branch: number; trail: CNode[] }[] = [
    { node: root, a0: 0, a1: TAU, branch: 0, trail: [] },
  ];
  for (let depth = 0; depth < levels && frontier.length; depth++) {
    const next: typeof frontier = [];
    for (const f of frontier) {
      const kids = mergeSmall(f.node.children, f.node.size);
      const sum = total(kids);
      if (sum <= 0) continue;
      let a = f.a0;
      for (let i = 0; i < kids.length; i++) {
        const node = kids[i];
        const span = ((f.a1 - f.a0) * node.size) / sum;
        const a1 = a + span;
        if (span >= minSpan && out.length < cap) {
          const branch = depth === 0 ? i : f.branch;
          const trail = [...f.trail, node];
          out.push({ node, parent: f.node, depth, branch, trail, a0: a, a1 });
          if (node.children.length) next.push({ node, a0: a, a1, branch, trail });
        }
        a = a1;
      }
    }
    frontier = next;
  }
  return out;
}

export interface RingGeom {
  cx: number;
  cy: number;
  hole: number;
  ring: number; // width of one level
}

export function ringGeom(w: number, h: number, levels = RING_LEVELS): RingGeom {
  const radius = Math.max(20, Math.min(w, h) / 2 - 3);
  const hole = radius * 0.38;
  return { cx: w / 2, cy: h / 2, hole, ring: (radius - hole) / levels };
}

export const CENTRE = -2;

/** Index of the arc under the point, `CENTRE` for the hole, -1 for nothing. */
export function hitRing(arcs: Arc[], g: RingGeom, x: number, y: number): number {
  const dx = x - g.cx;
  const dy = y - g.cy;
  const r = Math.hypot(dx, dy);
  if (r < g.hole) return CENTRE;
  const depth = Math.floor((r - g.hole) / g.ring);
  const a = (Math.atan2(dx, -dy) + TAU) % TAU;
  for (let i = 0; i < arcs.length; i++) {
    const s = arcs[i];
    if (s.depth === depth && a >= s.a0 && a < s.a1) return i;
  }
  return -1;
}

/** Where an arc goes when `focus` grows into the whole chart (`k` from 0 to 1). */
export function zoomArc(s: Arc, focus: Arc, k: number): { a0: number; a1: number; depth: number } {
  const span = Math.max(focus.a1 - focus.a0, 1e-9);
  const to = (a: number) => Math.min(1, Math.max(0, (a - focus.a0) / span)) * TAU;
  return {
    a0: s.a0 + (to(s.a0) - s.a0) * k,
    a1: s.a1 + (to(s.a1) - s.a1) * k,
    depth: s.depth - (focus.depth + 1) * k,
  };
}

/**
 * A zoom between two layouts: in when the new folder was drawn before, out when
 * an old one is drawn now. `focus` is that folder's shape in `outer`. None with
 * `animate` off (Reduce motion, or a hidden view): the new layout shows at once.
 */
export function zoomPlan<S extends { mode: string; root: CNode; crumbs: { id: number }[]; shapes: Shape[] }>(
  prev: S | null,
  next: S | null,
  animate: boolean,
): { outer: S; inner: S; focus: number; from: number; to: number } | null {
  if (!animate || !prev || !next || prev.mode !== next.mode || prev.root.id === next.root.id) return null;
  const find = (s: S, id: number | null) => s.shapes.findIndex((x) => x.node.id === id);
  const into = find(prev, next.root.id);
  if (into >= 0) return { outer: prev, inner: next, focus: into, from: 0, to: 1 };
  for (let i = prev.crumbs.length - 1; i >= 0; i--) {
    const focus = find(next, prev.crumbs[i].id);
    if (focus >= 0) return { outer: next, inner: prev, focus, from: 1, to: 0 };
  }
  return null;
}

// ------------------------------------------------------------ screen scale

/**
 * Canvas pixels for a `w` by `h` CSS pixel chart. Drawing is scaled by `dpr`
 * and pointer positions arrive in CSS pixels, so layout and hit-testing share
 * one coordinate system whatever the display scaling.
 */
export function backing(w: number, h: number, dpr: number): { cw: number; ch: number } {
  return { cw: Math.max(1, Math.round(w * dpr)), ch: Math.max(1, Math.round(h * dpr)) };
}

// ------------------------------------------------------------ treemap

/** Squarified treemap (Bruls et al.): rectangles with areas proportional to sizes. */
export function squarify(sizes: number[], x: number, y: number, w: number, h: number): Rect[] {
  const out: Rect[] = sizes.map(() => ({ x, y, w: 0, h: 0 }));
  // Empty items get an empty rectangle and take no part in the rows.
  const live = sizes.map((_, i) => i).filter((i) => sizes[i] > 0);
  const sum = live.reduce((s, i) => s + sizes[i], 0);
  if (sum <= 0 || w <= 0 || h <= 0) return out;
  const areas = live.map((i) => (sizes[i] / sum) * w * h);
  let rx = x,
    ry = y,
    rw = w,
    rh = h;
  let i = 0;
  while (i < areas.length) {
    const side = Math.max(Math.min(rw, rh), 1e-9);
    // Worst aspect ratio of a row, kept from its sum, smallest and largest area.
    const worst = (s: number, lo: number, hi: number) => Math.max((side * side * hi) / (s * s), (s * s) / (side * side * lo));
    let s = areas[i],
      lo = s,
      hi = s;
    let j = i + 1;
    for (; j < areas.length; j++) {
      const a = areas[j];
      if (worst(s + a, Math.min(lo, a), Math.max(hi, a)) > worst(s, lo, hi)) break;
      s += a;
      lo = Math.min(lo, a);
      hi = Math.max(hi, a);
    }
    if (rw >= rh) {
      const cw = s / Math.max(rh, 1e-9);
      let cy = ry;
      for (; i < j; i++) {
        out[live[i]] = { x: rx, y: cy, w: cw, h: areas[i] / cw };
        cy += areas[i] / cw;
      }
      rx += cw;
      rw = Math.max(0, rw - cw);
    } else {
      const ch = s / Math.max(rw, 1e-9);
      let cx = rx;
      for (; i < j; i++) {
        out[live[i]] = { x: cx, y: ry, w: areas[i] / ch, h: ch };
        cx += areas[i] / ch;
      }
      ry += ch;
      rh = Math.max(0, rh - ch);
    }
  }
  return out;
}

/** Blocks for the children of `root` and, inside folders with room, their children. */
export function treemapBlocks(root: CNode, width: number, height: number, head = 18, cap = MAX_SHAPES): Block[] {
  const out: Block[] = [];
  const place = (parent: CNode, area: Rect, depth: number, branch: number, trail: CNode[]) => {
    const kids = mergeSmall(parent.children, parent.size).sort((a, b) => b.size - a.size);
    const rects = squarify(
      kids.map((k) => k.size),
      area.x,
      area.y,
      area.w,
      area.h,
    );
    for (let i = 0; i < kids.length && out.length < cap; i++) {
      const r = rects[i];
      if (r.w < 1 || r.h < 1) continue;
      out.push({ node: kids[i], parent, depth, branch: depth ? branch : i, trail: [...trail, kids[i]], head: 0, ...r });
    }
  };
  place(root, { x: 0, y: 0, w: width, h: height }, 0, 0, []);
  // All top blocks first, then their insides: a later block lies on top.
  const top = out.length;
  for (let i = 0; i < top && out.length < cap; i++) {
    const b = out[i];
    if (!b.node.children.length || b.w < 40 || b.h < head + 18) continue;
    b.head = head;
    place(b.node, { x: b.x + 3, y: b.y + head, w: b.w - 6, h: b.h - head - 3 }, 1, b.branch, b.trail);
  }
  return out;
}

/** Index of the innermost block under the point, or -1. */
export function hitBlocks(blocks: Block[], x: number, y: number): number {
  for (let i = blocks.length - 1; i >= 0; i--) {
    const b = blocks[i];
    if (x >= b.x && x < b.x + b.w && y >= b.y && y < b.y + b.h) return i;
  }
  return -1;
}

/** Name filter over what is drawn: matches, and everything inside a match, stay bright. */
export function filterShapes(shapes: Shape[], query: string): { bright: boolean[]; count: number } {
  const q = query.trim().toLowerCase();
  const hit = (n: CNode) => n.id !== null && n.name.toLowerCase().includes(q);
  if (!q) return { bright: shapes.map(() => true), count: 0 };
  return {
    bright: shapes.map((s) => s.trail.some(hit)),
    count: shapes.filter((s) => hit(s.node)).length,
  };
}
