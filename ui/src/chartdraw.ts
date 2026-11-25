// Canvas 2D drawing for the disk charts. Takes laid-out shapes; keeps no state.
import { TAU, zoomArc, type Arc, type Block, type Rect, type RingGeom } from "./chartlayout.ts";
import { fillFor, textOn, type ColourMode, type Palette } from "./chartcolors.ts";

export interface Style {
  mode: ColourMode;
  palette: Palette;
  now: number;
  font: string; // font family
  code: string;
  px: number; // label size
}

type Ctx = CanvasRenderingContext2D;
const TOP = Math.PI / 2; // chart angles start at the top, canvas angles at the right

export function human(n: number): string {
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return i === 0 ? `${n} B` : `${v.toFixed(v >= 100 ? 0 : 1)} ${u[i]}`;
}

/** The text, cut with an ellipsis to fit; "" when not even two letters fit. */
export function fitText(ctx: Pick<Ctx, "measureText">, text: string, width: number): string {
  // A name far longer than the space is cut before it is measured.
  const max = Math.max(0, Math.ceil(width / 2));
  const short = text.length > max ? text.slice(0, max) : text;
  if (short === text && ctx.measureText(text).width <= width) return text;
  let lo = 0;
  let hi = short.length;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (ctx.measureText(`${short.slice(0, mid)}…`).width <= width) lo = mid;
    else hi = mid - 1;
  }
  return lo < 2 ? "" : `${short.slice(0, lo)}…`;
}

export const fillOf = (s: Arc | Block, st: Style) => fillFor(s.node, st.mode, s.branch, s.depth, st.palette, st.now);

/**
 * What drawing a ring can reuse from one frame to the next: each slice's path,
 * colour and label, and the fitted centre lines. Make a new one whenever the
 * layout, colours or fonts change.
 */
export interface RingCache {
  slices: Map<Arc, Slice>;
  fits: Map<string, string>;
}
export const ringCache = (): RingCache => ({ slices: new Map(), fits: new Map() });

export interface Slice {
  /** null: too thin to draw. */
  path: Path2D | null;
  round: number; // corner radius, stroked round the path
  fill: string;
  label: { text: string; font: string; colour: string; x: number; y: number; turn: number } | null;
}

// ------------------------------------------------------------ ring

const radius = (g: RingGeom, depth: number) => (depth >= 0 ? g.hole + depth * g.ring : g.hole * Math.max(0, 1 + depth));

type PathSink = Pick<Path2D, "arc" | "closePath">;

/** Adds one slice to a path; returns the corner radius to stroke it with, -1 when too thin to draw. */
export function traceSlice(path: PathSink, g: RingGeom, depth: number, a0: number, a1: number): number {
  const r0 = radius(g, depth) + 0.75;
  const r1 = radius(g, depth + 1) - 0.75;
  if (r1 - r0 < 0.5 || a1 - a0 <= 0) return -1;
  if (a1 - a0 >= TAU - 1e-6) {
    path.arc(g.cx, g.cy, r1, 0, TAU);
    path.arc(g.cx, g.cy, r0, TAU, 0, true);
    return 0;
  }
  // Slim slices get neither a gap nor round corners: they would vanish.
  const room = (a1 - a0) * r0;
  const gap = room > 4 ? 0.75 : 0;
  const round = room > 9 && r1 - r0 > 8 ? 2 : 0;
  const inner = r0 + round;
  const outer = r1 - round;
  const i = (gap + round) / Math.max(inner, 1);
  const o = (gap + round) / outer;
  path.arc(g.cx, g.cy, outer, a0 + o - TOP, a1 - o - TOP);
  path.arc(g.cx, g.cy, inner, a1 - i - TOP, a0 + i - TOP, true);
  path.closePath();
  return round;
}

function paintSlice(ctx: Ctx, path: Path2D, round: number, fill: string) {
  ctx.fillStyle = fill;
  ctx.fill(path);
  if (round > 0) {
    ctx.strokeStyle = fill;
    ctx.lineWidth = round * 2;
    ctx.lineJoin = "round";
    ctx.stroke(path);
  }
}

function sliceLabel(ctx: Ctx, g: RingGeom, s: Arc, fill: string, st: Style): Slice["label"] {
  const span = s.a1 - s.a0;
  const mid = (s.a0 + s.a1) / 2;
  const r = g.hole + (s.depth + 0.5) * g.ring;
  // Along the ring: a straight line that stays inside the band.
  const sag = (g.ring - st.px) / 2 - 1;
  const chord = span < Math.PI ? 2 * r * Math.sin(span / 2) : 2 * r;
  const along = sag > 0 ? Math.min(chord, 2 * Math.sqrt(Math.max(0, r * r - (r - sag) ** 2))) - 6 : 0;
  // Or from the centre outwards.
  const out = span * (r - g.ring / 2) >= st.px + 3 ? g.ring - 10 : 0;
  const room = Math.max(along, out);
  if (room < st.px * 2.4) return null;
  const font = `600 ${st.px}px ${st.font}`;
  ctx.font = font;
  const text = fitText(ctx, s.node.name, room);
  if (!text) return null;
  let turn = along >= out ? mid : mid - TOP;
  // Never upside down.
  if (along >= out ? mid > TOP && mid < 3 * TOP : mid > Math.PI) turn += Math.PI;
  return { text, font, colour: textOn(fill), x: g.cx + r * Math.sin(mid), y: g.cy - r * Math.cos(mid), turn };
}

/** A slice ready to paint, from the cache when it was drawn before. */
export function sliceOf(ctx: Ctx, g: RingGeom, s: Arc, st: Style, cache: RingCache): Slice {
  let out = cache.slices.get(s);
  if (!out) {
    const path = new Path2D();
    const round = traceSlice(path, g, s.depth, s.a0, s.a1);
    const fill = fillOf(s, st);
    out = { path: round < 0 ? null : path, round, fill, label: sliceLabel(ctx, g, s, fill, st) };
    cache.slices.set(s, out);
  }
  return out;
}

export function drawArc(ctx: Ctx, g: RingGeom, s: Arc, st: Style, cache: RingCache) {
  const { path, round, fill, label } = sliceOf(ctx, g, s, st, cache);
  if (path) paintSlice(ctx, path, round, fill);
  if (!label) return;
  ctx.save();
  ctx.translate(label.x, label.y);
  ctx.rotate(label.turn);
  ctx.font = label.font;
  ctx.fillStyle = label.colour;
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.fillText(label.text, 0, 0);
  ctx.restore();
}

/** One frame of the zoom: `focus` has grown `k` of the way to the whole ring. */
export function drawRingZoom(ctx: Ctx, g: RingGeom, arcs: Arc[], focus: Arc, k: number, levels: number, st: Style) {
  for (const s of arcs) {
    const z = zoomArc(s, focus, k);
    if (z.a1 - z.a0 < 0.003 || z.depth <= -1 || z.depth >= levels) continue;
    const path = new Path2D();
    const round = traceSlice(path, g, z.depth, z.a0, z.a1);
    if (round >= 0) paintSlice(ctx, path, round, fillOf(s, st));
  }
}

/** A two-tone line round `path` (else the current one), visible on light and dark fills. */
export function outline(ctx: Ctx, colour: string, under: string, width: number, dashed = false, path?: Path2D) {
  const stroke = () => (path ? ctx.stroke(path) : ctx.stroke());
  ctx.lineJoin = "round";
  ctx.strokeStyle = under;
  ctx.lineWidth = width + 2;
  stroke();
  if (dashed) ctx.setLineDash([4, 3]);
  ctx.strokeStyle = colour;
  ctx.lineWidth = width;
  stroke();
  ctx.setLineDash([]);
}

export function drawCentre(
  ctx: Ctx,
  g: RingGeom,
  lines: { text: string; strong?: boolean; code?: boolean }[],
  hot: boolean,
  st: Style,
  fits?: Map<string, string>,
) {
  const r = g.hole - 4;
  ctx.beginPath();
  ctx.arc(g.cx, g.cy, r, 0, TAU);
  ctx.fillStyle = st.palette.surface;
  ctx.fill();
  if (hot) {
    ctx.strokeStyle = st.palette.accent;
    ctx.lineWidth = 1.5;
    ctx.stroke();
  }
  const step = st.px + 4;
  let y = g.cy - ((lines.length - 1) * step) / 2;
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  for (const l of lines) {
    const dy = Math.abs(y - g.cy) + st.px / 2;
    const room = 2 * Math.sqrt(Math.max(0, r * r - dy * dy)) - 8;
    ctx.font = `${l.strong ? 600 : 400} ${l.strong ? st.px + 1 : st.px - 1}px ${l.code ? st.code : st.font}`;
    ctx.fillStyle = l.strong ? st.palette.text : st.palette.soft;
    const key = `${ctx.font}|${room}|${l.text}`;
    let text = fits?.get(key);
    if (text === undefined) {
      text = fitText(ctx, l.text, room);
      fits?.set(key, text);
    }
    ctx.fillText(text, g.cx, y);
    y += step;
  }
}

// ------------------------------------------------------------ treemap

export function blockPath(ctx: Ctx, r: Rect, inset: number) {
  const w = Math.max(0.5, r.w - inset * 2);
  const h = Math.max(0.5, r.h - inset * 2);
  ctx.beginPath();
  if (w > 10 && h > 10) ctx.roundRect(r.x + inset, r.y + inset, w, h, 3);
  else ctx.rect(r.x + inset, r.y + inset, w, h);
}

export function drawBlock(ctx: Ctx, b: Block, st: Style) {
  const fill = fillOf(b, st);
  blockPath(ctx, b, Math.min(1, b.w / 4, b.h / 4));
  ctx.fillStyle = fill;
  ctx.fill();
  // Insides can share the folder's colour: a base-coloured line keeps them apart.
  if (b.depth > 0 && b.w > 3 && b.h > 3) {
    ctx.strokeStyle = st.palette.base;
    ctx.lineWidth = 1;
    ctx.stroke();
  }
  const px = st.px;
  const room = b.w - 10;
  if (room < px * 2 || (!b.head && b.h < px + 6)) return;
  ctx.textAlign = "left";
  ctx.textBaseline = "middle";
  ctx.fillStyle = textOn(fill);
  ctx.font = `600 ${px}px ${st.font}`;
  if (b.head) {
    ctx.fillText(fitText(ctx, `${b.node.name}  ${human(b.node.size)}`, room), b.x + 5, b.y + b.head / 2 + 0.5);
    return;
  }
  ctx.fillText(fitText(ctx, b.node.name, room), b.x + 5, b.y + px / 2 + 4);
  if (b.h < px * 2 + 10) return;
  ctx.font = `400 ${px - 1}px ${st.code}`;
  const size = human(b.node.size);
  if (ctx.measureText(size).width <= room) ctx.fillText(size, b.x + 5, b.y + px * 1.5 + 7);
}

/** Blocks without labels, scaled so that rectangle `from` lands on `to`. */
export function drawBlocksMapped(ctx: Ctx, blocks: Block[], from: Rect, to: Rect, st: Style) {
  const sx = to.w / Math.max(from.w, 1e-6);
  const sy = to.h / Math.max(from.h, 1e-6);
  for (const b of blocks) {
    const r = { x: to.x + (b.x - from.x) * sx, y: to.y + (b.y - from.y) * sy, w: b.w * sx, h: b.h * sy };
    blockPath(ctx, r, Math.min(1, r.w / 4, r.h / 4));
    ctx.fillStyle = fillOf(b, st);
    ctx.fill();
    if (b.depth > 0) {
      ctx.strokeStyle = st.palette.base;
      ctx.lineWidth = 1;
      ctx.stroke();
    }
  }
}
