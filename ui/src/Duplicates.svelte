<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onDestroy } from "svelte";
  import Confirm, { type Prepared } from "./Confirm.svelte";
  import Preview from "./Preview.svelte";
  import { plain as errorText } from "./errors";

  interface Location {
    label: string;
    path: string;
  }
  interface TaskView {
    state: "idle" | "running" | "done" | "cancelled" | "failed";
    phase: string;
    files: number;
    bytes: number;
    error: string;
  }
  interface Group {
    id: number;
    size: number;
    wasted: number;
    files: { path: string; mtime: number }[];
  }
  interface Groups {
    groups: Group[];
    total_groups: number;
    reclaimed: number;
  }

  let {
    locations,
    chosen = $bindable(),
    pickFolder,
    gentle,
  }: { locations: Location[]; chosen: string; pickFolder: () => Promise<void>; gentle: boolean } = $props();

  let minSize = $state(1048576);
  let progress = $state<TaskView | null>(null);
  let data = $state<Groups>({ groups: [], total_groups: 0, reclaimed: 0 });
  let notice = $state("");
  let strategy = $state<"newest" | "oldest" | "shortest" | "manual">("newest");
  let manual = $state(new Map<number, number>());
  let picked = $state(new Set<number>());
  let open = $state(new Set<number>());
  let card = $state<Prepared | null>(null);
  let selPath = $state("");
  let timer: ReturnType<typeof setInterval> | undefined;

  const running = $derived(progress?.state === "running");
  const wasted = $derived(data.groups.filter((g) => picked.has(g.id)).reduce((a, g) => a + g.wasted, 0));

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
  const when = (s: number) => new Date(s * 1000).toLocaleDateString();

  // Which file of a group stays (the backend decides again when it builds the list).
  function keeper(g: Group): number {
    const n = g.files.length;
    const idx = [...Array(n).keys()];
    if (strategy === "manual") return manual.get(g.id) ?? idx.reduce((b, i) => (g.files[i].mtime > g.files[b].mtime ? i : b), 0);
    if (strategy === "oldest") return idx.reduce((b, i) => (g.files[i].mtime < g.files[b].mtime ? i : b), 0);
    if (strategy === "shortest") return idx.reduce((b, i) => (g.files[i].path.length < g.files[b].path.length ? i : b), 0);
    return idx.reduce((b, i) => (g.files[i].mtime > g.files[b].mtime ? i : b), 0);
  }

  async function load() {
    data = await invoke<Groups>("dupes_groups");
    picked = new Set();
    open = new Set();
  }

  async function poll() {
    try {
      progress = await invoke<TaskView | null>("dupes_progress");
      if (progress && progress.state !== "running") {
        stop();
        if (progress.state === "failed") notice = errorText(progress.error);
        await load().catch((e) => (notice = errorText(e)));
      }
    } catch {
      /* the next tick tries again */
    }
  }
  function stop() {
    if (timer) clearInterval(timer);
    timer = undefined;
  }
  onDestroy(stop);

  async function scan() {
    notice = "";
    data = { groups: [], total_groups: 0, reclaimed: data.reclaimed };
    manual = new Map();
    try {
      await invoke("dupes_start", { path: chosen, minSize, gentle });
    } catch (e) {
      notice = errorText(e);
      return;
    }
    stop();
    timer = setInterval(poll, 300);
    await poll();
  }
  const cancel = () => invoke("dupes_cancel").catch((e) => (notice = errorText(e)));

  function toggle(id: number) {
    picked.has(id) ? picked.delete(id) : picked.add(id);
    picked = new Set(picked);
  }
  function toggleOpen(id: number) {
    open.has(id) ? open.delete(id) : open.add(id);
    open = new Set(open);
  }
  function pickAll(on: boolean) {
    picked = on ? new Set(data.groups.map((g) => g.id)) : new Set();
  }
  function choose(g: Group, i: number) {
    manual.set(g.id, i);
    manual = new Map(manual);
    strategy = "manual";
  }

  async function preview() {
    notice = "";
    const m: [number, number][] = [];
    if (strategy === "manual") for (const g of data.groups) if (picked.has(g.id)) m.push([g.id, keeper(g)]);
    try {
      card = await invoke<Prepared>("dupes_prepare", { strategy, manual: m, groups: [...picked] });
    } catch (e) {
      notice = errorText(e);
    }
  }

  async function done(message: string) {
    card = null;
    notice = message;
    await scan();
    notice = message;
  }
</script>

<div class="dupes">
  <div class="picker">
    <select bind:value={chosen} disabled={running} aria-label="Folder to scan">
      {#each locations as l}<option value={l.path}>{l.label} — {l.path}</option>{/each}
      {#if chosen && !locations.some((l) => l.path === chosen)}<option value={chosen}>{chosen}</option>{/if}
    </select>
    <button class="btn" onclick={pickFolder} disabled={running}>Choose folder…</button>
    <label class="low">Files of at least
      <select bind:value={minSize} disabled={running}>
        <option value={102400}>100 KB</option><option value={1048576}>1 MB</option>
        <option value={10485760}>10 MB</option><option value={104857600}>100 MB</option>
      </select>
    </label>
    {#if running}
      <button class="btn" onclick={cancel}>Cancel</button>
    {:else}
      <button class="btn primary" onclick={scan} disabled={!chosen}>{progress ? "Scan again" : "Find duplicates"}</button>
    {/if}
  </div>
  <p class="status">
    Identical files, compared by size, then the first 64 KB, then the whole file. Hard links, cloud-only files and
    system or program folders are skipped.
  </p>

  {#if running && progress}
    <p class="status" role="status">
      {progress.phase} · {progress.files.toLocaleString()} files · {human(progress.bytes)} read
    </p>
  {/if}
  {#if progress?.state === "cancelled"}<p class="status">Cancelled, so there are no results. Press Scan again to look for duplicates.</p>{/if}
  {#if notice}<p class="warning" role="status">{notice}</p>{/if}
  {#if data.reclaimed > 0}<p class="status">Freed so far this session: <strong>{human(data.reclaimed)}</strong></p>{/if}

  {#if progress && progress.state === "done"}
    {#if data.groups.length === 0}
      <p class="status">No duplicates found. Try another folder or a smaller file size.</p>
    {:else}
      <div class="bar">
        <span>{data.total_groups.toLocaleString()} groups · {human(data.groups.reduce((a, g) => a + g.wasted, 0))} wasted
          {#if data.total_groups > data.groups.length}(showing the biggest {data.groups.length}){/if}</span>
        <label class="low">Keep
          <select bind:value={strategy}>
            <option value="newest">the newest</option><option value="oldest">the oldest</option>
            <option value="shortest">the shortest path</option><option value="manual">my choice (open a group)</option>
          </select>
        </label>
        <button class="btn" onclick={() => pickAll(picked.size === 0)}>{picked.size === 0 ? "Select all" : "Select none"}</button>
        <button class="btn primary" onclick={preview} disabled={picked.size === 0 || !!card}>
          Preview and move {human(wasted)} to the Recycle Bin…
        </button>
      </div>
      <ul class="groups">
        {#each data.groups as g (g.id)}
          {@const k = keeper(g)}
          <li>
            <div class="ghead">
              <input type="checkbox" checked={picked.has(g.id)} onchange={() => toggle(g.id)} aria-label="Select this group" />
              <button class="link" onclick={() => toggleOpen(g.id)} aria-expanded={open.has(g.id)}>
                {g.files.length} copies of {g.files[k].path.split("\\").pop()}
              </button>
              <span class="sz">{human(g.size)} each · <strong>{human(g.wasted)}</strong> wasted</span>
            </div>
            {#if open.has(g.id)}
              {#each g.files as f, i}
                <label class="file" class:keep={i === k} class:sel={selPath === f.path} onfocusin={() => (selPath = f.path)}>
                  <input type="radio" name={"g" + g.id} checked={i === k} onchange={() => choose(g, i)} />
                  <span class="p">{f.path}</span>
                  <span class="m">{when(f.mtime)}</span>
                  <span class="t">{i === k ? "Keep" : "Move to Recycle Bin"}</span>
                </label>
              {/each}
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  {/if}

  {#if selPath}<Preview source={{ path: selPath }} />{/if}

  {#if card}
    <Confirm
      prepared={card}
      ondone={done}
      oncancel={(e) => {
        card = null;
        if (e) notice = e;
      }}
    />
  {/if}
</div>

<style>
  .dupes {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-height: 0;
    flex: 1;
    overflow: auto;
  }
  .picker,
  .bar {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    align-items: center;
  }
  .bar {
    justify-content: space-between;
  }
  select {
    min-width: 120px;
  }
  .low {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--text-soft);
  }
  .status {
    margin: 0;
    color: var(--text-soft);
    font-size: 13px;
  }
  .groups {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  li {
    border: 1px solid var(--border-strong);
    border-radius: 8px;
    padding: 6px 10px;
  }
  .ghead {
    display: flex;
    gap: 10px;
    align-items: center;
  }
  .link {
    all: unset;
    cursor: pointer;
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .link:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .sz {
    color: var(--text-soft);
    font-size: 13px;
    white-space: nowrap;
  }
  .file {
    display: flex;
    gap: 8px;
    align-items: center;
    padding: 3px 0 3px 26px;
    font-size: 12px;
    font-family: var(--font-code);
    color: var(--text-soft);
  }
  .file.sel {
    background: var(--raised);
  }
  .file.keep {
    color: var(--text);
  }
  .p {
    flex: 1;
    min-width: 0;
    word-break: break-all;
  }
  .m,
  .t {
    white-space: nowrap;
  }
</style>
