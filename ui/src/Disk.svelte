<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onDestroy, onMount, tick } from "svelte";
  import Chart from "./Chart.svelte";
  import Cleanup from "./Cleanup.svelte";
  import Duplicates from "./Duplicates.svelte";
  import Preview, { type Source } from "./Preview.svelte";
  import { pv } from "./previewui.svelte";
  import type { CNode } from "./chartlayout";

  interface Location {
    label: string;
    path: string;
  }
  interface Progress {
    state: "idle" | "scanning" | "done" | "cancelled" | "failed";
    files: number;
    folders: number;
    bytes: number;
    unreadable: number;
    paused: boolean;
    current: string;
    message: string;
    root: string;
    saved_at: number | null;
  }
  interface Row {
    id: number;
    name: string;
    kind: "folder" | "file" | "others";
    size_disk: number;
    logical: number;
    items: number;
    modified: number;
    percent: number;
    unreadable: boolean;
    link: boolean;
    limited: boolean;
    partial: boolean;
    has_children: boolean;
  }
  interface Stats {
    files: number;
    folders: number;
    unreadable: number;
    folded_folders: number;
    links_skipped: number;
    cloud_folders: number;
    cloud_files: number;
    hard_links: number;
    hard_link_bytes: number;
    volume_used: number;
  }
  interface FileRow {
    idx: number;
    name: string;
    folder: string;
    size_disk: number;
    logical: number;
    modified: number;
  }
  type Tab = "tree" | "largest" | "old";
  interface View {
    node: number;
    crumbs: { id: number; name: string }[];
    rows: Row[];
    total_disk: number;
    total_logical: number;
    items: number;
    stats: Stats;
    scanned_at: number;
  }
  interface Prepared {
    ticket: string;
    action_id: string;
    label: string;
    risk: "safe" | "confirm" | "dangerous";
    preview: { from: string | null; title: string; lines: string[]; warnings: string[] };
  }
  interface Done {
    message: string;
  }
  interface Flat {
    row: Row;
    depth: number;
  }

  let { onclose }: { onclose: () => void } = $props();

  let locations: Location[] = $state([]);
  let chosen = $state("");
  // Low impact: one thread held to the Balanced duty (the slower, gentler scan).
  let lowImpact = $state(false);
  // Whether Fast scan (NTFS file table, asks Windows for permission) works for the chosen place.
  let fast = $state<{ available: boolean; reason: string }>({ available: false, reason: "" });
  $effect(() => {
    const p = chosen;
    if (!p) {
      fast = { available: false, reason: "Choose a place first." };
      return;
    }
    invoke<{ available: boolean; reason: string }>("disk_fast_info", { path: p })
      .then((i) => (fast = i))
      .catch(() => (fast = { available: false, reason: "Fast scan is not available." }));
  });
  let tab = $state<Tab>("tree");
  // Which page of the disk view: the scan, the clean-up list or the duplicate finder.
  let page = $state<"disk" | "cleanup" | "dupes">("disk");
  let chartMode = $state<"ring" | "treemap">("ring");
  let chart = $state<CNode | null>(null);
  let minMb = $state(100);
  let olderDays = $state(365);
  let files = $state<{ rows: FileRow[]; note: string }>({ rows: [], note: "" });
  let fileSel = $state(0);
  let removedHits = $state(new Set<number>());
  let progress = $state<Progress | null>(null);
  let view = $state<View | null>(null);
  let focus = $state(0);
  let kids = $state(new Map<number, Row[]>());
  let expanded = $state(new Set<number>());
  let removed = $state(new Set<number>());
  let sort = $state("size");
  let descending = $state(true);
  let selected = $state(-1);
  let notice = $state("");
  let saved: { root: string; saved_at: number } | null = $state(null);
  let menu: { x: number; y: number } | null = $state(null);
  let card: { prepared: Prepared; node: number; hit: number; presented: boolean; busy: boolean; error: string } | null =
    $state(null);
  let confirmButton: HTMLButtonElement | undefined = $state();
  let cancelButton: HTMLButtonElement | undefined = $state();
  let listEl: HTMLElement | undefined = $state();
  let timer: ReturnType<typeof setInterval> | undefined;
  // Per-chart expand: one pane at a time grows in place over the content area.
  let exp = $state(false);
  let chartW = $state(330);
  const basePath = $derived.by(() => {
    const names = (view?.crumbs ?? []).map((c) => c.name);
    return names.reduce((a, n) => (a ? a.replace(/\\$/, "") + "\\" + n : n), "");
  });
  const previewSource = $derived.by<Source | null>(() => {
    if (!finished) return null;
    if (tab === "tree") {
      const r = current();
      return r && r.kind !== "others" ? { node: r.id } : null;
    }
    const f = shownFiles[fileSel];
    return f ? { hit: f.idx } : null;
  });

  const setExp = (v: boolean) => (exp = v);
  // Drag the divider between the tree and the chart.
  function dragSplit(e: PointerEvent) {
    e.preventDefault();
    const startX = e.clientX;
    const start = chartW;
    const move = (m: PointerEvent) => {
      chartW = Math.min(window.innerWidth - 360, Math.max(240, start - (m.clientX - startX)));
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }

  const scanning = $derived(progress?.state === "scanning");
  const finished = $derived(progress?.state === "done" && view !== null);
  const errorText = (e: unknown) => (typeof e === "string" ? e : String(e));

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
  const when = (s: number) => (s ? new Date(s * 1000).toLocaleDateString() : "");

  const flat = $derived.by<Flat[]>(() => {
    const out: Flat[] = [];
    const walk = (id: number, depth: number) => {
      for (const row of kids.get(id) ?? []) {
        if (removed.has(row.id)) continue;
        out.push({ row, depth });
        if (row.kind === "folder" && expanded.has(row.id)) walk(row.id, depth + 1);
      }
    };
    walk(focus, 0);
    return out;
  });

  async function load(node: number) {
    const v = await invoke<View>("disk_view", { node, sort, descending });
    kids.set(node, v.rows);
    kids = new Map(kids);
    return v;
  }

  async function reloadAll() {
    const ids = [focus, ...expanded];
    kids = new Map();
    for (const id of ids) {
      try {
        await load(id);
      } catch {
        expanded.delete(id);
      }
    }
    expanded = new Set(expanded);
  }

  async function show(node: number) {
    focus = node;
    expanded = new Set();
    kids = new Map();
    selected = -1;
    view = await load(node);
    selected = view.rows.length ? 0 : -1;
    loadChart();
  }

  async function loadChart() {
    try {
      chart = await invoke<CNode>("disk_chart", { node: focus, levels: chartMode === "ring" ? 4 : 2 });
    } catch {
      chart = null;
    }
  }

  async function loadFiles() {
    if (tab === "tree") return;
    try {
      files = await invoke("disk_files", { kind: tab, minMb, olderDays });
      fileSel = files.rows.length ? 0 : -1;
    } catch (e) {
      notice = errorText(e);
    }
  }

  function setTab(t: Tab) {
    tab = t;
    menu = null;
    exp = false;
    loadFiles();
  }

  const shownFiles = $derived(files.rows.filter((f) => !removedHits.has(f.idx)));
  const crumbTitle = $derived.by(() => {
    const c = view?.crumbs[view.crumbs.length - 1]?.name ?? "";
    return c.split("\\").filter(Boolean).pop() ?? c;
  });
  // What the totals leave out, so they are not mistaken for the whole drive.
  const skipNote = $derived.by(() => {
    const s = view?.stats;
    if (!s) return "";
    const parts: string[] = [];
    if (s.unreadable) parts.push(`${s.unreadable.toLocaleString()} folder(s) can't be read without admin (Fast scan can read them)`);
    if (s.links_skipped) parts.push(`${s.links_skipped.toLocaleString()} shortcut folder(s) not followed`);
    if (s.cloud_folders) parts.push(`${s.cloud_folders.toLocaleString()} cloud-only folder(s) not opened`);
    if (s.cloud_files) parts.push(`${s.cloud_files.toLocaleString()} cloud-only file(s) take no space on this disk`);
    if (s.hard_links) parts.push(`${s.hard_links.toLocaleString()} file(s) that appear in more than one place are counted once (${human(s.hard_link_bytes)} is not counted twice)`);
    const note = "A file that appears in more than one place (a hard link) has its bytes under one path only, the first one found.";
    return parts.length
      ? `Totals cover what Lookout could read, not necessarily everything on the drive: ${parts.join("; ")}. ${note}`
      : note;
  });
  // Whole-drive scans: what Windows counts as used but the tree does not show.
  const systemLine = $derived.by(() => {
    const s = view?.stats;
    if (!s || view?.node !== 0 || !s.volume_used || s.volume_used <= view.total_disk) return "";
    const gap = human(s.volume_used - view.total_disk);
    if (s.unreadable)
      return `Not readable without admin: ${gap} (folders Lookout can't open, plus system files and metadata). Use Fast scan to include them.`;
    return `System files and metadata: ${gap} (not shown in the tree)`;
  });

  async function poll() {
    try {
      progress = await invoke<Progress | null>("disk_progress");
    } catch {
      return;
    }
    if (progress?.state === "done" && !view) {
      try {
        await show(0);
      } catch (e) {
        notice = errorText(e);
      }
    }
    if (progress && progress.state !== "scanning") stopTimer();
  }
  // The scan runs in the backend either way; the status poll runs only while
  // the window is visible, and catches up once when it is shown again.
  let polling = false;
  const clearTimer = () => {
    if (timer) clearInterval(timer);
    timer = undefined;
  };
  const stopTimer = () => {
    polling = false;
    clearTimer();
  };
  const startTimer = () => {
    clearTimer();
    polling = true;
    if (document.visibilityState === "visible") timer = setInterval(poll, 300);
  };
  function onVisibility() {
    if (!polling) return;
    if (document.visibilityState === "visible") {
      startTimer();
      poll();
    } else {
      clearTimer();
    }
  }

  async function scan(path: string, fastMode = false) {
    if (!path) return;
    notice = "";
    view = null;
    removed = new Set();
    try {
      if (fastMode) await invoke("disk_start_fast", { path });
      else await invoke("disk_start", { path, lowImpact });
    } catch (e) {
      notice = errorText(e);
      return;
    }
    chosen = path;
    startTimer();
    poll();
  }

  async function pickFolder() {
    try {
      const p = await invoke<string | null>("pick_folder");
      if (p) chosen = p;
    } catch (e) {
      notice = errorText(e);
    }
  }

  async function cancel() {
    await invoke("disk_cancel");
    startTimer();
  }

  async function save() {
    try {
      await invoke("disk_save");
      saved = await invoke("disk_saved_info");
      notice = "Saved (encrypted). “Delete my search data” removes it.";
    } catch (e) {
      notice = errorText(e);
    }
  }

  async function loadSaved() {
    try {
      await invoke("disk_load_saved");
      view = null;
      removed = new Set();
      poll();
    } catch (e) {
      notice = errorText(e);
    }
  }

  async function forgetSaved() {
    await invoke("disk_delete_saved");
    saved = null;
  }

  async function setSort(col: string) {
    if (sort === col) descending = !descending;
    else {
      sort = col;
      descending = col !== "name";
    }
    await reloadAll();
  }

  async function toggle(row: Row) {
    if (row.kind !== "folder" || !row.has_children) return;
    if (expanded.has(row.id)) expanded.delete(row.id);
    else {
      if (!kids.has(row.id)) await load(row.id);
      expanded.add(row.id);
    }
    expanded = new Set(expanded);
  }

  const current = () => (selected >= 0 ? flat[selected]?.row : undefined);

  // ------------------------------------------------------------ actions
  // Every action goes through the backend gate: prepare -> present -> run/confirm.
  const nextFrame = () => new Promise<void>((r) => requestAnimationFrame(() => r()));

  type Target = { node?: number; hit?: number };
  function currentTarget(): Target | undefined {
    if (tab === "tree") {
      const r = current();
      return r && r.kind !== "others" ? { node: r.id } : undefined;
    }
    const f = shownFiles[fileSel];
    return f ? { hit: f.idx } : undefined;
  }

  async function act(action: string, target: Target | undefined = currentTarget()) {
    if (!target || card) return;
    menu = null;
    notice = "";
    let prepared: Prepared;
    try {
      prepared =
        target.node !== undefined
          ? await invoke<Prepared>("disk_prepare", { action, node: target.node })
          : await invoke<Prepared>("disk_prepare_file", { action, hit: target.hit });
    } catch (e) {
      notice = errorText(e);
      return;
    }
    if (prepared.risk === "safe") {
      try {
        const d = await invoke<Done>("run_action", { ticket: prepared.ticket });
        notice = d.message;
      } catch (e) {
        notice = errorText(e);
      }
      return;
    }
    card = { prepared, node: target.node ?? -1, hit: target.hit ?? -1, presented: false, busy: false, error: "" };
    await tick();
    await nextFrame();
    try {
      await invoke("present_action", { ticket: prepared.ticket });
    } catch (e) {
      card = null;
      notice = errorText(e);
      return;
    }
    if (card && card.prepared.ticket === prepared.ticket) {
      card.presented = true;
      await tick();
      cancelButton?.focus(); // a stray Enter cannot confirm a deletion
    }
  }

  async function confirm(e: Event) {
    if (!e.isTrusted || !card || !card.presented || card.busy) return;
    const { ticket } = card.prepared;
    const { node, hit } = card;
    card.busy = true;
    try {
      const d = await invoke<Done>("confirm_action", { ticket });
      notice = d.message;
      if (card.prepared.action_id === "recycle") {
        if (node >= 0) {
          removed.add(node);
          removed = new Set(removed);
        }
        if (hit >= 0) {
          removedHits.add(hit);
          removedHits = new Set(removedHits);
        }
      }
      card = null;
    } catch (err) {
      const message = errorText(err);
      if (message.includes("too quickly") && card) {
        card.busy = false;
        card.error = message;
      } else {
        card = null;
        notice = message;
      }
    }
  }

  function dismissCard() {
    if (card) invoke("cancel_action", { ticket: card.prepared.ticket });
    card = null;
  }

  // ------------------------------------------------------------ keyboard
  async function up() {
    if (!view || focus === 0) return;
    const crumbs = view.crumbs;
    await show(crumbs.length >= 2 ? crumbs[crumbs.length - 2].id : 0);
  }

  function select(delta: number) {
    if (flat.length === 0) return;
    selected = Math.min(flat.length - 1, Math.max(0, selected + delta));
    tick().then(() => listEl?.querySelector<HTMLElement>(`[data-i="${selected}"]`)?.scrollIntoView({ block: "nearest" }));
  }

  function onKeydown(e: KeyboardEvent) {
    if (card) {
      if (e.key === "Escape") {
        e.preventDefault();
        dismissCard();
      }
      return;
    }
    const tag = (e.target as HTMLElement | null)?.tagName;
    if (tag === "SELECT" || tag === "INPUT") return;
    if (e.key === "Escape") {
      e.preventDefault();
      if (pv.overlay) pv.overlay = false;
      else if (menu) menu = null;
      else if (exp) exp = false;
      else if (scanning) cancel();
      else onclose();
      return;
    }
    if (!finished) return;
    if (e.ctrlKey && e.key.toLowerCase() === "e") {
      e.preventDefault();
      act("reveal");
      return;
    }
    if (e.ctrlKey && e.key.toLowerCase() === "c") {
      e.preventDefault();
      act("copy_path");
      return;
    }
    if (e.key === "Enter" && !e.ctrlKey) {
      e.preventDefault();
      if (!e.repeat) act("open");
      return;
    }
    if (e.key === "Delete") {
      e.preventDefault();
      act("recycle");
      return;
    }
    if (e.key === "F5") {
      e.preventDefault();
      scan(progress?.root || chosen);
      return;
    }
    if (tab !== "tree") {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        fileSel = Math.min(shownFiles.length - 1, Math.max(0, fileSel + (e.key === "ArrowDown" ? 1 : -1)));
        tick().then(() => document.querySelector(`[data-f="${fileSel}"]`)?.scrollIntoView({ block: "nearest" }));
      }
      return;
    }
    const row = current();
    if (e.ctrlKey && e.key === "Enter") {
      e.preventDefault();
      if (row?.kind === "folder") show(row.id);
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      select(1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      select(-1);
    } else if (e.key === "ArrowRight" && row) {
      e.preventDefault();
      if (row.kind === "folder" && !expanded.has(row.id)) toggle(row);
    } else if (e.key === "ArrowLeft" && row) {
      e.preventDefault();
      if (expanded.has(row.id)) toggle(row);
    } else if (e.key === "Backspace") {
      e.preventDefault();
      up();
    }
  }

  function openMenuAt(e: MouseEvent) {
    e.preventDefault();
    menu = { x: Math.min(e.clientX, window.innerWidth - 190), y: Math.min(e.clientY, window.innerHeight - 140) };
  }

  function openMenu(e: MouseEvent, i: number) {
    e.preventDefault();
    selected = i;
    menu = { x: Math.min(e.clientX, window.innerWidth - 190), y: Math.min(e.clientY, window.innerHeight - 140) };
  }

  onMount(async () => {
    try {
      locations = await invoke<Location[]>("disk_locations");
      chosen = locations[0]?.path ?? "";
      saved = await invoke("disk_saved_info");
      progress = await invoke<Progress | null>("disk_progress");
    } catch {
      /* the view still opens; Scan reports the error */
    }
  });

  // Closing the view frees the scan; nothing is kept unless it was saved.
  onDestroy(() => {
    stopTimer();
    invoke("disk_close").catch(() => {});
  });

  const arrow = (c: string) => (sort === c ? (descending ? " ▼" : " ▲") : "");
</script>

<svelte:window onkeydown={onKeydown} onclick={() => (menu = null)} />
<svelte:document onvisibilitychange={onVisibility} />

{#snippet expandBtn()}
  <button class="xbtn" type="button" data-no-drag aria-label="Expand chart" title="Expand chart" onclick={(e) => { e.stopPropagation(); exp = true; }}>
    <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M2 6V2h4M14 10v4h-4M10 2h4v4M6 14H2v-4" /></svg>
  </button>
{/snippet}

<section class="disk" aria-label="Disk usage">
  <header>
    <h2>Disk usage</h2>
    <span class="hint">Scans only when you press Scan. Closing this view frees the data.</span>
    <button class="btn" onclick={onclose}>Close <kbd>Esc</kbd></button>
  </header>

  <div class="tabs pages" role="tablist" aria-label="Disk tools">
    <button role="tab" aria-selected={page === "disk"} class:on={page === "disk"} onclick={() => (page = "disk")}>Disk usage</button>
    <button role="tab" aria-selected={page === "cleanup"} class:on={page === "cleanup"} onclick={() => (page = "cleanup")}>Clean up</button>
    <button role="tab" aria-selected={page === "dupes"} class:on={page === "dupes"} onclick={() => (page = "dupes")}>Duplicates</button>
  </div>

  {#if page === "disk"}

  <div class="picker">
    <select bind:value={chosen} disabled={scanning} aria-label="Location">
      {#each locations as l}<option value={l.path}>{l.label} — {l.path}</option>{/each}
      {#if chosen && !locations.some((l) => l.path === chosen)}<option value={chosen}>{chosen}</option>{/if}
    </select>
    <button class="btn" onclick={pickFolder} disabled={scanning}>Choose folder…</button>
    <span class="low" title="Slower, but lighter on your PC">
      <label class="switch">
        <input type="checkbox" role="switch" aria-label="Gentle" bind:checked={lowImpact} disabled={scanning} />
        <span class="track"></span>
      </label>
      Gentle
    </span>
    {#if scanning}
      <button class="btn" onclick={cancel}>Cancel</button>
    {:else}
      <button class="btn primary" onclick={() => scan(chosen)} disabled={!chosen}>
        {finished ? "Rescan" : "Scan"}{#if finished}<kbd>F5</kbd>{/if}
      </button>
      <button
        class="btn"
        onclick={() => scan(chosen, true)}
        disabled={!chosen || !fast.available}
        title={fast.available ? "Fast scan reads the drive's file table. Windows will ask for permission." : fast.reason}
      >Fast scan (admin)</button>
    {/if}
    {#if finished && !progress?.saved_at}<button class="btn" onclick={save}>Save</button>{/if}
    {#if saved && !scanning}
      <button class="btn" onclick={loadSaved} title={saved.root}>Open saved ({when(saved.saved_at)})</button>
      <button class="btn" onclick={forgetSaved}>Forget saved</button>
    {/if}
  </div>

  <p class="status">
    {#if fast.available || !chosen}Fast scan reads the drive's file table. Windows will ask for permission.{:else}Fast scan is off here: {fast.reason}{/if}
  </p>

  {#if scanning && progress}
    <p class="status" role="status">
      {#if progress.paused}Paused: the scan waits until Lookout is resumed.{:else}Scanning…{/if}
      {progress.files.toLocaleString()} files · {progress.folders.toLocaleString()} folders ·
      {human(progress.bytes)}
      {#if progress.unreadable}· {progress.unreadable.toLocaleString()} can't read{/if}
      <span class="cur">{progress.current}</span>
    </p>
  {:else if progress?.state === "cancelled"}
    <p class="status">Scan cancelled. Nothing was kept.</p>
  {:else if progress?.state === "failed"}
    <p class="status warn">{progress.message || "That location could not be scanned."}</p>
  {/if}
  {#if notice}<p class="status" role="status">{notice}</p>{/if}

  {#if finished && view}
    {#if progress?.saved_at}
      <p class="status">Saved scan from {when(progress.saved_at)}. Rescan for current sizes.</p>
    {/if}
    <nav class="crumbs" aria-label="Path">
      {#each view.crumbs as c, i}
        {#if i > 0}<span>›</span>{/if}
        <button class="crumb" onclick={() => show(c.id)}>{c.name}</button>
      {/each}
      <span class="total">{human(view.total_disk)} used · {human(view.total_logical)} file size · {view.items.toLocaleString()} items</span>
    </nav>
    {#if view.stats.folded_folders > 0}
      <p class="status">
        Very large scan: {view.stats.folded_folders.toLocaleString()} folders have no row of their own
        (their sizes are still counted in the folder above).
      </p>
    {/if}
    {#if systemLine}<p class="warning" role="note">{systemLine}</p>{/if}
    {#if skipNote}<p class="warning" role="note">{skipNote}</p>{/if}

    <div class="tabs" role="tablist">
      <button role="tab" aria-selected={tab === "tree"} class:on={tab === "tree"} onclick={() => setTab("tree")}>Folders</button>
      <button role="tab" aria-selected={tab === "largest"} class:on={tab === "largest"} onclick={() => setTab("largest")}>Largest 100 files</button>
      <button role="tab" aria-selected={tab === "old"} class:on={tab === "old"} onclick={() => setTab("old")}>Old big files</button>
    </div>

    {#if tab === "old"}
      <div class="picker">
        <label class="low">Larger than
          <select bind:value={minMb} onchange={loadFiles}>
            <option value={10}>10 MB</option><option value={50}>50 MB</option><option value={100}>100 MB</option>
            <option value={500}>500 MB</option><option value={1024}>1 GB</option><option value={5120}>5 GB</option>
          </select>
        </label>
        <label class="low">not modified for
          <select bind:value={olderDays} onchange={loadFiles}>
            <option value={30}>1 month</option><option value={90}>3 months</option><option value={180}>6 months</option>
            <option value={365}>1 year</option><option value={730}>2 years</option><option value={1825}>5 years</option>
          </select>
        </label>
      </div>
    {/if}

    {#if tab !== "tree"}
      {#if files.note}<p class="warning" role="note">{files.note}</p>{/if}
      <div class="split" style:grid-template-columns={`minmax(0, 1fr) 6px ${chartW}px`}>
      <div class="left">
      <div class="head files" role="row">
        <span>Name</span><span>Folder</span><span class="c-size">Space used</span><span class="c-mod">Modified</span>
      </div>
      <ul class="rows" role="listbox" aria-label="Files">
        {#each shownFiles as f, i (f.idx)}
          <li
            role="option"
            aria-selected={i === fileSel}
            class="filerow"
            class:selected={i === fileSel}
            data-f={i}
            onclick={() => (fileSel = i)}
            oncontextmenu={(e) => { fileSel = i; openMenuAt(e); }}
            onkeydown={() => {}}
          >
            <span class="nm">{f.name}</span>
            <span class="nm dim" title={f.folder}>{f.folder}</span>
            <span class="c-size">{human(f.size_disk)}</span>
            <span class="c-mod">{when(f.modified)}</span>
          </li>
        {/each}
        {#if shownFiles.length === 0}<li class="none">No files match.</li>{/if}
      </ul>
      </div>
      <div class="grip" role="separator" aria-orientation="vertical" onpointerdown={dragSplit}></div>
      <aside class="chartpane"><Preview source={previewSource} /></aside>
      </div>
      <p class="keys"><kbd>Space</kbd> preview · <kbd>↑↓</kbd> move · <kbd>Enter</kbd> open · <kbd>Ctrl+E</kbd> show in folder · <kbd>Ctrl+C</kbd> copy path · <kbd>Del</kbd> Recycle Bin · right-click for a menu</p>
    {:else}
    <div class="split" style:grid-template-columns={`minmax(0, 1fr) 6px ${chartW}px`}>
    <div class="left">
    <div class="head" role="row">
      <button class="c-name" onclick={() => setSort("name")}>Name{arrow("name")}</button>
      <span class="c-bar">Share of folder</span>
      <button class="c-size" onclick={() => setSort("size")}>Space used{arrow("size")}</button>
      <button class="c-items" onclick={() => setSort("items")}>Items{arrow("items")}</button>
      <button class="c-mod" onclick={() => setSort("modified")}>Modified{arrow("modified")}</button>
    </div>
    <ul class="rows" role="tree" bind:this={listEl}>
      {#each flat as f, i (f.row.id)}
        <li
          role="treeitem"
          aria-selected={i === selected}
          aria-expanded={f.row.kind === "folder" && f.row.has_children ? expanded.has(f.row.id) : undefined}
          class:selected={i === selected}
          data-i={i}
          onclick={() => (selected = i)}
          ondblclick={() => f.row.kind === "folder" && show(f.row.id)}
          oncontextmenu={(e) => openMenu(e, i)}
          onkeydown={() => {}}
        >
          <span class="c-name" style:padding-left={`${f.depth * 16}px`}>
            {#if f.row.kind === "folder" && f.row.has_children}
              <button class="chev" aria-label="Expand" onclick={(e) => { e.stopPropagation(); toggle(f.row); }}>
                {expanded.has(f.row.id) ? "▾" : "▸"}
              </button>
            {:else}<span class="chev"></span>{/if}
            <span class="nm" class:dim={f.row.kind === "others"}>{f.row.name}</span>
            {#if f.row.link}<em>shortcut, not followed</em>{/if}
            {#if f.row.limited}<em title="Part of what is inside is counted but not listed">partly listed</em>{/if}
          </span>
          <span class="c-bar">
            <span class="bar"><span class="fill" style:width={`${f.row.percent}%`}></span></span>
            <span class="pct">{f.row.unreadable ? "" : `${f.row.percent.toFixed(f.row.percent < 10 ? 1 : 0)}%`}</span>
          </span>
          <span class="c-size">
            {#if f.row.unreadable}<em class="bad">can't read</em>
            {:else if f.row.link}<em>not scanned</em>
            {:else}{f.row.partial ? "≥ " : ""}{human(f.row.size_disk)}{/if}
          </span>
          <span class="c-items">{f.row.unreadable || f.row.link ? "" : f.row.items.toLocaleString()}</span>
          <span class="c-mod">{when(f.row.modified)}</span>
        </li>
      {/each}
      {#if flat.length === 0}<li class="none">This folder is empty.</li>{/if}
    </ul>
    </div>
    <div class="grip" role="separator" aria-orientation="vertical" onpointerdown={dragSplit}></div>
    <aside class="chartpane" aria-label="Chart">
      <div class="pbar" data-tauri-drag-region>
        <div class="toggle" role="group" aria-label="Chart type">
          <button class:on={chartMode === "ring"} aria-pressed={chartMode === "ring"} onclick={() => { chartMode = "ring"; loadChart(); }}>Ring</button>
          <button class:on={chartMode === "treemap"} aria-pressed={chartMode === "treemap"} onclick={() => { chartMode = "treemap"; loadChart(); }}>Treemap</button>
        </div>
        {@render expandBtn()}
      </div>
      <div class="chartbox">
      <Chart
        data={chart}
        mode={chartMode}
        title={crumbTitle}
        highlightId={current()?.id ?? null}
        {basePath}
        ondrill={(id) => show(id)}
        onup={up}
        onselect={(id) => { const i = flat.findIndex((f) => f.row.id === id); if (i >= 0) selected = i; }}
      />
      </div>
      <p class="keys">Click a folder to go into it (or scroll the wheel forward); scroll back or click the centre to go up.</p>
      <Preview source={previewSource} />
    </aside>
    </div>
    <p class="keys">
      <kbd>Space</kbd> preview · <kbd>↑↓</kbd> move · <kbd>→←</kbd> expand · <kbd>Enter</kbd> open · <kbd>Ctrl+Enter</kbd> go into ·
      <kbd>Backspace</kbd> up · <kbd>Ctrl+E</kbd> show in folder · <kbd>Ctrl+C</kbd> copy path ·
      <kbd>Del</kbd> Recycle Bin · right-click for a menu
    </p>
    {/if}
  {/if}

  {#if exp && tab === "tree" && finished}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="lbback" onclick={() => (exp = false)}>
      <div class="lbox" role="dialog" tabindex="-1" aria-label="Chart" onclick={(e) => e.stopPropagation()}>
        <div class="lbbar">
          <div class="toggle" role="group" aria-label="Chart type">
            <button class:on={chartMode === "ring"} aria-pressed={chartMode === "ring"} onclick={() => { chartMode = "ring"; loadChart(); }}>Ring</button>
            <button class:on={chartMode === "treemap"} aria-pressed={chartMode === "treemap"} onclick={() => { chartMode = "treemap"; loadChart(); }}>Treemap</button>
          </div>
          <span class="lbtitle">{crumbTitle}</span>
          <button class="xbtn" type="button" data-no-drag aria-label="Close" title="Close (Esc)" onclick={() => (exp = false)}>
            <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" aria-hidden="true"><path d="M3.5 3.5l9 9M12.5 3.5l-9 9" /></svg>
          </button>
        </div>
        <div class="lbbody">
          <div class="lbchart">
            <Chart
              data={chart}
              mode={chartMode}
              title={crumbTitle}
              highlightId={current()?.id ?? null}
              {basePath}
              big={true}
              ondrill={(id) => show(id)}
              onup={up}
              onselect={(id) => { const i = flat.findIndex((f) => f.row.id === id); if (i >= 0) selected = i; }}
            />
          </div>
          <div class="lbside"><Preview source={previewSource} /></div>
        </div>
      </div>
    </div>
  {/if}

  {#if menu}
    <div class="menu" role="menu" style:left={`${menu.x}px`} style:top={`${menu.y}px`}>
      <button role="menuitem" onclick={() => act("open")}>Open</button>
      <button role="menuitem" onclick={() => act("reveal")}>Show in folder</button>
      <button role="menuitem" onclick={() => act("copy_path")}>Copy path</button>
      <button role="menuitem" onclick={() => act("recycle")}>Move to Recycle Bin…</button>
    </div>
  {/if}

  {:else if page === "cleanup"}
    <Cleanup gentle={lowImpact} />
  {:else}
    <Duplicates {locations} bind:chosen {pickFolder} gentle={lowImpact} />
  {/if}

  {#if card}
    {@const p = card.prepared}
    <div class="scrim">
      <div class="card" role="alertdialog" aria-labelledby="dk-title">
        <span class="label-caps">Needs confirmation</span>
        <h3 id="dk-title">{p.preview.title}</h3>
        {#each p.preview.lines as line}<p class="line">{line}</p>{/each}
        {#each p.preview.warnings as w}<p class="warning">{w}</p>{/each}
        {#if card.error}<p class="warning" role="alert">{card.error}</p>{/if}
        <div class="buttons">
          <button class="btn" bind:this={cancelButton} onclick={dismissCard}>Cancel <kbd>Esc</kbd></button>
          <button class="btn primary" bind:this={confirmButton} disabled={!card.presented || card.busy} onclick={confirm}>
            {p.label}
          </button>
        </div>
      </div>
    </div>
  {/if}
</section>

<style>
  .disk {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px 16px;
    overflow: hidden;
    flex: 1;
    min-height: 0;
    font-size: 14px;
  }
  header {
    display: flex;
    align-items: baseline;
    gap: 12px;
  }
  h2 {
    margin: 0;
    font-size: 20px;
    font-weight: 600;
  }
  header .hint {
    flex: 1;
    margin: 0;
  }
  .picker {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    align-items: center;
  }
  select {
    flex: 1;
    min-width: 180px;
  }
  .low {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--text-soft);
  }
  .tabs,
  .toggle {
    display: flex;
    gap: 6px;
  }
  .tabs button,
  .toggle button {
    all: unset;
    box-sizing: border-box;
    cursor: pointer;
    padding: 5px 16px;
    border-radius: 999px;
    border: 1px solid var(--glass-border);
    background: rgba(13, 16, 34, 0.7);
    color: var(--text-soft);
    font-size: 13px;
  }
  .tabs button:hover,
  .toggle button:hover {
    color: var(--text);
    border-color: rgba(255, 255, 255, 0.4);
  }
  .tabs button.on,
  .toggle button.on {
    background: var(--accent-bg);
    border-color: var(--accent-bg);
    color: var(--accent-on);
    font-weight: 600;
  }
  .tabs button:focus-visible,
  .toggle button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .split {
    display: grid;
    grid-template-rows: minmax(0, 1fr);
    gap: 14px;
    flex: 1;
    min-height: 0;
  }
  .left {
    display: flex;
    flex-direction: column;
    min-height: 0;
    min-width: 0;
  }
  .grip {
    cursor: col-resize;
    border-radius: 3px;
    background: var(--border);
    opacity: 0.5;
  }
  .grip:hover {
    background: var(--accent);
    opacity: 1;
  }
  .split {
    position: relative;
    overflow: hidden;
  }
  .disk > .keys {
    flex: none;
    padding-top: 4px;
  }
  .left { grid-column: 1; }
  .grip { grid-column: 2; }
  .chartpane { grid-column: 3; }
  .pbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    min-height: 30px;
    flex: none;
  }
  .xbtn {
    all: unset;
    box-sizing: border-box;
    margin-left: auto;
    width: 26px;
    height: 26px;
    display: grid;
    place-items: center;
    border-radius: 8px;
    cursor: pointer;
    color: var(--text-soft);
    border: 1px solid var(--glass-border);
    background: rgba(13, 16, 34, 0.7);
  }
  .xbtn:hover {
    color: var(--text);
    border-color: rgba(255, 255, 255, 0.4);
  }
  .xbtn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .chartbox {
    flex: none;
  }
  .lbback {
    position: absolute;
    inset: 0;
    z-index: 20;
    padding: 16px;
    display: flex;
    background: rgba(5, 7, 16, 0.6);
    animation: fadein 160ms ease-out;
  }
  .lbox {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px;
    border-radius: 14px;
    border: 1px solid var(--glass-border);
    background: rgba(13, 16, 34, 0.96);
    animation: pop 160ms ease-out;
  }
  .lbbar {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .lbtitle {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text-soft);
  }
  .lbbody {
    flex: 1;
    min-height: 0;
    display: flex;
    gap: 14px;
  }
  .lbchart {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .lbside {
    flex: none;
    width: 280px;
    min-height: 0;
    overflow-y: auto;
  }
  @keyframes fadein {
    from { opacity: 0; }
    to { opacity: 1; }
  }
  @keyframes pop {
    from { opacity: 0; transform: scale(0.97); }
    to { opacity: 1; transform: none; }
  }
  @media (prefers-reduced-motion: reduce) {
    .lbback,
    .lbox { animation: none; }
  }
  .chartpane {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-height: 0;
    min-width: 0;
    overflow-y: auto;
  }
  .head.files,
  li.filerow {
    display: grid;
    grid-template-columns: minmax(120px, 2fr) minmax(140px, 3fr) 92px 86px;
    gap: 8px;
    align-items: center;
  }
  li.filerow {
    padding: 4px 8px;
    border: 1px solid transparent;
    border-radius: 8px;
  }
  .status {
    margin: 0;
    color: var(--text-soft);
    font-size: 13px;
  }
  .status.warn {
    color: var(--danger);
  }
  .cur {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
    font-family: var(--font-code);
    font-size: 12px;
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: 4px;
    flex-wrap: wrap;
  }
  .crumb {
    all: unset;
    cursor: pointer;
    color: var(--accent);
    font-family: var(--font-code);
    font-size: 13px;
  }
  .crumb:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .total {
    margin-left: auto;
    color: var(--text-soft);
    font-family: var(--font-code);
    font-size: 12px;
  }
  .head,
  li[role="treeitem"] {
    display: grid;
    grid-template-columns: minmax(150px, 3fr) minmax(90px, 1.4fr) 92px 70px 86px;
    align-items: center;
    gap: 8px;
  }
  .head {
    color: var(--muted);
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    border-bottom: 1px solid var(--border);
    padding: 4px 8px;
  }
  .head button {
    all: unset;
    cursor: pointer;
  }
  .head button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .c-size,
  .c-items {
    text-align: right;
  }
  .c-size,
  .c-items,
  .c-mod {
    font-family: var(--font-code);
    font-size: 13px;
  }
  .head .c-size,
  .head .c-items,
  .head .c-mod {
    font-family: inherit;
    font-size: 12px;
  }
  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
    overflow-y: auto;
    flex: 1;
    min-height: 0;
  }
  li[role="treeitem"] {
    padding: 4px 8px;
    border: 1px solid transparent;
    border-radius: 8px;
    cursor: default;
  }
  li.selected {
    background: var(--raised);
    border-color: var(--accent);
  }
  .c-name {
    display: flex;
    align-items: center;
    min-width: 0;
    gap: 4px;
  }
  .nm {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chev {
    all: unset;
    width: 16px;
    flex: none;
    text-align: center;
    cursor: pointer;
    color: var(--text-soft);
  }
  .chev:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  em {
    color: var(--muted);
    font-size: 12px;
    font-style: normal;
    white-space: nowrap;
  }
  em.bad {
    color: var(--danger);
  }
  .c-bar {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .bar {
    flex: 1;
    height: 8px;
    border-radius: 4px;
    background: var(--border);
    overflow: hidden;
  }
  .fill {
    display: block;
    height: 100%;
    background: var(--c0);
  }
  .pct {
    width: 38px;
    text-align: right;
    font-family: var(--font-code);
    font-size: 12px;
    color: var(--text-soft);
  }
  .none {
    color: var(--muted);
    padding: 8px;
  }
  .keys {
    margin: 0;
    color: var(--muted);
    font-size: 12px;
  }
  .menu {
    position: fixed;
    display: flex;
    flex-direction: column;
    background: var(--raised);
    border: 1px solid var(--border-strong);
    border-radius: 10px;
    padding: 4px;
    z-index: 10;
    min-width: 180px;
  }
  .menu button {
    all: unset;
    padding: 7px 12px;
    border-radius: 8px;
    cursor: pointer;
  }
  .menu button:hover {
    background: var(--panel);
  }
  .menu button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .scrim {
    position: absolute;
    inset: 0;
    background: var(--scrim);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 20;
  }
  .card {
    background: var(--panel);
    border: 1px solid var(--border-strong);
    border-radius: 10px;
    padding: 16px 18px;
    max-width: 460px;
  }
  .card h3 {
    margin: 6px 0 10px;
    font-size: 20px;
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .line {
    margin: 3px 0;
    color: var(--text-soft);
    font-family: var(--font-code);
    font-size: 13px;
    word-break: break-all;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 14px;
  }
</style>
