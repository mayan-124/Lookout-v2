// Layout for the disk charts: pure functions, no DOM, no scan access.

export interface CNode {
  id: number | null; // null: grouped "smaller items" / "not listed"
  name: string;
  kind: "folder" | "file" | "files" | "other" | "unlisted";
  size: number;
  children: CNode[];
}

export interface Arc {
  node: CNode;
  depth: number; // 0 = innermost ring
  branch: number; // colour group: index of the top-level segment it belongs to
  path: string;
}

export interface Block {
  node: CNode;
  depth: number;
  branch: number;
  x: number;
  y: number;
  w: number;
  h: number;
}

export const VIEW = 400; // both charts draw into a VIEW x VIEW box

const TAU = Math.PI * 2;

function polar(cx: number, cy: number, r: number, a: number): [number, number] {
  return [cx + r * Math.sin(a), cy - r * Math.cos(a)];
}

export function arcPath(cx: number, cy: number, r0: number, r1: number, a0: number, a1: number): string {
  const end = Math.min(a1, a0 + TAU - 0.0001);
  const large = end - a0 > Math.PI ? 1 : 0;
  const [x0, y0] = polar(cx, cy, r1, a0);
  const [x1, y1] = polar(cx, cy, r1, end);
  const [x2, y2] = polar(cx, cy, r0, end);
  const [x3, y3] = polar(cx, cy, r0, a0);
  const f = (n: number) => n.toFixed(2);
  return (
    `M${f(x0)} ${f(y0)}A${f(r1)} ${f(r1)} 0 ${large} 1 ${f(x1)} ${f(y1)}` +
    `L${f(x2)} ${f(y2)}A${f(r0)} ${f(r0)} 0 ${large} 0 ${f(x3)} ${f(y3)}Z`
  );
}

/** Concentric rings: the children of `root` form ring 0, their children ring 1, ... */
export function ringArcs(root: CNode, hole = 62, ringWidth = 34): Arc[] {
  const out: Arc[] = [];
  const c = VIEW / 2;
  const walk = (node: CNode, a0: number, a1: number, depth: number, branch: number) => {
    const total = node.children.reduce((s, k) => s + k.size, 0);
    if (total <= 0) return;
    let a = a0;
    node.children.forEach((k, i) => {
      const span = ((a1 - a0) * k.size) / total;
      const b = depth === 0 ? i : branch;
      // Slivers narrower than half a pixel are not drawn.
      if (span * (hole + depth * ringWidth) >= 0.5) {
        const r0 = hole + depth * ringWidth;
        out.push({ node: k, depth, branch: b, path: arcPath(c, c, r0 + 1, r0 + ringWidth - 1, a, a + span) });
        walk(k, a, a + span, depth + 1, b);
      }
      a += span;
    });
  };
  walk(root, 0, TAU, 0, 0);
  return out;
}

/** Squarified treemap (Bruls et al.): rectangles with areas proportional to sizes. */
export function squarify(sizes: number[], x: number, y: number, w: number, h: number): { x: number; y: number; w: number; h: number }[] {
  const total = sizes.reduce((s, v) => s + v, 0);
  const out: { x: number; y: number; w: number; h: number }[] = [];
  if (total <= 0 || w <= 0 || h <= 0) return sizes.map(() => ({ x, y, w: 0, h: 0 }));
  const areas = sizes.map((s) => (s / total) * w * h);
  const worst = (row: number[], side: number) => {
    const sum = row.reduce((s, v) => s + v, 0);
    let m = 0;
    for (const a of row) m = Math.max(m, (side * side * a) / (sum * sum), (sum * sum) / (side * side * a));
    return m;
  };
  let rx = x,
    ry = y,
    rw = w,
    rh = h;
  let i = 0;
  while (i < areas.length) {
    const side = Math.min(rw, rh);
    let row = [areas[i]];
    let j = i + 1;
    while (j < areas.length && worst([...row, areas[j]], side) <= worst(row, side)) {
      row = [...row, areas[j]];
      j++;
    }
    const sum = row.reduce((s, v) => s + v, 0);
    if (rw >= rh) {
      const cw = sum / rh;
      let cy = ry;
      for (const a of row) {
        out.push({ x: rx, y: cy, w: cw, h: a / cw });
        cy += a / cw;
      }
      rx += cw;
      rw -= cw;
    } else {
      const chh = sum / rw;
      let cx = rx;
      for (const a of row) {
        out.push({ x: cx, y: ry, w: a / chh, h: chh });
        cx += a / chh;
      }
      ry += chh;
      rh -= chh;
    }
    i = j;
  }
  return out;
}

/** Blocks for the children of `root` and, inside each, their children. */
export function treemapBlocks(root: CNode, size = VIEW): Block[] {
  const out: Block[] = [];
  const place = (node: CNode, x: number, y: number, w: number, h: number, depth: number, branch: number) => {
    const kids = [...node.children].sort((a, b) => b.size - a.size);
    if (!kids.length) return;
    const rects = squarify(
      kids.map((k) => k.size),
      x,
      y,
      w,
      h,
    );
    kids.forEach((k, i) => {
      const r = rects[i];
      if (r.w < 1 || r.h < 1) return;
      const b = depth === 0 ? i : branch;
      out.push({ node: k, depth, branch: b, x: r.x, y: r.y, w: r.w, h: r.h });
      if (depth === 0 && r.w > 28 && r.h > 28) place(k, r.x + 2, r.y + 14, r.w - 4, r.h - 16, 1, b);
    });
  };
  place(root, 0, 0, size, size, 0, 0);
  return out;
}
