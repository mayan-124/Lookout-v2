<script lang="ts">
  // First-run setup: five short steps, each skippable. The bar stays usable;
  // typing in the search box puts this panel aside until the box is empty again.
  import { invoke } from "@tauri-apps/api/core";
  import { onMount, tick } from "svelte";
  import ThemePicker from "./ThemePicker.svelte";
  import ModePicker from "./ModePicker.svelte";
  import { captureCombo } from "./hotkey";
  import { STEPS, TRY_EXAMPLES } from "./setupflow";

  interface FolderChoice {
    path: string;
    checked: boolean;
  }
  interface Saved {
    open_hotkey: string;
    index_contents: boolean;
    ocr: boolean;
    start_with_windows: boolean;
    setup_start_with_windows: boolean;
  }

  let {
    step = $bindable(),
    onclose,
    ontry,
    onchanged,
  }: {
    step: number;
    /** Finished or skipped. */
    onclose: () => void;
    /** An example was picked: close and search for it. */
    ontry: (text: string) => void;
    /** Folders or the hotkey were saved. */
    onchanged: () => void;
  } = $props();

  let folders: FolderChoice[] = $state([]);
  let contents = $state(true);
  let ocr = $state(true);
  let hotkey = $state("");
  let startup = $state(false);
  let startupSaved = false;
  let loaded = $state(false);
  let busy = $state(false);
  let message = $state("");
  let hotkeyMessage = $state("");
  let panel: HTMLElement | undefined = $state();
  let primary: HTMLButtonElement | undefined = $state();

  const anyFolder = $derived(folders.some((f) => f.checked));
  const errorText = (e: unknown) => (typeof e === "string" ? e : String(e));

  onMount(async () => {
    try {
      const [list, saved] = await Promise.all([
        invoke<FolderChoice[]>("setup_folders"),
        invoke<Saved>("privacy_state"),
      ]);
      folders = list;
      contents = saved.index_contents;
      ocr = saved.ocr;
      hotkey = saved.open_hotkey;
      startup = saved.setup_start_with_windows;
      startupSaved = saved.start_with_windows;
    } catch (e) {
      message = errorText(e);
    }
    loaded = true;
  });

  /** Puts the focus on the step's main button, so Enter goes on. */
  export function focus() {
    const target = primary && !primary.disabled ? primary : panel?.querySelector<HTMLElement>(".sbody input, .sbody button");
    target?.focus();
  }
  $effect(() => {
    step;
    loaded;
    tick().then(focus);
  });

  function go(to: number) {
    message = "";
    hotkeyMessage = "";
    step = to;
  }

  async function addFolder() {
    try {
      const picked = await invoke<string | null>("pick_folder");
      if (!picked) return;
      const have = folders.find((f) => f.path.toLowerCase() === picked.toLowerCase());
      if (have) have.checked = true;
      else folders.push({ path: picked, checked: true });
    } catch (e) {
      message = errorText(e);
    }
  }

  async function startIndexing() {
    busy = true;
    message = "";
    try {
      await invoke("setup_start", {
        roots: folders.filter((f) => f.checked).map((f) => f.path),
        indexContents: contents,
        ocr,
      });
      onchanged();
      go(step + 1);
    } catch (e) {
      message = errorText(e);
    }
    busy = false;
  }

  async function saveStartup() {
    message = "";
    try {
      const s = await invoke<Saved>("set_start_with_windows", { on: startup });
      startupSaved = s.start_with_windows;
      return true;
    } catch (e) {
      startup = startupSaved;
      message = errorText(e);
      return false;
    }
  }

  // "Next" on the shortcuts step also applies a tick that was left as offered.
  async function next() {
    if (step === 3) {
      busy = true;
      const ok = await saveStartup();
      busy = false;
      if (!ok) return;
    }
    go(step + 1);
  }

  async function hotkeyKeydown(e: KeyboardEvent) {
    const got = captureCombo(e);
    if (!got) return;
    if ("hint" in got) {
      hotkeyMessage = got.hint;
      return;
    }
    try {
      const s = await invoke<Saved>("set_open_hotkey", { wanted: got.combo });
      hotkey = s.open_hotkey;
      hotkeyMessage = `Saved. Press ${s.open_hotkey} to open Lookout.`;
      onchanged();
    } catch (err) {
      hotkeyMessage = errorText(err);
    }
  }
</script>

<section class="setup" aria-labelledby="setup-title" bind:this={panel}>
  <div class="progress">
    <ol class="steps" aria-label="Setup steps">
      {#each STEPS as name, n}
        <li class:done={n + 1 < step} class:on={n + 1 === step} aria-current={n + 1 === step ? "step" : undefined}>
          <span class="sr">{name}</span>
        </li>
      {/each}
    </ol>
    <span class="count">Step {step} of {STEPS.length}</span>
  </div>

  <div class="sbody">
    {#if step === 1}
      <h2 id="setup-title">Welcome to Lookout</h2>
      <p class="lead">Find files, apps and things you saw, all on this PC. Nothing leaves it.</p>
      <p class="hint">Takes about a minute. Run it again from Settings.</p>
    {:else if step === 2}
      <h2 id="setup-title">What should Lookout search?</h2>
      <ul class="folders">
        {#each folders as f (f.path)}
          <li>
            <label>
              <input type="checkbox" bind:checked={f.checked} />
              <span class="code path">{f.path}</span>
            </label>
          </li>
        {:else}
          <li class="hint">{loaded ? "No folders yet. Add one below." : "Looking for your folders…"}</li>
        {/each}
      </ul>
      <button type="button" class="btn small" onclick={addFolder}>Add folder…</button>
      <p class="hint">Always skipped: node_modules, .git, Windows, Program Files, browser profiles and password-manager folders.</p>
      <label class="field">
        <span class="fl">
          <span class="ft">Search inside files</span>
          <span class="hint">Find files by the words written in them.</span>
        </span>
        <span class="switch">
          <input type="checkbox" role="switch" bind:checked={contents} />
          <span class="track"></span>
        </span>
      </label>
      <label class="field">
        <span class="fl">
          <span class="ft">Read text in pictures</span>
          <span class="hint">Find screenshots and photos by the words in them.</span>
        </span>
        <span class="switch">
          <input type="checkbox" role="switch" bind:checked={ocr} />
          <span class="track"></span>
        </span>
      </label>
      {#if loaded && !anyFolder}<p class="hint" role="status">Tick or add a folder to start.</p>{/if}
    {:else if step === 3}
      <h2 id="setup-title">Shortcuts</h2>
      <label class="field">
        <span class="fl">
          <span class="ft">Open Lookout</span>
          <span class="hint">Click the box, then press new keys.</span>
        </span>
        <input class="code combo" type="text" readonly value={hotkey} onkeydown={hotkeyKeydown} aria-label="Open Lookout hotkey" />
      </label>
      {#if hotkeyMessage}<p class="note" role="status">{hotkeyMessage}</p>{/if}
      <label class="field">
        <span class="fl">
          <span class="ft">Start Lookout when I sign in to Windows</span>
          <span class="hint">Keeps the hotkey working after restarts.</span>
        </span>
        <span class="switch">
          <input type="checkbox" role="switch" bind:checked={startup} disabled={!loaded} onchange={saveStartup} />
          <span class="track"></span>
        </span>
      </label>
    {:else if step === 4}
      <h2 id="setup-title">Make it yours</h2>
      <ModePicker />
      <ThemePicker />
    {:else}
      <h2 id="setup-title">Try it</h2>
      <p class="hint">Pick one to try it.</p>
      <ul class="tries">
        {#each TRY_EXAMPLES as [example, what]}
          <li>
            <button type="button" class="try" onclick={() => ontry(example)}>
              <span class="code">{example}</span>
              <span class="hint">{what}</span>
            </button>
          </li>
        {/each}
      </ul>
      <p class="lead">Type <kbd>help</kbd> to see what you can do.</p>
    {/if}
  </div>

  <div class="sfoot">
    {#if step < STEPS.length}
      <button type="button" class="btn text" onclick={onclose}>Skip setup <kbd>Esc</kbd></button>
    {/if}
    <span class="msg" role="status">{message}</span>
    {#if step > 1}<button type="button" class="btn" onclick={() => go(step - 1)}>Back</button>{/if}
    {#if step === 1}
      <button bind:this={primary} type="button" class="btn primary" onclick={() => go(2)}>Start setup</button>
    {:else if step === 2}
      <button type="button" class="btn" onclick={() => go(3)}>Skip this step</button>
      <button bind:this={primary} type="button" class="btn primary" disabled={busy || !anyFolder} onclick={startIndexing}>Start indexing</button>
    {:else if step < STEPS.length}
      <button bind:this={primary} type="button" class="btn primary" disabled={busy || !loaded} onclick={next}>Next</button>
    {:else}
      <button bind:this={primary} type="button" class="btn primary" onclick={onclose}>Done</button>
    {/if}
  </div>
</section>

<style>
  .setup {
    display: flex;
    flex-direction: column;
    max-height: 480px;
    font-size: 14px;
  }
  .progress {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 18px 24px 0;
  }
  .steps {
    display: flex;
    gap: 6px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .steps li {
    width: 28px;
    height: 4px;
    border-radius: 2px;
    background: var(--toggle-off);
  }
  .steps li.done {
    background: var(--accent-line);
  }
  .steps li.on {
    background: var(--accent);
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
  .count {
    color: var(--muted);
    font-size: 12px;
  }
  .sbody {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 10px 24px 14px;
  }
  h2 {
    margin: 4px 0 8px;
    font-size: 20px;
    font-weight: 600;
  }
  .lead {
    margin: 8px 0;
    font-size: var(--fs-md);
    color: var(--text-soft);
  }
  .folders {
    list-style: none;
    margin: 0 0 8px;
    padding: 0;
  }
  .folders label {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 4px 0;
    cursor: pointer;
  }
  .path {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 13px;
    color: var(--text-soft);
  }
  .field {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 20px;
    padding: 8px 0;
    border-top: 1px solid var(--border);
  }
  .fl {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .ft {
    font-weight: 600;
  }
  .combo {
    width: 190px;
  }
  .tries {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin: 10px 0;
    padding: 0;
    list-style: none;
  }
  .try {
    all: unset;
    box-sizing: border-box;
    width: 100%;
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    padding: 10px 14px;
    border: 1px solid var(--hairline);
    border-radius: var(--r-md);
    background: var(--btn-bg);
    cursor: pointer;
  }
  .try:hover {
    background: var(--btn-bg-hover);
  }
  .try:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .sfoot {
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px 20px;
    border-top: 1px solid var(--border);
  }
  .msg {
    flex: 1;
    min-width: 0;
    color: var(--text-soft);
    font-size: 13px;
  }
</style>
