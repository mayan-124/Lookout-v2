<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onDestroy } from "svelte";
  import Confirm, { type Prepared } from "./Confirm.svelte";

  interface TaskView {
    state: "idle" | "running" | "done" | "cancelled" | "failed";
    phase: string;
    files: number;
    bytes: number;
    error: string;
  }
  interface Item {
    id: string;
    title: string;
    why: string;
    risk: "safe" | "check_first";
    mode: "recycle" | "delete" | "empty_bin";
    bytes: number;
    count: number;
  }
  interface Preview {
    rows: { path: string; size: number }[];
    total: number;
  }

  let { gentle }: { gentle: boolean } = $props();

  let progress = $state<TaskView | null>(null);
  let items = $state<Item[]>([]);
  let reclaimed = $state(0);
  let notice = $state("");
  let preview = $state<{ id: string; data: Preview } | null>(null);
  let card = $state<Prepared | null>(null);
  let timer: ReturnType<typeof setInterval> | undefined;

  const running = $derived(progress?.state === "running");
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

  async function load() {
    const r = await invoke<{ items: Item[]; reclaimed: number }>("cleanup_items");
    items = r.items;
    reclaimed = r.reclaimed;
  }

  async function poll() {
    try {
      progress = await invoke<TaskView | null>("cleanup_progress");
      if (progress && progress.state !== "running") {
        stop();
        if (progress.state === "failed") notice = progress.error;
        await load();
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

  async function scan(keepNotice = false) {
    if (!keepNotice) notice = "";
    preview = null;
    items = [];
    try {
      await invoke("cleanup_start", { gentle });
    } catch (e) {
      notice = errorText(e);
      return;
    }
    stop();
    timer = setInterval(poll, 300);
    await poll();
  }
  const cancel = () => invoke("cleanup_cancel");

  async function showPreview(id: string) {
    if (preview?.id === id) {
      preview = null;
      return;
    }
    try {
      preview = { id, data: await invoke<Preview>("cleanup_preview", { id }) };
    } catch (e) {
      notice = errorText(e);
    }
  }

  async function clean(id: string) {
    notice = "";
    try {
      card = await invoke<Prepared>("cleanup_prepare", { id });
    } catch (e) {
      notice = errorText(e);
    }
  }

  async function done(message: string) {
    card = null;
    preview = null;
    notice = message;
    await scan(true); // the list is out of date now; the total reclaimed refreshes when it ends
  }
</script>

<div class="clean">
  <div class="picker">
    <span class="status">Known space-wasters on this PC. Nothing is removed until you press Clean and confirm.</span>
    {#if running}
      <button class="btn" onclick={cancel}>Cancel</button>
    {:else}
      <button class="btn primary" onclick={() => scan()}>{progress ? "Scan again" : "Find things to clean"}</button>
    {/if}
  </div>

  {#if running && progress}
    <p class="status" role="status">
      {progress.phase} · {progress.files.toLocaleString()} files looked at · {human(progress.bytes)}
    </p>
  {/if}
  {#if progress?.state === "cancelled"}<p class="status">Cancelled: the list may be incomplete.</p>{/if}
  {#if notice}<p class="warning" role="status">{notice}</p>{/if}
  {#if reclaimed > 0}<p class="status">Cleared so far this session: <strong>{human(reclaimed)}</strong></p>{/if}

  {#if progress && !running && items.length === 0 && progress.state !== "failed"}
    <p class="status">Nothing worth cleaning was found.</p>
  {/if}

  <ul class="list">
    {#each items as it (it.id)}
      <li>
        <div class="row">
          <div class="what">
            <strong>{it.title}</strong>
            <span class="tag" class:check={it.risk === "check_first"}>{it.risk === "safe" ? "Safe" : "Check first"}</span>
            <p class="why">{it.why}</p>
            <p class="how">
              {it.mode === "recycle"
                ? "Goes to the Recycle Bin."
                : it.mode === "empty_bin"
                  ? "Deleted for good."
                  : "Deleted directly (rebuilt by the program)."}
              {#if it.mode !== "empty_bin"}{it.count.toLocaleString()} item{it.count === 1 ? "" : "s"}.{/if}
            </p>
          </div>
          <div class="size">{human(it.bytes)}</div>
          <div class="btns">
            {#if it.mode !== "empty_bin"}
              <button class="btn" onclick={() => showPreview(it.id)}>{preview?.id === it.id ? "Hide list" : "Preview"}</button>
            {/if}
            <button class="btn" onclick={() => clean(it.id)} disabled={!!card}>Clean…</button>
          </div>
        </div>
        {#if preview?.id === it.id}
          <div class="plist" role="region" aria-label="What would be removed">
            {#each preview.data.rows as r}
              <p class="line"><span>{r.path}</span><span>{human(r.size)}</span></p>
            {/each}
            {#if preview.data.total > preview.data.rows.length}
              <p class="status">…and {(preview.data.total - preview.data.rows.length).toLocaleString()} more</p>
            {/if}
          </div>
        {/if}
      </li>
    {/each}
  </ul>

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
  .clean {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-height: 0;
    flex: 1;
    overflow: auto;
  }
  .picker {
    display: flex;
    gap: 8px;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
  }
  .status {
    margin: 0;
    color: var(--text-soft);
    font-size: 13px;
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  li {
    border: 1px solid var(--border-strong);
    border-radius: 8px;
    padding: 10px 12px;
  }
  .row {
    display: flex;
    gap: 12px;
    align-items: flex-start;
  }
  .what {
    flex: 1;
    min-width: 0;
  }
  .why,
  .how {
    margin: 3px 0 0;
    color: var(--text-soft);
    font-size: 13px;
  }
  .size {
    font-variant-numeric: tabular-nums;
    font-weight: 600;
    min-width: 72px;
    text-align: right;
  }
  .btns {
    display: flex;
    gap: 6px;
  }
  .tag {
    margin-left: 8px;
    font-size: 12px;
    padding: 1px 8px;
    border-radius: 99px;
    border: 1px solid var(--border-strong);
    color: var(--text-soft);
  }
  .tag.check {
    color: var(--danger);
    border-color: var(--danger);
  }
  .plist {
    margin-top: 8px;
    max-height: 220px;
    overflow: auto;
    border-top: 1px solid var(--border-strong);
    padding-top: 6px;
  }
  .line {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    margin: 2px 0;
    font-family: var(--font-code);
    font-size: 12px;
    color: var(--text-soft);
  }
  .line span:first-child {
    word-break: break-all;
  }
</style>
