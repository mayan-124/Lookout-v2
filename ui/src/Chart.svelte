<script lang="ts" module>
  import type { CNode } from "./chartlayout";

  export interface ChartData {
    root: CNode;
    crumbs: { id: number; name: string }[];
  }
  /** What is under the pointer, for the details line. */
  export interface Detail {
    name: string;
    path: string;
    size: number;
    share: number;
    items: number;
    modified: number;
  }
</script>

<script lang="ts">
  import { onDestroy, onMount, untrack } from "svelte";
  import {
    CENTRE,
    RING_LEVELS,
    backing,
    filterShapes,
    hitBlocks,
    hitRing,
    ringArcs,
    ringGeom,
    treemapBlocks,
    zoomPlan,
    type Arc,
    type Block,
    type RingGeom,
    type Shape,
  } from "./chartlayout";
  import { COLOUR_MODES, legend, palette, type ColourMode } from "./chartcolors";
  import { selectionShape, shapePath, stepShape, type Step } from "./chartsync";
  import {
    blockPath,
    drawArc,
    drawBlock,
    drawBlocksMapped,
    drawCentre,
    drawRingZoom,
    human,
    outline,
    ringCache,
    sliceOf,
    type Style,
  } from "./chartdraw";
  import { look } from "./look.svelte";

  let {
    data,
    mode,
    colours,
    selected,
    loading = false,
    big = false,
    query = $bindable(""),
    ondrill,
    onup,
    onselect,
    onmenu,
    oncolours,
    onhover,
  }: {
    data: ChartData | null;
    mode: "ring" | "treemap";
    colours: ColourMode;
    /** Ids from the chart's folder down to the selected item. */
    selected: number[] | null;
    loading?: boolean;
    big?: boolean;
    /** The treemap's name filter. */
    query?: string;
    ondrill: (id: number) => void;
    onup: () => void;
    onselect: (path: number[]) => void;
    onmenu: (path: number[], x: number, y: number) => void;
    oncolours: (mode: ColourMode) => void;
    onhover: (detail: Detail | null) => void;
  } = $props();

  interface Scene {
    mode: "ring" | "treemap";
    root: CNode;
    crumbs: ChartData["crumbs"];
    w: number;
    h: number;
    geom: RingGeom;
    arcs: Arc[];
    blocks: Block[];
    shapes: Shape[];
    now: number;
  }
  /** A zoom between a layout (`outer`) and the layout of one of its folders. */
  type Zoom = NonNullable<ReturnType<typeof zoomPlan<Scene>>> & { start: number };
  const DURATION = 260;

  let wrap: HTMLElement | undefined = $state();
  // Three layers: the still picture, its dimmed copy (the ring under a hover,
  // swapped in for the still one) and on top what follows the pointer.
  let base: HTMLCanvasElement | undefined = $state();
  let dimmed: HTMLCanvasElement | undefined = $state();
  let canvas: HTMLCanvasElement | undefined = $state();
  let w = $state(0);
  let h = $state(0);
  let dpr = $state(window.devicePixelRatio || 1);
  let scene = $state.raw<Scene | null>(null);
  let hover = $state(-1);
  let pointer = $state.raw<{ x: number; y: number } | null>(null);

  const px = $derived(big ? 14 : 12);
  const colourSet = $derived(palette(look.theme, look.accent));
  const marked = $derived(scene ? selectionShape(scene.shapes, selected) : { index: -1, exact: false });
  const filtered = $derived(scene?.mode === "treemap" && query.trim() ? filterShapes(scene.blocks, query) : null);
  const keys = $derived(scene ? legend(colours, colourSet, scene.now, scene.root.split) : []);
  const folderName = (n: string) => n.split("\\").filter(Boolean).pop() ?? n;
  const title = $derived(data ? folderName(data.root.name) : "");
  const hovered = $derived(hover >= 0 ? scene?.shapes[hover] : undefined);
  const share = (s: Scene, n: CNode) => (s.root.size > 0 ? (n.size / s.root.size) * 100 : 0);
  const percent = (v: number) => `${v.toFixed(v < 10 ? 1 : 0)}%`;
  const count = (n: number) => `${n.toLocaleString()} item${n === 1 ? "" : "s"}`;
  const date = (s: number) => (s ? new Date(s * 1000).toLocaleDateString() : "unknown");
  const join = (parts: string[]) => parts.filter(Boolean).reduce((a, b) => (a ? `${a.replace(/\\$/, "")}\\${b}` : b), "");
  const pathOf = (trail: CNode[]) => join([...(data?.crumbs ?? []).map((c) => c.name), ...trail.map((n) => n.name)]);
  const empty = $derived(
    !data
      ? loading
        ? "Loading chart…"
        : "No chart for this folder."
      : scene && scene.shapes.length === 0
        ? data.root.items === 0
          ? "This folder is empty."
          : "Nothing here takes up space on this disk."
        : "",
  );
  const summary = $derived(
    data
      ? `${mode === "ring" ? "Ring chart" : "Treemap"} of ${title}: ${human(data.root.size)}, ${count(data.root.items)}. Arrow keys move between items, Enter goes into a folder, Backspace goes up.`
      : "Disk chart",
  );
  const spoken = $derived.by(() => {
    const s = marked.exact ? scene?.shapes[marked.index] : undefined;
    return s && scene ? `${s.node.name}, ${s.node.kind}, ${human(s.node.size)}, ${percent(share(scene, s.node))}` : "";
  });

  // ------------------------------------------------------------ drawing
  let raf = 0;
  let zoom: Zoom | null = null;
  // What the next frame has to do.
  let stale = true; // the still picture needs redrawing
  let dimStale = true; // so does its dimmed copy
  let dirty = true; // and what lies over them
  let moved: { x: number; y: number } | null = null; // the latest pointer event
  let cache = ringCache();
  let fonts = { font: "sans-serif", code: "monospace" };

  const style = (s: Scene): Style => ({ mode: colours, palette: colourSet, now: s.now, px, ...fonts });
  const visible = () => document.visibilityState === "visible";
  function schedule() {
    if (!raf && visible()) raf = requestAnimationFrame(frame);
  }
  function redraw() {
    dirty = true;
    schedule();
  }
  function invalidate() {
    stale = true;
    redraw();
  }

  /** The layer's context, cleared if asked, sized to the chart and scaled to CSS pixels. */
  function layer(el: HTMLCanvasElement, clear: boolean) {
    const { cw, ch } = backing(w, h, dpr);
    const ctx = el.getContext("2d");
    if (!ctx) return null;
    if (el.width !== cw || el.height !== ch) {
      el.width = cw;
      el.height = ch;
    } else if (clear) {
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.clearRect(0, 0, cw, ch);
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    return ctx;
  }
  function showDimmed(on: boolean) {
    if (!base || !dimmed) return;
    base.style.visibility = on ? "hidden" : "";
    dimmed.style.visibility = on ? "" : "hidden";
  }

  function frame(time: number) {
    raf = 0;
    if (!base || !dimmed || !canvas) return;
    // However many pointer events came in, only the last one is looked at.
    if (moved) {
      pointer = moved;
      moved = null;
      if (!zoom) {
        const i = hit(pointer.x, pointer.y);
        if (i !== hover) setHover(i);
      }
    }
    const s = scene;
    if (zoom) {
      zoom.start ||= time;
      const t = Math.min(1, (time - zoom.start) / DURATION);
      if (t < 1) {
        const eased = 1 - (1 - t) ** 3;
        showDimmed(false);
        layer(canvas, true);
        const ctx = layer(base, true);
        if (ctx) drawZoom(ctx, zoom, zoom.from + (zoom.to - zoom.from) * eased);
        schedule();
        return;
      }
      zoom = null;
      stale = true;
      if (pointer) setHover(hit(pointer.x, pointer.y));
    }
    // The same slice as before and nothing else changed: nothing to draw.
    if (!stale && !dirty) return;
    if (!s) {
      showDimmed(false);
      layer(base, true);
      layer(canvas, true);
      return;
    }
    const { cw, ch } = backing(w, h, dpr);
    if (stale || base.width !== cw || base.height !== ch) {
      cache = ringCache();
      const ctx = layer(base, true);
      if (ctx) drawStill(ctx, s);
      dimStale = true;
      dimmed.width = 0; // freed until a hover needs it again
      stale = false;
    }
    const dim = s.mode === "ring" && !!s.shapes[hover];
    if (dim && dimStale) {
      drawDimmed();
      dimStale = false;
    }
    showDimmed(dim);
    const over = layer(canvas, true);
    if (over) drawOverlay(over, s);
    dirty = false;
  }

  function drawStill(ctx: CanvasRenderingContext2D, s: Scene) {
    const st = style(s);
    if (s.mode === "ring") {
      for (const a of s.arcs) drawArc(ctx, s.geom, a, st, cache);
    } else {
      s.blocks.forEach((b, i) => {
        ctx.globalAlpha = filtered && !filtered.bright[i] ? 0.22 : 1;
        drawBlock(ctx, b, st);
      });
      ctx.globalAlpha = 1;
    }
  }

  /** Made at the first hover of a layout, then only shown and hidden. */
  function drawDimmed() {
    const ctx = dimmed && layer(dimmed, true);
    if (!ctx || !base) return;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.drawImage(base, 0, 0);
    // Dim what is drawn, not the empty canvas.
    ctx.save();
    ctx.globalCompositeOperation = "source-atop";
    ctx.globalAlpha = 0.3;
    ctx.fillStyle = colourSet.base;
    ctx.fillRect(0, 0, ctx.canvas.width, ctx.canvas.height);
    ctx.restore();
  }

  /** The hovered item, the selection and the ring's centre, over the still picture. */
  function drawOverlay(ctx: CanvasRenderingContext2D, s: Scene) {
    const st = style(s);
    const p = colourSet;
    const at = s.shapes[hover];
    if (s.mode === "ring") {
      const arc = s.arcs[hover];
      if (arc) drawArc(ctx, s.geom, arc, st, cache);
      const m = s.arcs[marked.index];
      const selected = m && sliceOf(ctx, s.geom, m, st, cache).path;
      if (selected) outline(ctx, p.accent, p.base, 2, !marked.exact, selected);
      const hot = arc && sliceOf(ctx, s.geom, arc, st, cache).path;
      if (hot) outline(ctx, p.text, p.base, 1, false, hot);
      const node = at?.node ?? s.root;
      drawCentre(
        ctx,
        s.geom,
        [
          { text: at ? node.name : folderName(s.root.name), strong: true },
          { text: human(node.size), code: true },
          ...(at ? [{ text: percent(share(s, node)) }] : []),
          { text: count(node.items) },
          ...(!at && s.crumbs.length > 1 ? [{ text: "▲ up" }] : []),
        ],
        hover === CENTRE && s.crumbs.length > 1,
        st,
        cache.fits,
      );
    } else {
      const m = s.blocks[marked.index];
      if (m) {
        blockPath(ctx, m, 2);
        outline(ctx, p.accent, p.base, 2.5, !marked.exact);
      }
      if (at) {
        blockPath(ctx, s.blocks[hover], 1.5);
        outline(ctx, p.accent, p.base, 1.5);
      }
    }
  }

  function drawZoom(ctx: CanvasRenderingContext2D, z: Zoom, k: number) {
    const st = style(z.outer);
    if (z.outer.mode === "ring") {
      drawRingZoom(ctx, z.outer.geom, z.outer.arcs, z.outer.arcs[z.focus], k, RING_LEVELS, st);
      const s = z.to === 1 ? z.inner : z.outer;
      drawCentre(ctx, s.geom, [{ text: folderName(s.root.name), strong: true }, { text: human(s.root.size), code: true }], false, st);
      return;
    }
    const f = z.outer.blocks[z.focus];
    const full = { x: 0, y: 0, w: z.outer.w, h: z.outer.h };
    const now = {
      x: f.x * (1 - k),
      y: f.y * (1 - k),
      w: f.w + (full.w - f.w) * k,
      h: f.h + (full.h - f.h) * k,
    };
    ctx.globalAlpha = 1 - k;
    drawBlocksMapped(ctx, z.outer.blocks, f, now, st);
    ctx.globalAlpha = k;
    drawBlocksMapped(ctx, z.inner.blocks, { x: 0, y: 0, w: z.inner.w, h: z.inner.h }, now, st);
    ctx.globalAlpha = 1;
  }

  // ------------------------------------------------------------ layout
  function build(d: ChartData, m: "ring" | "treemap", width: number, height: number, label: number): Scene {
    const arcs = m === "ring" ? ringArcs(d.root) : [];
    const blocks = m === "treemap" ? treemapBlocks(d.root, width, height, label + 8) : [];
    return {
      mode: m,
      root: d.root,
      crumbs: d.crumbs,
      w: width,
      h: height,
      geom: ringGeom(width, height),
      arcs,
      blocks,
      shapes: m === "ring" ? arcs : blocks,
      now: Date.now() / 1000,
    };
  }

  function rebuild(d: ChartData | null, m: "ring" | "treemap", width: number, height: number, label: number) {
    const prev = scene;
    const next = d && width > 0 && height > 0 ? build(d, m, width, height, label) : null;
    // Read now, so switching Reduce motion counts from the next change on.
    const plan = zoomPlan(prev, next, !look.reduced && visible());
    zoom = plan && { ...plan, start: 0 };
    scene = next;
    setHover(!zoom && pointer && next ? hit(pointer.x, pointer.y) : -1);
    invalidate();
  }

  $effect(() => {
    const [d, m, width, height, label] = [data, mode, w, h, px];
    untrack(() => rebuild(d, m, width, height, label));
  });
  $effect(() => {
    void [colourSet, colours, filtered, dpr];
    untrack(invalidate);
  });
  $effect(() => {
    void marked;
    untrack(redraw);
  });
  // A screen with another scale: redraw at its pixel density.
  $effect(() => {
    const scale = window.matchMedia(`(resolution: ${dpr}dppx)`);
    const changed = () => (dpr = window.devicePixelRatio || 1);
    scale.addEventListener("change", changed);
    return () => scale.removeEventListener("change", changed);
  });

  // ------------------------------------------------------------ pointer and keys
  function hit(x: number, y: number): number {
    const s = scene;
    if (!s) return -1;
    return s.mode === "ring" ? hitRing(s.arcs, s.geom, x, y) : hitBlocks(s.blocks, x, y);
  }
  /** Does not ask for a frame: callers outside one call `redraw`. */
  function setHover(i: number) {
    if (i !== hover) dirty = true;
    hover = i;
    const s = scene;
    const at = i >= 0 ? s?.shapes[i] : undefined;
    onhover(
      at && s
        ? {
            name: at.node.name,
            path: pathOf(at.trail),
            size: at.node.size,
            share: share(s, at.node),
            items: at.node.items,
            modified: at.node.modified,
          }
        : null,
    );
  }
  function move(e: PointerEvent) {
    moved = { x: e.offsetX, y: e.offsetY };
    schedule();
  }
  function leave() {
    moved = pointer = null;
    setHover(-1);
    schedule();
  }
  function click(e: MouseEvent) {
    const i = hit(e.offsetX, e.offsetY);
    if (i === CENTRE) return onup();
    const s = scene?.shapes[i];
    const path = s && shapePath(s);
    if (!s || !path) return;
    if (s.node.kind === "folder") ondrill(path[path.length - 1]);
    else onselect(path);
  }
  function contextMenu(e: MouseEvent) {
    e.preventDefault();
    // The menu key sends the event to the chart itself: use the selection.
    const keyboard = e.target !== canvas;
    const i = keyboard ? (marked.exact ? marked.index : -1) : hit(e.offsetX, e.offsetY);
    const s = scene?.shapes[i];
    const path = s && shapePath(s);
    if (!s || !path || (s.node.kind !== "folder" && s.node.kind !== "file")) return;
    onselect(path);
    const box = wrap!.getBoundingClientRect();
    onmenu(path, keyboard ? box.left + box.width / 2 : e.clientX, keyboard ? box.top + box.height / 2 : e.clientY);
  }
  // Wheel forward goes into the folder under the pointer, wheel back goes up one.
  let lastWheel = 0;
  function wheel(e: WheelEvent) {
    const now = Date.now();
    if (now - lastWheel < 300) return;
    // Under the pointer now, even if its frame has not run yet.
    const node = zoom ? undefined : scene?.shapes[hit(e.offsetX, e.offsetY)]?.node;
    if (e.deltaY < 0 && node?.kind === "folder" && node.id !== null) {
      lastWheel = now;
      ondrill(node.id);
    } else if (e.deltaY > 0) {
      lastWheel = now;
      onup();
    }
  }
  const ARROWS: Record<string, Step> = { ArrowLeft: "prev", ArrowRight: "next", ArrowDown: "in", ArrowUp: "out" };
  function key(e: KeyboardEvent) {
    // Anything not handled here (Space, F5, Delete, Esc, Tab…) goes on to the view.
    if (!scene || e.ctrlKey || e.altKey || e.metaKey) return;
    const s = scene.shapes[marked.index];
    if (ARROWS[e.key]) {
      const next = scene.shapes[stepShape(scene.shapes, marked.index, ARROWS[e.key])];
      const path = next && shapePath(next);
      if (path) onselect(path);
    } else if (e.key === "Enter" && marked.exact && s?.node.kind === "folder" && s.node.id !== null) {
      if (!e.repeat) ondrill(s.node.id);
    } else if (e.key === "Backspace") {
      onup();
    } else return;
    e.preventDefault();
    e.stopPropagation();
  }
  /** Buttons above the chart keep Enter for themselves. */
  const keepEnter = (e: KeyboardEvent) => {
    if (e.key === "Enter") e.stopPropagation();
  };

  function onVisibility() {
    if (visible()) return invalidate();
    // Nothing runs while hidden.
    cancelAnimationFrame(raf);
    raf = 0;
    zoom = null;
    moved = null;
  }

  onMount(() => {
    const root = getComputedStyle(document.documentElement);
    fonts = {
      font: root.getPropertyValue("--font-ui").trim() || fonts.font,
      code: root.getPropertyValue("--font-code").trim() || fonts.code,
    };
    invalidate();
    if (big) wrap?.focus();
  });
  onDestroy(() => {
    cancelAnimationFrame(raf);
    onhover(null);
  });

  const tip = $derived.by(() => {
    if (!hovered || !pointer || scene?.mode !== "treemap") return null;
    return {
      left: Math.max(0, Math.min(pointer.x + 14, w - 220)),
      top: pointer.y + 130 > h ? Math.max(0, pointer.y - 110) : pointer.y + 16,
    };
  });
</script>

<svelte:document onvisibilitychange={onVisibility} />

<div class="chart" class:big>
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div class="tools" role="toolbar" tabindex="-1" aria-label="Chart options" onkeydown={keepEnter}>
    <div class="seg" role="group" aria-label="Colour by">
      <span class="by">Colour</span>
      {#each COLOUR_MODES as m}
        <button class:on={colours === m.id} aria-pressed={colours === m.id} onclick={() => oncolours(m.id)}>{m.label}</button>
      {/each}
    </div>
    {#if mode === "treemap"}
      <input
        type="search"
        placeholder="Filter by name"
        aria-label="Filter the treemap by name"
        bind:value={query}
        onkeydown={(e) => {
          if (e.key !== "Escape") return;
          e.stopPropagation();
          if (query) query = "";
          else e.currentTarget.blur();
        }}
      />
      {#if filtered}<span class="found" role="status">{filtered.count.toLocaleString()} match{filtered.count === 1 ? "" : "es"}</span>{/if}
    {/if}
  </div>
  {#if mode === "ring"}
    <p class="trail" title={data ? pathOf(hovered?.trail ?? []) : ""}>{data ? pathOf(hovered?.trail ?? []) : ""}</p>
  {:else}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <nav class="trail" aria-label="Chart path" onkeydown={keepEnter}>
      {#each data?.crumbs ?? [] as c, i}
        {#if i > 0}<span aria-hidden="true">›</span>{/if}
        <button disabled={i === (data?.crumbs.length ?? 0) - 1} onclick={() => ondrill(c.id)}>{c.name}</button>
      {/each}
    </nav>
  {/if}
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
  <div
    class="stage"
    bind:this={wrap}
    bind:clientWidth={w}
    bind:clientHeight={h}
    role="application"
    tabindex="0"
    aria-label={summary}
    onkeydown={key}
    oncontextmenu={contextMenu}
  >
    <canvas bind:this={base} aria-hidden="true"></canvas>
    <canvas bind:this={dimmed} aria-hidden="true" style="visibility: hidden"></canvas>
    <canvas
      bind:this={canvas}
      aria-hidden="true"
      class:point={hovered ? hovered.node.id !== null : hover === CENTRE && (data?.crumbs.length ?? 0) > 1}
      onpointermove={move}
      onpointerleave={leave}
      onclick={click}
      onwheel={wheel}
    ></canvas>
    {#if empty}<p class="none">{empty}</p>{/if}
    {#if tip && hovered && scene}
      <div class="tip" style:left={`${tip.left}px`} style:top={`${tip.top}px`}>
        <strong>{hovered.node.name}</strong>
        <span>{human(hovered.node.size)} · {percent(share(scene, hovered.node))}</span>
        <span>{count(hovered.node.items)}</span>
        <span>Modified {date(hovered.node.modified)}</span>
      </div>
    {/if}
  </div>
  <span class="sr" aria-live="polite">{spoken}</span>
  {#if keys.length}
    <ul class="legend" aria-label="Colour legend">
      {#each keys as k}<li><i style:background={k.colour}></i>{k.label}{k.size ? ` ${human(k.size)}` : ""}</li>{/each}
    </ul>
  {/if}
</div>

<style>
  .chart {
    display: flex;
    flex-direction: column;
    gap: 6px;
    flex: none;
    min-width: 0;
  }
  .chart.big {
    flex: 1;
    min-height: 0;
  }
  .tools {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }
  .seg {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .by,
  .found {
    color: var(--muted);
    font-size: 12px;
  }
  .by {
    margin-right: 4px;
  }
  .seg button {
    all: unset;
    box-sizing: border-box;
    cursor: pointer;
    padding: 3px 10px;
    border: 1px solid var(--glass-border);
    background: var(--field-bg);
    color: var(--text-soft);
    font-size: 12px;
  }
  .seg button:first-of-type {
    border-radius: 999px 0 0 999px;
  }
  .seg button:last-of-type {
    border-radius: 0 999px 999px 0;
  }
  .seg button:hover {
    color: var(--text);
    border-color: var(--hover-border);
  }
  .seg button.on {
    background: var(--accent-bg);
    border-color: var(--accent-bg);
    color: var(--accent-on);
    font-weight: 600;
  }
  .seg button:focus-visible,
  .trail button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  input {
    flex: 1;
    min-width: 90px;
    box-sizing: border-box;
    padding: 3px 10px;
    border-radius: 999px;
    border: 1px solid var(--glass-border);
    background: var(--field-bg);
    color: var(--text);
    font: inherit;
    font-size: 12px;
  }
  .trail {
    margin: 0;
    height: 18px;
    line-height: 18px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text-soft);
    font-family: var(--font-code);
    font-size: 12px;
  }
  nav.trail {
    display: flex;
    gap: 4px;
    height: auto;
    overflow-x: auto;
    padding: 2px;
    flex: none;
  }
  .trail button {
    all: unset;
    cursor: pointer;
    color: var(--accent);
    flex: none;
  }
  .trail button:disabled {
    cursor: default;
    color: var(--text-soft);
  }
  .stage {
    position: relative;
    width: 100%;
    aspect-ratio: 1;
    min-width: 0;
    border-radius: 8px;
  }
  .big .stage {
    aspect-ratio: auto;
    flex: 1;
    min-height: 0;
  }
  .stage:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  /* Out of the flow, so the canvas never changes the size it is measured from. */
  canvas {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    display: block;
  }
  canvas.point {
    cursor: pointer;
  }
  .none {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 8px;
    margin: 0;
    text-align: center;
    pointer-events: none;
    color: var(--muted);
    font-size: 13px;
  }
  .tip {
    position: absolute;
    z-index: 1;
    pointer-events: none;
    box-sizing: border-box;
    max-width: 220px;
    display: flex;
    flex-direction: column;
    padding: 6px 10px;
    border-radius: 8px;
    border: 1px solid var(--border-strong);
    background: var(--solid-bg);
    color: var(--text-soft);
    font-size: 12px;
  }
  .tip strong {
    color: var(--text);
    overflow-wrap: anywhere;
  }
  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
    margin: 0;
    padding: 0;
    list-style: none;
    color: var(--text-soft);
    font-size: 12px;
  }
  .legend li {
    display: flex;
    align-items: center;
    gap: 5px;
  }
  .legend i {
    width: 10px;
    height: 10px;
    border-radius: 3px;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
