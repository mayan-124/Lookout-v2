// One selection shared by the folder list, the ring and the treemap, named by
// the path of row ids from the current folder down to the item. Pure functions.
import type { Shape } from "./chartlayout.ts";

/** Path of the list row at `index`, read from the depths of the rows above it. */
export function listPath(rows: { depth: number; row: { id: number } }[], index: number): number[] | null {
  const at = rows[index];
  if (!at) return null;
  const path = [at.row.id];
  let depth = at.depth;
  for (let i = index - 1; i >= 0 && depth > 0; i--) {
    if (rows[i].depth < depth) {
      depth = rows[i].depth;
      path.unshift(rows[i].row.id);
    }
  }
  return path;
}

/** The ids of a chart shape's path; null for "Other" and "not listed". */
export function shapePath(shape: Shape): number[] | null {
  const ids = shape.trail.map((n) => n.id);
  return ids.every((id) => id !== null) ? (ids as number[]) : null;
}

/**
 * The shape that shows a selected path. `exact` is false when the item itself is
 * not drawn: then it is the "Other" group holding it, else its nearest drawn folder.
 */
export function selectionShape(shapes: Shape[], path: number[] | null): { index: number; exact: boolean } {
  if (!path?.length) return { index: -1, exact: false };
  const find = (id: number) => shapes.findIndex((s) => s.node.id === id);
  const own = find(path[path.length - 1]);
  if (own >= 0) return { index: own, exact: true };
  for (let k = path.length - 2; k >= -1; k--) {
    const folder = k < 0 ? -1 : find(path[k]);
    if (k >= 0 && folder < 0) continue;
    const group = shapes.findIndex(
      (s) => s.node.kind === "other" && s.depth === k + 1 && (k < 0 || s.parent.id === path[k]),
    );
    return { index: group >= 0 ? group : folder, exact: false };
  }
  return { index: -1, exact: false };
}

export type Step = "prev" | "next" | "in" | "out";

/** Arrow keys: between siblings, into a folder and out of it. Groups are skipped. */
export function stepShape(shapes: Shape[], from: number, step: Step): number {
  const real = (s: Shape) => s.node.id !== null;
  const at = shapes[from];
  if (!at) return shapes.findIndex(real);
  if (step === "in" || step === "out") {
    const to = shapes.findIndex((s) => real(s) && (step === "in" ? s.parent === at.node : s.node === at.parent));
    return to < 0 ? from : to;
  }
  const dir = step === "next" ? 1 : -1;
  for (let i = from + dir; i >= 0 && i < shapes.length; i += dir) {
    if (shapes[i].parent === at.parent && real(shapes[i])) return i;
  }
  return from;
}
