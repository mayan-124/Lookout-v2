<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount, tick } from "svelte";
  import Plugins from "./Plugins.svelte";
  import Health from "./Health.svelte";
  import ThemePicker from "./ThemePicker.svelte";
  import ModePicker from "./ModePicker.svelte";
  import { modeLabel, type Feature } from "./modes";
  import { resetToRecommended, setAdvancedOpen, setHideAfterCopy, setHideTips, setShow, ui } from "./uistate.svelte";
  import { captureCombo } from "./hotkey";
  import { look, setGlass, setReduceMotion } from "./look.svelte";
  import { getVersion } from "@tauri-apps/api/app";
  import { plain as errorText } from "./errors";
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
    cli_access: boolean;
    start_with_windows: boolean;
    remember_searches: boolean;
    remember_last_search: boolean;
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
      updMsg = "Could not check for updates. Check your internet connection.";
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
      updMsg = "Could not download or check the update, so nothing changed. Try again later.";
      updBusy = false;
    }
  }

  // Quick keys: list, add, rename, delete. Targets that are apps or system actions
  // are set from a search result ("Set quick key"); here a path or web address is typed.
  interface QuickKey {
    alias: string;
    title: string;
    subtitle: string | null;
    kind: string;
  }
  let qkeys: QuickKey[] = $state([]);
  let qkAlias = $state("");
  let qkTarget = $state("");
  let qkError = $state("");
  let qkEditing: string | null = $state(null);
  let qkEditAlias = $state("");
  async function loadKeys() {
    try {
      qkeys = await invoke<QuickKey[]>("quickkey_list");
    } catch {
      qkeys = [];
    }
  }
  async function addKey() {
    qkError = "";
    try {
      await invoke("quickkey_add", { alias: qkAlias, target: qkTarget });
      qkAlias = "";
      qkTarget = "";
      await loadKeys();
    } catch (e) {
      qkError = errorText(e);
    }
  }
  async function saveEdit(old: string) {
    qkError = "";
    try {
      await invoke("quickkey_rename", { old, alias: qkEditAlias });
      qkEditing = null;
      await loadKeys();
    } catch (e) {
      qkError = errorText(e);
    }
  }
  async function deleteKey(alias: string) {
    qkError = "";
    try {
      await invoke("quickkey_delete", { alias });
    } catch (e) {
      qkError = errorText(e);
    }
    await loadKeys();
  }
  onMount(loadKeys);

  // Basic is the General page; everything else sits under "Advanced".
  type Section =
    | "general"
    | "features"
    | "folders"
    | "privacy"
    | "speed"
    | "addons"
    | "quickkeys"
    | "repair"
    | "delete";
  const ADVANCED: [Section, string][] = [
    ["features", "Features"],
    ["folders", "Skipped folders"],
    ["privacy", "Privacy"],
    ["speed", "Speed"],
    ["addons", "Add-ons"],
    ["quickkeys", "Quick keys"],
    ["repair", "Repair"],
    ["delete", "Delete my data"],
  ];

  // Repair: diagnostics, the log folder and the index rebuild.
  let confirmingRebuild = $state(false);
  let rebuildLine = $state("");
  let repairNote = $state("");
  async function repair(command: string, done = "") {
    repairNote = "";
    try {
      await invoke(command);
      repairNote = done;
    } catch (e) {
      repairNote = errorText(e);
    }
  }
  async function rebuildIndex() {
    confirmingRebuild = false;
    await repair("rebuild_index");
    await pollRebuild();
  }
  async function pollRebuild() {
    try {
      const p = await invoke<{ line: string; done: boolean } | null>("rebuild_status");
      // A finished rebuild is reported once; its line stays on the page.
      if (p) rebuildLine = p.line;
    } catch {
      // The next poll tries again.
    }
  }
  $effect(() => {
    if (section !== "repair") return;
    pollRebuild();
    const timer = setInterval(pollRebuild, 1000);
    return () => clearInterval(timer);
  });
  // [switch, name, what it is, where it shows], under the feature families.
  const FEATURES: [string, [Feature, string, string, string][]][] = [
    [
      "Find",
      [
        ["quick_keys", "Quick keys", "Open things with a short word.", "Shows in help, tips and results."],
        ["filters", "Date and kind filters", "Search like “pdf last week”.", "Shows in help and tips."],
      ],
    ],
    [
      "Capture",
      [
        ["capture", "Capture", "Take screenshots and recordings.", "Shows in the dock and help."],
        ["clipboard", "Clipboard", "Find text you copied.", "Shows in the dock and help."],
      ],
    ],
    [
      "Everyday",
      [
        ["timers", "Timers", "Count down from the search box.", "Shows in help and tips."],
        ["notes", "Notes", "Save short notes from the search box.", "Shows in help, tips and Home."],
      ],
    ],
    ["This PC", [["disk", "Disk analyzer", "See what uses space.", "Shows in the dock and help."]]],
  ];
  const ALWAYS_SKIPPED = [
    "Lookout's own data and cache folders",
    "AppData (unless you add a folder inside it)",
    "node_modules",
    ".git",
    "Windows",
    "Program Files",
    "browser profiles",
    "password-manager folders",
  ];

  let {
    onclose,
    onsetup,
    start = "general",
  }: { onclose: () => void; onsetup: () => void; start?: "general" | "appearance" | "privacy" } = $props();

  let skipList: string[] = $state([]);
  invoke<string[]>("skip_list").then((l) => (skipList = l)).catch(() => {});

  let info: PrivacyState | null = $state(null);
  let section: Section = $state("general");
  let roots: string[] = $state([]);
  let excluded: string[] = $state([]);
  let busy = $state(false);
  let message = $state("");
  let confirmingDelete = $state(false);
  let idleSecs = $state(10);
  let rescanHours = $state(0);
  let maxMb = $state(2048);
  let speed: "gentle" | "balanced" | "fast" = $state("balanced");
  let first: HTMLButtonElement | undefined = $state();
  let appearanceCard: HTMLDivElement | undefined = $state();

  // Chosen folders that cannot be reached right now: kept, and flagged.
  let missing: string[] = $state([]);
  $effect(() => {
    const paths = [...roots];
    let live = true;
    invoke<string[]>("missing_folders", { paths })
      .then((m) => live && (missing = m))
      .catch(() => {});
    return () => {
      live = false;
    };
  });

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
      (roots.join("|") !== i.roots.join("|") ||
        excluded.join("|") !== i.excluded_folders.join("|") ||
        idleSecs !== i.idle_secs ||
        rescanHours !== i.rescan_hours ||
        maxMb !== i.content_index_max_mb ||
        speed !== i.index_speed)
    );
  });

  // Close, Esc and Ctrl+, all come here: an open question is cancelled first,
  // then unsaved edits are asked about.
  export function requestClose() {
    // Only a question on the open page counts.
    if (section === "delete" && confirmingDelete) confirmingDelete = false;
    else if (section === "general" && confirmingReset) confirmingReset = false;
    else if (section === "privacy" && (askClear || askClearNotes)) askClear = askClearNotes = false;
    else if (section === "quickkeys" && qkEditing !== null) qkEditing = null;
    else if (section === "repair" && confirmingRebuild) confirmingRebuild = false;
    else if (discarding) discarding = false;
    else if (dirty) discarding = true;
    else onclose();
  }

  function load(s: PrivacyState) {
    info = s;
    roots = [...s.roots];
    excluded = [...s.excluded_folders];
    idleSecs = s.idle_secs;
    rescanHours = s.rescan_hours;
    maxMb = s.content_index_max_mb;
    speed = s.index_speed;
  }

  onMount(async () => {
    try {
      load(await invoke<PrivacyState>("privacy_state"));
    } catch {
      message = "Could not load your settings. Close Settings and open it again.";
    }
    first?.focus();
    // Opened for one thing: go straight to it.
    if (start === "privacy") {
      section = "privacy";
      choose(setAdvancedOpen(true));
    } else if (start === "appearance") {
      await tick();
      appearanceCard?.scrollIntoView({ block: "start" });
      // The theme in use, so one arrow press moves to its neighbour.
      appearanceCard?.querySelector<HTMLElement>('[role="radio"][aria-checked="true"]')?.focus({ preventScroll: true });
    }
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
          },
        }),
      );
      message = "Saved.";
      onclose();
    } catch (e) {
      message = errorText(e);
    }
    busy = false;
  }

  let pauseMessage = $state("");
  async function setPaused(el: HTMLInputElement) {
    pauseMessage = "";
    try {
      await invoke("set_paused", { paused: el.checked });
      if (info) info.paused = el.checked;
    } catch (e) {
      el.checked = !el.checked;
      pauseMessage = errorText(e);
    }
  }

  let cliMessage = $state("");
  async function setCliAccess(el: HTMLInputElement) {
    cliMessage = "";
    try {
      const s = await invoke<PrivacyState>("set_cli_access", { on: el.checked });
      if (info) info.cli_access = s.cli_access;
    } catch (e) {
      el.checked = false;
      if (info) info.cli_access = false;
      cliMessage = errorText(e);
    }
  }

  let historyMessage = $state("");
  // "Keep copied images" is a setting of the Clipboard add-on.
  type Addon = { id: string; config: Record<string, unknown> };
  let keepImages = $state(false);
  let imagesMessage = $state("");
  const showImages = (list: Addon[]) =>
    (keepImages = list.find((p) => p.id === "com.lookout.clipboard")?.config.keep_images === "on");
  invoke<Addon[]>("plugins_list").then(showImages).catch(() => {});
  async function setKeepImages(el: HTMLInputElement) {
    imagesMessage = "";
    try {
      showImages(await invoke<Addon[]>("set_keep_images", { on: el.checked }));
    } catch (e) {
      imagesMessage = errorText(e);
    }
    el.checked = keepImages;
  }

  // Asked once each time remembering is switched off.
  let askClear = $state(false);
  async function setRememberSearches(el: HTMLInputElement) {
    historyMessage = "";
    askClear = false;
    try {
      const s = await invoke<PrivacyState>("set_remember_searches", { on: el.checked });
      if (info) info.remember_searches = s.remember_searches;
      askClear = !s.remember_searches;
    } catch (e) {
      el.checked = !el.checked;
      historyMessage = errorText(e);
    }
  }
  async function clearHistory() {
    askClear = false;
    try {
      await invoke("clear_search_history");
      historyMessage = "Deleted your search history.";
    } catch (e) {
      historyMessage = errorText(e);
    }
  }

  let notesMessage = $state("");
  // Cleared notes cannot be undone, so it is asked first.
  let askClearNotes = $state(false);
  async function clearNotes() {
    askClearNotes = false;
    try {
      await invoke("clear_notes");
      notesMessage = "Deleted all your notes.";
    } catch (e) {
      notesMessage = errorText(e);
    }
  }

  let windowMessage = $state("");
  async function setRememberLast(el: HTMLInputElement) {
    windowMessage = "";
    try {
      const s = await invoke<PrivacyState>("set_remember_last_search", { on: el.checked });
      if (info) info.remember_last_search = s.remember_last_search;
    } catch (e) {
      el.checked = !el.checked;
      windowMessage = errorText(e);
    }
  }
  async function resetWindowPosition() {
    windowMessage = "";
    try {
      await invoke("reset_window_position");
      windowMessage = "Window position reset.";
    } catch (e) {
      windowMessage = errorText(e);
    }
  }

  let startupMessage = $state("");
  async function setStartWithWindows(el: HTMLInputElement) {
    startupMessage = "";
    try {
      const s = await invoke<PrivacyState>("set_start_with_windows", { on: el.checked });
      if (info) info.start_with_windows = s.start_with_windows;
    } catch (e) {
      el.checked = !el.checked;
      startupMessage = errorText(e);
    }
  }

  let contentMessage = $state("");
  async function setContentOption(which: "index_contents" | "ocr", el: HTMLInputElement) {
    contentMessage = "";
    if (!info) return;
    const want = { index_contents: info.index_contents, ocr: info.ocr, [which]: el.checked };
    try {
      const s = await invoke<PrivacyState>("set_content_options", {
        indexContents: want.index_contents,
        ocr: want.ocr,
      });
      if (info) {
        info.index_contents = s.index_contents;
        info.ocr = s.ocr;
      }
    } catch (e) {
      el.checked = !el.checked;
      contentMessage = errorText(e);
    }
  }

  let grabMessage = $state("");
  let openMessage = $state("");

  /** Saves a hotkey from a capture box; returns the line to show under it. */
  async function hotkeyKeydown(command: "set_grab_hotkey" | "set_open_hotkey", e: KeyboardEvent, show: (text: string) => void) {
    const got = captureCombo(e);
    if (!got) return;
    if ("hint" in got) {
      show(got.hint);
      return;
    }
    show("");
    const combo = got.combo;
    try {
      // Keep unsaved edits in the other fields: only the hotkey comes from the answer.
      const keep = { roots: [...roots], excluded: [...excluded], idleSecs, rescanHours, maxMb, speed };
      const s = await invoke<PrivacyState>(command, { wanted: combo });
      load(s);
      ({ roots, excluded, idleSecs, rescanHours, maxMb, speed } = keep);
      show(`Saved: ${combo}. Active now.`);
    } catch (err) {
      show(errorText(err));
    }
  }

  // Interface choices: each applies at once.
  let uiError = $state("");
  const choose = async (change: Promise<string>) => (uiError = await change);
  function toggleAdvanced() {
    if (ui.advanced_open) section = "general";
    choose(setAdvancedOpen(!ui.advanced_open));
  }
  let confirmingReset = $state(false);
  async function reset() {
    confirmingReset = false;
    section = "general";
    await choose(resetToRecommended());
    if (!uiError) message = "Reset to recommended.";
  }

  async function deleteIndex() {
    busy = true;
    message = "";
    try {
      load(await invoke<PrivacyState>("delete_index"));
      message = "Deleted your data and its key.";
    } catch (e) {
      message = errorText(e);
    }
    confirmingDelete = false;
    busy = false;
  }
</script>

{#snippet folderList(items: string[], empty: string, stop: (p: string) => void, what: string, away: string[] = [])}
  <ul class="folders">
    {#each items as p (p)}
      <li>
        <span class="path code">{p}</span>
        {#if away.includes(p)}<span class="away">Not reachable</span>{/if}
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

{#snippet toggle(title: string, hint: string, on: boolean, change: (el: HTMLInputElement) => void, disabled = false)}
  <label class="field">
    {@render field(title, hint)}
    <span class="switch">
      <input type="checkbox" role="switch" checked={on} {disabled} onchange={(e) => change(e.currentTarget)} />
      <span class="track"></span>
    </span>
  </label>
{/snippet}

<!-- One control, shown on Privacy and on Features. -->
{#snippet cliSwitch()}
  {@render toggle(
    "Command line access",
    "Any program running as you can search your files and screen text through this. Off unless you need it.",
    info?.cli_access ?? false,
    setCliAccess,
  )}
  {#if cliMessage}<p class="hint" role="alert">{cliMessage}</p>{/if}
{/snippet}

<section class="settings" aria-labelledby="priv-title">
  <div class="layout">
    <nav class="menu" aria-label="Settings sections">
      <button class="item" class:on={section === "general"} aria-current={section === "general" ? "page" : undefined} onclick={() => (section = "general")}
        >General</button
      >
      <button class="item fold" aria-expanded={ui.advanced_open} onclick={toggleAdvanced}>{ui.advanced_open ? "▾" : "▸"} Advanced</button>
      {#if ui.advanced_open}
        {#each ADVANCED as [id, name]}
          <button
            class="item sub"
            class:on={section === id}
            class:danger={id === "delete"}
            aria-current={section === id ? "page" : undefined}
            onclick={() => (section = id)}>{name}</button
          >
        {/each}
      {/if}
    </nav>

    <div class="body">
      {#if uiError}<p class="danger-text" role="alert">{uiError}</p>{/if}
      {#if section === "general"}
        <h2 id="priv-title">General</h2>
        <div class="scard">
          <h3 class="stitle">Folders</h3>
          <p class="hint">Lookout only reads files inside these folders.</p>
          {@render folderList(roots, "No folders chosen", (p) => (roots = roots.filter((r) => r !== p)), "Remove", missing)}
          {#if roots.some((p) => missing.includes(p))}
            <p class="hint" role="status">A folder that is not reachable stays chosen: reconnect it and restart Lookout, or remove it.</p>
          {/if}
          <p><button bind:this={first} class="btn" onclick={() => add("roots")}>Add folder…</button></p>
        </div>
        <div class="scard">
          <h3 class="stitle">Shortcuts</h3>
          <div class="field">
            {@render field("Open Lookout", "Select the box, then press the new keys.")}
            <input
              class="code"
              type="text"
              readonly
              value={info?.open_hotkey ?? ""}
              onkeydown={(e) => hotkeyKeydown("set_open_hotkey", e, (t) => (openMessage = t))}
              aria-label="Open Lookout shortcut"
            />
          </div>
          {#if openMessage}<p class="note" role="status">{openMessage}</p>{/if}
          <div class="field">
            {@render field("Text from screen", "Select the box, then press the new keys.")}
            <input
              class="code"
              type="text"
              readonly
              value={info?.grab_hotkey ?? ""}
              onkeydown={(e) => hotkeyKeydown("set_grab_hotkey", e, (t) => (grabMessage = t))}
              aria-label="Text from screen shortcut"
            />
          </div>
          {#if grabMessage}<p class="note" role="status">{grabMessage}</p>{/if}
          <div class="field">
            {@render field("Pause", "Pauses and resumes background work from anywhere.")}
            {#if info?.pause_hotkey}<kbd>{info.pause_hotkey}</kbd>{/if}
          </div>
        </div>
        <div class="scard" bind:this={appearanceCard}>
          <h3 class="stitle">Appearance</h3>
          <ThemePicker part="theme" />
          <ThemePicker part="accent" />
          {@render toggle("Glass effect", "Tinted panels. Off makes them solid.", look.glass, (el) => setGlass(el.checked))}
          {@render toggle("Reduce motion", "Stops animations. Follows Windows until you change it.", look.reduced, (el) => setReduceMotion(el.checked))}
          {@render toggle("Tips on Home", "Short examples of what to type.", !ui.hide_tips, (el) => choose(setHideTips(!el.checked)))}
        </div>
        <div class="scard">
          <h3 class="stitle">Mode</h3>
          <ModePicker label="Current" />
        </div>
        <div class="scard">
          <h3 class="stitle">Start with Windows</h3>
          {@render toggle("Open at sign-in", "Keeps the shortcut working after restarts.", info?.start_with_windows ?? false, setStartWithWindows, !info)}
          {#if startupMessage}<p class="hint" role="alert">{startupMessage}</p>{/if}
        </div>
        <div class="scard">
          <h3 class="stitle">Setup</h3>
          <div class="field">
            {@render field("First-run setup", dirty ? "Save or discard your changes first." : "Folders, shortcuts, mode and theme, step by step.")}
            <button class="btn" disabled={dirty} onclick={onsetup}>Run setup again</button>
          </div>
        </div>
        <div class="scard">
          <h3 class="stitle">Updates</h3>
          <div class="field">
            {@render field(`Version ${appVersion}`, "Connects to the internet only when you click a button here, and only receives files.")}
            <button class="btn" disabled={updBusy} onclick={checkUpdates}>Check for updates</button>
          </div>
          {#if upd}
            <div class="field">
              {@render field(`Version ${upd.version} is available`, upd.body ?? "")}
              <button class="btn primary" disabled={updBusy} onclick={installUpdate}>Download and install</button>
            </div>
          {/if}
          {#if updMsg}<p role="status">{updMsg}</p>{/if}
        </div>
        <div class="scard">
          <h3 class="stitle">Status</h3>
          <Health />
        </div>
        <div class="scard">
          {#if confirmingReset}
            <p class="warning" role="alert">
              Reset mode, features, tips, theme and layout? Folders, privacy settings and saved data stay.
            </p>
            <div class="row end">
              <button class="btn" onclick={() => (confirmingReset = false)}>Cancel</button>
              <button class="btn primary" onclick={reset}>Reset</button>
            </div>
          {:else}
            <div class="field">
              {@render field("Recommended settings", "Restores what you see. Your data stays.")}
              <button class="btn" onclick={() => (confirmingReset = true)}>Reset to recommended</button>
            </div>
          {/if}
        </div>
      {:else if section === "features"}
        <h2 id="priv-title">Features</h2>
        <p class="hint">Mode: {modeLabel(ui)}. Hidden features still work when you type them.</p>
        {#each FEATURES as [family, switches] (family)}
          <h3 class="label-caps">{family}</h3>
          {#each switches as [key, name, what, where] (key)}
            {@render toggle(name, `${what} ${where}`, ui.show[key], (el) => choose(setShow(key, el.checked)))}
          {/each}
          {#if family === "This PC"}{@render cliSwitch()}{/if}
        {/each}
        <h3 class="label-caps">Window</h3>
        {@render toggle(
          "Hide the bar after copy or save",
          "After Copy, Copy path, Copy file or saving a note.",
          ui.hide_after_copy,
          (el) => choose(setHideAfterCopy(el.checked)),
        )}
        {@render toggle(
          "Remember my last search for 30 seconds",
          "Reopened soon, the bar shows it again. It is never saved to disk.",
          info?.remember_last_search ?? true,
          setRememberLast,
          !info,
        )}
        <div class="field">
          {@render field("Window position", "Forgets where you dragged the window, on every screen.")}
          <button class="btn" onclick={resetWindowPosition}>Reset window position</button>
        </div>
        {#if windowMessage}<p class="note" role="status">{windowMessage}</p>{/if}
      {:else if section === "folders"}
        <h2 id="priv-title">Skipped folders</h2>
        <h3 class="label-caps">Folders to skip</h3>
        <p class="hint">Names, file text and text in pictures in these folders are never read.</p>
        {@render folderList(excluded, "No skipped folders", (p) => (excluded = excluded.filter((r) => r !== p)), "Stop skipping")}
        <button class="btn" onclick={() => add("excluded")}>Skip folder…</button>

        <h3 class="label-caps">Always skipped</h3>
        <p class="hint">These are never read, whatever you choose above.</p>
        <div class="tags">{#each ALWAYS_SKIPPED as t}<span class="tag">{t}</span>{/each}</div>
        <h3 class="label-caps">Not read inside AppData folders you add</h3>
        <p class="hint">In a folder inside AppData that you add yourself, these cache and log folders are not read.</p>
        <div class="tags">{#each skipList as t}<span class="tag">{t}</span>{/each}</div>

        <h3 class="label-caps">Pause</h3>
        {@render toggle(
          "Pause all background work",
          "Stops preparing files, reading file text and reading text in pictures until you turn it back on.",
          info?.paused ?? false,
          setPaused,
        )}
        {#if pauseMessage}<p class="hint" role="alert">{pauseMessage}</p>{/if}
      {:else if section === "privacy"}
        <h2 id="priv-title">Privacy</h2>
        <h3 class="label-caps">What is stored</h3>
        <p class="hint">File names, file text, usage and search history, your notes and the undo log. Nothing leaves this PC.</p>
        <h3 class="label-caps">How it is protected</h3>
        <p class="hint">
          Encrypted with a key only your Windows account can unlock. API keys, private keys and card numbers are
          masked before text is saved.
        </p>
        <h3 class="label-caps">What Lookout reads</h3>
        {@render toggle(
          "Search inside files",
          "Reads the text inside your files so you can search for words in them.",
          info?.index_contents ?? false,
          (el) => setContentOption("index_contents", el),
        )}
        {@render toggle(
          "Text in pictures",
          "Finds words inside images and screenshots so they can be searched.",
          info?.ocr ?? false,
          (el) => setContentOption("ocr", el),
        )}
        {#if contentMessage}<p class="hint" role="alert">{contentMessage}</p>{/if}
        <h3 class="label-caps">Clipboard</h3>
        {@render toggle(
          "Keep copied images",
          "The last 20 images, 5 MB each at most, for 24 hours. Needs the Clipboard add-on. They are not checked for secrets; turning this off deletes them.",
          keepImages,
          setKeepImages,
        )}
        {#if imagesMessage}<p class="hint" role="alert">{imagesMessage}</p>{/if}
        <h3 class="label-caps">Search history</h3>
        {@render toggle(
          "Remember my searches",
          "Press the Up arrow in an empty box to see your last searches.",
          info?.remember_searches ?? true,
          setRememberSearches,
        )}
        {#if askClear}
          <p class="warning" role="alert">Also delete the searches already saved?</p>
          <div class="row end">
            <button class="btn small" onclick={() => (askClear = false)}>Keep them</button>
            <button class="btn small primary" onclick={clearHistory}>Delete them</button>
          </div>
        {:else}
          <p><button class="btn small" onclick={clearHistory}>Delete search history</button></p>
        {/if}
        {#if historyMessage}<p class="hint" role="status">{historyMessage}</p>{/if}
        <h3 class="label-caps">Notes</h3>
        {#if askClearNotes}
          <p class="warning" role="alert">Delete every note? This cannot be undone.</p>
          <div class="row end">
            <button class="btn small" onclick={() => (askClearNotes = false)}>Keep them</button>
            <button class="btn small danger" onclick={clearNotes}>Delete all notes</button>
          </div>
        {:else}
          <p><button class="btn small" onclick={() => ((askClearNotes = true), (notesMessage = ""))}>Delete all notes…</button></p>
        {/if}
        {#if notesMessage}<p class="hint" role="status">{notesMessage}</p>{/if}
        <h3 class="label-caps">Other programs</h3>
        {@render cliSwitch()}
      {:else if section === "speed"}
        <h2 id="priv-title">Speed</h2>
        <label class="field">
          {@render field("Background speed", "Used only while you are idle.")}
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
          {@render field("File text size limit", "Disk space for file text, in MB. At least 100.")}
          <span class="num">
            <input type="number" bind:value={maxMb} aria-invalid={!!mbError} aria-describedby="e-mb" />
            <span id="e-mb" class="err" role="alert">{mbError}</span>
          </span>
        </label>
        <p class="note">
          Gentle works up to 10% of the time, Balanced (default) 25%, Fast 60% (under one CPU core).
        </p>
      {:else if section === "addons"}
        <h2 id="priv-title">Add-ons</h2>
        <Plugins />
      {:else if section === "quickkeys"}
        <h2 id="priv-title">Quick keys</h2>
        <p class="hint">
          A short word for anything you open often. Type it and a space to put it on top. For apps and system actions,
          select the result and click Set quick key.
        </p>
        {#each qkeys as k (k.alias)}
          <div class="field">
            <span class="fl">
              <span class="ft"><code>{k.alias}</code> · {k.title}</span>
              <span class="hint">{k.kind}{k.subtitle ? ` · ${k.subtitle}` : ""}</span>
            </span>
            {#if qkEditing === k.alias}
              <input
                type="text"
                class="code"
                maxlength="8"
                bind:value={qkEditAlias}
                aria-label="New quick key"
                onkeydown={(e) => {
                  if (e.key === "Enter") saveEdit(k.alias);
                  else if (e.key === "Escape") {
                    e.preventDefault();
                    e.stopPropagation();
                    qkEditing = null;
                  }
                }}
              />
              <button class="btn small primary" onclick={() => saveEdit(k.alias)}>Save</button>
              <button class="btn small" onclick={() => (qkEditing = null)}>Cancel</button>
            {:else}
              <button class="btn small" onclick={() => ((qkEditing = k.alias), (qkEditAlias = k.alias), (qkError = ""))}>Edit</button>
              <button class="btn small danger" onclick={() => deleteKey(k.alias)}>Delete</button>
            {/if}
          </div>
        {/each}
        <div class="field">
          <input type="text" class="code" maxlength="8" placeholder="Quick key" bind:value={qkAlias} aria-label="Quick key" />
          <input type="text" placeholder="Folder, file path or web address" bind:value={qkTarget} aria-label="Target" onkeydown={(e) => e.key === "Enter" && addKey()} />
          <button class="btn small primary" onclick={addKey}>Add</button>
        </div>
        {#if qkError}<p class="danger-text" role="alert">{qkError}</p>{/if}
      {:else if section === "repair"}
        <h2 id="priv-title">Repair</h2>
        <div class="scard">
          <h3 class="stitle">Diagnostics</h3>
          <p class="hint">Versions, screen setup, index counts and recent errors. No file names, searches, notes or clipboard text.</p>
          <div class="row">
            <button class="btn" onclick={() => repair("copy_diagnostics", "Diagnostics copied.")}>Copy diagnostics</button>
            <button class="btn" onclick={() => repair("open_log_folder")}>Open log folder</button>
          </div>
        </div>
        <div class="scard">
          <h3 class="stitle">Search index</h3>
          <p class="hint">Reads file names and file contents again. Notes, clipboard history and settings are not touched.</p>
          {#if confirmingRebuild}
            <p class="warning" role="alert">Rebuild the search index? Results from inside files come back as the files are read again.</p>
            <div class="row">
              <button class="btn" onclick={() => (confirmingRebuild = false)}>Cancel</button>
              <button class="btn primary" onclick={rebuildIndex}>Rebuild</button>
            </div>
          {:else}
            <p><button class="btn" onclick={() => (confirmingRebuild = true)}>Rebuild search index</button></p>
          {/if}
          {#if rebuildLine}<p class="note" role="status">{rebuildLine}</p>{/if}
        </div>
        {#if repairNote}<p class="note" role="status">{repairNote}</p>{/if}
      {:else}
        <h2 id="priv-title" class="danger-text">Delete my data</h2>
        {#if confirmingDelete}
          <p class="warning" role="alert">
            This deletes your saved file names, file text, usage and search history, your notes, the undo log and the encryption key.
            Your files are not touched. You will be asked to choose folders again.
          </p>
          <div class="row">
            <button class="btn" onclick={() => (confirmingDelete = false)}>Cancel</button>
            <button class="btn danger" disabled={busy} onclick={deleteIndex}>Delete my data</button>
          </div>
        {:else}
          <p class="hint">Removes your notes and the search data Lookout stored about your files. Your files are not touched.</p>
          <p><button class="btn danger" onclick={() => (confirmingDelete = true)}>Delete my data…</button></p>
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
      <!-- Saving before the settings arrived would save empty ones. -->
      <button class="btn primary" disabled={busy || invalid || !info} onclick={saveAll}>Save</button>
    {/if}
  </div>
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
    width: 190px;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 12px 8px 12px 12px;
    overflow-y: auto;
  }
  .item {
    all: unset;
    box-sizing: border-box;
    padding: 6px 16px;
    border-radius: 999px;
    color: var(--text-soft);
    cursor: pointer;
  }
  .item:hover {
    background: var(--accent-soft);
    color: var(--text);
  }
  .item.on {
    background: var(--accent-bg);
    color: var(--accent-on);
    font-weight: 600;
  }
  .item.danger:not(.on) {
    color: var(--danger);
  }
  .item.fold {
    margin-top: 8px;
  }
  .item.sub {
    padding: 4px 10px 4px 24px;
    white-space: nowrap;
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
    margin: 12px 12px 12px 4px;
    padding: 16px 22px 18px;
    border: 1px solid var(--hairline);
    border-radius: 20px;
    background: var(--panel);
  }
  .scard {
    margin: 0 0 12px;
    padding: 4px 16px 6px;
    border: 1px solid var(--hairline);
    border-radius: 16px;
    background: var(--panel);
  }
  .stitle {
    margin: 10px 0 0;
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .scard .field:last-child {
    border-bottom: 0;
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
  .away {
    flex: none;
    font-size: 12px;
    color: var(--text-soft);
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
  .row.end {
    justify-content: flex-end;
    margin-bottom: 8px;
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
