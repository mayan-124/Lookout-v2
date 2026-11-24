<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onDestroy, onMount } from "svelte";
  import Plugins from "./Plugins.svelte";
  import Health from "./Health.svelte";
  import { applyTheme, type ThemePref } from "./theme";
  import { ACCENTS, look, setAccent, setGlass, setReduceMotion } from "./look.svelte";
  import { getVersion } from "@tauri-apps/api/app";
  import { check, type Update } from "@tauri-apps/plugin-updater";

  interface PrivacyState {
    roots: string[];
    excluded_folders: string[];
    paused: boolean;
    needs_setup: boolean;
    pause_hotkey: string;
    grab_hotkey: string;
    open_hotkey: string;
    index_contents: boolean;
    ocr: boolean;
    idle_secs: number;
    rescan_hours: number;
    index_speed: "gentle" | "balanced" | "fast";
    content_index_max_mb: number;
    theme: ThemePref;
  }

  // Manual update check: nothing runs until the user clicks; nothing downloads until the second click.
  let appVersion = $state("");
  let upd: Update | null = $state(null);
  let updMsg = $state("");
  let updBusy = $state(false);
  getVersion().then((v: string) => (appVersion = v)).catch(() => {});
  async function checkUpdates() {
    updBusy = true;
    updMsg = "";
    upd = null;
    try {
      const u = await check();
      if (u) upd = u;
      else updMsg = "Lookout is up to date.";
    } catch {
      updMsg = "Could not check for updates. Check your internet connection and try again.";
    }
    updBusy = false;
  }
  async function installUpdate() {
    if (!upd) return;
    updBusy = true;
    updMsg = "Downloading…";
    try {
      await upd.downloadAndInstall();
      updMsg = "Installing. Lookout will restart.";
    } catch {
      updMsg = "The update could not be downloaded or verified. Nothing was changed.";
      updBusy = false;
    }
  }

  type Section = "general" | "folders" | "privacy" | "speed" | "addons" | "shortcuts" | "status" | "delete";
  const SECTIONS: [Section, string][] = [
    ["general", "General"],
    ["folders", "Folders"],
    ["privacy", "Privacy"],
    ["speed", "Speed"],
    ["addons", "Add-ons"],
    ["shortcuts", "Shortcuts"],
    ["status", "Status"],
    ["delete", "Delete my data"],
  ];
  const ALWAYS_SKIPPED = [
    "node_modules",
    ".git",
    "Windows",
    "Program Files",
    "browser profiles",
    "password-manager folders",
  ];

  let { onclose }: { onclose: () => void } = $props();

  let skipList: string[] = $state([]);
  invoke<string[]>("skip_list").then((l) => (skipList = l)).catch(() => {});

  let info: PrivacyState | null = $state(null);
  let section: Section = $state("folders");
  let roots: string[] = $state([]);
  let excluded: string[] = $state([]);
  let busy = $state(false);
  let message = $state("");
  let confirmingDelete = $state(false);
  let idleSecs = $state(10);
  let rescanHours = $state(0);
  let maxMb = $state(2048);
  let speed: "gentle" | "balanced" | "fast" = $state("balanced");
  let theme: ThemePref = $state("auto");
  // The theme last saved. Choosing a theme previews it live; closing reverts to this.
  let savedTheme: ThemePref = "auto";
  let first: HTMLButtonElement | undefined = $state();

  $effect(() => applyTheme(theme));
  onDestroy(() => applyTheme(savedTheme));

  let discarding = $state(false);

  const LIMITS = { idle: [0, 3600], rescan: [0, 720], mb: [100, 1_000_000] } as const;
  const rangeError = (v: unknown, [lo, hi]: readonly [number, number]) =>
    typeof v === "number" && Number.isInteger(v) && v >= lo && v <= hi
      ? ""
      : `Enter a number from ${lo.toLocaleString("en-US")} to ${hi.toLocaleString("en-US")}`;
  const idleError = $derived(rangeError(idleSecs, LIMITS.idle));
  const rescanError = $derived(rangeError(rescanHours, LIMITS.rescan));
  const mbError = $derived(rangeError(maxMb, LIMITS.mb));
  const invalid = $derived(!!(idleError || rescanError || mbError));

  const dirty = $derived.by(() => {
    const i = info as PrivacyState | null;
    return (
      !!i &&
      !i.needs_setup &&
      (roots.join("|") !== i.roots.join("|") ||
        excluded.join("|") !== i.excluded_folders.join("|") ||
        idleSecs !== i.idle_secs ||
        rescanHours !== i.rescan_hours ||
        maxMb !== i.content_index_max_mb ||
        speed !== i.index_speed ||
        theme !== i.theme)
    );
  });

  // Close, Esc and Ctrl+, all come here: ask first only when there are unsaved edits.
  export function requestClose() {
    if (discarding) discarding = false;
    else if (dirty) discarding = true;
    else onclose();
  }

  const errorText = (e: unknown) => (typeof e === "string" ? e : String(e));

  function load(s: PrivacyState) {
    info = s;
    roots = [...s.roots];
    excluded = [...s.excluded_folders];
    idleSecs = s.idle_secs;
    rescanHours = s.rescan_hours;
    maxMb = s.content_index_max_mb;
    speed = s.index_speed;
    theme = s.theme;
    savedTheme = s.theme;
  }

  onMount(async () => {
    try {
      load(await invoke<PrivacyState>("privacy_state"));
    } catch (e) {
      message = errorText(e);
    }
    first?.focus();
  });

  async function add(list: "roots" | "excluded") {
    try {
      const picked = await invoke<string | null>("pick_folder");
      if (!picked) return;
      const target = list === "roots" ? roots : excluded;
      if (!target.some((p) => p.toLowerCase() === picked.toLowerCase())) target.push(picked);
    } catch (e) {
      message = errorText(e);
    }
  }

  // First start: only the folder choice exists.
  async function save() {
    busy = true;
    message = "";
    try {
      load(await invoke<PrivacyState>("apply_folders", { roots, excluded }));
      message = roots.length > 0 ? "Saved. Getting these folders ready for search." : "Saved. No folders will be searched.";
      if (!info?.needs_setup) onclose();
    } catch (e) {
      message = errorText(e);
    }
    busy = false;
  }

  // The one Save in Settings: one call that checks everything, then applies it all or nothing.
  async function saveAll() {
    if (invalid) return;
    busy = true;
    message = "";
    try {
      load(
        await invoke<PrivacyState>("save_settings", {
          args: {
            roots: [...roots],
            excluded: [...excluded],
            idleSecs,
            rescanHours,
            speed,
            contentIndexMaxMb: maxMb,
            theme,
          },
        }),
      );
      message = "Saved.";
      if (!info?.needs_setup) onclose();
    } catch (e) {
      message = errorText(e);
    }
    busy = false;
  }

  async function setPaused(paused: boolean) {
    await invoke("set_paused", { paused });
    if (info) info.paused = paused;
  }

  let grabMessage = $state("");

  function comboFrom(e: KeyboardEvent): string | null {
    let key: string | null = null;
    if (/^Key[A-Z]$/.test(e.code)) key = e.code.slice(3);
    else if (/^Digit[0-9]$/.test(e.code)) key = e.code.slice(5);
    else if (/^F([1-9]|1[0-9]|2[0-4])$/.test(e.code)) key = e.code;
    else if (e.code === "Space") key = "Space";
    if (!key) return null;
    const mods = [e.ctrlKey && "Ctrl", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Super"].filter(Boolean);
    if (mods.length === 0) {
      grabMessage = "Hold Ctrl, Alt, Shift or Win together with the key.";
      return null;
    }
    return [...mods, key].join("+");
  }

  async function grabKeydown(e: KeyboardEvent) {
    if (e.key === "Tab" || e.key === "Escape") return;
    e.preventDefault();
    e.stopPropagation();
    if (["Control", "Alt", "Shift", "Meta"].includes(e.key)) return;
    grabMessage = "";
    const combo = comboFrom(e);
    if (!combo) {
      if (!grabMessage) grabMessage = "Use a letter, digit, F-key or Space with a modifier.";
      return;
    }
    try {
      // Keep unsaved edits in the other fields: only the hotkey comes from the answer.
      const keep = { roots: [...roots], excluded: [...excluded], idleSecs, rescanHours, maxMb, speed, theme };
      const s = await invoke<PrivacyState>("set_grab_hotkey", { wanted: combo });
      load(s);
      ({ roots, excluded, idleSecs, rescanHours, maxMb, speed, theme } = keep);
      grabMessage = `Saved: ${combo}. Active now.`;
    } catch (err) {
      grabMessage = errorText(err);
    }
  }

  async function deleteIndex() {
    busy = true;
    message = "";
    try {
      load(await invoke<PrivacyState>("delete_index"));
      message = "Your search data and its encryption key were deleted.";
    } catch (e) {
      message = errorText(e);
    }
    confirmingDelete = false;
    busy = false;
  }
</script>

{#snippet folderList(items: string[], empty: string, stop: (p: string) => void, what: string)}
  <ul class="folders">
    {#each items as p (p)}
      <li>
        <span class="path code">{p}</span>
        <button class="btn small" onclick={() => stop(p)} aria-label={`${what} ${p}`}>Remove</button>
      </li>
    {:else}
      <li class="none">{empty}</li>
    {/each}
  </ul>
{/snippet}

{#snippet field(title: string, hint: string)}
  <span class="fl">
    <span class="ft">{title}</span>
    <span class="hint">{hint}</span>
  </span>
{/snippet}

<section class="settings" aria-labelledby="priv-title">
  {#if info?.needs_setup}
    <div class="body setup">
      <h2 id="priv-title">Choose which folders Lookout may search</h2>
      <p class="hint">
        Nothing is read until you choose folders. Everything stays on this PC, encrypted with a key that only
        your Windows account can unlock.
      </p>
      {#if info.open_hotkey}
        <p class="hint">Open Lookout any time with <kbd>{info.open_hotkey}</kbd>.</p>
      {/if}

      <h3 class="label-caps">Folders to search</h3>
      {@render folderList(roots, "No folders chosen", (p) => (roots = roots.filter((r) => r !== p)), "Remove")}
      <button bind:this={first} class="btn" onclick={() => add("roots")}>Add folder…</button>

      <h3 class="label-caps">Folders to skip</h3>
      {@render folderList(excluded, "No exclusions", (p) => (excluded = excluded.filter((r) => r !== p)), "Stop excluding")}
      <button class="btn" onclick={() => add("excluded")}>Exclude folder…</button>
    </div>
    <div class="savebar">
      <span class="msg" role="status">{message}</span>
      <button
        class="btn"
        disabled={busy}
        onclick={() => {
          roots = [];
          excluded = [];
          save();
        }}>Skip for now</button
      >
      <button class="btn primary" disabled={busy} onclick={save}>Start</button>
    </div>
  {:else}
    <div class="layout">
      <nav class="menu" aria-label="Settings sections">
        {#each SECTIONS as [id, name]}
          <button
            class="item"
            class:on={section === id}
            class:danger={id === "delete"}
            aria-current={section === id ? "page" : undefined}
            onclick={() => (section = id)}>{name}</button
          >
        {/each}
      </nav>

      <div class="body">
        {#if section === "general"}
          <h2 id="priv-title">General</h2>
          <label class="field">
            {@render field("Appearance", "Auto follows Windows light or dark mode. The choice applies as you pick it.")}
            <select bind:value={theme}>
              <option value="auto">Auto (follow Windows)</option>
              <option value="light">Light</option>
              <option value="dark">Dark</option>
            </select>
          </label>
          <div class="field">
            {@render field("Accent color", "The one highlight color. Applies as you pick it.")}
            <div class="swatches" role="radiogroup" aria-label="Accent color">
              {#each ACCENTS as a}
                <button
                  type="button"
                  class="swatch"
                  role="radio"
                  aria-checked={look.accent === a.value}
                  aria-label={a.name}
                  title={a.name}
                  style:background={a.value}
                  onclick={() => setAccent(a.value)}
                ></button>
              {/each}
            </div>
          </div>
          <div class="field">
            {@render field("Glass effect", "Frosted glass panels. Off gives flat solid panels with no blur.")}
            <label class="switch">
              <input type="checkbox" role="switch" checked={look.glass} onchange={(e) => setGlass(e.currentTarget.checked)} />
              <span class="track"></span>
            </label>
          </div>
          <div class="field">
            {@render field("Reduce motion", "Stops the orb and result animations. On by default when Windows animations are off.")}
            <label class="switch">
              <input type="checkbox" role="switch" checked={look.reduced} onchange={(e) => setReduceMotion(e.currentTarget.checked)} />
              <span class="track"></span>
            </label>
          </div>
          <div class="field">
            {@render field("Updates", `Version ${appVersion}. Connects to the internet only when you click a button here, and only receives files.`)}
            <button class="btn" disabled={updBusy} onclick={checkUpdates}>Check for updates</button>
          </div>
          {#if upd}
            <div class="field">
              {@render field(`Version ${upd.version} is available`, upd.body ?? "")}
              <button class="btn primary" disabled={updBusy} onclick={installUpdate}>Download and install</button>
            </div>
          {/if}
          {#if updMsg}<p role="status">{updMsg}</p>{/if}
        {:else if section === "folders"}
          <h2 id="priv-title">Folders</h2>
          <h3 class="label-caps">Folders to search</h3>
          <p class="hint">Lookout only reads files inside these folders.</p>
          {@render folderList(roots, "No folders chosen", (p) => (roots = roots.filter((r) => r !== p)), "Remove")}
          <button bind:this={first} class="btn" onclick={() => add("roots")}>Add folder…</button>

          <h3 class="label-caps">Folders to skip</h3>
          <p class="hint">Names, file text and text in pictures in these folders are never read.</p>
          {@render folderList(excluded, "No exclusions", (p) => (excluded = excluded.filter((r) => r !== p)), "Stop excluding")}
          <button class="btn" onclick={() => add("excluded")}>Exclude folder…</button>

          <h3 class="label-caps">Always skipped</h3>
          <p class="hint">These are never read, whatever you choose above.</p>
          <div class="tags">{#each ALWAYS_SKIPPED as t}<span class="tag">{t}</span>{/each}</div>
          <h3 class="label-caps">Not read inside AppData</h3>
          <p class="hint">
            AppData\Local and AppData\Roaming can be found by name, but the contents of these
            cache and log folders are not read. The folders' own names stay searchable.
          </p>
          <div class="tags">{#each skipList as t}<span class="tag">{t}</span>{/each}</div>

          <h3 class="label-caps">Pause</h3>
          <label class="field">
            {@render field("Pause all background work", "Stops preparing files, reading file text and reading text in pictures until you turn it back on.")}
            <span class="switch">
              <input
                type="checkbox"
                role="switch"
                checked={info?.paused ?? false}
                onchange={(e) => setPaused(e.currentTarget.checked)}
              />
              <span class="track"></span>
            </span>
          </label>
        {:else if section === "privacy"}
          <h2 id="priv-title">Privacy</h2>
          <h3 class="label-caps">What is stored</h3>
          <p class="hint">
            Search data: file names, text from your files, your usage history and the undo log. Nothing leaves this PC.
          </p>
          <h3 class="label-caps">How it is protected</h3>
          <p class="hint">
            Everything is encrypted with a key that only your Windows account can unlock. Secrets such as API keys,
            private keys and card numbers are masked before text is stored.
          </p>
        {:else if section === "speed"}
          <h2 id="priv-title">Speed</h2>
          <label class="field">
            {@render field("Background speed", "Only applies while you are idle.")}
            <select bind:value={speed}>
              <option value="gentle">Gentle</option>
              <option value="balanced">Balanced</option>
              <option value="fast">Fast</option>
            </select>
          </label>
          <label class="field">
            {@render field("Wait before reading", "Seconds you must be idle before file text is read.")}
            <span class="num">
              <input type="number" bind:value={idleSecs} aria-invalid={!!idleError} aria-describedby="e-idle" />
              <span id="e-idle" class="err" role="alert">{idleError}</span>
            </span>
          </label>
          <label class="field">
            {@render field("Check for changes every", "Hours between full checks. 0 means never.")}
            <span class="num">
              <input type="number" bind:value={rescanHours} aria-invalid={!!rescanError} aria-describedby="e-rescan" />
              <span id="e-rescan" class="err" role="alert">{rescanError}</span>
            </span>
          </label>
          <label class="field">
            {@render field("File text size limit", "Most disk space for saved file text, in MB (at least 100).")}
            <span class="num">
              <input type="number" bind:value={maxMb} aria-invalid={!!mbError} aria-describedby="e-mb" />
              <span id="e-mb" class="err" role="alert">{mbError}</span>
            </span>
          </label>
          <p class="note">
            Gentle works at most 10% of the time. Balanced (default) up to 25%. Fast up to 60%, under one CPU core.
          </p>
        {:else if section === "addons"}
          <h2 id="priv-title">Add-ons</h2>
          <Plugins />
        {:else if section === "shortcuts"}
          <h2 id="priv-title">Shortcuts</h2>
          <div class="field">
            {@render field("Read text from the screen", "Click the box, then press the new combination.")}
            <input class="code" type="text" readonly value={info?.grab_hotkey ?? ""} onkeydown={grabKeydown} aria-label="Grab text hotkey" />
          </div>
          {#if grabMessage}<p class="note" role="status">{grabMessage}</p>{/if}
          <div class="field">
            {@render field("Pause", "Pauses and resumes background work from anywhere.")}
            {#if info?.pause_hotkey}<kbd>{info.pause_hotkey}</kbd>{/if}
          </div>
        {:else if section === "status"}
          <h2 id="priv-title">Status</h2>
          <Health />
        {:else}
          <h2 id="priv-title" class="danger-text">Delete my data</h2>
          {#if confirmingDelete}
            <p class="warning" role="alert">
              This deletes your saved file names, file text, usage history, the undo log and the encryption key.
              Your files are not touched. You will be asked to choose folders again.
            </p>
            <div class="row">
              <button class="btn" onclick={() => (confirmingDelete = false)}>Cancel</button>
              <button class="btn danger" disabled={busy} onclick={deleteIndex}>Delete everything</button>
            </div>
          {:else}
            <p class="hint">Removes the search data Lookout stored about your files. Your files are not touched.</p>
            <p><button class="btn danger" onclick={() => (confirmingDelete = true)}>Delete my search data…</button></p>
          {/if}
        {/if}
      </div>
    </div>
    <div class="savebar">
      {#if discarding}
        <span class="msg" role="alertdialog" aria-label="Discard changes?">Discard changes?</span>
        <button class="btn" onclick={() => (discarding = false)}>Keep editing</button>
        <button class="btn danger" onclick={onclose}>Discard</button>
      {:else}
        <span class="msg" role="status">
          {#if invalid && section !== "speed"}Some numbers in Speed are not valid.{:else}{message}{/if}
        </span>
        <button class="btn" onclick={requestClose}>Close <kbd>Esc</kbd></button>
        <button class="btn primary" disabled={busy || invalid} onclick={saveAll}>Save</button>
      {/if}
    </div>
  {/if}
</section>

<style>
  .settings {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    font-size: 14px;
  }
  .layout {
    flex: 1;
    min-height: 0;
    display: flex;
  }
  .menu {
    flex: none;
    width: 170px;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    gap: 0;
    padding: 8px;
    border-right: 1px solid var(--border);
    background: var(--panel);
    overflow-y: auto;
  }
  .item {
    all: unset;
    box-sizing: border-box;
    padding: 5px 12px;
    border-radius: 8px;
    border-left: 2px solid transparent;
    color: var(--text-soft);
    cursor: pointer;
  }
  .item:hover {
    background: var(--raised);
  }
  .item.on {
    background: var(--raised);
    border-left-color: var(--accent);
    color: var(--text);
    font-weight: 600;
  }
  .item.danger {
    color: var(--danger);
  }
  .item:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .body {
    flex: 1;
    min-width: 0;
    min-height: 0;
    overflow-y: auto;
    padding: 14px 22px 18px;
  }
  .body.setup {
    padding-top: 16px;
  }
  h2 {
    margin: 0 0 8px;
    font-size: 20px;
    font-weight: 600;
  }
  .danger-text {
    color: var(--danger);
  }
  h3 {
    margin: 18px 0 2px;
  }
  .folders {
    list-style: none;
    margin: 6px 0 8px;
    padding: 0;
  }
  .folders li {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 3px 0;
  }
  .folders .path {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 13px;
    color: var(--text-soft);
  }
  .none {
    color: var(--muted);
  }
  .tags {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 8px;
  }
  .field {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 20px;
    padding: 10px 0;
    border-bottom: 1px solid var(--border);
  }
  .swatches {
    display: flex;
    gap: 10px;
  }
  .swatch {
    all: unset;
    box-sizing: border-box;
    width: 24px;
    height: 24px;
    border-radius: 50%;
    cursor: pointer;
    border: 2px solid transparent;
  }
  .swatch[aria-checked="true"] {
    border-color: var(--text);
    box-shadow: 0 0 0 2px var(--solid-bg) inset;
  }
  .swatch:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .fl {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .ft {
    font-weight: 600;
  }
  .num {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 3px;
  }
  .err {
    color: var(--danger);
    font-size: 12px;
  }
  input[aria-invalid="true"] {
    border-color: var(--danger);
  }
  input[type="number"] {
    width: 110px;
    text-align: right;
  }
  input[type="text"][readonly] {
    width: 190px;
  }
  .row {
    display: flex;
    gap: 8px;
  }
  .savebar {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 8px;
    padding: 10px 20px;
    border-top: 1px solid var(--border);
    background: var(--panel);
  }
  .msg {
    flex: 1;
    min-width: 0;
    color: var(--text-soft);
    font-size: 13px;
  }
</style>
