<script lang="ts">
  import { onMount, tick } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import Privacy from "./Privacy.svelte";
  import Disk from "./Disk.svelte";
  import { flip } from "svelte/animate";
  import { fade, fly } from "svelte/transition";
  import PreviewPanel from "./Preview.svelte";
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
    modified_at?: number | null;
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
  // Every result of the latest query, grouped. `items` is what the active
  // filter chip shows; selection and Enter work on `items`.
  let all: Item[] = $state([]);
  // The filter chips. They only narrow the existing groups; "More" (answers,
  // ports, plugin results) shows under All.
  const LANES = [
    { name: "All", groups: null },
    { name: "Files", groups: ["Files", "Inside files", "Apps"] },
    { name: "Screens", groups: ["Seen on screen"] },
    { name: "Clipboard", groups: ["Clipboard"] },
    { name: "By meaning", groups: ["By meaning"] },
  ] as const;
  let lane = $state(0);
  const items = $derived.by(() => {
    const g = LANES[lane].groups;
    return g ? all.filter((i) => (g as readonly string[]).includes(groupOf(i))) : all;
  });
  // Home list: the latest clips, then recently modified indexed files. Both come
  // from ordinary queries ("clip " and "past week"); nothing is stored.
  let homeGen = 0;
  let homeGen2 = 0;
  let homeClips: Item[] = $state([]);
  let homeFiles: Item[] = $state([]);
  const mask = (t: string) => t.replaceAll("[masked]", "\u2022\u2022\u2022\u2022");
  function loadHome() {
    generation += 1;
    homeGen = generation;
    homeGen2 = 0;
    invoke("query", { generation, text: "clip " }).catch(() => {});
  }
  const AGE = /(\d+)\s*(min|h|days?)\s+ago$/;
  // A clip's age is the last part of its subtitle ("5 min ago", "just now").
  function clipTime(i: Item): number {
    const now = Math.floor(Date.now() / 1000);
    const last = (i.subtitle ?? "").split(" \u00b7 ").pop() ?? "";
    const m = AGE.exec(last);
    if (!m) return now;
    const n = Number(m[1]);
    return now - n * (m[2] === "min" ? 60 : m[2] === "h" ? 3600 : 86400);
  }
  const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
  function dayLabel(ts: number): string {
    const d = new Date(ts * 1000);
    const start = new Date();
    start.setHours(0, 0, 0, 0);
    const days = Math.floor((start.getTime() - new Date(d).setHours(0, 0, 0, 0)) / 86400000);
    if (days <= 0) return "Today";
    if (days === 1) return "Yesterday";
    if (days < 7) return WEEKDAYS[d.getDay()];
    return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
  }
  interface Recent {
    item: Item;
    ts: number;
    clip: boolean;
  }
  const recent = $derived.by<Recent[]>(() => {
    const rows: Recent[] = [
      ...homeClips.slice(0, 3).map((item) => ({ item, ts: clipTime(item), clip: true })),
      ...homeFiles
        .filter((i) => i.modified_at)
        .map((item) => ({ item, ts: item.modified_at as number, clip: false })),
    ];
    return rows.sort((a, b) => b.ts - a.ts).slice(0, 6);
  });
  function openRecent(r: Recent) {
    if (r.clip) gotoSection(2);
    else activate(r.item);
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
      all = [];
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

  // The five dock sections. Clipboard and Capture are the existing "clip" and
  // "shot" searches; Disk and Settings are their own pages.
  const SECTIONS = ["Search", "Disk", "Clipboard", "Capture", "Settings"] as const;
  const section = $derived(
    panel
      ? 4
      : disk
        ? 1
        : /^\s*(clip|clipboard|clips)(\s|$)/i.test(query)
          ? 2
          : /^\s*(shot|screenshot)(\s|$)/i.test(query)
            ? 3
            : 0,
  );
  function gotoSection(n: number) {
    if (card) return;
    if (panel) {
      if (n !== 4) leavePanel();
      return;
    }
    if (disk) {
      if (n === 1) return;
      disk = false;
    }
    if (n === 1) {
      openDisk();
      return;
    }
    if (n === 4) {
      panel = true;
      return;
    }
    query = n === 2 ? "clip " : n === 3 ? "shot " : "";
    run(query);
    tick().then(() => input?.focus());
  }
  // The empty-bar welcome: what to type and what it does.
  const TRY: [string, string][] = [
    ["renovation invoice", "find words inside files"],
    ["kill port 3000", "see what uses a port"],
    ["10 km to miles", "convert and calculate"],
  ];
  function tryExample(text: string) {
    query = text;
    run(text);
    input?.focus();
  }
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
      all = [];
      done = true;
      selected = 0;
      lane = 0;
      loadHome();
      return;
    }
    done = false;
    invoke("query", { generation, text }).catch(() => {});
  }

  function onUpdate(u: Update) {
    if (u.generation !== generation) return;
    if (u.generation === homeGen) {
      homeClips = u.items.filter((i) => kindName(i.kind) === "clipboard").slice(0, 3);
      if (u.done) {
        generation += 1;
        homeGen2 = generation;
        invoke("query", { generation, text: "past week" }).catch(() => {});
      }
      return;
    }
    if (u.generation === homeGen2) {
      homeFiles = u.items
        .filter((i) => ["file", "screenshot"].includes(kindName(i.kind)) && i.modified_at)
        .sort((a, b) => (b.modified_at ?? 0) - (a.modified_at ?? 0))
        .slice(0, 6);
      return;
    }
    const keep = items[selected]?.id;
    // One block per group, in the order each group first appears; similar-meaning matches go last.
    const rest = u.items.filter((i) => i.source !== "semantic");
    const order = [...new Set(rest.map(groupOf))];
    all = [
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
        else if (query.trim() === "" && !panel && !disk) loadHome();
      } else {
        query = "";
        all = [];
        done = true;
        selected = 0;
        lane = 0;
        panel = false;
        disk = false;
        loadHome(); // also invalidates anything still in flight
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
    if (e.ctrlKey && !e.shiftKey && !e.altKey && ["1", "2", "3", "4", "5"].includes(e.key)) {
      e.preventDefault();
      gotoSection(Number(e.key) - 1);
      return;
    }
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
          // Tab moves to the next filter chip; Shift+Tab still moves focus.
          e.preventDefault();
          lane = (lane + 1) % LANES.length;
          selected = 0;
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
    if (query.trim() !== "" && items.length > 0) return "found";
    if (s?.paused) return "paused";
    if (s?.indexing) return "indexing";
    return "idle";
  });

  // ---- results view helpers ----
  const WINPATH = /^([a-z]:[\\/]|\\\\)/i;
  const IMAGE_EXT = /\.(png|jpe?g|gif|webp|bmp)$/i;
  const pathOf = (i: Item | undefined) => (i?.uri && WINPATH.test(i.uri) ? i.uri : null);
  const isImage = (i: Item | undefined) =>
    !!i && !!pathOf(i) && (kindName(i.kind) === "screenshot" || IMAGE_EXT.test(i.uri ?? ""));
  // Only files, folders, screenshots and projects can be previewed from disk; the
  // backend's preview_file keeps hiding sensitive files and never reads cloud placeholders.
  const previewable = (i: Item | undefined) =>
    !!pathOf(i) && ["file", "folder", "screenshot", "project"].includes(kindName(i!.kind));
  const home = $derived(query.trim() === "" && !panel && !disk && !card);
  const cur = $derived(items[selected] as Item | undefined);
  const previewSource = $derived(previewable(cur) ? { path: pathOf(cur)! } : null);

  // Thumbnail of the top hit, through the same safe preview command.
  const thumbItem = $derived(home ? recent[0]?.item : items[0]);
  let thumb = $state<{ path: string; src: string } | null>(null);
  $effect(() => {
    const p = isImage(thumbItem) ? pathOf(thumbItem) : null;
    if (!p) {
      thumb = null;
      return;
    }
    let dead = false;
    const t = setTimeout(() => {
      invoke<{ state: string; image: string | null }>("preview_file", { node: null, hit: null, path: p })
        .then((r) => {
          if (!dead && r.state === "ok" && r.image) thumb = { path: p, src: r.image };
        })
        .catch(() => {});
    }, 120);
    return () => {
      dead = true;
      clearTimeout(t);
    };
  });
  const thumbSrc = $derived.by(() => {
    const t = thumb as { path: string; src: string } | null;
    return t && t.path === pathOf(thumbItem) ? t.src : null;
  });

  const ACTION_LABEL: Record<string, string> = {
    open: "Open",
    paste_clip: "Paste",
    copy_clip: "Copy",
    kill_port: "Stop…",
  };
  const openLabel = (i: Item | undefined) => ACTION_LABEL[defaultAction(i) ?? ""] ?? "Run";
  function copyText(i: Item | undefined) {
    if (!i || card) return;
    begin(() => invoke<Prepared>("prepare_action", { action: "copy_clip", id: i.id }));
  }

  // Blurred layers are only composited while the window is on screen.
  $effect(() => {
    document.documentElement.dataset.shown = visible ? "true" : "false";
  });
  const dur = (n: number) => (look.reduced ? 0 : n);
</script>

<svelte:window
  onkeydown={onKeydown}
  onblur={() => !pinned && (visible = false)}
  onfocus={() => (visible = true)}
/>
<svelte:document onvisibilitychange={() => (visible = document.visibilityState === "visible")} />

<main class="bar">
  <div class="col" class:wide={disk}>
    <div class="searchrow glass" data-tauri-drag-region>
      <Orb mode={orbMode} active={visible && !look.reduced} />
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
      {#if query.trim() !== "" && !panel && !disk && !card}
        <span class="count" aria-live="polite">{items.length} {items.length === 1 ? "result" : "results"}</span>
      {/if}
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

    {#if !panel && !disk && !card}
      <div class="chips" role="tablist" aria-label="Filter results">
        {#each LANES as l, n}
          <button
            type="button"
            class="chip"
            class:on={lane === n}
            role="tab"
            aria-selected={lane === n}
            onclick={() => {
              lane = n;
              selected = 0;
              input?.focus();
            }}>{l.name}</button
          >
        {/each}
      </div>
    {/if}

    <div class="stage">
      {#if disk}
        <Disk onclose={closeDisk} />
      {:else if panel}
        <Privacy bind:this={privacy} onclose={closePanel} />
      {:else if card?.phase === "confirm"}
        {@const p = card.prepared}
        <div
          class="card glass"
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
        <section class="card glass" role="status">
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
      {:else if home}
        <div class="home" out:fade={{ duration: dur(70) }}>
          <section class="card glass pick" aria-label="Pick up where you left off">
            <h3 class="label-caps">Pick up where you left off</h3>
            {#if recent.length === 0}
              <p class="empty">Nothing here yet. Search, or add folders.</p>
            {:else}
              <ul class="recent">
                {#each recent as r, n (r.item.id)}
                  <li>
                    <button class="row rrow" class:top={n === 0} onclick={() => openRecent(r)}>
                      {#if n === 0 && thumbSrc}
                        <img class="thumb" src={thumbSrc} alt="" />
                      {:else}
                        <span class="icon code">{r.clip ? "clip" : badgeOf(r.item)}</span>
                      {/if}
                      <span class="text">
                        <span class="title">{r.clip ? mask(r.item.title) : r.item.title}</span>
                        <span class="sub" class:code={!r.clip}
                          >{r.clip ? "Copied" : (r.item.uri ?? r.item.subtitle ?? "")}</span
                        >
                      </span>
                      <span class="when">{dayLabel(r.ts)}</span>
                    </button>
                  </li>
                {/each}
              </ul>
            {/if}
          </section>

          <div class="side">
            <section class="card glass trycard" aria-label="Tips">
              <h3 class="label-caps">Try typing</h3>
              <ul class="tips">
                {#each TRY as [example, what]}
                  <li>
                    <button class="ex" onclick={() => tryExample(example)}>{example}</button>
                    <span class="what">{what}</span>
                  </li>
                {/each}
              </ul>
            </section>
            <section class="card glass idx" aria-label="Index">
              <div>
                <strong>{status ? fmt(status.apps) : "…"} apps · {status ? fmt(status.files) : "…"} files</strong>
                <span class="sub"
                  >{#if status?.paused}Paused{:else if status?.indexing}Preparing files…{:else}Add folders to search files{/if}</span
                >
              </div>
              <button class="btn small primary" onclick={() => (panel = true)}>Choose</button>
            </section>
          </div>
        </div>
      {:else if isDiskCommand(query)}
        <div class="res one" in:fly={{ x: 16, duration: dur(120), delay: dur(50) }}>
          <ul class="results card glass" role="listbox">
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
        </div>
      {:else}
        <div class="res" class:one={items.length === 0} in:fly={{ x: 16, duration: dur(120), delay: dur(50) }}>
          <div class="card glass listcard">
            {#if corrected}<p class="note">Showing results for <b>{corrected}</b></p>{/if}
            {#if items.length > 0}
              <ul id="results" class="results" role="listbox" bind:this={list}>
                {#each items as item, i (item.id)}
                  {@const top = i === 0}
                  <li
                    role="presentation"
                    class="entry"
                    style:animation-delay="{Math.min(i, 7) * 20}ms"
                    animate:flip={{ duration: dur(140) }}
                  >
                    {#if !top && (i === 1 || groupOf(items[i - 1]) !== groupOf(item))}
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
                      {#if top && thumbSrc}
                        <img class="thumb" src={thumbSrc} alt="" />
                      {:else}
                        <span class="icon code">{badgeOf(item)}</span>
                      {/if}
                      <span class="text">
                        <span class="l1">
                          {#if top}<span class="tophit">Top hit</span>{/if}
                          <span class="title"
                            >{#each highlight(item.title) as part}{#if part.hit}<mark>{part.text}</mark
                                >{:else}{part.text}{/if}{/each}</span
                          >
                          {#if !top && !isPlugin(item)}
                            {#if typeof item.metadata?.snippet === "string" && item.uri}
                              <span class="meta code">{item.uri}</span>
                            {:else if item.subtitle}
                              <span class="meta" class:code={isPath(item)}>{kindName(item.kind) === "clipboard" ? mask(item.subtitle) : item.subtitle}</span>
                            {/if}
                          {/if}
                        </span>
                        {#if top && item.subtitle}
                          <span class="sub" class:code={isPath(item)} class:wrap={isPlugin(item)}
                            >{#each highlight(kindName(item.kind) === "clipboard" ? mask(item.subtitle) : item.subtitle) as part}{#if part.hit}<mark>{part.text}</mark
                                >{:else}{part.text}{/if}{/each}</span
                          >
                        {:else if !top && isPlugin(item) && item.subtitle}
                          <span class="sub wrap">{kindName(item.kind) === "clipboard" ? mask(item.subtitle) : item.subtitle}</span>
                        {/if}
                        {#if typeof item.metadata?.snippet === "string"}
                          <span class="snippet"
                            >{#each snippetParts(item.metadata.snippet) as part}{#if part.hit}<mark>{part.text}</mark
                                >{:else}{part.text}{/if}{/each}</span
                          >
                        {/if}
                      </span>
                      {#if i === selected && defaultAction(item)}
                        <span class="hint-enter"><kbd>Enter</kbd>{#if top}{openLabel(item)}{/if}</span>
                      {/if}
                    </div>
                  </li>
                {/each}
              </ul>
            {:else if done}
              <p class="empty">No results for “{query.trim()}”</p>
            {/if}
          </div>

          {#if items.length > 0 && cur}
            <aside class="card glass prevcard" aria-label="Preview" in:fly={{ x: 24, duration: dur(120), delay: dur(50) }}>
              {#if previewSource}
                <PreviewPanel source={previewSource} embedded />
              {:else}
                <div class="plain">
                  <strong class="nm">{mask(cur.title)}</strong>
                  {#if cur.subtitle}<span class="sub wrap">{mask(cur.subtitle)}</span>{/if}
                  <span class="sub">{groupOf(cur)}</span>
                </div>
              {/if}
              <div class="prevactions">
                {#if defaultAction(cur)}
                  <button class="btn small primary" onclick={() => activate(cur)}>{openLabel(cur)}</button>
                {/if}
                {#if kindName(cur.kind) === "clipboard" && defaultAction(cur) !== "copy_clip"}
                  <button class="btn small" onclick={() => copyText(cur)}>Copy text</button>
                {/if}
              </div>
            </aside>
          {/if}
        </div>
      {/if}
    </div>

    {#if !card}
      <nav class="dock glass" aria-label="Sections">
        {#each SECTIONS as name, n}
          <button
            type="button"
            class="dockbtn"
            class:on={section === n}
            aria-current={section === n ? "true" : undefined}
            title="Ctrl+{n + 1}"
            onclick={() => gotoSection(n)}>{name}</button
          >
        {/each}
      </nav>
    {/if}

    {#if !panel && !disk}
      <footer>
        {#if notice}
          <span class="warn">{notice}</span>
        {:else}
          <span><kbd>Enter</kbd> open</span>
          <span><kbd>Space</kbd> preview</span>
          <span><kbd>Tab</kbd> next lane</span>
          <span><kbd>Ctrl+1-5</kbd> sections</span>
          <span><kbd>Esc</kbd> back</span>
        {/if}
        <span class="spacer"></span>
        {#if openHotkey}<span class="hotkey">{openHotkey} open</span>{/if}
      </footer>
    {/if}
  </div>
</main>

<style>
  .bar {
    width: 100%;
    height: 100%;
    box-sizing: border-box;
    display: flex;
    justify-content: center;
    background: var(--window-bg);
    color: var(--text);
    border: 1px solid var(--glass-border);
    border-radius: var(--r-window);
    box-shadow: var(--shadow-window);
    overflow: hidden;
  }
  .col {
    width: 900px;
    max-width: 100%;
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 20px 0 12px;
    min-height: 0;
  }
  .col.wide {
    width: 100%;
    padding-left: 12px;
    padding-right: 12px;
  }
  /* The glass panel every surface shares. Flat and unblurred when glass is off. */
  .glass {
    background: var(--glass-bg);
    -webkit-backdrop-filter: var(--glass-blur);
    backdrop-filter: var(--glass-blur);
    border: 1px solid var(--glass-border);
    box-shadow: var(--glass-shadow);
  }

  .searchrow {
    flex: none;
    box-sizing: border-box;
    height: 68px;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: 0 var(--sp-4) 0 22px;
    border-radius: var(--r-pill);
  }
  .count {
    flex: none;
    color: var(--muted);
    font-size: var(--fs-sm);
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
    font-size: 24px;
    font-weight: 500;
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

  .chips {
    flex: none;
    display: flex;
    gap: 8px;
    padding: 0 6px;
  }
  .chip {
    all: unset;
    box-sizing: border-box;
    cursor: pointer;
    padding: 5px 16px;
    border: 1px solid var(--hairline);
    border-radius: var(--r-pill);
    color: var(--text-soft);
    font-size: var(--fs-sm);
    transition:
      background-color var(--dur-fast) var(--ease),
      color var(--dur-fast) var(--ease);
  }
  .chip:hover {
    background: var(--raised);
    color: var(--text);
  }
  .chip.on {
    background: var(--accent-bg);
    border-color: var(--accent-bg);
    color: var(--accent-on);
    font-weight: 600;
  }
  .chip:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .stage {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-rows: minmax(0, 1fr);
  }
  .stage > :global(*) {
    grid-area: 1 / 1;
    min-height: 0;
  }

  /* ---- home ---- */
  .home {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 300px;
    gap: 12px;
    max-height: 400px;
  }
  .home .card {
    border-radius: 28px;
  }
  .pick {
    padding: 18px 20px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-height: 0;
    overflow: hidden;
  }
  .pick h3,
  .trycard h3 {
    margin: 0;
  }
  .recent {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
    overflow-y: auto;
    min-height: 0;
  }
  .rrow {
    all: unset;
    box-sizing: border-box;
    width: 100%;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    height: 58px;
    padding: 0 12px;
    border: 1px solid transparent;
    border-radius: var(--r-lg);
    cursor: pointer;
  }
  .rrow:hover {
    background: var(--raised);
  }
  .rrow:focus-visible {
    outline: 2px solid var(--accent);
  }
  .rrow.top {
    height: 62px;
    border-radius: 18px;
    border-color: var(--accent-line);
    box-shadow: 0 0 24px var(--accent-soft);
    background: var(--raised);
  }
  .rrow .icon,
  .rrow .thumb {
    width: 56px;
    height: 42px;
  }
  .rrow .title {
    max-width: 100%;
  }
  .when {
    flex: none;
    color: var(--muted);
    font-size: var(--fs-sm);
  }
  .side {
    display: flex;
    flex-direction: column;
    gap: 12px;
    min-height: 0;
  }
  .trycard {
    flex: 1;
    padding: 18px 20px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .idx {
    flex: none;
    padding: 14px 18px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
  }
  .idx div {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .tips {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .tips li {
    display: flex;
    flex-direction: column;
  }
  .ex {
    all: unset;
    cursor: pointer;
    color: var(--text);
    font-size: var(--fs-lg);
    font-weight: 500;
  }
  .ex:hover {
    color: var(--accent);
  }
  .ex:focus-visible {
    outline: 2px solid var(--accent);
  }
  .what {
    color: var(--text-soft);
    font-size: var(--fs-sm);
  }

  /* ---- results ---- */
  .res {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 300px;
    gap: 12px;
    min-height: 0;
  }
  .res.one {
    grid-template-columns: minmax(0, 1fr);
  }
  .card {
    border-radius: var(--r-tile);
  }
  .listcard {
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow: hidden;
  }
  .prevcard {
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    min-height: 0;
    overflow: hidden;
  }
  .plain {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-height: 0;
    overflow: auto;
  }
  .nm {
    overflow-wrap: anywhere;
  }
  .prevactions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .results {
    list-style: none;
    margin: 0;
    padding: 8px 10px;
    overflow-y: auto;
    flex: 1;
    min-height: 0;
  }
  .results.card {
    padding: 8px 10px;
  }
  .entry {
    animation: enter 120ms var(--ease) both;
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
    padding: 12px 12px 4px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-height: 44px;
    padding: 6px 12px;
    border: 1px solid transparent;
    border-radius: var(--r-lg);
    cursor: default;
    transition: background-color var(--dur-fast) var(--ease);
  }
  .row.selected {
    background: var(--accent-soft);
    border-color: var(--accent-line);
  }
  .row.top {
    min-height: 72px;
    padding: 9px 14px;
    border-radius: 18px;
    border-color: var(--accent-line);
    box-shadow: 0 0 24px var(--accent-soft);
    background: var(--raised);
  }
  .row.top.selected {
    background: var(--accent-soft);
    border-color: var(--accent);
  }

  .icon {
    flex: none;
    width: 32px;
    height: 32px;
    box-sizing: border-box;
    display: grid;
    place-items: center;
    border: 1px solid var(--hairline);
    border-radius: var(--r-md);
    background: var(--panel);
    color: var(--text-soft);
    font-size: 11px;
    overflow: hidden;
  }
  .row.top .icon {
    width: 54px;
    height: 54px;
    border-radius: var(--r-lg);
    color: var(--accent);
    font-size: var(--fs-xs);
  }
  .thumb {
    flex: none;
    width: 72px;
    height: 54px;
    object-fit: cover;
    border-radius: var(--r-md);
    border: 1px solid var(--hairline);
  }
  .tophit {
    flex: none;
    padding: 1px 8px;
    border-radius: var(--r-pill);
    background: var(--accent-bg);
    color: var(--accent-on);
    font-size: 11px;
    font-weight: 600;
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
  /* Path or source, right-aligned in compact rows. */
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

  .note {
    margin: 0;
    padding: 8px 20px 0;
  }
  .empty {
    padding: 18px 22px;
    color: var(--muted);
    font-size: var(--fs-md);
    margin: 0;
  }

  /* ---- confirm / result card ---- */
  .card:is(section, div[role="alertdialog"]) {
    display: flex;
    flex-direction: column;
    padding: 18px 22px;
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

  /* ---- dock ---- */
  .dock {
    flex: none;
    align-self: center;
    box-sizing: border-box;
    height: 50px;
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 0 8px;
    border-radius: var(--r-pill);
  }
  .dockbtn {
    all: unset;
    box-sizing: border-box;
    cursor: pointer;
    padding: 7px 18px;
    border-radius: var(--r-pill);
    color: var(--text-soft);
    font-size: var(--fs-sm);
    transition:
      background-color var(--dur-fast) var(--ease),
      color var(--dur-fast) var(--ease);
  }
  .dockbtn:hover {
    background: var(--raised);
    color: var(--text);
  }
  .dockbtn.on {
    background: var(--accent-bg);
    color: var(--accent-on);
    font-weight: 600;
  }
  .dockbtn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  footer {
    flex: none;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: center;
    gap: 2px 16px;
    color: var(--muted);
    font-size: var(--fs-xs);
  }
  footer .spacer {
    display: none;
  }
  footer .hotkey {
    color: var(--text-3);
  }
  footer > span {
    white-space: nowrap;
  }
  footer kbd {
    padding: 0 5px;
    color: var(--text-soft);
  }
  .warn {
    color: var(--accent);
  }
</style>
