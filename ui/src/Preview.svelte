<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { pv } from "./previewui.svelte";

  export interface Source {
    node?: number;
    hit?: number;
    path?: string;
  }
  interface PV {
    name: string;
    path: string;
    is_dir: boolean;
    size: number;
    modified: number;
    kind: "text" | "image" | "folder" | "other";
    state: "ok" | "hidden" | "online_only" | "too_large" | "missing" | "denied" | "locked" | "error";
    message: string;
    text: string | null;
    truncated: boolean;
    image: string | null;
    items: number | null;
  }

  let { source, embedded = false }: { source: Source | null; embedded?: boolean } = $props();

  let data = $state<PV | null>(null);
  let loading = $state(false);
  let err = $state("");
  let badImage = $state(false);
  let seq = 0;
  const key = $derived(source ? JSON.stringify(source) : "");

  // Reads on a background task. A newer selection makes the older answer be dropped,
  // and a short wait keeps fast arrow-key scrolling from reading every file it passes.
  $effect(() => {
    const k = key;
    const src: Source | null = k ? JSON.parse(k) : null;
    const mine = ++seq;
    badImage = false;
    if (!src) {
      data = null;
      loading = false;
      err = "";
      return;
    }
    loading = true;
    const t = setTimeout(async () => {
      try {
        const r = await invoke<PV>("preview_file", { node: src.node ?? null, hit: src.hit ?? null, path: src.path ?? null });
        if (mine === seq) {
          data = r;
          err = "";
        }
      } catch (e) {
        if (mine === seq) {
          data = null;
          err = typeof e === "string" ? e : String(e);
        }
      } finally {
        if (mine === seq) loading = false;
      }
    }, 120);
    return () => clearTimeout(t);
  });

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
  const icon = (d: PV) => (d.kind === "folder" ? "📁" : d.kind === "image" ? "🖼" : d.kind === "text" ? "📄" : "▫");

  function onKey(e: KeyboardEvent) {
    if (e.key !== " " || !source || e.repeat) return;
    const t = e.target as HTMLElement | null;
    const tag = t?.tagName;
    if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA" || tag === "BUTTON" || t?.isContentEditable) return;
    e.preventDefault();
    pv.overlay = !pv.overlay;
  }
</script>

<svelte:window onkeydown={onKey} />

{#snippet body(large: boolean)}
  {#if err}
    <p class="note">{err}</p>
  {:else if data}
    <div class="info">
      <span class="ic" aria-hidden="true">{icon(data)}</span>
      <div class="meta">
        <strong class="nm">{data.name}</strong>
        <span>{data.kind === "folder" && data.items !== null ? `${data.items.toLocaleString()} items · ` : ""}{human(data.size)}{data.modified ? ` · modified ${new Date(data.modified * 1000).toLocaleString()}` : ""}</span>
        <span class="path">{data.path}</span>
      </div>
    </div>
    {#if data.state !== "ok"}
      <p class="note" role="status">{data.message}</p>
    {:else if data.kind === "image" && data.image}
      {#if badImage}<p class="note">This image can't be displayed.</p>{:else}
        <img class:large src={data.image} alt={data.name} onerror={() => (badImage = true)} />
      {/if}
    {:else if data.kind === "text" && data.text !== null}
      <pre class:large>{data.text}</pre>
      {#if data.truncated}<p class="note">Truncated: only the first 64 KB is shown.</p>{/if}
    {:else if data.kind === "other"}
      <p class="note">No preview for this type of file.</p>
    {/if}
  {:else if loading}
    <p class="note">Loading…</p>
  {/if}
{/snippet}

{#if source}
  <aside class="panel" class:embedded aria-label="File preview" aria-busy={loading}>
    {@render body(false)}
    {#if data}<p class="hintline"><kbd>Space</kbd> larger preview</p>{/if}
  </aside>
  {#if pv.overlay}
    <div class="scrim" role="presentation" onclick={() => (pv.overlay = false)}>
      <div class="big" role="dialog" aria-label="Large preview" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={() => {}}>
        <button class="x" onclick={() => (pv.overlay = false)}>Close <kbd>Esc</kbd></button>
        {@render body(true)}
      </div>
    </div>
  {/if}
{/if}

<style>
  .panel {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-height: 0;
    overflow: auto;
    border-top: 1px solid var(--border);
    padding-top: 8px;
    max-height: 48%;
  }
  .panel.embedded {
    border-top: none;
    padding-top: 0;
    max-height: none;
    flex: 1;
  }
  .info {
    display: flex;
    gap: 10px;
    align-items: flex-start;
  }
  .ic {
    font-size: 26px;
    line-height: 1;
  }
  .meta {
    display: flex;
    flex-direction: column;
    min-width: 0;
    font-size: 13px;
    color: var(--text-soft);
  }
  .nm {
    color: var(--text);
    overflow-wrap: anywhere;
  }
  .path {
    font-family: var(--font-code);
    font-size: 12px;
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  .note,
  .hintline {
    margin: 0;
    color: var(--muted);
    font-size: 12px;
  }
  img {
    max-width: 100%;
    max-height: 220px;
    object-fit: contain;
    align-self: flex-start;
  }
  img.large {
    max-height: 72vh;
  }
  pre {
    margin: 0;
    max-height: 200px;
    overflow: auto;
    font-family: var(--font-code);
    font-size: 12px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    background: var(--code-bg);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 8px;
  }
  pre.large {
    max-height: 66vh;
    font-size: 14px;
  }
  .scrim {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 30;
  }
  .big {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 10px;
    width: min(92vw, 1100px);
    max-height: 92vh;
    overflow: auto;
    background: var(--panel);
    border: 1px solid var(--border-strong);
    border-radius: 10px;
    padding: 16px 18px;
  }
  .x {
    align-self: flex-end;
    all: unset;
    cursor: pointer;
    color: var(--text-soft);
    font-size: 13px;
    margin-left: auto;
  }
  .x:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
