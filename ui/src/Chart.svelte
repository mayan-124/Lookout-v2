<script lang="ts">
  import { ringArcs, treemapBlocks, VIEW, type CNode } from "./chartlayout";

  let {
    data,
    mode,
    title,
    highlightId,
    ondrill,
    onup,
    onselect,
  }: {
    data: CNode | null;
    mode: "ring" | "treemap";
    title: string;
    highlightId: number | null;
    ondrill: (id: number) => void;
    onup: () => void;
    onselect: (id: number) => void;
  } = $props();

  let hover: { node: CNode; x: number; y: number } | null = $state(null);
  let box: HTMLElement | undefined = $state();

  const arcs = $derived(data && mode === "ring" ? ringArcs(data) : []);
  const blocks = $derived(data && mode === "treemap" ? treemapBlocks(data) : []);

  function human(n: number): string {
    const u = ["B", "KB", "MB", "GB", "TB"];
    let i = 0;
    let v = n;
    while (v >= 1024 && i < u.length - 1) {
      v /= 1024;
      i++;
    }
    return i === 0 ? `${n} B` : `${v.toFixed(v >= 100 ? 0 : 1)} ${u[i]}`;
  }
  const pct = (n: CNode) => (data && data.size > 0 ? (n.size / data.size) * 100 : 0);
  const label = (n: CNode) => `${n.name}, ${human(n.size)}, ${pct(n).toFixed(1)}% of ${title}`;

  function move(e: MouseEvent, node: CNode) {
    const r = box?.getBoundingClientRect();
    if (r) hover = { node, x: e.clientX - r.left, y: e.clientY - r.top };
  }
  function pick(node: CNode) {
    if (node.id === null) return;
    if (node.kind === "folder") ondrill(node.id);
    else onselect(node.id);
  }
  function key(e: KeyboardEvent, node: CNode) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      e.stopPropagation();
      pick(node);
    }
  }
  const fits = (b: { w: number; h: number }, name: string) => b.w > 10 + name.length * 8.2 && b.h > 22;
</script>

<div class="chart" bind:this={box} role="group" aria-label="Chart of {title}">
  {#if data}
    <svg viewBox="0 0 {VIEW} {VIEW}" role="img" aria-label="{mode === 'ring' ? 'Ring chart' : 'Treemap'} of {title}">
      {#if mode === "ring"}
        {#each arcs as a}
          <path
            d={a.path}
            class="seg b{a.branch % 8} k-{a.node.kind}"
            class:sel={a.node.id !== null && a.node.id === highlightId}
            style:--depth={a.depth}
            tabindex="0"
            role="button"
            aria-label={label(a.node)}
            onmousemove={(e) => move(e, a.node)}
            onmouseleave={() => (hover = null)}
            onclick={() => pick(a.node)}
            onkeydown={(e) => key(e, a.node)}
          />
        {/each}
        <g
          class="centre"
          role="button"
          tabindex="0"
          aria-label="Up one folder. Now showing {title}, {human(data.size)}"
          onclick={onup}
          onkeydown={(e) => {
            if (e.key === "Enter") onup();
          }}
        >
          <circle cx={VIEW / 2} cy={VIEW / 2} r="58" />
          <text x={VIEW / 2} y={VIEW / 2 - 4} text-anchor="middle" class="ct">{title.length > 11 ? title.slice(0, 10) + "…" : title}</text>
          <text x={VIEW / 2} y={VIEW / 2 + 14} text-anchor="middle" class="cs">{human(data.size)}</text>
        </g>
      {:else}
        {#each blocks as b}
          <g
            class="blk"
            tabindex="0"
            role="button"
            aria-label={label(b.node)}
            onmousemove={(e) => move(e, b.node)}
            onmouseleave={() => (hover = null)}
            onclick={() => pick(b.node)}
            onkeydown={(e) => key(e, b.node)}
          >
            <rect
              x={b.x + 0.5}
              y={b.y + 0.5}
              width={Math.max(0, b.w - 1)}
              height={Math.max(0, b.h - 1)}
              class="seg b{b.branch % 8} k-{b.node.kind}"
              class:inner={b.depth === 1}
              class:sel={b.node.id !== null && b.node.id === highlightId}
            />
            {#if b.depth === 0 && fits(b, b.node.name)}
              <text x={b.x + 5} y={b.y + 17} class="bt">{b.node.name}</text>
            {/if}
          </g>
        {/each}
      {/if}
    </svg>
    {#if hover}
      <div class="tip" style:left={`${Math.min(hover.x + 12, (box?.clientWidth ?? 300) - 170)}px`} style:top={`${hover.y + 12}px`}>
        <strong>{hover.node.name}</strong>
        <span>{human(hover.node.size)} · {pct(hover.node).toFixed(1)}%</span>
      </div>
    {/if}
  {:else}
    <p class="none">No chart for this folder.</p>
  {/if}
</div>

<style>
  /* Segment colors come from --c0..--c7 in app.css (color-blind-safe, none near the amber accent). */
  .chart {
    position: relative;
    min-width: 0;
    min-height: 0;
    flex: 1;
  }
  svg {
    width: 100%;
    height: 100%;
    display: block;
  }
  .seg {
    stroke: var(--bg);
    stroke-width: 1;
    cursor: pointer;
    /* Deeper rings are lighter, but never so light that they blend into the page. */
    fill: color-mix(in srgb, var(--c) calc(100% - var(--depth, 0) * 9%), var(--panel));
  }
  .seg.inner {
    fill: color-mix(in srgb, var(--c) 78%, var(--panel));
  }
  .chart :global(.b0) { --c: var(--c0); }
  .chart :global(.b1) { --c: var(--c1); }
  .chart :global(.b2) { --c: var(--c2); }
  .chart :global(.b3) { --c: var(--c3); }
  .chart :global(.b4) { --c: var(--c4); }
  .chart :global(.b5) { --c: var(--c5); }
  .chart :global(.b6) { --c: var(--c6); }
  .chart :global(.b7) { --c: var(--c7); }
  .chart :global(.k-other),
  .chart :global(.k-unlisted) {
    --c: var(--c-grey);
    cursor: default;
  }
  .chart :global(.k-unlisted) {
    stroke-dasharray: 3 2;
  }
  .seg:hover {
    stroke: var(--text);
    stroke-width: 2;
  }
  .seg:focus-visible {
    stroke: var(--accent);
    stroke-width: 3;
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .seg.sel {
    stroke: var(--text);
    stroke-width: 3;
  }
  .centre {
    cursor: pointer;
  }
  .centre circle {
    fill: var(--bg);
    stroke: var(--border);
  }
  .centre:focus-visible {
    outline: none;
  }
  .centre:focus-visible circle {
    stroke: var(--accent);
    stroke-width: 3;
  }
  .ct {
    fill: var(--text);
    font-size: 16px;
    font-weight: 600;
  }
  .cs {
    fill: var(--text-soft);
    font-family: var(--font-code);
    font-size: 14px;
  }
  .bt {
    fill: var(--chart-label);
    font-size: 14px;
    font-weight: 600;
    pointer-events: none;
    paint-order: stroke;
    stroke: var(--chart-halo);
    stroke-width: 3px;
  }
  .tip {
    position: absolute;
    pointer-events: none;
    background: var(--raised);
    color: var(--text);
    border: 1px solid var(--border-strong);
    border-radius: 8px;
    padding: 6px 10px;
    font-size: 13px;
    display: flex;
    flex-direction: column;
    max-width: 220px;
  }
  .tip strong {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .none {
    color: var(--muted);
    font-size: 13px;
  }
</style>
