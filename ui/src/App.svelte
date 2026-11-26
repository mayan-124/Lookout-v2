<script lang="ts">
  import { onMount, tick } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import Privacy from "./Privacy.svelte";
  import Disk from "./Disk.svelte";
  import { flip } from "svelte/animate";
  import { fade, fly } from "svelte/transition";
  import PreviewPanel from "./Preview.svelte";
  import Orb from "./Orb.svelte";
  import { look } from "./look.svelte";
  import Setup from "./Setup.svelte";
  import Help from "./Help.svelte";
  import { isFileRow, movedEnough } from "./dragout";
  import { keepsPage, keepsSearch } from "./lastsearch";
  import { FOLDER_STEP, chooseFoldersTarget, isHelpQuery, stepOnShow } from "./setupflow";
  import { dockItems, footerHints, helpEntries, isDiskCommand, tipsFor, typedSection, type Action, type View } from "./modes";
  import { ACTIONS, ICONS, actionById, homeActions } from "./registry";
  import { didYouMean, ghostFor, opensHistory, placeSuggestions, readLine, suggest, worthRemembering } from "./discover";
  import { recentRows } from "./home";
  import { NO_VALUES, OPERATOR_HINT, completions, operatorGhost, removeOperator, type OperatorValues } from "./operators";
  import { dismissDiskNotice, loadUi, setHideTips, setShow, ui } from "./uistate.svelte";
  import { footerStatus, indexLine, type IndexCounts } from "./indexstatus";
  import { EMPTY_DECK, deal, fixedTips, startRotation, turn } from "./tips";

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
    filters?: { kind: string | null; date: string | null; operators: string[]; invalid_operator: boolean };
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
  interface Shown {
    id: number;
    hidden_secs: number | null;
    remember_last_search: boolean;
  }
  interface Status extends IndexCounts {
    folders: number;
    apps: number;
    needs_setup: boolean;
    semantic_done: number;
    semantic_total: number;
    sizes_pending: boolean;
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
      skip = [];
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
  // Home list: the latest clips (an ordinary "clip " query), then the recently
  // changed files the backend picked. Nothing is stored.
  let homeGen = 0;
  let homeClips: Item[] = $state([]);
  let homeFiles: Item[] = $state([]);
  const mask = (t: string) => t.replaceAll("[masked]", "\u2022\u2022\u2022\u2022");
  function loadHome() {
    generation += 1;
    const asked = (homeGen = generation);
    invoke("query", { generation, text: "clip " }).catch(() => {});
    invoke<Item[]>("recent_files")
      .then((files) => {
        if (asked === homeGen) homeFiles = files;
      })
      .catch(() => {});
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
  // A clip of a temp-folder path is noise in the recent list.
  const TEMPISH = /[\\/]temp[\\/]/i;
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
  const recent = $derived.by<Recent[]>(() =>
    recentRows<Recent>([
      ...homeClips
        .filter((i) => !TEMPISH.test(i.title))
        .slice(0, 3)
        .map((item) => ({ item, ts: clipTime(item), clip: true })),
      ...homeFiles.map((item) => ({ item, ts: item.modified_at ?? 0, clip: false })),
    ]),
  );
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
  // How the last answered command was read ("Timer · 10 minutes").
  let understoodLine = $state("");
  // Date and kind words the backend understood ("PDF", "modified last week"),
  // shown as removable labels. `skip` lists the ones the user removed.
  let filters = $state<{ kind: string | null; date: string | null } | null>(null);
  let skip: string[] = $state([]);
  const understood = $derived(!!(filters && (filters.kind || filters.date)));
  // The "exact words" row is the one after the last result (or the only row).
  const rows = $derived(items.length + (understood ? 1 : 0));
  function removeFilter(name: "kind" | "date") {
    skip = [...skip, name];
    run(query);
    input?.focus();
  }
  function exactWords() {
    skip = ["all"];
    run(query);
    input?.focus();
  }
  // Typed operators ("ext:pdf") the backend understood, one chip each, and
  // whether something only looked like one.
  let typedOps: string[] = $state([]);
  let badOperator = $state(false);
  const typedSize = () => typedOps.some((t) => /^size:/i.test(t));
  // Values for `ext:` and `in:`, asked for once per open, when first needed.
  let opValues: OperatorValues = $state(NO_VALUES);
  let opAsked = false;
  function loadOperatorValues(text: string) {
    if (opAsked || ui.mode === "simple" || !/(^|\s)(ext|in):/i.test(text)) return;
    opAsked = true;
    invoke<OperatorValues>("operator_values")
      .then((v) => (opValues = v))
      .catch(() => (opAsked = false));
  }
  function dropOperator(typed: string) {
    query = removeOperator(query, typed);
    run(query);
    input?.focus();
  }
  let selected = $state(0);
  let status: Status | null = $state(null);
  let notice = $state("");
  let card: Card | null = $state(null);
  // The settings panel (first-run folder choice, exclusions, pause, delete index).
  let panel = $state(false);
  let privacy: ReturnType<typeof Privacy> | undefined = $state();
  // The disk usage analyzer. It scans only while this view is open.
  let disk = $state(false);
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
  // Settings and Disk fill a fixed window; everything else sizes the window to
  // its content so the dock sits right under the panels.

  // Window drag from any non-interactive area. Interactive controls, lists, charts,
  // previews and scrollbars (data-no-drag, or the selector below) stay clickable.
  const NO_DRAG =
    'button, input, select, textarea, a, label, canvas, svg, li, [role="option"], [role="listbox"], [role="tree"], [role="treeitem"], [role="tab"], [role="menu"], [role="separator"], .rows, .chartbox, .grip, .menu, .prevcard, .results, [data-no-drag]';
  const DRAG_TEXT = "h1, h2, h3, h4, header, footer, .label-caps, .hint, .keys, .status, kbd, .total, .crumbs";
  function dragFrom(e: MouseEvent) {
    if (e.button !== 0) return;
    const t = e.target as HTMLElement;
    if (!(t instanceof HTMLElement) || t.hasAttribute("data-tauri-drag-region")) return;
    if (t.closest(NO_DRAG)) return;
    // Scrollbar of a scroll container: the target is the container itself.
    if (t.scrollHeight > t.clientHeight && e.offsetX >= t.clientWidth) return;
    if (t.scrollWidth > t.clientWidth && e.offsetY >= t.clientHeight) return;
    // Plain text stays selectable unless it is a title, header or hint line.
    const own = Array.from(t.childNodes).some((n) => n.nodeType === 3 && (n.textContent ?? "").trim() !== "");
    if (own && !t.closest(DRAG_TEXT)) return;
    getCurrentWindow().startDragging().catch(() => {});
  }
  let colEl: HTMLDivElement | undefined = $state();
  let fitH = $state(0);
  $effect(() => {
    if (!colEl) return;
    const el = colEl;
    const ro = new ResizeObserver(() => {
      fitH = Math.ceil(el.offsetHeight);
    });
    ro.observe(el);
    return () => ro.disconnect();
  });
  $effect(() => {
    if (fitH > 0) {
      invoke("bar_resize", { wide: disk, huge: false, height: fitH }).catch(() => {});
    }
  });
  let confirmButton: HTMLButtonElement | undefined = $state();
  let cancelButton: HTMLButtonElement | undefined = $state();

  // The frontend numbers its own queries. Any update that is not for the
  // latest number is dropped, so a slow answer to an old keystroke can never
  // replace newer results.
  let generation = 0;

  // Reopened soon after hiding, the bar shows the last search again, its text
  // selected (see lastsearch.ts). Settings or Disk come back too, unless Esc or
  // opening something set `cleared`.
  let cleared = false;
  // A search brought back: no ghost text or suggestions until the user types.
  let quiet = $state(false);
  // The text already saved to search history, so a restored one is not saved twice.
  let savedSearch: string | null = null;

  /** Keeps the results as they were, without the suggestion rows. */
  function bringBackSearch() {
    const exact = selected === items.length;
    const keep = items[selected]?.id;
    quiet = true;
    composing = false;
    all = all.filter((i) => i.source !== "action");
    selected = exact && understood ? items.length : Math.max(0, items.findIndex((i) => i.id === keep));
  }

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
    system: "System",
    link: "Link",
    timer: "Timer",
    quickkey: "Quick key",
    action: "Action",
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
      .map((t) => (/^name:/i.test(t) ? t.slice(5).replaceAll('"', "") : t))
      .filter((t) => t.length > 0 && !t.includes(":"))
      .slice(0, 6)
      .map(escapeRe);
    if (terms.length === 0 || !text) return [{ text, hit: false }];
    return text
      .split(new RegExp(`(${terms.join("|")})`, "i"))
      .map((chunk, n) => ({ text: chunk, hit: n % 2 === 1 }))
      .filter((p) => p.text !== "");
  }

  // Clipboard and Capture are the "clip" and "shot" searches; Disk and
  // Settings are their own pages. The mode only decides which the dock lists.
  const section = $derived(panel ? 4 : disk ? 1 : typedSection(query));
  async function uiChange(change: Promise<string>) {
    const error = await change;
    if (error) notice = error;
    input?.focus();
  }
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
  function tryExample(text: string) {
    setInput(text);
  }
  /** Puts text in the box, searches for it and leaves the cursor at its end. */
  function setInput(text: string) {
    query = text;
    skip = [];
    earlier = null;
    run(text);
    tick().then(() => {
      input?.focus();
      input?.setSelectionRange(text.length, text.length);
      caretAtEnd = true;
    });
  }

  // An action that starts one result as soon as the search for it has answered.
  let pending: { generation: number; find: (i: Item) => boolean; missing: string } | null = null;
  function runItem(text: string, find: (i: Item) => boolean, missing: string) {
    setInput(text);
    pending = { generation, find, missing };
  }
  let settingsStart: "general" | "appearance" | "privacy" = $state("general");
  function openSettings(start: typeof settingsStart) {
    disk = false;
    earlier = null;
    settingsStart = start;
    panel = true;
  }
  /** Choosing a suggestion or a quick-action button. */
  function runAction(a: Action) {
    if (card) return;
    switch (a.run) {
      case "fill":
      case "show":
        setInput(a.stem ?? a.example);
        break;
      case "disk":
        openDisk();
        break;
      case "settings":
        openSettings("general");
        break;
      case "appearance":
        openSettings("appearance");
        break;
      case "privacy":
        openSettings("privacy");
        break;
      case "setup":
        query = "";
        run("");
        runSetupAgain();
        break;
      case "theme":
        runItem(a.example, (i) => i.id === "system:dark_mode", "");
        break;
      case "capture":
        runItem(
          a.commands?.[0] ?? a.example,
          (i) => i.metadata?.plugin_action === a.target,
          "Capture needs the Screenshot add-on. Turn it on in Settings > Advanced > Add-ons.",
        );
        break;
    }
  }
  /** A help row or tip: shows its example; screens open at once. */
  function pickAction(a: Action) {
    if (a.try === undefined && ["settings", "appearance", "privacy", "setup"].includes(a.run)) runAction(a);
    else tryExample(a.try ?? a.example);
  }
  const actionItem = (a: Action): Item => ({
    id: `action:${a.id}`,
    kind: "action",
    title: a.name,
    subtitle: a.description,
    uri: null,
    source: "action",
    score: 0,
    metadata: { action: a.id, example: a.example },
  });
  const actionOf = (i: Item) => (i.source === "action" ? actionById(i.metadata!.action as string) : undefined);

  // Earlier searches, newest first; asked for only when Up opens them in the
  // Recent card.
  let earlier = $state<string[] | null>(null);
  let earlierAt = $state(0);
  let historyList: HTMLElement | undefined = $state();
  function moveEarlier(to: number) {
    earlierAt = to;
    tick().then(() => historyList?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest" }));
  }
  async function forgetSearch() {
    if (earlier === null) return;
    const text = earlier[earlierAt];
    const left = earlier.filter((h) => h !== text);
    earlier = left.length > 0 ? left : null;
    moveEarlier(Math.max(0, Math.min(earlierAt, left.length - 1)));
    input?.focus();
    await invoke("remove_search", { text }).catch((e) => (notice = errorText(e)));
  }
  // The faint rest of a command. Not while an input method is composing, with
  // the cursor elsewhere, or after Esc dismissed it for this text.
  let caretAtEnd = $state(true);
  let composing = $state(false);
  let ghostOff = $state<string | null>(null);
  function trackCaret() {
    if (input) caretAtEnd = input.selectionStart === input.value.length && input.selectionEnd === input.value.length;
  }
  const offering = $derived(
    caretAtEnd && !quiet && !composing && ghostOff !== query && !panel && !disk && !card && earlier === null,
  );
  // Chips, suggestions and the hint: not in Simple, nor in a clip or shot search.
  const opsShown = $derived(ui.mode !== "simple" && !panel && !disk && !card && typedSection(query) === 0);
  const offers = $derived(offering && opsShown ? completions(query, opValues) : []);
  const ghost = $derived(
    !offering ? "" : offers.length > 0 ? operatorGhost(query, offers) : ghostFor(query, ACTIONS, ui.mode, ui.show),
  );

  async function openHistory() {
    const asked = query;
    const found = await invoke<string[]>("search_history").catch(() => []);
    if (query !== asked || found.length === 0 || panel || disk || card) return;
    earlier = found;
    earlierAt = 0;
  }
  // Leaving Home drops the list; leaving Settings, the page it was asked for.
  $effect(() => {
    if (!home) earlier = null;
    if (!panel) settingsStart = "general";
  });
  function remember(text: string) {
    if (text === savedSearch || !worthRemembering(text)) return;
    savedSearch = text;
    invoke("record_search", { text }).catch(() => {});
  }
  // Which action Enter starts for a result. The backend decides what is allowed;
  // this only names the action.
  const defaultAction = (i: Item | undefined): string | null => {
    if (!i) return null;
    if (i.source === "action") return "action";
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
      const s = await invoke<{ open_hotkey: string }>("privacy_state");
      openHotkey = s.open_hotkey;
    } catch {
      openHotkey = "";
    }
  }

  async function refreshStatus() {
    const collecting = !!status?.sizes_pending;
    try {
      status = await invoke<Status | null>("index_status");
    } catch {
      status = null;
    }
    // The sizes have arrived: a `size:` search on screen is asked again.
    if (collecting && status && !status.sizes_pending && typedSize()) run(query);
  }

  // Settings may ask "Discard changes?" first.
  function leavePanel() {
    if (privacy) privacy.requestClose();
    else closePanel();
  }

  // The first-run setup: the open step, or null. It never blocks the bar.
  let setup: number | null = $state(null);
  let setupView: ReturnType<typeof Setup> | undefined = $state();
  const showSetup = $derived(setup !== null && query.trim() === "" && !panel && !disk && !card);

  function endSetup() {
    setup = null;
    if (status) status.needs_setup = false;
    invoke("setup_finish").catch((e) => (notice = errorText(e)));
    tick().then(() => input?.focus());
  }
  function trySetupExample(text: string) {
    endSetup();
    tryExample(text);
  }
  function setupChanged() {
    refreshStatus();
    loadHotkey();
  }
  function runSetupAgain() {
    panel = false;
    setup = 1;
  }

  const noFolders = $derived(indexLine(status).state === "nofolders");
  function chooseFolders() {
    if (chooseFoldersTarget(!!status?.needs_setup) === "settings") {
      panel = true; // opens on the Folders page
      return;
    }
    query = "";
    run("");
    setup = FOLDER_STEP;
  }

  // Help, the Home tips and the quick actions all come from the registry.
  let helpSelected = $state(0);
  let helpAdvanced = $state(false);
  const helpList = $derived(helpEntries(ACTIONS, ui.mode, ui.show, helpAdvanced));
  // Three tips on Home. With motion, one row at a time is swapped on a timer;
  // with Reduce motion they only change from one open to the next.
  let deck = $state(EMPTY_DECK);
  let opens = $state(-1);
  let tipsHovered = $state(false);
  let tipsFocused = $state(false);
  const tipPool = $derived(tipsFor(ACTIONS, ui.mode, ui.show));
  const dealt = $derived(deal(tipPool, deck));
  const tips = $derived(
    ui.hide_tips
      ? []
      : (look.reduced ? fixedTips(tipPool, Math.max(0, opens)) : dealt.shown).flatMap(
          (e) => tipPool.find((r) => r.example === e) ?? [],
        ),
  );
  const quickActions = $derived(homeActions(ui.mode, ui.show));
  const showHelp = $derived(isHelpQuery(query) && !panel && !disk && !card);
  function closePanel() {
    panel = false;
    loadPinned();
    refreshStatus();
    loadHotkey();
    tick().then(() => input?.focus());
  }

  function run(text: string) {
    cleared = false;
    quiet = false;
    savedSearch = null;
    generation += 1;
    notice = "";
    pending = null;
    understoodLine = "";
    if (text.trim() === "") {
      all = [];
      filters = null;
      typedOps = [];
      badOperator = false;
      skip = [];
      done = true;
      selected = 0;
      lane = 0;
      loadHome();
      return;
    }
    if (isHelpQuery(text)) {
      all = [];
      filters = null;
      typedOps = [];
      badOperator = false;
      done = true;
      selected = 0;
      helpSelected = 0;
      return;
    }
    done = false;
    loadOperatorValues(text);
    invoke("query", { generation, text, skip }).catch(() => {});
  }

  function onUpdate(u: Update) {
    if (u.generation !== generation) return;
    if (u.generation === homeGen) {
      homeClips = u.items.filter((i) => kindName(i.kind) === "clipboard").slice(0, 3);
      return;
    }
    const keep = items[selected]?.id;
    // One block per group, in the order each group first appears; similar-meaning matches go last.
    const rest = u.items.filter((i) => i.source !== "semantic");
    const order = [...new Set(rest.map(groupOf))];
    const results = [
      ...order.flatMap((g) => rest.filter((i) => groupOf(i) === g)),
      ...u.items.filter((i) => i.source === "semantic"),
    ];
    done = u.done;
    filters = u.filters && (u.filters.kind || u.filters.date) ? u.filters : null;
    typedOps = u.filters?.operators ?? [];
    badOperator = !!u.filters?.invalid_operator;
    // Suggestions wait for something to sit among, unless the word is a command.
    const found = quiet ? [] : suggest(query, ACTIONS, ui.mode, ui.show);
    const intent = found.some((s) => s.exact);
    const ready = u.done || results.length > 0 || intent;
    all = ready ? placeSuggestions(results, found, query, actionItem) : results;
    understoodLine = readLine(results, query);
    // A suggestion never keeps the selection when better rows arrive above it.
    const at = keep && !keep.startsWith("action:") ? items.findIndex((i) => i.id === keep) : -1;
    // An intent word's action is selected unless the user has moved off the top.
    selected = at >= 0 && !(intent && selected === 0) ? at : 0;
    if (pending?.generation === u.generation) {
      const wanted = results.find(pending.find);
      if (wanted) {
        pending = null;
        activate(wanted);
      } else if (u.done) {
        notice = pending.missing;
        pending = null;
      }
    }
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
    const suggested = actionOf(item);
    if (suggested) {
      runAction(suggested);
      return;
    }
    remember(query);
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
    if (rows === 0) return;
    selected = (selected + delta + rows) % rows;
    tick().then(() =>
      list?.querySelector<HTMLElement>(`[data-index="${selected}"]`)?.scrollIntoView({ block: "nearest" }),
    );
  }

  onMount(() => {
    loadHotkey();
    loadPinned();
    loadUi();
    const unlistenShown = listen<Shown>("bar-shown", async (event) => {
      const { hidden_secs, remember_last_search } = event.payload;
      const search = !panel && !disk && keepsSearch(hidden_secs, remember_last_search, query);
      const page = keepsPage(hidden_secs, panel || disk || setup !== null, cleared);
      notice = "";
      earlier = null;
      press = null;
      refusal = null;
      visible = true;
      opens += 1;
      opAsked = false;
      tipsHovered = false;
      // A confirm card is never brought back: its preview must be seen fresh.
      if (card?.phase === "confirm") invoke("cancel_action", { ticket: card.prepared.ticket });
      card = null;
      if (search) {
        bringBackSearch();
      } else {
        query = "";
        all = [];
        filters = null;
        typedOps = [];
        badOperator = false;
        understoodLine = "";
        skip = [];
        quiet = false;
        savedSearch = null;
        done = true;
        selected = 0;
        lane = 0;
        if (!page) {
          panel = false;
          disk = false;
        }
        loadHome(); // also invalidates anything still in flight
      }
      cleared = false;
      input?.focus();
      // Typing replaces a restored search; Enter and the arrows work as before.
      if (search) input?.select();
      loadHotkey();
      // First run: the setup. Not awaited, so showing the bar stays as quick.
      refreshStatus().then(async () => {
        const wasOpen = setup !== null;
        setup = stepOnShow(setup, !!status?.needs_setup, search || page);
        // A panel that was already open takes the focus back; a new one does that itself.
        if (wasOpen && showSetup) {
          await tick();
          setupView?.focus();
        }
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
    // The status refresh runs only while the window is visible: no timer at all when hidden.
    let poll: ReturnType<typeof setInterval> | undefined;
    const syncPoll = () => {
      if (poll !== undefined) clearInterval(poll);
      poll = undefined;
      if (document.visibilityState === "visible") {
        refreshStatus();
        poll = setInterval(refreshStatus, 1000);
      }
    };
    syncPoll();
    document.addEventListener("visibilitychange", syncPoll);
    return () => {
      if (poll !== undefined) clearInterval(poll);
      document.removeEventListener("visibilitychange", syncPoll);
      unlistenShown.then((f) => f());
      unlistenResults.then((f) => f());
    };
  });

  function onKeydown(e: KeyboardEvent) {
    // Keys that belong to an input method are left to it.
    if (e.isComposing) return;
    // Esc during a drag cancels the drag, nothing else.
    if (dragging) {
      e.preventDefault();
      return;
    }
    const plain = !e.ctrlKey && !e.altKey && !e.metaKey && !e.shiftKey;
    if (earlier !== null) {
      if (plain && e.key === "ArrowUp") {
        e.preventDefault();
        moveEarlier((earlierAt + 1) % earlier.length);
        return;
      }
      if (plain && e.key === "Delete") {
        e.preventDefault();
        forgetSearch();
        return;
      }
      if (plain && e.key === "Enter" && e.target === input) {
        e.preventDefault();
        setInput(earlier[earlierAt]);
        return;
      }
      if (e.key === "Escape" || (plain && e.key === "ArrowDown")) {
        e.preventDefault();
        earlier = null;
        input?.focus();
        return;
      }
      // Any other key goes back to typing; Tab and a button's Enter reach the list.
      if (!["Shift", "Tab"].includes(e.key) && !(e.key === "Enter" && e.target instanceof HTMLButtonElement)) earlier = null;
    }
    // An operator value may need quotes, which the faint text cannot add.
    if (offers.length > 0 && plain && e.target === input && (e.key === "Tab" || e.key === "Escape")) {
      e.preventDefault();
      if (e.key === "Tab") setInput(offers[0].text);
      else ghostOff = query;
      return;
    }
    if (ghost && plain && e.target === input) {
      if (e.key === "Tab") {
        e.preventDefault();
        setInput(query + ghost);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        ghostOff = query;
        return;
      }
    }
    if (plain && e.key === "ArrowUp" && e.target === input && opensHistory(view, query)) {
      e.preventDefault();
      openHistory();
      return;
    }
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
        leavePanel();
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
    if (showSetup) {
      // The setup's controls keep their own keys. Esc skips; typing searches.
      if (e.key === "Escape") {
        e.preventDefault();
        endSetup();
        return;
      }
      const field = e.target instanceof HTMLInputElement && e.target.type === "text";
      if (!field && e.key.length === 1 && e.key !== " " && !e.ctrlKey && !e.altKey && !e.metaKey) input?.focus();
      return;
    }
    if (showHelp) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const n = helpList.length;
        if (n > 0) helpSelected = (helpSelected + (e.key === "ArrowDown" ? 1 : n - 1)) % n;
        return;
      }
      if (e.key === "Enter") {
        if (e.target !== input) return; // a focused row clicks itself
        e.preventDefault();
        const entry = helpList[helpSelected];
        if (!entry || e.repeat) return;
        if ("row" in entry) pickAction(entry.row);
        else helpAdvanced = !helpAdvanced;
        return;
      }
    }
    if (e.ctrlKey && e.shiftKey && !e.altKey && e.key.toLowerCase() === "c" && isFile(cur)) {
      e.preventDefault();
      if (!e.repeat) copyFile(cur);
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
        // Away from the search box, on Home, on help and with no folders, Tab moves focus.
        if (e.target !== input || home || showHelp || (noFolders && items.length === 0)) break;
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
        remember(query);
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
        if (e.target instanceof HTMLButtonElement) break; // the button's own click
        e.preventDefault();
        if (isDiskCommand(query)) openDisk();
        else if (!e.repeat && understood && selected === items.length) exactWords();
        else if (!e.repeat && noFolders && done && items.length === 0) chooseFolders();
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
  // One simple icon per kind; kinds without one (answers, ports) keep their short text badge.
  type Ico = "app" | "folder" | "image" | "clipboard" | "file" | "rec" | "gif" | "scan" | "timer" | "moon" | "sun" | "gear" | "globe" | "network" | "key" | "warn" | "dot";
  const BADGE_KINDS = ["calculator", "conversion", "port"];
  function iconKind(i: Item): Ico | null {
    const k = kindName(i.kind);
    if (k.startsWith("timer")) return "timer";
    if (k === "quickkey_missing" || k === "path") return "warn";
    if (k === "url") return "globe";
    if (k === "system") return /dark mode/i.test(i.title) ? "moon" : /light mode/i.test(i.title) ? "sun" : "gear";
    if ((k === "folder" || k === "file") && i.uri?.startsWith("\\\\")) return "network";
    if (k === "app") return "app";
    if (k === "folder") return "folder";
    if (k === "clipboard") return "clipboard";
    if (k === "screenshot" || IMAGE_EXT.test(i.title)) return "image";
    if (k === "file") return "file";
    if (/^record.*gif/i.test(i.title)) return "gif";
    if (/^record/i.test(i.title)) return "rec";
    if (/^extract text/i.test(i.title)) return "scan";
    if (typeof i.metadata?.quick_key === "string") return "key";
    return BADGE_KINDS.includes(k) ? null : "dot";
  }

  // ---- timers: live text only while the window is visible, 1 update per second ----
  let tickNow = $state(Date.now());
  const hasTimers = $derived(items.some((i) => kindName(i.kind) === "timer"));
  $effect(() => {
    if (!visible || !hasTimers) return;
    tickNow = Date.now();
    const id = setInterval(() => (tickNow = Date.now()), 1000);
    return () => clearInterval(id);
  });
  const isRunning = (i: Item | undefined) => !!i && kindName(i.kind) === "timer" && typeof i.metadata?.end_ms === "number";
  const isNewTimer = (i: Item | undefined) => !!i && kindName(i.kind) === "timer_new" && typeof i.metadata?.secs === "number";
  const secsLeft = (i: Item) => Math.max(0, Math.ceil(((i.metadata!.end_ms as number) - tickNow) / 1000));
  const pad2 = (n: number) => String(n).padStart(2, "0");
  function clockText(s: number) {
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    return h > 0 ? `${h}:${pad2(m)}:${pad2(s % 60)}` : `${pad2(m)}:${pad2(s % 60)}`;
  }
  function leftText(s: number) {
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    return h > 0 ? `${h}h ${m}m` : m > 0 ? `${m}m ${s % 60}s` : `${s}s`;
  }
  const endClock = (ms: number) => `${pad2(new Date(ms).getHours())}:${pad2(new Date(ms).getMinutes())}`;
  const RING = 56.55;
  const ringOffset = (i: Item) => RING * (1 - Math.min(1, secsLeft(i) / Math.max(1, i.metadata!.secs as number)));
  // Subtitle text; running timers show the live time left.
  const subOf = (i: Item) =>
    isRunning(i) ? `${leftText(secsLeft(i))} left · ends at ${endClock(i.metadata!.end_ms as number)}` : i.subtitle;
  const home = $derived(query.trim() === "" && !panel && !disk && !card && setup === null);
  const idx = $derived(indexLine(status));
  // No timer unless the tips are on screen and nobody is reading them.
  $effect(() =>
    startRotation(
      {
        visible,
        home,
        hidden: ui.hide_tips,
        hovered: tipsHovered,
        focused: tipsFocused,
        reduced: look.reduced,
        poolSize: tipPool.length,
      },
      () => (deck = turn(tipPool, dealt)),
    ),
  );
  // Rows that leave the screen send no "pointer left" or "focus left".
  $effect(() => {
    if (!visible || !home) tipsHovered = false;
    if (!home) tipsFocused = false;
  });
  const cur = $derived(items[selected] as Item | undefined);
  const meant = $derived(quiet ? "" : didYouMean(suggest(query, ACTIONS, ui.mode, ui.show), cur));
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
    open_url: "Open",
    system_page: "Open",
    system_theme: "Switch",
    system_radio: "Turn…",
    timer_start: "Start",
    timer_cancel: "Cancel timer",
  };

  // Quick keys: a short word that puts a result on top when typed with a space.
  let keyFor: { id: string; alias: string; error: string } | null = $state(null);
  let keyDone = $state("");
  let keyInput: HTMLInputElement | undefined = $state();
  const canKey = (i: Item | undefined) =>
    !!i &&
    !i.source.startsWith("plugin:") &&
    !!i.uri &&
    ["file", "folder", "app", "url", "system"].includes(kindName(i.kind));
  function startKey(i: Item | undefined) {
    if (!i || card) return;
    keyDone = "";
    keyFor = { id: i.id, alias: "", error: "" };
    tick().then(() => keyInput?.focus());
  }
  async function saveKey() {
    const f = keyFor;
    if (!f) return;
    try {
      const alias = await invoke<string>("quickkey_set", { id: f.id, alias: f.alias });
      keyDone = `Quick key set. Type "${alias} " (with the space) to get it.`;
      keyFor = null;
      input?.focus();
    } catch (e) {
      f.error = errorText(e);
    }
  }
  function keyKeydown(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === "Enter") {
      e.preventDefault();
      saveKey();
    } else if (e.key === "Escape") {
      e.preventDefault();
      keyFor = null;
      input?.focus();
    }
  }
  $effect(() => {
    cur?.id;
    keyFor = null;
    keyDone = "";
  });
  const openLabel = (i: Item | undefined) => {
    const a = i && actionOf(i);
    if (a) return a.run === "fill" ? "Type it" : "Open";
    return ACTION_LABEL[defaultAction(i) ?? ""] ?? "Run";
  };
  function copyText(i: Item | undefined) {
    if (!i || card) return;
    begin(() => invoke<Prepared>("prepare_action", { action: "copy_clip", id: i.id }));
  }

  const isFile = (i: Item | undefined) => !!i && isFileRow(kindName(i.kind), i.source, i.uri);
  /** Puts the file itself on the clipboard: what dragging does, from the keyboard. */
  function copyFile(i: Item | undefined) {
    if (!i || card) return;
    remember(query);
    begin(() => invoke<Prepared>("prepare_action", { action: "copy_file", id: i.id }));
  }

  // Dragging a file row into another program. A press becomes a drag only after
  // the pointer has travelled; the shell then runs the drag until the release.
  let press: { id: string; x: number; y: number } | null = null;
  let dragging = false;
  // The click that may follow a drag must not open the result.
  let dragged = false;
  // Why the row under the pointer cannot be dragged, shown as its tooltip.
  let refusal = $state<{ id: string; text: string } | null>(null);
  function checkDrag(i: Item) {
    if (!isFile(i)) return;
    invoke<string | null>("drag_check", { id: i.id })
      .then((text) => (refusal = text ? { id: i.id, text } : null))
      .catch(() => {});
  }
  function pressRow(e: PointerEvent, i: Item) {
    dragged = false;
    press = null;
    // Shift extends a selection; touch scrolls; a button in the row is its own.
    if (e.button !== 0 || e.shiftKey || e.pointerType === "touch" || card || !isFile(i)) return;
    if ((e.target as Element).closest("button")) return;
    press = { id: i.id, x: e.clientX, y: e.clientY };
  }
  function dragMove(e: PointerEvent) {
    if (!press || dragging) return;
    if (!(e.buttons & 1)) {
      press = null;
      return;
    }
    if (!movedEnough(e.clientX - press.x, e.clientY - press.y)) return;
    const { id } = press;
    press = null;
    if (refusal?.id === id) return;
    dragging = true;
    dragged = true;
    notice = "";
    invoke("start_drag", { id })
      .catch((err) => (notice = errorText(err)))
      .finally(() => {
        dragging = false;
        input?.focus(); // the press took the focus from the search box
      });
  }

  const view = $derived.by<View>(() => {
    if (card) return card.phase === "confirm" ? "confirm" : "done";
    if (panel) return "settings";
    if (disk) return "disk";
    if (showSetup) return "setup";
    if (showHelp) return "help";
    if (query.trim() !== "") return "results";
    return earlier === null ? "home" : "history";
  });
  const hints = $derived(
    footerHints(view, {
      mode: ui.mode,
      hotkey: openHotkey,
      enter: defaultAction(cur) ? openLabel(cur) : null,
      path: PATHLIKE.test(query),
      clears: pinned && query !== "",
    }),
  );

  // One quiet line: the status only while something happens, then the keys.
  const footText = $derived([footerStatus(status), ...hints].filter(Boolean).join(" · "));

  const dur = (n: number) => (look.reduced ? 0 : n);
</script>

<svelte:window
  onkeydown={onKeydown}
  onblur={() => !pinned && (visible = false)}
  onfocus={() => (visible = true)}
  onpointermove={dragMove}
  onpointerup={() => (press = null)}
/>
<svelte:document onvisibilitychange={() => (visible = document.visibilityState === "visible")} onselectionchange={trackCaret} />

{#snippet exactRow()}
  <li role="presentation" class="entry">
    <div
      role="option"
      tabindex="-1"
      id={`result-${items.length}`}
      aria-selected={selected === items.length}
      class="row"
      class:selected={selected === items.length}
      data-index={items.length}
      onmousemove={() => (selected = items.length)}
      onclick={exactWords}
      onkeydown={() => {}}
    >
      <span class="icon" aria-hidden="true"
        ><svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"
          ><circle cx="7" cy="7" r="4.5" /><path d="M10.5 10.5L14 14" /></svg
        ></span
      >
      <span class="text">
        <span class="title">Search for the exact words instead</span>
        <span class="meta">“{query.trim()}”</span>
      </span>
      {#if selected === items.length}<span class="hint-enter"><kbd>Enter</kbd></span>{/if}
    </div>
  </li>
{/snippet}

{#snippet glyph(category: string)}
  <svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" stroke-linecap="round" aria-hidden="true"
    ><path d={ICONS[category] ?? ICONS.Search} /></svg
  >
{/snippet}

{#snippet tile(it: Item)}
  {@const k = iconKind(it)}
  {@const act = actionOf(it)}
  {#if act}
    <span class="icon" aria-hidden="true">{@render glyph(act.category)}</span>
  {:else if isRunning(it)}
    <span class="icon ring" aria-hidden="true">
      <svg viewBox="0 0 24 24" fill="none" stroke-width="2.4" stroke-linecap="round">
        <circle cx="12" cy="12" r="9" stroke="var(--hairline)" />
        <circle cx="12" cy="12" r="9" stroke="currentColor" transform="rotate(-90 12 12)" stroke-dasharray={RING} stroke-dashoffset={ringOffset(it)} />
      </svg>
    </span>
  {:else if k}
    <span class="icon" aria-hidden="true">
      <svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" stroke-linecap="round">
        {#if k === "app"}
          <rect x="2.5" y="2.5" width="4.5" height="4.5" rx="1" /><rect x="9" y="2.5" width="4.5" height="4.5" rx="1" /><rect x="2.5" y="9" width="4.5" height="4.5" rx="1" /><rect x="9" y="9" width="4.5" height="4.5" rx="1" />
        {:else if k === "folder"}
          <path d="M2 4.5A1.5 1.5 0 0 1 3.5 3h2.6l1.4 1.5h5A1.5 1.5 0 0 1 14 6v5.5a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 11.5z" />
        {:else if k === "image"}
          <rect x="2" y="3" width="12" height="10" rx="1.6" /><circle cx="5.7" cy="6.5" r="1.1" /><path d="M2.5 12l3.6-3.4 2.6 2.4 2-1.8 3.3 2.8" />
        {:else if k === "rec"}
          <circle cx="8" cy="8" r="5.5" /><circle cx="8" cy="8" r="2.4" fill="currentColor" />
        {:else if k === "gif"}
          <rect x="1.8" y="3.5" width="12.4" height="9" rx="1.8" /><path d="M6.6 6.6a1.6 1.6 0 1 0 0 2.8V8.2H5.8M8.8 6.4v3.2M10.4 9.6V6.4h1.7M10.4 8h1.3" />
        {:else if k === "scan"}
          <path d="M2 5.5V3.5A1.5 1.5 0 0 1 3.5 2h2M10 2h2.5A1.5 1.5 0 0 1 14 3.5v2M14 10.5v2a1.5 1.5 0 0 1-1.5 1.5H10M6 14H3.5A1.5 1.5 0 0 1 2 12.5v-2M5.5 6h5M8 6v4.5" />
        {:else if k === "timer"}
          <circle cx="8" cy="9" r="5" /><path d="M8 9V6.4M6.5 2h3M12 4l.9-.9" />
        {:else if k === "moon"}
          <path d="M13 9.3A5.5 5.5 0 0 1 6.7 3a5.5 5.5 0 1 0 6.3 6.3z" />
        {:else if k === "sun"}
          <circle cx="8" cy="8" r="2.6" /><path d="M8 1.8v1.5M8 12.7v1.5M1.8 8h1.5M12.7 8h1.5M3.6 3.6l1 1M11.4 11.4l1 1M3.6 12.4l1-1M11.4 4.6l1-1" />
        {:else if k === "gear"}
          <circle cx="8" cy="8" r="2" /><path d="M8 1.8l1 1.6 1.9-.4.5 1.8 1.8.6-.5 1.9L14 8l-1.3 1.2.5 1.9-1.8.6-.5 1.8-1.9-.4L8 14.2l-1-1.6-1.9.4-.5-1.8-1.8-.6.5-1.9L2 8l1.3-1.2-.5-1.9 1.8-.6.5-1.8 1.9.4z" />
        {:else if k === "globe"}
          <circle cx="8" cy="8" r="5.8" /><path d="M2.2 8h11.6M8 2.2c2 1.7 2.8 3.6 2.8 5.8S10 12.1 8 13.8C6 12.1 5.2 10.2 5.2 8S6 3.9 8 2.2z" />
        {:else if k === "network"}
          <rect x="6" y="2" width="4" height="3" rx=".8" /><rect x="1.8" y="11" width="4" height="3" rx=".8" /><rect x="10.2" y="11" width="4" height="3" rx=".8" /><path d="M8 5v3M4 11V8h8v3" />
        {:else if k === "key"}
          <circle cx="5.2" cy="10.8" r="2.7" /><path d="M7.2 8.8l6.3-6.3M11 5l1.8 1.8M9.3 6.7l1.4 1.4" />
        {:else if k === "warn"}
          <path d="M8 2.2L14.2 13H1.8z" /><path d="M8 6.5v3M8 11.3v.1" />
        {:else if k === "dot"}
          <circle cx="8" cy="8" r="1.6" fill="currentColor" />
        {:else if k === "clipboard"}
          <rect x="3.5" y="3" width="9" height="10.5" rx="1.5" /><path d="M6 3V2h4v1M5.8 7h4.4M5.8 9.5h4.4" />
        {:else}
          <path d="M4 2h5l3 3v9H4z" /><path d="M9 2v3h3" />
        {/if}
      </svg>
    </span>
  {:else}
    <span class="icon code">{badgeOf(it)}</span>
  {/if}
{/snippet}

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<main
  class="bar"
  onmousedown={(e) => {
    // Clicking the transparent gaps must not take focus from the search box.
    const t = e.target as HTMLElement;
    if (t === e.currentTarget || t.classList.contains("col") || t.classList.contains("stage")) e.preventDefault();
    dragFrom(e);
  }}
>
  <div class="col" class:wide={disk} bind:this={colEl}>
    <div class="head">
    <div class="searchrow glass" data-tauri-drag-region>
      <Orb mode={orbMode} active={visible && !look.reduced} />
      <div class="searchbox">
      {#if ghost}
        <span class="ghost" aria-hidden="true"><span class="typed">{query}</span>{ghost}</span>
      {/if}
      <input
        bind:this={input}
        bind:value={query}
        oninput={() => {
          skip = [];
          earlier = null;
          trackCaret();
          run(query);
        }}
        onkeyup={trackCaret}
        onclick={trackCaret}
        onselect={trackCaret}
        oncompositionstart={() => (composing = true)}
        oncompositionend={() => {
          composing = false;
          trackCaret();
        }}
        class="search"
        type="text"
        placeholder={panel ? "Settings" : "Search files and apps, or type help"}
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
      </div>
      <span class="sr" aria-live="polite">{ghost ? `Suggestion: ${query}${ghost}. Press Tab to accept.` : offers.length > 0 ? `Suggestion: ${offers[0].label}. Press Tab to accept.` : ""}</span>
      {#if query.trim() !== "" && !panel && !disk && !card && !showHelp}
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

    {#if !panel && !disk && !card && !showHelp && !showSetup}
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
    </div>

    <div class="stage">
      {#if setup !== null && !panel}
        <!-- Stays while a search is typed, so ticked folders are not lost. -->
        <div class="setupwrap glass" hidden={!showSetup}>
          <Setup bind:this={setupView} bind:step={setup} onclose={endSetup} ontry={trySetupExample} onchanged={setupChanged} />
        </div>
      {/if}
      {#if showSetup}
        <!-- drawn above -->
      {:else if disk}
        <div class="page glass diskpage"><Disk onclose={closeDisk} /></div>
      {:else if panel}
        <div class="page glass"><Privacy bind:this={privacy} start={settingsStart} onclose={closePanel} onsetup={runSetupAgain} /></div>
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
      {:else if showHelp}
        <div class="card glass">
          <Help entries={helpList} bind:selected={helpSelected} onpick={pickAction} ontoggle={() => (helpAdvanced = !helpAdvanced)} />
        </div>
      {:else if home}
        <div class="home" out:fade={{ duration: dur(70) }}>
          <section class="card glass pick" aria-label={earlier === null ? "Recent" : "Recent searches"}>
            <!-- Recent keeps its place under the searches, so the height never changes. -->
            <div class="face" class:away={earlier !== null}>
              <h3 class="label-caps">Recent</h3>
              {#if recent.length === 0}
                <p class="empty">{noFolders ? "Nothing yet. Choose folders to search." : "Nothing yet. Type to search."}</p>
              {:else}
                <ul class="recent">
                  {#each recent as r, n (r.item.id)}
                    <li>
                      <button class="row rrow" class:top={n === 0} onclick={() => openRecent(r)}>
                        {#if n === 0 && thumbSrc}
                          <img class="thumb" src={thumbSrc} alt="" />
                        {:else}
                          {@render tile(r.item)}
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
            </div>
            {#if earlier !== null}
              <div class="face over" data-no-drag>
                <h3 class="label-caps">Recent searches</h3>
                <ul class="recent" role="listbox" aria-label="Recent searches" bind:this={historyList}>
                  {#each earlier as h, n (h)}
                    <li role="presentation">
                      <button
                        type="button"
                        class="row rrow"
                        class:top={n === earlierAt}
                        class:picked={n === earlierAt}
                        role="option"
                        aria-selected={n === earlierAt}
                        onfocus={() => (earlierAt = n)}
                        onmousemove={() => (earlierAt = n)}
                        onclick={() => setInput(h)}
                      >
                        <span class="icon" aria-hidden="true">{@render glyph("Search")}</span>
                        <span class="text"><span class="title">{h}</span></span>
                      </button>
                    </li>
                  {/each}
                </ul>
                <!-- Focus stays in the search box, so the chosen line is announced. -->
                <span class="sr" aria-live="polite">{earlier[earlierAt]}</span>
              </div>
            {/if}
          </section>

          <section class="card glass side" aria-label={tips.length > 0 ? "Tips and index" : "Index"}>
            {#if tips.length > 0}
              <div class="tiphead">
                <h3 class="label-caps">Tips</h3>
                <button class="btn small" onclick={() => uiChange(setHideTips(true))}>Hide tips</button>
              </div>
              <!-- Not a live region: screen readers must not hear each swap. -->
              <ul
                class="tips"
                onpointerenter={() => (tipsHovered = true)}
                onpointerleave={() => (tipsHovered = false)}
                onfocusin={() => (tipsFocused = true)}
                onfocusout={(e) => (tipsFocused = e.currentTarget.contains(e.relatedTarget as Node | null))}
              >
                {#each tips as t, n (n)}
                  <li>
                    {#key t.example}
                      <div class="tip" in:fade={{ duration: dur(160) }} out:fade={{ duration: dur(160) }}>
                        <svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" stroke-linecap="round" aria-hidden="true"
                          ><path d={ICONS[t.category] ?? ICONS.Search} /></svg
                        >
                        <span class="what" title={t.description}>{t.description}</span>
                        <button class="ex" onclick={() => pickAction(t)}>{t.example}</button>
                      </div>
                    {/key}
                  </li>
                {/each}
              </ul>
            {/if}
            <div class="qgrid" role="group" aria-label="Quick actions">
              {#each quickActions as a (a.id)}
                <button type="button" class="btn small qa" onclick={() => runAction(a)}>
                  {@render glyph(a.category)}<span>{a.label ?? a.name}</span>
                </button>
              {/each}
            </div>
            <div class="idx">
              <div>
                <strong>{idx.text}</strong>
                <span class="sub">{status ? fmt(status.apps) : "…"} apps</span>
              </div>
              {#if noFolders}
                <button class="btn small primary" onclick={chooseFolders}>Choose folders</button>
              {:else}
                <button class="btn small" onclick={() => (panel = true)}>Manage folders</button>
              {/if}
            </div>
          </section>
        </div>
      {:else if isDiskCommand(query)}
        <div class="res one" in:fly={{ x: 16, duration: dur(120), delay: dur(50) }}>
          <ul class="results card glass" role="listbox">
            <li role="presentation" class="entry">
              <div role="option" tabindex="-1" aria-selected="true" class="row selected" onclick={openDisk} onkeydown={() => {}}>
                <span class="icon" aria-hidden="true"
                  ><svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.5"
                    ><circle cx="8" cy="8" r="5.5" /><path d="M8 8V2.5M8 8l4 3" /></svg
                  ></span
                >
                <span class="text">
                  <span class="title">Disk analyzer</span>
                  <span class="snippet">See what uses space on a drive or folder. Scans only when you press Scan.</span>
                </span>
                <span class="hint-enter"><kbd>Enter</kbd></span>
              </div>
            </li>
          </ul>
        </div>
      {:else}
        <div class="res" class:one={items.length === 0} in:fly={{ x: 16, duration: dur(120), delay: dur(50) }}>
          <div class="card glass listcard">
            {#if offers.length > 0}
              <div class="filters" role="group" aria-label="Suggestions">
                {#each offers as o, n (o.text)}
                  <button type="button" class="fchip" class:first={n === 0} onclick={() => setInput(o.text)}>{o.label}</button>
                {/each}
                <span class="tabkey"><kbd>Tab</kbd></span>
              </div>
            {/if}
            {#if filters || (opsShown && typedOps.length > 0)}
              <div class="filters" aria-label="Understood from your words">
                {#if opsShown}
                  {#each typedOps as t, n (n)}
                    <button type="button" class="fchip" title="Remove this filter" onclick={() => dropOperator(t)}>
                      <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round" stroke-linecap="round" aria-hidden="true"
                        ><path d="M2 3h12l-4.5 5.5V13l-3-1.5V8.5z" /></svg
                      >{t}<svg class="x" viewBox="0 0 16 16" width="10" height="10" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" aria-hidden="true"
                        ><path d="M4 4l8 8M12 4l-8 8" /></svg
                      >
                    </button>
                  {/each}
                {/if}
                {#if filters?.kind}
                  <button type="button" class="fchip" title="Remove this filter" onclick={() => removeFilter("kind")}>
                    <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round" stroke-linecap="round" aria-hidden="true"
                      ><path d="M2 3h12l-4.5 5.5V13l-3-1.5V8.5z" /></svg
                    >{filters.kind}<svg class="x" viewBox="0 0 16 16" width="10" height="10" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" aria-hidden="true"
                      ><path d="M4 4l8 8M12 4l-8 8" /></svg
                    >
                  </button>
                {/if}
                {#if filters?.date}
                  <button type="button" class="fchip" title="Remove this filter" onclick={() => removeFilter("date")}>
                    <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round" stroke-linecap="round" aria-hidden="true"
                      ><rect x="2" y="3.5" width="12" height="10.5" rx="2" /><path d="M2 7h12M5.5 2v3M10.5 2v3" /></svg
                    >{filters.date}<svg class="x" viewBox="0 0 16 16" width="10" height="10" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" aria-hidden="true"
                      ><path d="M4 4l8 8M12 4l-8 8" /></svg
                    >
                  </button>
                {/if}
              </div>
            {/if}
            {#if status?.sizes_pending && typedSize()}<p class="readline muted" role="status">Sizes are still being collected</p>{/if}
            {#if opsShown && badOperator && offers.length === 0}<p class="readline" role="status">{OPERATOR_HINT}</p>{/if}
            {#if meant}<p class="readline" role="status">{meant}</p>{/if}
            {#if understoodLine}<p class="readline" role="status">{understoodLine}</p>{/if}
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
                      class:draggable={isFile(item)}
                      data-index={i}
                      title={refusal?.id === item.id ? refusal.text : undefined}
                      onmousemove={() => (selected = i)}
                      onpointerenter={() => checkDrag(item)}
                      onpointerdown={(e) => pressRow(e, item)}
                      ondragstart={(e) => e.preventDefault()}
                      onclick={() => {
                        if (!dragged) activate(item);
                        dragged = false;
                      }}
                      onkeydown={() => {}}
                    >
                      {#if top && thumbSrc}
                        <img class="thumb" src={thumbSrc} alt="" />
                      {:else}
                        {@render tile(item)}
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
                              <span class="meta" class:code={isPath(item)}>{kindName(item.kind) === "clipboard" ? mask(item.subtitle) : subOf(item)}</span>
                            {/if}
                          {/if}
                        </span>
                        {#if top && item.subtitle}
                          <span class="sub" class:code={isPath(item)} class:wrap={isPlugin(item)}
                            >{#each highlight(kindName(item.kind) === "clipboard" ? mask(item.subtitle) : (subOf(item) ?? "")) as part}{#if part.hit}<mark>{part.text}</mark
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
                      {#if item.source === "action"}
                        <kbd class="qk" title="Example">{item.metadata?.example}</kbd>
                      {/if}
                      {#if typeof item.metadata?.quick_key === "string"}
                        <kbd class="qk" title="Quick key">{item.metadata.quick_key}</kbd>
                      {/if}
                      {#if isRunning(item)}
                        <button
                          type="button"
                          class="btn small rowcancel"
                          onclick={(e) => {
                            e.stopPropagation();
                            activate(item);
                          }}>Cancel</button
                        >
                      {/if}
                      {#if i === selected && defaultAction(item)}
                        <span class="hint-enter"><kbd>Enter</kbd>{#if top}{openLabel(item)}{/if}</span>
                      {/if}
                    </div>
                  </li>
                {/each}
                {#if understood}{@render exactRow()}{/if}
              </ul>
            {:else}
              {#if understood}
                <ul id="results" class="results" role="listbox">{@render exactRow()}</ul>
              {/if}
              {#if done}
                <p class="empty">No results for “{query.trim()}”</p>
                {#if noFolders}
                  <div class="nofolders">
                    <span>Lookout has no folders to search yet.</span>
                    <button class="btn primary" onclick={chooseFolders}>Choose folders to search <kbd>Enter</kbd></button>
                  </div>
                {/if}
              {/if}
            {/if}
          </div>

          {#if items.length > 0 && cur}
            <aside class="card glass prevcard" aria-label="Preview" in:fly={{ x: 24, duration: dur(120), delay: dur(50) }}>
              {#if previewSource}
                <PreviewPanel source={previewSource} embedded />
              {:else if isRunning(cur) || isNewTimer(cur)}
                <div class="plain timerprev">
                  <span class="big" aria-live="off">{clockText(isRunning(cur) ? secsLeft(cur!) : (cur!.metadata!.secs as number))}</span>
                  <strong class="nm">{mask(cur!.title)}</strong>
                  <span class="sub wrap">{isRunning(cur) ? `Ends at ${endClock(cur!.metadata!.end_ms as number)}` : (cur!.subtitle ?? "")}</span>
                </div>
              {:else}
                <div class="plain">
                  <strong class="nm">{mask(cur.title)}</strong>
                  {#if cur.subtitle}<span class="sub wrap">{mask(cur.subtitle)}</span>{/if}
                  {#if cur.source === "action"}<span class="sub">Example: <span class="code">{cur.metadata?.example}</span></span>{/if}
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
                {#if isFile(cur)}
                  <button class="btn small" title="Ctrl+Shift+C" onclick={() => copyFile(cur)}>Copy file</button>
                {/if}
                {#if ui.show.quick_keys && canKey(cur) && !keyFor}
                  <button class="btn small" onclick={() => startKey(cur)}>Set quick key</button>
                {/if}
              </div>
              {#if keyFor}
                <div class="keyform">
                  <input
                    type="text"
                    bind:this={keyInput}
                    bind:value={keyFor.alias}
                    maxlength="8"
                    placeholder="2–8 letters or digits"
                    aria-label="Quick key"
                    aria-invalid={keyFor.error ? "true" : undefined}
                    onkeydown={keyKeydown}
                  />
                  <button class="btn small primary" onclick={saveKey}>Save</button>
                  <button class="btn small" onclick={() => ((keyFor = null), input?.focus())}>Cancel</button>
                </div>
                {#if keyFor.error}<p class="keymsg warn" role="alert">{keyFor.error}</p>{/if}
              {:else if keyDone}
                <p class="keymsg" role="status">{keyDone}</p>
              {/if}
            </aside>
          {/if}
        </div>
      {/if}
    </div>

    <div class="foot">
    {#if ui.disk_notice && !card && !showSetup}
      <div class="moved glass" role="status">
        <span>Disk moved to Full mode in Settings &gt; Features</span>
        <button type="button" class="btn small primary" onclick={() => uiChange(setShow("disk", true))}>Turn Disk on</button>
        <button type="button" class="btn small" onclick={() => uiChange(dismissDiskNotice())}>Dismiss</button>
      </div>
    {/if}
    {#if !card}
      <nav class="dock glass" aria-label="Sections">
        {#each dockItems(ui.show) as { name, n } (n)}
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

    <footer title={notice || footText}>
      {#if notice}<span class="warn" role="alert">{notice}</span>{:else}{footText}{/if}
    </footer>
    </div>
  </div>
</main>

<style>
  /* No outer card: no background, border, radius or shadow on the wrapper. */
  .bar {
    width: 100%;
    height: 100%;
    display: flex;
    justify-content: center;
    align-items: flex-start;
    color: var(--text);
  }
  .head,
  .foot {
    flex: none;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .foot {
    gap: 8px;
  }
  .col {
    width: 900px;
    max-width: 100%;
    display: flex;
    flex-direction: column;
    gap: 16px;
    padding: 20px 0 14px;
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
  .searchbox {
    position: relative;
    display: flex;
    align-items: center;
    flex: 1;
    min-width: 0;
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
  .ghost {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    overflow: hidden;
    white-space: pre;
    pointer-events: none;
    font-size: 24px;
    font-weight: 500;
    color: var(--muted);
    opacity: 0.6;
  }
  .ghost .typed {
    visibility: hidden;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
  .readline {
    margin: 0;
    padding: 8px 20px 0;
    color: var(--text-soft);
    font-size: var(--fs-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .readline.muted {
    color: var(--muted);
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
    padding: 0;
  }
  .chip {
    all: unset;
    box-sizing: border-box;
    cursor: pointer;
    padding: 5px 16px;
    background: var(--glass-bg);
    border: 1px solid var(--glass-border);
    border-radius: var(--r-pill);
    color: var(--text-soft);
    font-size: var(--fs-sm);
    transition:
      background-color var(--dur-fast) var(--ease),
      color var(--dur-fast) var(--ease);
  }
  .chip:hover {
    color: var(--text);
    border-color: var(--hover-border);
  }
  .chip.on {
    background: var(--accent-bg);
    border-color: var(--chip-on-border);
    box-shadow: 0 0 0 1px var(--chip-on-glow);
    color: var(--accent-on);
    font-weight: 600;
  }
  .chip:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .page {
    height: 500px;
    background: var(--glass-bg);
    border-radius: var(--r-tile);
    display: flex;
    flex-direction: column;
    overflow: hidden;
    min-height: 0;
  }
  .stage {
    flex: none;
    min-height: 0;
    display: grid;
    grid-template-rows: auto;
  }
  .stage > :global(*) {
    grid-area: 1 / 1;
    min-height: 0;
  }

  .moved {
    display: flex;
    align-items: center;
    justify-content: center;
    flex-wrap: wrap;
    gap: 10px;
    align-self: center;
    padding: 8px 16px;
    border-radius: var(--r-pill);
    font-size: var(--fs-sm);
  }

  /* ---- home ---- */
  .home {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 300px;
    gap: 16px;
    align-self: start;
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
  .pick h3 {
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
    background: var(--accent-soft);
  }
  .rrow:focus-visible {
    outline: 2px solid var(--accent);
  }
  .rrow.top {
    height: 62px;
    border-radius: 18px;
    border-color: var(--accent-line);
    box-shadow: 0 0 24px var(--accent-soft);
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
  .pick {
    position: relative;
    min-height: 230px;
  }
  .face {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-height: 0;
  }
  .face.away {
    visibility: hidden;
  }
  .face.over {
    position: absolute;
    inset: 18px 22px;
  }
  .rrow.picked {
    background: var(--accent-soft);
  }
  .qgrid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 8px;
  }
  .qa {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    min-width: 0;
  }
  .qa :global(svg) {
    flex: none;
  }
  .qa span {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .side {
    padding: 18px 20px;
    display: flex;
    flex-direction: column;
    gap: 14px;
    min-height: 0;
  }
  .side h3 {
    margin: 0;
  }
  .setupwrap {
    justify-self: center;
    box-sizing: border-box;
    width: min(640px, 100%);
    border-radius: var(--r-tile);
    overflow: hidden;
  }
  .nofolders {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 12px;
    padding: 0 22px 20px;
    color: var(--text-soft);
    font-size: var(--fs-md);
  }
  .idx {
    margin-top: auto;
    flex-wrap: wrap;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    text-align: left;
  }
  .idx div {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .idx strong {
    font-weight: 500;
  }
  .tiphead {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  .tips {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  /* The old and new tip share one cell while they crossfade. */
  .tips li {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
  }
  .tip {
    grid-area: 1 / 1;
    min-width: 0;
    display: grid;
    grid-template-columns: 16px minmax(0, 1fr);
    align-items: center;
    column-gap: 10px;
  }
  .tip svg {
    grid-row: span 2;
    color: var(--muted);
  }
  .ex {
    all: unset;
    justify-self: start;
    max-width: 100%;
    cursor: pointer;
    color: var(--text);
    font-size: var(--fs-md);
    font-weight: 500;
  }
  .ex:hover {
    color: var(--accent);
  }
  .ex:focus-visible {
    outline: 2px solid var(--accent);
  }
  .what {
    color: var(--muted);
    font-size: var(--fs-sm);
  }
  .what,
  .ex {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* ---- results ---- */
  .res {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 300px;
    gap: 16px;
    min-height: 120px;
    max-height: 440px;
  }
  .res.one {
    min-height: 0;
    grid-template-columns: minmax(0, 1fr);
  }
  .card {
    border-radius: var(--r-tile);
  }
  .listcard {
    display: flex;
    flex-direction: column;
    min-height: 0;
    max-height: 440px;
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
  .keyform {
    display: flex;
    gap: 8px;
    margin-top: 8px;
  }
  .keyform input {
    flex: 1;
    min-width: 0;
  }
  .keymsg {
    margin: 6px 0 0;
    font-size: 12px;
    color: var(--text-soft);
  }
  .keymsg.warn {
    color: var(--danger);
  }
  .prevactions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  /* A press on a file row starts a drag, not a text selection. */
  .row.draggable {
    user-select: none;
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
    max-height: 440px;
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
    background: transparent;
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
    border-color: var(--accent);
    box-shadow: 0 0 26px var(--accent-soft);
  }
  .row.top.selected {
    background: var(--accent-soft);
    border-color: var(--accent);
  }
  /* secondary text is too faint on the highlight */
  .row.selected .meta {
    color: var(--text-soft);
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
    overflow: hidden;
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
    font-weight: 500;
  }
  .row.top .title {
    flex: 0 1 auto;
    min-width: 0;
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
    color: var(--muted);
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
  /* Typed words: accent colour only, no box, normal weight. */
  mark {
    padding: 0;
    background: none;
    color: var(--match-text);
    font-weight: inherit;
  }
  .icon.ring {
    color: var(--accent);
  }
  .icon.ring svg {
    width: 22px;
    height: 22px;
  }
  .row.top .icon.ring svg {
    width: 30px;
    height: 30px;
  }
  .qk {
    flex: none;
  }
  .rowcancel {
    flex: none;
  }
  .timerprev .big {
    font-family: var(--font-code);
    font-size: 40px;
    line-height: 1.1;
    font-weight: 500;
    font-variant-numeric: tabular-nums;
    color: var(--accent);
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
  .filters {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 8px 20px 0;
  }
  .fchip {
    all: unset;
    box-sizing: border-box;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 3px 10px;
    background: var(--glass-bg);
    border: 1px solid var(--glass-border);
    border-radius: var(--r-pill);
    color: var(--text-soft);
    font-size: var(--fs-xs);
  }
  .fchip:hover {
    color: var(--text);
    border-color: var(--hover-border);
  }
  .fchip .x {
    opacity: 0.6;
  }
  .fchip.first {
    color: var(--text);
    border-color: var(--hover-border);
  }
  .tabkey {
    align-self: center;
    font-size: var(--fs-xs);
    color: var(--muted);
  }
  .fchip:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .empty {
    padding: 20px 22px;
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
    background: var(--glass-bg-dock);
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

  /* No box: the shadow keeps it readable over whatever is behind the window. */
  footer {
    flex: none;
    max-width: 100%;
    box-sizing: border-box;
    padding: 0 14px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    text-align: center;
    color: var(--muted);
    font-size: var(--fs-xs);
    text-shadow:
      0 0 4px var(--panel-base),
      0 0 8px var(--panel-base);
  }
  .warn {
    color: var(--accent);
  }
</style>
