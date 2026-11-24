<script lang="ts">
  import { onMount, tick } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import Privacy from "./Privacy.svelte";
  import Disk from "./Disk.svelte";
  import { flip } from "svelte/animate";
  import { applyTheme, type ThemePref } from "./theme";
  import Orb from "./Orb.svelte";
  import { look } from "./look.svelte";

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

  // The orb animates only while the window is on screen.
  let visible = $state(document.visibilityState === "visible");
  function hideBar() {
    visible = false;
    invoke("hide_bar");
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
  // The orb mirrors existing status: error, results just arrived, paused, indexing, idle.
  let foundFlash = $state(false);
  let foundTimer: ReturnType<typeof setTimeout> | undefined;
  function flashFound() {
    foundFlash = true;
    clearTimeout(foundTimer);
    foundTimer = setTimeout(() => (foundFlash = false), 700);
  }
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
    files: "Files",
    content: "Inside files",
    semantic: "By meaning",
    ports: "Ports",
  };
  // Plugin results carry messages (a rename preview, an error): wrap, never cut off.
  const isPlugin = (i: Item) => !(i.source in GROUPS);
  const groupOf = (i: Item) => {
    const kind = kindName(i.kind);
    if (kind === "clipboard") return "Clipboard";
    if (kind === "screenshot") return "Seen on screen";
    return GROUPS[i.source] ?? "More";
  };
  // Bold the typed words inside a title or path. Operators like kind:folder are skipped.
  const BACKSLASH = String.fromCharCode(92);
  const escapeRe = (t: string) => t.replace(/\W/g, (c) => BACKSLASH + c);
  function highlight(text: string): { text: string; hit: boolean }[] {
    const terms = query
      .trim()
      .split(/\s+/)
      .filter((t) => t.length > 0 && !t.includes(":"))
      .slice(0, 6)
      .map(escapeRe);
    if (terms.length === 0 || !text) return [{ text, hit: false }];
    return text
      .split(new RegExp(`(${terms.join("|")})`, "i"))
      .map((chunk, n) => ({ text: chunk, hit: n % 2 === 1 }))
      .filter((p) => p.text !== "");
  }

  // The four sections. Clipboard and Screenshots are the existing "clip" and
  // "shot" searches; Disk is the disk view. The current one follows the query.
  const SECTIONS = ["Search", "Disk", "Clipboard", "Screenshots"] as const;
  const section = $derived(
    disk
      ? 1
      : /^\s*(clip|clipboard|clips)(\s|$)/i.test(query)
        ? 2
        : /^\s*(shot|screenshot)(\s|$)/i.test(query)
          ? 3
          : 0,
  );
  function gotoSection(n: number) {
    if (panel || card) return;
    if (n === 1) {
      openDisk();
      return;
    }
    query = n === 2 ? "clip " : n === 3 ? "shot " : "";
    run(query);
    input?.focus();
  }
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
    if (u.done && items.length > 0) flashFound();
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
      hideBar(); // opened: get out of the way
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
      visible = true;
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
          hideBar();
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
    if (e.ctrlKey && !e.shiftKey && !e.altKey && ["1", "2", "3", "4"].includes(e.key)) {
      e.preventDefault();
      gotoSection(Number(e.key) - 1);
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
        } else if (!e.shiftKey) {
          // Tab moves to the next section; Shift+Tab still moves focus.
          e.preventDefault();
          gotoSection((section + 1) % SECTIONS.length);
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
        hideBar();
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
  const orbMode = $derived.by(() => {
    const s = status as Status | null;
    if (notice) return "error";
    if (foundFlash) return "found";
    if (s?.paused) return "paused";
    if (s?.indexing) return "indexing";
    return "idle";
  });
</script>

<svelte:window
  onkeydown={onKeydown}
  onblur={() => !pinned && (visible = false)}
  onfocus={() => (visible = true)}
/>
<svelte:document onvisibilitychange={() => (visible = document.visibilityState === "visible")} />

<main class="bar">
  <div class="searchrow" data-tauri-drag-region>
    <Orb mode={orbMode} active={visible} />
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
      <li role="presentation" class="entry">
        <div role="option" tabindex="-1" aria-selected="true" class="row selected" onclick={openDisk} onkeydown={() => {}}>
          <span class="icon code">disk</span>
          <span class="text">
            <span class="title">Disk usage analyzer</span>
            <span class="snippet">See what takes the space on a drive or folder. Scans only when you press Scan.</span>
          </span>
          <span class="hint-enter"><kbd>Enter</kbd></span>
        </div>
      </li>
    </ul>
  {:else if items.length > 0}
    {#if corrected}<p class="note">Showing results for <b>{corrected}</b></p>{/if}
    <ul id="results" class="results" role="listbox" bind:this={list}>
      {#each items as item, i (item.id)}
        {@const top = i === 0}
        <li
          role="presentation"
          class="entry"
          style:animation-delay="{Math.min(i, 5) * 10}ms"
          animate:flip={{ duration: look.reduced ? 0 : 140 }}
        >
          {#if top}
            <div class="group label-caps">Top hit</div>
          {:else if i === 1 || groupOf(items[i - 1]) !== groupOf(item)}
            <div class="group label-caps">{groupOf(item)}</div>
          {/if}
          <div
            role="option"
            tabindex="-1"
            id={`result-${i}`}
            aria-selected={i === selected}
            class="row"
            class:top
            class:selected={i === selected}
            data-index={i}
            onmousemove={() => (selected = i)}
            onclick={() => activate(item)}
            onkeydown={() => {}}
          >
            <span class="icon code">{badgeOf(item)}</span>
            <span class="text">
              <span class="l1">
                <span class="title"
                  >{#each highlight(item.title) as part}{#if part.hit}<mark>{part.text}</mark
                      >{:else}{part.text}{/if}{/each}</span
                >
                {#if !top && !isPlugin(item)}
                  {#if typeof item.metadata?.snippet === "string" && item.uri}
                    <span class="meta code">{item.uri}</span>
                  {:else if item.subtitle}
                    <span class="meta" class:code={isPath(item)}>{item.subtitle}</span>
                  {/if}
                {/if}
              </span>
              {#if top && item.subtitle}
                <span class="sub" class:code={isPath(item)} class:wrap={isPlugin(item)}
                  >{#each highlight(item.subtitle) as part}{#if part.hit}<mark>{part.text}</mark
                      >{:else}{part.text}{/if}{/each}</span
                >
              {:else if !top && isPlugin(item) && item.subtitle}
                <span class="sub wrap">{item.subtitle}</span>
              {/if}
              {#if typeof item.metadata?.snippet === "string"}
                <span class="snippet"
                  >{#each snippetParts(item.metadata.snippet) as part}{#if part.hit}<mark>{part.text}</mark
                      >{:else}{part.text}{/if}{/each}</span
                >
              {/if}
            </span>
            {#if i === selected && defaultAction(item)}
              <span class="hint-enter"><kbd>Enter</kbd>{#if top}Open{/if}</span>
            {/if}
          </div>
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

  {#if !panel && !disk && !card}
    <nav class="pills" aria-label="Sections">
      {#each SECTIONS as name, n}
        <button
          type="button"
          class="pill"
          class:on={section === n}
          aria-current={section === n ? "true" : undefined}
          title="Ctrl+{n + 1}"
          onclick={() => gotoSection(n)}>{name}</button
        >
      {/each}
    </nav>
  {/if}

  {#if !panel}
  <footer>
    {#if notice}
      <span class="warn">{notice}</span>
    {:else if status}
      <span class="local"><i class="dot"></i>local only</span>
      {#if !disk && !card && items.length > 0}
        <span><kbd>Enter</kbd> Open</span>
        <span><kbd>Esc</kbd> Back</span>
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
    <button class="btn text" onclick={() => { if (!card && !disk) panel = true; }}>Ctrl+, settings</button>
  </footer>
  {/if}
</main>

<style>
  .bar {
    height: 100%;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    background: var(--glass-bg);
    color: var(--text);
    border: 1px solid var(--glass-border);
    border-radius: var(--r-window);
    box-shadow: var(--shadow-window);
    overflow: hidden;
  }
  :global(:root[data-glass="on"]) .bar {
    border-radius: 0;
    box-shadow: none;
  }

  .searchrow {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: 0 var(--sp-4) 0 var(--sp-5);
    border-bottom: 1px solid var(--hairline);
  }
  .pin {
    all: unset;
    cursor: pointer;
    font-size: var(--fs-xs);
    padding: 3px 8px;
    border-radius: var(--r-sm);
    color: var(--muted);
    border: 1px solid var(--hairline);
  }
  .pin:hover,
  .pin:focus-visible {
    color: var(--text);
    outline: 2px solid var(--accent);
  }
  .pin.on {
    color: var(--accent);
    border-color: var(--accent-line);
  }
  .search {
    all: unset;
    box-sizing: border-box;
    flex: 1;
    min-width: 0;
    padding: 18px 0;
    font-size: var(--fs-xl);
    caret-color: var(--accent);
  }
  .search[readonly] {
    caret-color: transparent;
  }
  .search::placeholder {
    color: var(--muted);
  }
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
  .entry {
    animation: enter 90ms var(--ease) both;
  }
  @keyframes enter {
    from {
      opacity: 0;
      transform: translateY(4px);
    }
    to {
      opacity: 1;
      transform: none;
    }
  }
  .group {
    padding: 10px 12px 4px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-height: 40px;
    padding: 5px 12px;
    border: 1px solid transparent;
    border-radius: var(--r-md);
    cursor: default;
    transition: background-color var(--dur-fast) var(--ease);
  }
  .row.selected {
    background: var(--accent-soft);
    border-color: var(--accent-line);
  }
  .row.top {
    min-height: 64px;
    padding: 10px 14px;
    border-radius: var(--r-lg);
    background: var(--raised);
  }
  .row.top.selected {
    background: var(--accent-soft);
  }

  .icon {
    flex: none;
    min-width: 36px;
    box-sizing: border-box;
    text-align: center;
    padding: 5px 4px;
    border: 1px solid var(--hairline);
    border-radius: var(--r-md);
    background: var(--panel);
    color: var(--text-soft);
    font-size: var(--fs-xs);
  }
  .row.top .icon {
    min-width: 46px;
    padding: 10px 4px;
    border-radius: var(--r-lg);
    color: var(--accent);
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
    gap: var(--sp-3);
    min-width: 0;
  }
  .title {
    flex: none;
    max-width: 60%;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: var(--fs-md);
    font-weight: 600;
  }
  .row.top .title {
    max-width: 100%;
    font-size: var(--fs-hit);
  }
  /* Path or meta, right-aligned in compact rows. */
  .meta {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    text-align: right;
    color: var(--muted);
    font-size: var(--fs-xs);
  }
  .meta:not(.code) {
    font-size: var(--fs-sm);
  }
  .sub {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text-soft);
    font-size: var(--fs-sm);
  }
  .sub.code {
    font-size: var(--fs-xs);
  }
  .sub.wrap {
    white-space: normal;
    overflow: visible;
    overflow-wrap: anywhere;
  }
  .snippet {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text-soft);
    font-size: var(--fs-sm);
  }
  mark {
    padding: 0 2px;
    border-radius: 3px;
    background: var(--match-bg);
    color: var(--match-text);
  }
  .hint-enter {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--text-soft);
    font-size: var(--fs-xs);
  }

  .pills {
    display: flex;
    gap: 6px;
    padding: 8px 14px;
    border-top: 1px solid var(--hairline);
  }
  .pill {
    all: unset;
    box-sizing: border-box;
    cursor: pointer;
    padding: 4px 14px;
    border: 1px solid var(--hairline);
    border-radius: var(--r-pill);
    color: var(--text-soft);
    font-size: var(--fs-sm);
    transition:
      background-color var(--dur-fast) var(--ease),
      color var(--dur-fast) var(--ease);
  }
  .pill:hover {
    background: var(--raised);
    color: var(--text);
  }
  .pill.on {
    background: var(--accent-bg);
    border-color: var(--accent-bg);
    color: var(--accent-on);
    font-weight: 600;
  }
  .pill:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .note {
    margin: 0;
    padding: 6px 20px 2px;
  }

  .empty {
    padding: 16px 20px;
    color: var(--muted);
    font-size: var(--fs-md);
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
    font-size: var(--fs-xl);
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .risk {
    align-self: flex-start;
    padding: 2px 8px;
    border: 1px solid var(--border-strong);
    border-radius: var(--r-sm);
    color: var(--text-soft);
    font-size: var(--fs-xs);
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
    font-size: var(--fs-md);
  }
  .line,
  .warning {
    margin: 0 0 4px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .from {
    margin: 0 0 4px;
    font-size: var(--fs-sm);
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
    font-size: var(--fs-md);
  }
  .what {
    color: var(--text-soft);
    font-size: var(--fs-md);
  }

  footer {
    display: flex;
    flex-wrap: wrap; /* at 200% scaling on a small screen the links drop to a second line */
    align-items: center;
    gap: 2px 14px;
    padding: 6px 14px 6px 20px;
    border-top: 1px solid var(--hairline);
    color: var(--muted);
    font-size: var(--fs-xs);
  }
  footer .spacer {
    flex: 1;
  }
  footer .hotkey {
    color: var(--text-soft);
  }
  footer .btn.text {
    font-size: var(--fs-xs);
    white-space: nowrap;
    color: var(--accent);
  }
  footer .btn.text:hover {
    text-decoration: underline;
  }
  footer > span {
    white-space: nowrap;
  }
  footer kbd {
    padding: 0 5px;
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
