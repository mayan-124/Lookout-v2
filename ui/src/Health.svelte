<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

  interface Feature {
    state: string;
    reason: string;
    last_error: string | null;
  }
  interface HealthState {
    name_index: { count: number; state: string; reason: string };
    content: Feature & { files_done: number; files_queued: number; read_done: number; read_total: number; ocr_done: number; ocr_total: number; index_now_running: boolean; size_bytes: number; max_bytes: number; full: boolean };
    screenshots: Feature;
    plugins: (Feature & { name: string })[];
  }

  let health: HealthState | null = $state(null);
  let loading = $state(true);
  let failed = $state(false);
  let open: Record<string, boolean> = $state({});

  async function load() {
    try {
      health = await invoke<HealthState | null>("health");
      failed = false;
    } catch {
      health = null;
      failed = true;
    }
    loading = false;
  }

  async function indexNow(on: boolean) {
    await invoke("index_now", { on });
    await load();
  }

  onMount(() => {
    load();
    let t: ReturnType<typeof setInterval> | undefined;
    const sync = () => {
      if (t !== undefined) clearInterval(t);
      t = undefined;
      if (document.visibilityState === "visible") {
        load();
        t = setInterval(load, 1000);
      }
    };
    sync();
    document.addEventListener("visibilitychange", sync);
    return () => {
      if (t !== undefined) clearInterval(t);
      document.removeEventListener("visibilitychange", sync);
    };
  });

  const mb = (b: number) => `${(b / 1048576).toLocaleString(undefined, { maximumFractionDigits: 1 })} MB`;
  const STATE: Record<string, string> = {
    "up to date": "Ready",
    available: "Ready",
    running: "Working",
    "waiting for idle": "Waiting until you are idle",
    paused: "Paused",
    off: "Off",
    full: "Full",
    unavailable: "Not available",
  };
  const word = (s: string) => STATE[s] ?? s.charAt(0).toUpperCase() + s.slice(1);
  // Healthy areas get one line; the reason is added only when something needs saying.
  const quiet = (s: string) => s === "up to date" || s === "available";
</script>

{#snippet line(name: string, state: string, extra: string, reason: string, error: string | null)}
  <div class="item">
    <span class="dot" class:ok={quiet(state)} class:bad={state === "unavailable" || state === "full"}></span>
    <span class="name">{name}</span>
    <span class="state">{word(state)}{extra ? ` · ${extra}` : ""}</span>
  </div>
  {#if reason && !quiet(state)}<p class="note">{reason}</p>{/if}
  {#if error}
    <p class="note" role="status">
      1 file could not be read
      <button class="btn text" aria-expanded={!!open[name]} onclick={() => (open[name] = !open[name])}>Details</button>
    </p>
    {#if open[name]}<p class="note code">{error}</p>{/if}
  {/if}
{/snippet}

{#if health}
  {@render line("File names", health.name_index.state, `${health.name_index.count.toLocaleString()} found`, health.name_index.reason, null)}
  {@render line(
    "File text",
    health.content.state,
    health.content.read_total > 0 && health.content.read_done < health.content.read_total
      ? `Reading text: ${health.content.read_done.toLocaleString()} of ${health.content.read_total.toLocaleString()} files`
      : health.content.ocr_total > 0 && health.content.ocr_done < health.content.ocr_total
        ? `Reading pictures: ${health.content.ocr_done.toLocaleString()} of ${health.content.ocr_total.toLocaleString()} files`
        : `${health.content.files_done.toLocaleString()} files read`,
    health.content.reason,
    health.content.last_error,
  )}
  {#if health.content.max_bytes > 0}
    <p class="note">File text uses {mb(health.content.size_bytes)} of {mb(health.content.max_bytes)}.</p>
  {/if}
  {#if health.content.full}
    <p class="warning"><strong>File text storage is full.</strong> Raise the limit in Speed or delete your search data.</p>
  {/if}
  {#if health.content.state !== "off"}
    <div class="action">
      {#if health.content.index_now_running}
        <button class="btn small" onclick={() => indexNow(false)}>Stop checking</button>
      {:else}
        <button class="btn small" onclick={() => indexNow(true)}>Check files now</button>
      {/if}
      <span class="hint">Reads everything now, without waiting.</span>
    </div>
  {/if}
  {@render line("Text in pictures", health.screenshots.state, "", health.screenshots.reason, health.screenshots.last_error)}
  {#each health.plugins as p (p.name)}
    {@render line(p.name, p.state, "", p.reason, p.last_error)}
  {/each}
{:else if loading}
  <p class="note" role="status">Checking status…</p>
{:else if failed}
  <p class="warning" role="alert">Could not read status. Restart Lookout.</p>
{:else}
  <p class="note">Search is not running.</p>
{/if}

<style>
  .item {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 0 2px;
  }
  .name {
    font-weight: 600;
  }
  .state {
    margin-left: auto;
    color: var(--text-soft);
  }
  .dot {
    width: 8px;
    height: 8px;
    flex: none;
    border-radius: 50%;
    background: var(--muted);
  }
  .dot.ok {
    background: var(--success);
  }
  .dot.bad {
    background: var(--danger);
  }
  .note {
    margin: 2px 0 4px 18px;
  }
  .action {
    display: flex;
    align-items: center;
    gap: 12px;
    margin: 8px 0;
  }
  .action .hint {
    margin: 0;
  }
</style>
