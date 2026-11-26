<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { nextRequest, pv } from "./previewui.svelte";
  import { plain } from "./errors";

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
    kind: "text" | "image" | "folder" | "archive" | "other";
    state: "ok" | "hidden" | "skipped" | "online_only" | "too_large" | "missing" | "denied" | "locked" | "error";
    message: string;
    text: string | null;
    truncated: boolean;
    image: string | null;
    items: number | null;
    entries: { name: string; size: number }[] | null;
    thumbnail: boolean;
  }
  // A picture from the Windows shell: a thumbnail, else the file type's icon.
  interface Shot {
    image: string;
    icon: boolean;
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
          err = plain(e);
        }
      } finally {
        if (mine === seq) loading = false;
      }
    }, 120);
    return () => clearTimeout(t);
  });

  let shot = $state<{ path: string; shot: Shot } | null>(null);
  // Asked for once the file's preview is known; a larger one for the overlay.
  // Moving on cancels the request, and a failure leaves the plain preview.
  $effect(() => {
    const d = data;
    const src: Source | null = key ? JSON.parse(key) : null;
    const size = pv.overlay ? 1024 : 512;
    if (!d?.thumbnail || !src || loading) return;
    const request = nextRequest();
    let live = true;
    invoke<Shot | null>("preview_thumbnail", { node: src.node ?? null, hit: src.hit ?? null, path: src.path ?? null, size, request })
      .then((r) => {
        if (live && r) shot = { path: d.path, shot: r };
      })
      .catch(() => {});
    return () => {
      live = false;
      invoke("cancel_thumbnail", { request }).catch(() => {});
    };
  });
  const picture = $derived(data?.thumbnail && shot?.path === data.path ? shot.shot : null);

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
  // The same line icons as the result rows.
  const FILE_ICON = "M4 2h5l3 3v9H4zM9 2v3h3";
  const ICON: Record<string, string> = {
    folder: "M2 4.5A1.5 1.5 0 0 1 3.5 3h2.6l1.4 1.5h5A1.5 1.5 0 0 1 14 6v5.5a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 11.5z",
    image: "M2 3h12v10H2zM2.5 12l3.6-3.4 2.6 2.4 2-1.8 3.3 2.8",
  };
  // What the backend says about a file it does not show, as a sentence with a next step.
  const NEXT: Record<string, string> = {
    too_large: "Open it to see it.",
    missing: "Search again.",
    locked: "Close that program and select the file again.",
    denied: "Open its folder to check access.",
    error: "Try opening it instead.",
  };
  const said = (d: PV) => [plain(d.message), NEXT[d.state]].filter(Boolean).join(" ");

  // The large preview takes the focus and gives it back when it closes.
  function takeFocus(button: HTMLButtonElement) {
    const before = document.activeElement as HTMLElement | null;
    button.focus();
    return {
      destroy() {
        // Not when the focus has already moved on, as when the bar is reopened.
        if (document.activeElement === button || document.activeElement === document.body) before?.focus();
      },
    };
  }

  function onKey(e: KeyboardEvent) {
    // Two panels can be mounted (the expanded chart has its own): one Space, one toggle.
    if (e.key !== " " || !source || e.repeat || e.defaultPrevented) return;
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
      <svg class="ic" viewBox="0 0 16 16" width="24" height="24" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round" stroke-linecap="round" aria-hidden="true"><path d={ICON[data.kind] ?? FILE_ICON} /></svg>
      <div class="meta">
        <strong class="nm">{data.name}</strong>
        <span>{data.kind === "folder" && data.items !== null ? `${data.items.toLocaleString()} items · ` : ""}{human(data.size)}{data.modified ? ` · modified ${new Date(data.modified * 1000).toLocaleString()}` : ""}</span>
        <span class="path">{data.path}</span>
      </div>
    </div>
    {#if data.state !== "ok"}
      <p class="note" role="status">{said(data)}</p>
    {:else if data.kind === "image" && data.image}
      {#if badImage}<p class="note">This image cannot be shown here. Open it to see it.</p>{:else}
        <img class:large src={data.image} alt={data.name} onerror={() => (badImage = true)} />
      {/if}
    {:else if data.kind === "text" && data.text !== null}
      <pre class:large>{data.text}</pre>
      {#if data.truncated}<p class="note">Only the first 64 KB is shown. Open the file to see the rest.</p>{/if}
    {:else if data.kind === "archive" && data.entries}
      {#if picture && !picture.icon}<img class:large src={picture.image} alt={data.name} />{/if}
      <ul class="zip" class:large aria-label="Files in this zip">
        {#each data.entries as e, n (n)}
          <li><span class="en">{e.name}</span><span>{e.name.endsWith("/") ? "" : human(e.size)}</span></li>
        {/each}
      </ul>
      {#if (data.items ?? 0) > data.entries.length}
        <p class="note">The first {data.entries.length} of {data.items?.toLocaleString()} entries.</p>
      {/if}
    {:else if data.kind === "other"}
      {#if picture}
        <img class:large class:fileicon={picture.icon} src={picture.image} alt={picture.icon ? "" : data.name} />
      {/if}
      {#if !picture || picture.icon}<p class="note">No preview for this type of file. Open it to see it.</p>{/if}
    {/if}
  {:else if loading}
    <p class="note">Loading…</p>
  {/if}
{/snippet}

{#if source}
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <aside class="panel" class:embedded tabindex="0" aria-label="File preview" aria-busy={loading}>
    {@render body(false)}
    {#if data}<p class="hintline">Click or Tab here, then <kbd>Space</kbd> for a larger preview</p>{/if}
  </aside>
  {#if pv.overlay}
    <div class="scrim" role="presentation" onclick={() => (pv.overlay = false)}>
      <div class="big" role="dialog" aria-label="Large preview" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={() => {}}>
        <button class="x" use:takeFocus onclick={() => (pv.overlay = false)}>Close <kbd>Esc</kbd></button>
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
    flex: none;
    color: var(--text-soft);
  }
  .panel:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
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
  img.fileicon {
    width: 64px;
    height: 64px;
  }
  .zip {
    margin: 0;
    padding: 6px 8px;
    list-style: none;
    max-height: 200px;
    overflow: auto;
    flex: none;
    font-family: var(--font-code);
    font-size: 12px;
    background: var(--code-bg);
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  .zip.large {
    max-height: 66vh;
    font-size: 14px;
  }
  .zip li {
    display: flex;
    justify-content: space-between;
    gap: 12px;
  }
  .en {
    overflow-wrap: anywhere;
    min-width: 0;
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
