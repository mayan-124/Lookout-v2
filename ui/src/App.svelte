<script lang="ts">
  import { onMount, tick } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import Privacy from "./Privacy.svelte";
  import Disk from "./Disk.svelte";
  import { applyTheme, type ThemePref } from "./theme";

  type Kind = string | { custom: string };
  interface Item {
    id: string;
    kind: Kind;
    title: string;
    subtitle: string | null;
    uri: string | null;
    source: string;
    score: number;
    metadata?: Record<string, unknown>;
  }
  interface Update {
    generation: number;
    items: Item[];
    done: boolean;
  }
  type Risk = "safe" | "confirm" | "dangerous";
  interface Prepared {
    ticket: string;
    action_id: string;
    label: string;
    risk: Risk;
    preview: { from: string | null; title: string; lines: string[]; warnings: string[] };
    undoable: boolean;
    is_undo: boolean;
  }
  interface Done {
    action_id: string;
    message: string;
    items: string[];
    undoable: boolean;
    was_undo: boolean;
  }
  // What the card shows: a preview waiting for Confirm, or the outcome.
  type Card =
    | { phase: "confirm"; prepared: Prepared; presented: boolean; busy: boolean; error: string }
    | { phase: "result"; ok: boolean; message: string; undoable: boolean };
  interface Status {
    files: number;
    folders: number;
    apps: number;
    roots: number;
    indexing: boolean;
    paused: boolean;
    needs_setup: boolean;
    semantic_done: number;
    semantic_total: number;
    read_done: number;
    read_total: number;
    ocr_done: number;
    ocr_total: number;
  }

  let input: HTMLInputElement | undefined = $state();
  let list: HTMLElement | undefined = $state();
  let query = $state("");
  let pinned = $state(false);
  const PATHLIKE = /^\s*"?([a-z]:|\\\\|%|~|appdata[\\/])/i;

  async function loadPinned() {
    try {
      pinned = (await invoke<{ pinned: boolean }>("window_state")).pinned;
    } catch {
      /* keep the last known state */
    }
  }

  function togglePin() {
    pinned = !pinned;
    invoke("set_pinned", { pinned }).catch(() => {});
    input?.focus();
  }

  /** Tab in a path query completes the next folder name. */
  async function completePath() {
    const text = query;
    const next = await invoke<string | null>("complete_path", { text }).catch(() => null);
    if (next && next !== text && query === text) {
      query = next;
      run(next);
    }
  }
  let items: Item[] = $state([]);
  // Matched words in a content snippet sit between  and .
  function snippetParts(snippet: string): { text: string; hit: boolean }[] {
    return snippet
      .split("")
      .flatMap((chunk, i) => {
        const [hit, rest] = i === 0 ? [undefined, chunk] : chunk.split("");
        return [
          ...(hit !== undefined ? [{ text: hit, hit: true }] : []),
          { text: rest ?? "", hit: false },
        ];
      })
      .filter((p) => p.text !== "");
  }
  const corrected = $derived(
    items.map((i) => i.metadata?.corrected_word).find((w) => typeof w === "string") as
      | string
      | undefined,
  );
  let done = $state(true);
  let selected = $state(0);
  let status: Status | null = $state(null);
  let notice = $state("");
  let card: Card | null = $state(null);
  // The settings panel (first-run folder choice, exclusions, pause, delete index).
  let panel = $state(false);
  let privacy: ReturnType<typeof Privacy> | undefined = $state();
  // The disk usage analyzer. It scans only while this view is open.
  let disk = $state(false);
  const isDiskCommand = (q: string) => /^disk( usage| analy[sz]er)?$/i.test(q.trim());
  function openDisk() {
    disk = true;
    query = "";
    run("");
  }
  function closeDisk() {
    disk = false;
    tick().then(() => input?.focus());
  }
  // Settings get a taller window; the pinned Save/Close bar is their footer.
  $effect(() => {
    if (panel) {
      query = "";
      items = [];
    }
  });
  $effect(() => {
    if (disk) return;
    invoke("bar_resize", { wide: false, tall: panel }).catch(() => {});
  });
  let confirmButton: HTMLButtonElement | undefined = $state();
  let cancelButton: HTMLButtonElement | undefined = $state();

  // The frontend numbers its own queries. Any update that is not for the
  // latest number is dropped, so a slow answer to an old keystroke can never
  // replace newer results.
  let generation = 0;

  // Hidden for less than this long, the bar comes back as it was: text,
  // results, selection, and Settings or Disk if one was open. Esc, running an
  // action or a closing click sets `cleared`, and the next show starts empty.
  const RESTORE_SECS = 5 * 60;
  let cleared = false;

  const nextFrame = () => new Promise<void>((r) => requestAnimationFrame(() => r()));

  const kindName = (k: Kind) => (typeof k === "string" ? k : k.custom);
  // File-type badge: ".txt" for files, the plain kind for everything else.
  const badgeOf = (i: Item) => {
    const name = kindName(i.kind);
    if (name === "file") {
      const dot = i.title.lastIndexOf(".");
      return dot > 0 && i.title.length - dot <= 6 ? i.title.slice(dot).toLowerCase() : "file";
    }
    return { calculator: "calc", conversion: "convert", folder: "dir" }[name] ?? name;
  };
  // Paths are shown in the code font; other subtitles (answers, ports) are plain text.
  const isPath = (i: Item) => ["file", "folder", "app", "screenshot", "project"].includes(kindName(i.kind));
  // Plain group names, by where a result came from.
  const GROUPS: Record<string, string> = {
    calculator: "Answer",
    units: "Answer",
    apps: "Apps",
    files: "Files and folders",
    content: "Inside files",
    semantic: "Similar meaning",
    ports: "Ports",
  };
  // Plugin results carry messages (a rename preview, an error): wrap, never cut off.
  const isPlugin = (i: Item) => !(i.source in GROUPS);
  const groupOf = (i: Item) => GROUPS[i.source] ?? "More";
  // The empty-bar welcome: what to type and what it does.
  const TRY: [string, string][] = [
    ["budget", "Find files and apps by name"],
    ["renovation invoice", "Find words inside your files"],
    ["kind:folder photos", "Only folders"],
    ["kill port 3000", "See what uses a port, then stop it; you confirm"],
    ["disk", "See what is using space on a drive or folder"],
    ["10 km to miles", "Convert units and calculate"],
  ];
  // Which action Enter starts for a result. The backend decides what is allowed;
  // this only names the action.
  const defaultAction = (i: Item | undefined): string | null => {
    if (!i) return null;
    const plugin = i.metadata?.default_action;
    if (typeof plugin === "string") return plugin;
    if (i.kind === "file" || i.kind === "folder" || i.kind === "app") return "open";
    if (i.kind === "port") return "kill_port";
    return null;
  };
  const errorText = (e: unknown) => (typeof e === "string" ? e : String(e));

  let openHotkey = $state("");
  async function loadHotkey() {
    try {
      const s = await invoke<{ open_hotkey: string; theme: ThemePref }>("privacy_state");
      openHotkey = s.open_hotkey;
      applyTheme(s.theme);
    } catch {
      openHotkey = "";
    }
  }

  async function refreshStatus() {
    try {
      status = await invoke<Status | null>("index_status");
    } catch {
      status = null;
    }
  }

  // Settings may ask "Discard changes?" first.
  function leavePanel() {
    if (privacy) privacy.requestClose();
    else closePanel();
  }

  function closePanel() {
    panel = false;
    refreshStatus();
    loadHotkey();
    tick().then(() => input?.focus());
  }

  function run(text: string) {
    cleared = false;
    generation += 1;
    notice = "";
    if (text.trim() === "") {
      items = [];
      done = true;
      selected = 0;
    } else {
      done = false;
    }
    invoke("query", { generation, text }).catch(() => {});
  }

  function onUpdate(u: Update) {
    if (u.generation !== generation) return;
    const keep = items[selected]?.id;
    // One block per group, in the order each group first appears; similar-meaning matches go last.
    const rest = u.items.filter((i) => i.source !== "semantic");
    const order = [...new Set(rest.map(groupOf))];
    items = [
      ...order.flatMap((g) => rest.filter((i) => groupOf(i) === g)),
      ...u.items.filter((i) => i.source === "semantic"),
    ];
    done = u.done;
    const at = keep ? items.findIndex((i) => i.id === keep) : -1;
    selected = at >= 0 ? at : 0;
  }

  // Every action starts with `prepare`, which builds the preview from the live
  // state. `safe` actions then run at once; anything else shows the card and
  // waits for the Confirm button. The backend refuses to run a confirm/dangerous
  // ticket any other way.
  async function begin(prepare: () => Promise<Prepared>) {
    notice = "";
    let prepared: Prepared;
    try {
      prepared = await prepare();
    } catch (e) {
      notice = errorText(e);
      return;
    }
    if (prepared.risk === "safe") {
      try {
        finish(await invoke<Done>("run_action", { ticket: prepared.ticket }));
      } catch (e) {
        notice = errorText(e);
      }
      return;
    }
    card = { phase: "confirm", prepared, presented: false, busy: false, error: "" };
    // Tell the backend the preview is really on screen, then focus a button.
    await tick();
    await nextFrame();
    try {
      await invoke("present_action", { ticket: prepared.ticket });
    } catch (e) {
      card = { phase: "result", ok: false, message: errorText(e), undoable: false };
      return;
    }
    if (card?.phase === "confirm" && card.prepared.ticket === prepared.ticket) {
      card.presented = true;
      await tick();
      // A dangerous action starts on Cancel, so a stray Enter cannot confirm it.
      (prepared.risk === "dangerous" ? cancelButton : confirmButton)?.focus();
    }
  }

  function activate(item: Item | undefined) {
    const action = defaultAction(item);
    if (!item || !action || card) return;
    begin(() => invoke<Prepared>("prepare_action", { action, id: item.id }));
  }

  function startUndo() {
    if (card?.phase === "confirm") return;
    begin(() => invoke<Prepared>("prepare_undo"));
  }

  function finish(done: Done) {
    if (done.action_id === "open" && !done.was_undo) {
      cleared = true;
      invoke("hide_bar"); // opened: get out of the way
      return;
    }
    card = { phase: "result", ok: true, message: done.message, undoable: done.undoable };
  }

  async function confirm(e: Event) {
    // Only a real key press or click counts; script-made events are ignored.
    if (!e.isTrusted) return;
    if (card?.phase !== "confirm" || !card.presented || card.busy) return;
    const ticket = card.prepared.ticket;
    card.busy = true;
    try {
      finish(await invoke<Done>("confirm_action", { ticket }));
    } catch (e) {
      const message = errorText(e);
      // "Too quickly" leaves the ticket alive; anything else ends it.
      if (message.includes("too quickly") && card?.phase === "confirm") {
        card.busy = false;
        card.error = message;
      } else {
        card = { phase: "result", ok: false, message, undoable: false };
      }
    }
  }

  function dismissCard() {
    if (card?.phase === "confirm") invoke("cancel_action", { ticket: card.prepared.ticket });
    card = null;
    input?.focus();
  }

  function move(delta: number) {
    if (items.length === 0) return;
    selected = (selected + delta + items.length) % items.length;
    tick().then(() =>
      list?.querySelector<HTMLElement>(`[data-index="${selected}"]`)?.scrollIntoView({ block: "nearest" }),
    );
  }

  onMount(() => {
    loadHotkey();
    loadPinned();
    const unlistenShown = listen<{ id: number; hidden_secs: number | null }>("bar-shown", async (event) => {
      const { hidden_secs } = event.payload;
      const restore =
        !cleared &&
        hidden_secs !== null &&
        hidden_secs < RESTORE_SECS &&
        (query.trim() !== "" || panel || disk);
      notice = "";
      // A confirm card is never brought back: its preview must be seen fresh.
      if (card?.phase === "confirm") invoke("cancel_action", { ticket: card.prepared.ticket });
      card = null;
      if (restore) {
        // Same page, same text; the results are asked for again so they are current.
        if (query.trim() !== "" && !panel && !disk) run(query);
      } else {
        query = "";
        items = [];
        done = true;
        selected = 0;
        generation += 1; // invalidate anything still in flight
        panel = false;
        disk = false;
      }
      cleared = false;
      input?.focus();
      // First run: ask which folders may be indexed before anything is.
      loadHotkey();
      refreshStatus().then(() => {
        if (status?.needs_setup) panel = true;
      });
      // Two frames: the first commits the DOM change, the second proves the
      // compositor presented it. The backend turns this into "visible" latency.
      await nextFrame();
      await nextFrame();
      await invoke("bar_painted", { id: event.payload.id });
    });
    const unlistenResults = listen<Update>("query-results", (e) => onUpdate(e.payload));
    // Only report ready once the listeners exist, so no show event is lost.
    Promise.all([unlistenShown, unlistenResults]).then(() => invoke("frontend_ready"));
    const poll = setInterval(() => {
      if (document.visibilityState === "visible") refreshStatus();
    }, 1000);
    return () => {
      clearInterval(poll);
      unlistenShown.then((f) => f());
      unlistenResults.then((f) => f());
    };
  });

  function onKeydown(e: KeyboardEvent) {
    // The disk view handles its own keys.
    if (disk) return;
    if (e.ctrlKey && !e.shiftKey && !e.altKey && e.key === ",") {
      e.preventDefault();
      if (panel) leavePanel();
      else if (!card) panel = true;
      return;
    }
    if (panel) {
      if (e.key === "Escape") {
        e.preventDefault();
        if (status?.needs_setup) {
          cleared = true;
          invoke("hide_bar");
        } else leavePanel();
      }
      return;
    }
    if (card) {
      // With a card open, Escape closes the card and nothing else is handled
      // here: Enter and Space work only through the focused button, so Confirm
      // happens only by pressing the visible Confirm button.
      if (e.key === "Escape") {
        e.preventDefault();
        dismissCard();
      }
      return;
    }
    if (e.ctrlKey && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "z" && query === "") {
      // With text in the box, Ctrl+Z stays the text field's own undo.
      e.preventDefault();
      startUndo();
      return;
    }
    switch (e.key) {
      case "Tab":
        if (!e.shiftKey && PATHLIKE.test(query)) {
          e.preventDefault();
          completePath();
        }
        break;
      case "Escape":
        e.preventDefault();
        if (pinned && query !== "") {
          // A pinned window stays: the first Esc clears the query, the next hides it.
          query = "";
          run("");
          break;
        }
        cleared = true;
        invoke("hide_bar");
        break;
      case "ArrowDown":
        e.preventDefault();
        move(1);
        break;
      case "ArrowUp":
        e.preventDefault();
        move(-1);
        break;
      case "Enter":
        e.preventDefault();
        if (isDiskCommand(query)) openDisk();
        else if (!e.repeat) activate(items[selected]);
        break;
    }
  }

  const fmt = (n: number) => n.toLocaleString();
</script>

<svelte:window onkeydown={onKeydown} />

<main class="bar">
  <div class="searchrow" data-tauri-drag-region>
    <span class="prompt code" aria-hidden="true" data-tauri-drag-region>›</span>
    <input
      bind:this={input}
      bind:value={query}
      oninput={() => run(query)}
      class="search"
      type="text"
      placeholder={panel ? "settings" : "Search files and apps, or calculate: 2+2, 10 km to miles"}
      readonly={panel}
      autocomplete="off"
      spellcheck="false"
      role="combobox"
      aria-expanded={items.length > 0}
      aria-controls="results"
      aria-autocomplete="list"
      aria-activedescendant={items.length > 0 && items[selected] ? `result-${selected}` : undefined}
      aria-label="Search"
    />
    <button
      class="pin"
      class:on={pinned}
      type="button"
      aria-pressed={pinned}
      aria-label="Pin window"
      title={pinned ? "Pinned: stays on top and does not hide when you click away" : "Pin window on top"}
      onclick={togglePin}>{pinned ? "Pinned" : "Pin"}</button
    >
    <kbd aria-hidden="true" data-tauri-drag-region>Esc</kbd>
  </div>

  {#if disk}
    <Disk onclose={closeDisk} />
  {:else if panel}
    <Privacy bind:this={privacy} onclose={closePanel} />
  {:else if card?.phase === "confirm"}
    {@const p = card.prepared}
    <div
      class="card"
      class:danger={p.risk === "dangerous"}
      role="alertdialog"
      aria-labelledby="card-title"
      aria-describedby="card-body"
    >
      <div class="card-head">
        <span class="risk" class:danger={p.risk === "dangerous"}>
          {p.risk === "dangerous" ? "Dangerous" : "Needs confirmation"}
        </span>
        {#if p.preview.from}<p class="from">{p.preview.from}</p>{/if}
        <h2 id="card-title">{p.preview.title}</h2>
      </div>
      <div id="card-body" class="card-body">
        {#each p.preview.lines as line}<p class="line">{line}</p>{/each}
        {#each p.preview.warnings as w}<p class="warning">{w}</p>{/each}
        {#if card.error}<p class="warning" role="alert">{card.error}</p>{/if}
      </div>
      <div class="card-actions">
        <button bind:this={cancelButton} class="btn" onclick={dismissCard}>Cancel <kbd>Esc</kbd></button>
        <button
          bind:this={confirmButton}
          class="btn"
          class:primary={p.risk !== "dangerous"}
          class:danger={p.risk === "dangerous"}
          disabled={!card.presented || card.busy}
          onclick={confirm}
        >
          {p.is_undo ? "Undo" : "Confirm"}
        </button>
      </div>
    </div>
  {:else if card?.phase === "result"}
    <section class="card" role="status">
      <div class="card-head">
        <span class="risk" class:danger={!card.ok}>{card.ok ? "Done" : "Not done"}</span>
        <h2 id="card-title">{card.message}</h2>
      </div>
      <div class="card-actions">
        <button class="btn" onclick={dismissCard}>Close <kbd>Esc</kbd></button>
        {#if card.ok && card.undoable}
          <button class="btn primary" onclick={startUndo}>Undo</button>
        {/if}
      </div>
    </section>
  {:else if isDiskCommand(query)}
    <ul class="results" role="listbox">
      <li role="option" aria-selected="true" class="selected" onclick={openDisk} onkeydown={() => {}}>
        <span class="badge code">disk</span>
        <span class="text">
          <span class="l1"><span class="title">Disk usage analyzer</span></span>
          <span class="snippet">See what takes the space on a drive or folder. Scans only when you press Scan.</span>
        </span>
        <kbd>Enter</kbd>
      </li>
    </ul>
  {:else if items.length > 0}
    {#if corrected}<p class="note">Showing results for <b>{corrected}</b></p>{/if}
    <ul id="results" class="results" role="listbox" bind:this={list}>
      {#each items as item, i (item.id)}
        {#if i === 0 || groupOf(items[i - 1]) !== groupOf(item)}
          <li role="presentation" class="group label-caps">{groupOf(item)}</li>
        {/if}
        <li
          role="option"
          id={`result-${i}`}
          aria-selected={i === selected}
          class:selected={i === selected}
          data-index={i}
          onmousemove={() => (selected = i)}
          onclick={() => activate(item)}
          onkeydown={() => {}}
        >
          <span class="badge code">{badgeOf(item)}</span>
          <span class="text">
            <span class="l1">
              <span class="title">{item.title}</span>
              {#if typeof item.metadata?.snippet === "string" && item.uri}
                <span class="path code">{item.uri}</span>
              {:else if item.subtitle}
                <span class="path" class:code={isPath(item)} class:wrap={isPlugin(item)}>{item.subtitle}</span>
              {/if}
            </span>
            {#if typeof item.metadata?.snippet === "string"}
              <span class="snippet"
                >{#each snippetParts(item.metadata.snippet) as part}{#if part.hit}<mark>{part.text}</mark
                    >{:else}{part.text}{/if}{/each}</span
              >
            {/if}
          </span>
          {#if i === selected && defaultAction(item)}<kbd>Enter</kbd>{/if}
        </li>
      {/each}
    </ul>
  {:else if query.trim() !== "" && done}
    <p class="empty">No results for “{query.trim()}”</p>
  {:else if query.trim() === ""}
    <section class="empty" aria-label="Tips">
      <p class="label-caps">Try typing</p>
      <ul class="tips">
        {#each TRY as [example, what]}
          <li><span class="ex code">{example}</span><span class="what">{what}</span></li>
        {/each}
      </ul>
      {#if status && status.roots === 0 && status.files === 0}
        <p class="warning">No folders are chosen yet. Press <kbd>Ctrl+,</kbd> to choose some.</p>
      {/if}
    </section>
  {/if}

  {#if !panel}
  <footer class="code">
    {#if notice}
      <span class="warn">{notice}</span>
    {:else if status}
      <span class="local"><i class="dot"></i>local only</span>
      {#if !panel && !disk && !card && items.length > 0}
        <span><kbd>↑↓</kbd> move</span>
        <span><kbd>Enter</kbd> open</span>
        <span><kbd>Esc</kbd> close</span>
      {:else}
        <span>
          {fmt(status.files)} files · {fmt(status.folders)} folders · {fmt(status.apps)} apps
          {#if status.paused}· paused{:else if status.indexing}· preparing files…{:else if status.read_total > 0 && status.read_done < status.read_total}· Reading text: {fmt(status.read_done)} of {fmt(status.read_total)} files{:else if status.ocr_total > 0 && status.ocr_done < status.ocr_total}· Reading pictures: {fmt(status.ocr_done)} of {fmt(status.ocr_total)} files{/if}
          {#if status.semantic_total > 0}· similar meaning {fmt(status.semantic_done)}/{fmt(status.semantic_total)}{/if}
        </span>
      {/if}
    {:else}
      <span>Search unavailable</span>
    {/if}
    <span class="spacer"></span>
    {#if openHotkey}<span class="hotkey">{openHotkey} open</span>{/if}
    <span class="links">
      <button class="btn text code" onclick={() => { if (!card && !disk) panel = true; }}>Ctrl+, settings</button>
      <button class="btn text code" onclick={openDisk}>disk usage</button>
    </span>
  </footer>
  {/if}
</main>

<style>
  .bar {
    height: 100%;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    background: var(--bg);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 14px;
    overflow: hidden;
  }

  .searchrow {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 0 18px 0 20px;
    border-bottom: 1px solid var(--border);
  }
  .pin {
    all: unset;
    cursor: pointer;
    font-size: 12px;
    padding: 3px 8px;
    border-radius: 6px;
    color: var(--muted);
    border: 1px solid var(--border);
  }
  .pin:hover,
  .pin:focus-visible {
    color: var(--text);
    outline: 2px solid var(--accent);
  }
  .pin.on {
    color: var(--accent);
    border-color: var(--accent);
  }
  .prompt {
    color: var(--accent);
    font-size: 24px;
    line-height: 1;
  }
  .search {
    all: unset;
    box-sizing: border-box;
    flex: 1;
    min-width: 0;
    padding: 18px 0;
    font-size: 20px;
    caret-color: var(--accent);
  }
  .search[readonly] {
    caret-color: transparent;
  }
  .search::placeholder {
    color: var(--muted);
  }
  /* The caret and the prompt show the focus, so the field needs no extra outline. */
  .search:focus-visible {
    outline: none;
  }

  .results {
    list-style: none;
    margin: 0;
    padding: 6px 8px;
    overflow-y: auto;
    flex: 1;
    min-height: 0;
  }
  .results li {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 48px;
    padding: 6px 12px;
    border: 1px solid transparent;
    border-radius: 10px;
    cursor: default;
  }
  .results li.group {
    min-height: 0;
    padding: 10px 12px 4px;
    border: none;
  }
  .results li.selected {
    background: var(--raised);
    border-color: var(--accent);
  }

  .badge {
    flex: none;
    min-width: 52px;
    box-sizing: border-box;
    text-align: center;
    padding: 2px 6px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--panel);
    color: var(--text-soft);
    font-size: 12px;
  }

  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .l1 {
    display: flex;
    align-items: baseline;
    gap: 10px;
    min-width: 0;
  }
  .title {
    flex: none;
    max-width: 55%;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 15px;
    font-weight: 600;
  }
  .path {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--muted);
    font-size: 12px;
  }
  .path.wrap {
    flex: 1 1 auto;
    white-space: normal;
    overflow: visible;
    text-overflow: clip;
    overflow-wrap: anywhere;
  }
  .l1:has(.path.wrap) {
    flex-wrap: wrap;
  }
  .path:not(.code) {
    font-size: 13px;
  }
  .snippet {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text-soft);
    font-size: 13px;
  }
  .snippet mark {
    padding: 0 2px;
    border-radius: 3px;
    background: var(--match-bg);
    color: var(--match-text);
  }

  .note {
    margin: 0;
    padding: 6px 20px 2px;
  }

  .empty {
    padding: 16px 20px;
    color: var(--muted);
    font-size: 14px;
    margin: 0;
    flex: 1;
    overflow-y: auto;
  }

  .card {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    padding: 14px 20px;
    gap: 10px;
  }
  .card-head {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .card h2 {
    margin: 0;
    font-size: 20px;
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .risk {
    align-self: flex-start;
    padding: 2px 8px;
    border: 1px solid var(--border-strong);
    border-radius: 6px;
    color: var(--text-soft);
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }
  .risk.danger {
    border-color: var(--danger);
    color: var(--danger);
  }
  .card-body {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    font-size: 14px;
  }
  .line,
  .warning {
    margin: 0 0 4px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .from {
    margin: 0 0 4px;
    font-size: 13px;
    font-weight: 600;
    color: var(--muted);
  }
  .line {
    color: var(--text-soft);
  }
  .card-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  .tips {
    list-style: none;
    margin: 10px 0 0;
    padding: 0;
  }
  .tips li {
    display: flex;
    align-items: baseline;
    gap: 16px;
    padding: 6px 0;
  }
  .ex {
    flex: none;
    width: 190px;
    color: var(--accent);
    font-size: 14px;
  }
  .what {
    color: var(--text-soft);
    font-size: 14px;
  }

  footer {
    display: flex;
    flex-wrap: wrap; /* at 200% scaling on a small screen the links drop to a second line */
    align-items: center;
    gap: 2px 14px;
    padding: 6px 14px 6px 20px;
    border-top: 1px solid var(--border);
    background: var(--panel);
    color: var(--muted);
    font-size: 12px;
  }
  footer .spacer {
    flex: 1;
  }
  footer .hotkey {
    color: var(--text-soft);
  }
  footer .btn.text {
    font-size: 12px;
    white-space: nowrap;
    color: var(--accent);
  }
  footer .btn.text:hover {
    text-decoration: underline;
  }
  footer .links {
    display: inline-flex;
    gap: 14px;
  }
  footer > span {
    white-space: nowrap;
  }
  footer kbd {
    padding: 0;
    border: none;
    background: none;
    color: var(--text-soft);
  }
  .local {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--success);
  }
  .warn {
    color: var(--accent);
  }
</style>
