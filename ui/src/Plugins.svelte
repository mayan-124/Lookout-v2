<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onDestroy, onMount } from "svelte";
  import { plain as errorText } from "./errors";

  interface PluginInfo {
    id: string;
    name: string;
    version: string;
    description: string;
    summary: string;
    permissions: string[];
    risk_floor: "safe" | "confirm" | "dangerous";
    enabled: boolean;
    approved: boolean;
    running: boolean;
    last_error: string | null;
    config: Record<string, number | string>;
  }

  interface SemanticStatus {
    state: "off" | "unavailable" | "waiting" | "indexing" | "ready";
    done: number;
    total: number;
    reason: string | null;
    assets_dir: string;
  }
  const SEMANTIC_ID = "com.lookout.semantic";
  let semantic: SemanticStatus | null = $state(null);

  async function loadSemantic() {
    try {
      semantic = await invoke<SemanticStatus | null>("semantic_status");
    } catch {
      semantic = null;
    }
  }

  async function deleteSemantic() {
    message = "";
    try {
      await invoke("semantic_delete_index");
      message = "Deleted meaning data and turned off search by meaning.";
      load(await invoke<PluginInfo[]>("plugins_list"));
      await loadSemantic();
    } catch (e) {
      message = errorText(e);
    }
  }

  interface SetupInfo {
    total_bytes: number;
    files: [string, number][];
    hosts: string[];
  }
  interface SetupStatus {
    phase: "not_set_up" | "downloading" | "ready" | "failed";
    message: string | null;
    done_bytes: number;
    total_bytes: number;
    current: string | null;
  }
  let setupStatus: SetupStatus | null = $state(null);
  let setupInfo: SetupInfo | null = $state(null);
  let confirming = $state(false);
  let poll: ReturnType<typeof setInterval> | null = null;
  const mb = (n: number) => (n / 1_000_000).toFixed(1);

  async function loadSetup() {
    try {
      setupStatus = await invoke<SetupStatus>("semantic_setup_status");
    } catch {
      setupStatus = null;
    }
    if (setupStatus?.phase === "downloading") {
      if (!poll) poll = setInterval(loadSetup, 500);
    } else if (poll) {
      clearInterval(poll);
      poll = null;
      await loadSemantic();
    }
  }

  async function askSetup() {
    message = "";
    try {
      setupInfo = await invoke<SetupInfo>("semantic_setup_info");
      confirming = true;
    } catch (e) {
      message = errorText(e);
    }
  }

  async function startSetup() {
    confirming = false;
    try {
      await invoke("semantic_setup_start");
    } catch (e) {
      message = errorText(e);
    }
    await loadSetup();
  }

  async function cancelSetup() {
    await invoke("semantic_setup_cancel").catch((e) => (message = errorText(e)));
  }

  async function removeSemanticFiles() {
    message = "";
    try {
      await invoke("semantic_remove_files");
      message = "Removed downloaded files and turned off search by meaning.";
      load(await invoke<PluginInfo[]>("plugins_list"));
    } catch (e) {
      message = errorText(e);
    }
    await loadSetup();
    await loadSemantic();
  }

  let plugins: PluginInfo[] = $state([]);
  let loading = $state(true);
  let failed = $state(false);
  // The add-on whose permissions are on screen, waiting for "Allow".
  let asking: string | null = $state(null);
  let open: Record<string, boolean> = $state({});
  let message = $state("");
  let drafts: Record<string, Record<string, number | string>> = $state({});

  function load(list: PluginInfo[]) {
    plugins = list;
    for (const p of list) drafts[p.id] = { ...p.config };
  }

  onMount(async () => {
    try {
      load(await invoke<PluginInfo[]>("plugins_list"));
    } catch {
      failed = true; // said below, with what to do
    }
    loading = false;
    await loadSemantic();
    await loadSetup();
    try {
      setupInfo = await invoke<SetupInfo>("semantic_setup_info"); // sizes only, no network
    } catch {
      setupInfo = null;
    }
  });
  onDestroy(() => {
    if (poll) clearInterval(poll);
  });

  async function setEnabled(p: PluginInfo, enabled: boolean, accept: boolean) {
    message = "";
    try {
      load(await invoke<PluginInfo[]>("plugin_set_enabled", { id: p.id, enabled, accept }));
      asking = null;
      await loadSemantic();
    } catch (e) {
      message = errorText(e);
    }
  }

  // Turning an add-on on needs its permissions approved first.
  function onSwitch(p: PluginInfo, e: Event & { currentTarget: HTMLInputElement }) {
    const on = e.currentTarget.checked;
    if (on && !p.approved) {
      e.currentTarget.checked = false;
      asking = p.id;
      open[p.id] = true;
      return;
    }
    setEnabled(p, on, on);
  }

  async function saveConfig(p: PluginInfo) {
    message = "";
    try {
      load(await invoke<PluginInfo[]>("plugin_set_config", { id: p.id, config: drafts[p.id] }));
      message = `Saved settings for ${p.name}.`;
    } catch (e) {
      message = errorText(e);
    }
  }

  // One line for the card: the manifest's summary, else the first sentence without "(type: ...)" examples.
  function oneLine(p: PluginInfo): string {
    let t = p.summary.trim();
    if (!t) {
      t = p.description.replace(/\s*\((?:type|e\.g\.)[^)]*\)/gi, "").trim();
      const m = t.match(/^.*?[.!?](?=\s|$)/);
      if (m) t = m[0];
    }
    if (t.length <= 110) return t;
    const cut = t.slice(0, 110);
    return `${cut.slice(0, cut.lastIndexOf(" ")).replace(/[,;:\s]+$/, "")}…`;
  }
  const label = (key: string) => key.replace(/_/g, " ");
</script>

{#each plugins as p (p.id)}
  <div class="card">
    <div class="head">
      <span class="fl">
        <span class="ft">{p.name}</span>
        {#if p.summary || p.description}<span class="hint">{oneLine(p)}</span>{/if}
      </span>
      <button
        class="btn small text dtl"
        aria-expanded={open[p.id] || asking === p.id}
        onclick={() => (open[p.id] = !open[p.id])}>{open[p.id] || asking === p.id ? "▾" : "▸"} Details</button
      >
      <span class="onoff">{p.enabled ? "On" : "Off"}</span>
      <label class="switch" title={p.enabled ? "On" : "Off"}>
        <input
          type="checkbox"
          role="switch"
          aria-label={`${p.name} on or off`}
          checked={p.enabled}
          onchange={(e) => onSwitch(p, e)}
        />
        <span class="track"></span>
      </label>
    </div>

    {#if open[p.id] || asking === p.id}
      <div class="details">
        {#if p.description}<p class="note">{p.description}</p>{/if}
        <p class="note">
          Version {p.version}. {#if !p.enabled}Off.{:else if p.running}On and running.{:else}On; starts when needed.{/if}
        </p>
        {#if p.permissions.length > 0}
          <p class="note">This add-on wants to:</p>
          <ul class="perms">
            {#each p.permissions as perm}<li>{perm}</li>{/each}
          </ul>
        {/if}
        <p class="note">
          {p.risk_floor === "safe"
            ? "Its actions need no extra confirmation."
            : p.risk_floor === "confirm"
              ? "Its actions ask you to confirm."
              : "Its actions ask you to confirm carefully."} It can see what you can see.
        </p>
        {#if asking === p.id}
          <div class="row">
            <button class="btn primary small" onclick={() => setEnabled(p, true, true)}>Allow and turn on</button>
            <button class="btn small" onclick={() => (asking = null)}>Cancel</button>
          </div>
        {/if}
        {#if p.enabled && Object.keys(p.config).length > 0}
          <div class="config">
            {#each Object.keys(p.config) as key (key)}
              <label class="field">
                <span class="ft cap">{label(key)}</span>
                <input
                  type={typeof p.config[key] === "number" ? "number" : "text"}
                  bind:value={drafts[p.id][key]}
                  min="0"
                  step="any"
                />
              </label>
            {/each}
            <button class="btn small" onclick={() => saveConfig(p)}>Save settings</button>
          </div>
        {/if}
      </div>
    {/if}

    {#if p.id === SEMANTIC_ID && semantic}
      <p class="note" role="status">
        {#if semantic.state === "off"}Search by meaning is off.
        {:else if semantic.state === "unavailable"}Search by meaning is not working. {semantic.reason ? errorText(semantic.reason) : "Turn it off and on again."}
        {:else if semantic.state === "waiting"}Waiting until you are idle.
        {:else if semantic.state === "indexing"}Looking at {semantic.done} of {semantic.total} files.
        {:else}Ready.{/if}
        {#if setupStatus === null}
          Could not check the download. Open Add-ons again in a moment.
        {:else if setupStatus.phase === "downloading"}
          Downloading {setupStatus.current ?? ""}: {mb(setupStatus.done_bytes)} of {mb(setupStatus.total_bytes)} MB.
        {:else if setupStatus.phase === "ready"}Files downloaded.
        {:else if setupStatus.phase === "failed"}
          {setupStatus.message ? errorText(setupStatus.message) : "The download failed. Try again."}
        {/if}
      </p>
      {#if setupStatus?.phase === "downloading"}
        <progress
          class="dl"
          aria-label="Download progress"
          max={setupStatus.total_bytes || 1}
          value={setupStatus.done_bytes}
        ></progress>
      {/if}
      {#if confirming && setupInfo}
        <div class="warning">
          <p>
            This downloads {mb(setupInfo.total_bytes)} MB from {setupInfo.hosts.join(" and ")}. Every file is checked for safety before it is used. Lookout only receives files; nothing about you or your files is sent.
          </p>
          <ul>
            {#each setupInfo.files as [name, bytes]}<li><span class="code">{name}</span>: {mb(bytes)} MB</li>{/each}
          </ul>
          <div class="row">
            <button class="btn primary small" onclick={startSetup}>Download {mb(setupInfo.total_bytes)} MB</button>
            <button class="btn small" onclick={() => (confirming = false)}>Not now</button>
          </div>
        </div>
      {/if}
      <div class="row">
        {#if setupStatus?.phase === "ready" && !p.enabled}
          <button class="btn primary small" onclick={() => (p.approved ? setEnabled(p, true, true) : (asking = p.id))}>
            Turn on search by meaning
          </button>
        {/if}
        {#if setupStatus?.phase === "downloading"}
          <button class="btn small" onclick={cancelSetup}>Cancel</button>
        {:else if setupStatus?.phase !== "ready" && !confirming}
          <button class="btn small" onclick={askSetup}>
            Set up{setupInfo ? ` (${mb(setupInfo.total_bytes)} MB)` : ""}
          </button>
        {/if}
        {#if setupStatus?.phase === "ready"}
          <button class="btn small" onclick={removeSemanticFiles}>Remove downloaded files</button>
        {/if}
        {#if semantic.done > 0 || semantic.state === "ready"}
          <button class="btn small danger" onclick={deleteSemantic}>Delete meaning data</button>
        {/if}
      </div>
      <p class="note">Downloads only when you click. Your files and searches never leave this PC.</p>
    {/if}
    {#if p.last_error}<p class="warning">Last problem: {errorText(p.last_error)} Turn the add-on off and on to try again.</p>{/if}
  </div>
{:else}
  {#if loading}
    <p class="note" role="status">Loading add-ons…</p>
  {:else if failed}
    <p class="warning" role="alert">Could not load add-ons. Restart Lookout.</p>
  {:else}
    <p class="note">No add-ons found. Reinstall Lookout to get them back.</p>
  {/if}
{/each}
<p class="note">Type <kbd>clip</kbd> to see clipboard history, <kbd>clip clear</kbd> to delete it.</p>
{#if message}<p class="note" role="status">{message}</p>{/if}

<style>
  .btn.dtl {
    color: var(--accent-text);
  }
  .btn.dtl:hover {
    text-decoration: underline;
  }
  .onoff {
    font-size: 12px;
    color: var(--muted);
    min-width: 24px;
    text-align: right;
  }
  .card {
    margin: 0 0 10px;
    padding: 12px 14px;
    border: 1px solid var(--border);
    border-radius: 10px;
    background: var(--panel);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .fl {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .ft {
    font-weight: 600;
  }
  .cap {
    text-transform: capitalize;
  }
  .details {
    margin-top: 6px;
    padding-top: 6px;
    border-top: 1px solid var(--border);
  }
  .perms {
    margin: 0 0 4px;
    padding-left: 20px;
    color: var(--text-soft);
    font-size: 13px;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin: 8px 0 4px;
  }
  .config {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-top: 8px;
  }
  .field {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
  }
  .field input {
    width: 130px;
  }
  .dl {
    display: block;
    width: 100%;
    height: 6px;
    margin: 4px 0 8px;
    accent-color: var(--accent-bg);
  }
</style>
