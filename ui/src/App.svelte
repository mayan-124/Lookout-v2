<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
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
  import Row from "./Row.svelte";
  import Icon, { type Ico } from "./Icon.svelte";
  import { ageLabel, changesList, hidesBar, rowKeys, verbOf } from "./rows";
  import { isClipRow, isFileRow, movedEnough } from "./dragout";
  import { iconLoader, iconTarget, rowWatcher } from "./appicons";
  import { keepsPage, keepsSearch } from "./lastsearch";
  import { FOLDER_STEP, chooseFoldersTarget, isHelpQuery, stepOnShow } from "./setupflow";
  import { dockItems, footerHints, helpEntries, isDiskCommand, tipsFor, typedSection, type Action, type View } from "./modes";
  import { ACTIONS, ICONS, actionById, homeActions } from "./registry";
  import { didYouMean, ghostFor, isListCommand, isNoteText, opensHistory, placeSuggestions, readLine, suggest, worthRemembering } from "./discover";
  import { isFreshClip, recentRows } from "./home";
  import { NO_VALUES, OPERATOR_HINT, completions, operatorGhost, removeOperator, type OperatorValues } from "./operators";
  import { dismissDiskNotice, loadUi, setHideTips, setShow, ui } from "./uistate.svelte";
  import { footerStatus, indexLine, type IndexCounts } from "./indexstatus";
  import { EMPTY_DECK, deal, fixedTips, startRotation, turn } from "./tips";
  import { plain as errorText } from "./errors";
  import { pv } from "./previewui.svelte";

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
    // `hides`: Enter closes the card and puts the bar away.
    | { phase: "result"; ok: boolean; message: string; undoable: boolean; hides?: boolean };
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
  // A clip says when it was copied, and whether it is code.
  const clipParams = (i: Item) =>
    (i.metadata?.plugin_params ?? {}) as { ts?: number; code?: boolean; image?: boolean };
  const clipTime = (i: Item) => clipParams(i).ts ?? 0;
  const isCode = (i: Item) => kindName(i.kind) === "clipboard" && !!clipParams(i).code;
  // A clip of a temp-folder path is noise in the recent list.
  const TEMPISH = /[\\/]temp[\\/]/i;
  interface Recent {
    item: Item;
    ts: number;
    clip: boolean;
    /** Copied in the last few minutes. */
    fresh?: boolean;
  }
  const recent = $derived.by<Recent[]>(() => {
    const now = Math.floor(Date.now() / 1000);
    const clips = homeClips
      .filter((i) => !TEMPISH.test(i.title))
      .map((item) => ({ item, ts: clipTime(item), clip: true }))
      // Newest first: pinned clips lead the query's own order, however old.
      .sort((a, b) => b.ts - a.ts)
      .slice(0, 3);
    const files = homeFiles.map((item) => ({ item, ts: item.modified_at ?? 0, clip: false }));
    return recentRows<Recent>([...clips, ...files], now).map((r) => ({ ...r, fresh: isFreshClip(r, now) }));
  });
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
    'button, input, select, textarea, a, label, canvas, svg, li, [role="option"], [role="listbox"], [role="tree"], [role="treeitem"], [role="tab"], [role="menu"], [role="separator"], .rows, .chartbox, .grip, .ctxmenu, .prevcard, .results, [data-no-drag]';
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
  // One line each: a contained fault or a repaired index, and safe mode.
  let problems = $state<string[]>([]);
  let safeMode = $state(false);
  async function loadNotices() {
    try {
      const n = await invoke<{ safe_mode: boolean; lines: string[] }>("notices");
      safeMode = n.safe_mode;
      problems = [...new Set([...problems, ...n.lines])];
    } catch {
      // Asked again when the bar is next shown.
    }
  }
  async function leaveSafeMode() {
    try {
      await invoke("leave_safe_mode");
    } catch (e) {
      problems = [...problems, errorText(e)];
    }
    await loadNotices();
  }
  onMount(() => {
    loadNotices();
    const unlisten = listen("notices-changed", loadNotices);
    return () => {
      unlisten.then((f) => f());
    };
  });
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
    websearch: "Web search",
    link: "Link",
    timer: "Timer",
    quickkey: "Quick key",
    action: "Action",
    dates: "Answer",
    zones: "Answer",
    notes: "Notes",
    saved: "Saved",
    windows: "Open windows",
  };
  // Plugin results carry messages (a rename preview, an error): wrap, never cut off.
  const isPlugin = (i: Item) => !(i.source in GROUPS);
  // An answer says how the question was read: that line wraps too.
  const wraps = (i: Item) => isPlugin(i) || i.source === "dates" || i.source === "zones";
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
    // "win" and "fav" ask for a list; only the words after them match.
    const terms = (isListCommand(query) ? query.trim().replace(/^\S+/, "") : query)
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
  // Settings opened by typing a command: one Esc then hides the bar.
  let settingsTyped = $state(false);
  // Set only while that Esc asks Settings to close.
  let hideOnClose = false;
  function openSettings(start: typeof settingsStart) {
    settingsTyped = false;
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
  const opsShown = $derived(
    ui.mode !== "simple" && !panel && !disk && !card && typedSection(query) === 0 && !isNoteText(query),
  );
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
    if (!panel) {
      settingsStart = "general";
      settingsTyped = false;
    }
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
    if (hideOnClose) {
      cleared = true;
      hideBar();
    }
  }

  /** A query shortcut and a space: the longer text takes the box. Nothing is run. */
  async function expandShortcut(typed: string) {
    if (composing || !/^[a-z0-9]{2,8}\s+$/i.test(typed)) return;
    const text = await invoke<string | null>("quickkey_expand", { typed }).catch(() => null);
    if (typeof text !== "string" || query !== typed) return;
    query = text;
    skip = [];
    run(text);
    await tick();
    input?.setSelectionRange(text.length, text.length);
    trackCaret();
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
    const asked = generation;
    invoke<boolean>("query", { generation, text, skip })
      .then((started) => !started && searchFailed(asked))
      .catch(() => searchFailed(asked));
  }

  function searchFailed(asked: number) {
    if (asked !== generation) return;
    all = [];
    done = true;
    selected = 0;
    notice = errorText("search is not running");
  }

  // "Searching…" only when an empty list has been waiting for a moment.
  let slow = $state(false);
  $effect(() => {
    slow = false;
    if (done || query.trim() === "") return;
    const t = setTimeout(() => (slow = true), 400);
    return () => clearTimeout(t);
  });

  const isFromClipboard = (i: Item | undefined) => i?.source === "plugin:com.lookout.clipboard";
  function onUpdate(u: Update) {
    if (u.generation !== generation) return;
    if (u.generation === homeGen) {
      homeClips = u.items.filter((i) => kindName(i.kind) === "clipboard");
      return;
    }
    const keep = items[selected]?.id;
    const hadClips = all.some(isFromClipboard);
    // One block per group, in the order each group first appears; similar-meaning matches go last.
    const rest = u.items.filter((i) => i.source !== "semantic");
    const order = [...new Set(rest.map(groupOf))];
    let results = [
      ...order.flatMap((g) => rest.filter((i) => groupOf(i) === g)),
      ...u.items.filter((i) => i.source === "semantic"),
    ];
    done = u.done;
    filters = u.filters && (u.filters.kind || u.filters.date) ? u.filters : null;
    typedOps = u.filters?.operators ?? [];
    badOperator = !!u.filters?.invalid_operator;
    // Suggestions wait for something to sit among, unless the word is a command.
    const found = quiet ? [] : suggest(query, ACTIONS, ui.mode, ui.show);
    // A suggested action counts as a match: no web search beside it.
    if (found.length > 0) results = results.filter((i) => !i.metadata?.fallback);
    const intent = found.some((s) => s.exact);
    const ready = u.done || results.length > 0 || intent;
    all = ready ? placeSuggestions(results, found, query, actionItem) : results;
    understoodLine = readLine(results, query);
    // A suggestion never keeps the selection when better rows arrive above it.
    const at = keep && !keep.startsWith("action:") ? items.findIndex((i) => i.id === keep) : -1;
    // An intent word's action is selected unless the user has moved off the top.
    selected = at >= 0 && !(intent && selected === 0) ? at : 0;
    // In the clipboard view, an app that matched the words too does not keep the
    // selection when the clips arrive.
    if (typedSection(query) === 2 && (!hadClips || at < 0)) {
      const first = items.findIndex(isFromClipboard);
      if (first >= 0 && !isFromClipboard(items[selected])) selected = first;
    }
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
      const typed = query.trim() !== "";
      runAction(suggested);
      if (typed && panel) settingsTyped = true;
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
    if ((done.action_id === "open" || done.action_id === "reveal") && !done.was_undo) {
      cleared = true;
      hideBar(); // opened: get out of the way
      return;
    }
    if (changesList(done.action_id)) {
      // A saved note leaves the box, so Enter cannot save it twice; lists reload.
      if (done.action_id === "note_save" && !done.was_undo) query = "";
      skip = [];
      run(query);
    }
    if (!done.was_undo && hidesBar(done.action_id, done.message, ui.hide_after_copy)) {
      card = null;
      cleared = true;
      hideBar();
      // The next show starts in the search box.
      tick().then(() => input?.focus());
      return;
    }
    const hides = !done.was_undo && verbOf(done.action_id) === "rename_files";
    card = { phase: "result", ok: true, message: done.message, undoable: done.undoable, hides };
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
      menu = null;
      press = null;
      refusal = null;
      visible = true;
      fitScreen();
      loadNotices();
      opens += 1;
      opAsked = false;
      tipsHovered = false;
      pv.overlay = false;
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

  const caretIsAtEnd = () => !!input && input.selectionStart === input.value.length && input.selectionEnd === input.value.length;
  const previewButtons = () => Array.from(document.querySelectorAll<HTMLButtonElement>(".prevactions button"));
  function onKeydown(e: KeyboardEvent) {
    // Keys that belong to an input method are left to it.
    if (e.isComposing) return;
    // Esc during a drag cancels the drag, nothing else.
    if (dragging) {
      e.preventDefault();
      return;
    }
    if (menu) {
      menuKey(e);
      return;
    }
    // Esc closes the large preview before anything else. Disk does its own.
    if (pv.overlay && !disk && e.key === "Escape") {
      e.preventDefault();
      pv.overlay = false;
      return;
    }
    const plain = !e.ctrlKey && !e.altKey && !e.metaKey && !e.shiftKey;
    const menuAsked = e.key === "ContextMenu" || (e.shiftKey && e.key === "F10");
    if (earlier !== null) {
      if (menuAsked) {
        e.preventDefault();
        const r = historyList?.querySelector('[aria-selected="true"]')?.getBoundingClientRect();
        if (r) openMenu(searchMenu(earlier[earlierAt]), r.left + 48, r.bottom - 8);
        return;
      }
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
        // A question on the page is answered first: then nothing is hidden.
        hideOnClose = settingsTyped;
        leavePanel();
        hideOnClose = false;
      }
      return;
    }
    if (card) {
      // A key held down never presses a button: Confirm needs its own press.
      if (e.repeat && (e.key === "Enter" || e.key === " ")) e.preventDefault();
      // With a card open, Escape closes the card and nothing else is handled
      // here: Enter and Space work only through the focused button, so Confirm
      // happens only by pressing the visible Confirm button.
      if (e.key === "Escape") {
        e.preventDefault();
        dismissCard();
      } else if (e.key === "Enter" && card.phase === "result" && card.hides) {
        // A focused button (Undo, Close) keeps its own Enter.
        if (e.repeat || e.target instanceof HTMLButtonElement) return;
        e.preventDefault();
        dismissCard();
        cleared = true;
        hideBar();
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
    // Right at the end of the search text goes to the preview's buttons; Left
    // comes back, as do Up and Down before they move the selection.
    if (plain && e.key === "ArrowRight" && e.target === input && caretIsAtEnd()) {
      const first = previewButtons()[0];
      if (first) {
        e.preventDefault();
        first.focus();
        return;
      }
    }
    if (plain && e.target instanceof HTMLButtonElement) {
      const buttons = previewButtons();
      const at = buttons.indexOf(e.target);
      if (at >= 0 && (e.key === "ArrowLeft" || e.key === "ArrowRight")) {
        e.preventDefault();
        if (e.key === "ArrowRight") buttons[Math.min(at + 1, buttons.length - 1)].focus();
        else (buttons[at - 1] ?? input)?.focus();
        return;
      }
      if (at >= 0 && (e.key === "ArrowUp" || e.key === "ArrowDown")) input?.focus();
    }
    if (rowKey(e, menuAsked)) return;
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
        else if (!e.repeat && offersFolders && done && items.length === 0) chooseFolders();
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
  const BADGE_KINDS = ["calculator", "conversion", "port"];
  function iconKind(i: Item): Ico | null {
    const k = kindName(i.kind);
    if (k.startsWith("timer")) return "timer";
    if (k === "date_answer") return "calendar";
    if (k === "time_answer") return "clock";
    if (k.startsWith("note")) return "note";
    if (k === "quickkey_missing" || k === "path" || k === "answer_info") return "warn";
    if (k === "url") return "globe";
    if (k === "system") return /dark mode/i.test(i.title) ? "moon" : /light mode/i.test(i.title) ? "sun" : "gear";
    if ((k === "folder" || k === "file") && i.uri?.startsWith("\\\\")) return "network";
    if (k === "app" || k === "window") return "app";
    if (k === "folder") return "folder";
    if (k === "clipboard") return "clipboard";
    if (k === "screenshot" || IMAGE_EXT.test(i.title)) return "image";
    if (k === "file") return "file";
    if (/^record.*gif/i.test(i.title)) return "gif";
    if (/^record/i.test(i.title)) return "rec";
    if (/^text from screen/i.test(i.title)) return "scan";
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

  // Apps show their own icon over the generic one, asked for only while a row
  // is on screen and forgotten when the bar hides.
  let appIcons = $state<Record<string, string>>({});
  const icons = iconLoader(
    (path) => invoke<string | null>("app_icon", { path }),
    (path, src) => {
      if (src) appIcons[path] = src;
      else delete appIcons[path];
    },
  );
  const watchIcon = rowWatcher(icons, (changed) => new IntersectionObserver(changed));
  $effect(() => {
    const on = visible;
    untrack(() => icons.setActive(on));
  });

  // Image clips show a small picture, asked for one at a time while their rows
  // are on screen and forgotten when the bar hides.
  const clipImageKey = (i: Item) => (clipParams(i).image ? `${i.id}@${clipTime(i)}` : "");
  let clipThumbs = $state<Record<string, string>>({});
  $effect(() => {
    if (!visible) {
      clipThumbs = {};
      return;
    }
    const wanted = [...items, ...recent.map((r) => r.item)].filter((i) => clipImageKey(i));
    let dead = false;
    void (async () => {
      for (const i of wanted) {
        const key = clipImageKey(i);
        if (dead) return;
        if (untrack(() => clipThumbs[key])) continue;
        const src = await invoke<string | null>("clip_thumbnail", { id: i.id }).catch(() => null);
        if (src && !dead) clipThumbs[key] = src;
      }
    })();
    return () => {
      dead = true;
    };
  });

  // Keyed by the verb, so an add-on's action reads like Lookout's own.
  const ACTION_LABEL: Record<string, string> = {
    open: "Open",
    paste_clip: "Paste",
    copy_clip: "Copy",
    kill_port: "Stop…",
    open_url: "Open",
    system_page: "Open",
    system_theme: "Switch",
    system_lock: "Lock",
    system_empty_bin: "Empty…",
    system_sleep: "Sleep…",
    system_restart: "Restart…",
    system_sign_out: "Sign out…",
    window_focus: "Switch",
    saved_add: "Save",
    saved_remove: "Remove",
    system_radio: "Turn…",
    timer_start: "Start",
    timer_cancel: "Cancel timer",
    answer_copy: "Copy",
    note_save: "Save",
    note_copy: "Copy",
    note_delete: "Delete",
    clear_history: "Delete",
  };

  // Quick keys: a short word that puts a result on top when typed with a space.
  let keyFor: { id: string; alias: string; error: string } | null = $state(null);
  let keyDone = $state("");
  let keyInput: HTMLInputElement | undefined = $state();
  const canKey = (i: Item | undefined) =>
    !!i &&
    !i.source.startsWith("plugin:") &&
    i.source !== "websearch" &&
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
    return ACTION_LABEL[verbOf(defaultAction(i) ?? "")] ?? "Run";
  };
  const isFile = (i: Item | undefined) => !!i && isFileRow(kindName(i.kind), i.source, i.uri);
  /** Runs one action on a result: the keys, the menu and the preview use this. */
  function doAction(action: string, i: Item | undefined) {
    if (!i || card) return;
    remember(query);
    begin(() => invoke<Prepared>("prepare_action", { action, id: i.id }));
  }
  const isNote = (i: Item | undefined) => !!i && kindName(i.kind) === "note";

  /** The row a key acts on: the focused Recent row on Home, else the selection. */
  function keyRow(target: EventTarget | null): { item: Item; el: Element | null } | undefined {
    if (home) {
      const el = (target as Element | null)?.closest?.("[data-recent]");
      const r = el ? recent[Number(el.getAttribute("data-recent"))] : undefined;
      return r && { item: r.item, el: el! };
    }
    // The Disk analyzer row stands alone: nothing hidden behind it is acted on.
    if (isDiskCommand(query)) return;
    return cur && { item: cur, el: list?.querySelector(`[data-index="${selected}"]`) ?? null };
  }

  /** Menu, Shift+Enter, Ctrl+C, Ctrl+Shift+C, Ctrl+E and Delete. True if the key was used. */
  function rowKey(e: KeyboardEvent, menuAsked: boolean): boolean {
    const row = keyRow(e.target);
    if (!row) return false;
    const keys = rowKeys(row.item);
    const letter = e.key.toLowerCase();
    const ctrl = e.ctrlKey && !e.altKey && !e.metaKey;
    let run: (() => void) | undefined;
    if (menuAsked) {
      const r = row.el?.getBoundingClientRect();
      if (r) run = () => openMenu(rowMenu(row.item), r.left + 48, r.bottom - 8);
    } else if (e.key === "Enter" && e.shiftKey && !e.ctrlKey && !e.altKey && !e.metaKey) {
      // A focused button that is not a Home row keeps its own Enter.
      const action = home || !(e.target instanceof HTMLButtonElement) ? keys.paste : undefined;
      if (action) run = () => doAction(action, row.item);
    } else if (ctrl && letter === "c" && e.shiftKey) {
      const action = keys.copyFile;
      if (action) run = () => doAction(action, row.item);
    } else if (ctrl && letter === "c") {
      // Selected text is copied as text.
      const box = e.target === input && input?.selectionStart !== input?.selectionEnd;
      const action = box || String(window.getSelection()) ? undefined : keys.copy?.action;
      if (action) run = () => doAction(action, row.item);
    } else if (ctrl && !e.shiftKey && letter === "e") {
      const action = keys.reveal;
      if (action) run = () => doAction(action, row.item);
    } else if (e.key === "Delete" && !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey) {
      // In the search box, Delete edits the text until the cursor is at its end.
      const action = e.target !== input || caretIsAtEnd() ? keys.remove?.action : undefined;
      if (action) run = () => doAction(action, row.item);
    }
    if (!run) return false;
    e.preventDefault();
    if (!e.repeat) run();
    return true;
  }

  interface Entry {
    label: string;
    run: () => void;
  }
  /** Everything one result offers, Enter first. */
  function rowMenu(i: Item): Entry[] {
    const keys = rowKeys(i);
    const first = defaultAction(i);
    const entries: Entry[] = first ? [{ label: openLabel(i), run: () => activate(i) }] : [];
    const add = (action: string | undefined, label: string) => {
      if (action && action !== first) entries.push({ label, run: () => doAction(action, i) });
    };
    add(keys.copy?.action, keys.copy?.label ?? "");
    add(keys.paste, "Paste");
    add(keys.copyFile, "Copy file");
    add(keys.reveal, "Show in folder");
    add(keys.save, "Save");
    add(keys.remove?.action, keys.remove?.label ?? "");
    if (!home && ui.show.quick_keys && canKey(i)) entries.push({ label: "Set quick key", run: () => startKey(i) });
    return entries;
  }
  const searchMenu = (text: string): Entry[] => [
    { label: "Search", run: () => setInput(text) },
    { label: "Delete", run: forgetSearch },
  ];

  // Right-click or the Menu key on any result row.
  let menu = $state<{ entries: Entry[]; x: number; y: number } | null>(null);
  let menuEl: HTMLElement | undefined = $state();
  function openMenu(entries: Entry[], x: number, y: number) {
    if (card || entries.length === 0) return;
    // Kept inside the window, which is only as tall as what it shows.
    const height = 12 + entries.length * 32;
    menu = { entries, x: Math.min(x, window.innerWidth - 160), y: Math.max(0, Math.min(y, window.innerHeight - height)) };
    tick().then(() => menuEl?.querySelector("button")?.focus());
  }
  function rightClick(e: MouseEvent, entries: Entry[]) {
    e.preventDefault();
    openMenu(entries, e.clientX, e.clientY);
  }
  function closeMenu() {
    menu = null;
    input?.focus();
  }
  function menuKey(e: KeyboardEvent) {
    const buttons = [...(menuEl?.querySelectorAll("button") ?? [])];
    const at = buttons.indexOf(document.activeElement as HTMLButtonElement);
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      buttons[(at + (e.key === "ArrowDown" ? 1 : buttons.length - 1)) % buttons.length]?.focus();
    } else if (e.key === "Escape" || e.key === "Tab") {
      e.preventDefault();
      closeMenu();
    } else if (e.key !== "Enter" && e.key !== " ") {
      closeMenu();
    }
  }
  $effect(() => {
    // The menu goes with its list.
    items;
    recent;
    menu = null;
  });

  const isClip = (i: Item) => isClipRow(kindName(i.kind), i.source);
  const canDrag = (i: Item) => isFile(i) || isClip(i);

  // Dragging a file or clip row into another program. A press becomes a drag only after
  // the pointer has travelled; the shell then runs the drag until the release.
  let press: { id: string; x: number; y: number } | null = null;
  let dragging = false;
  // The click that may follow a drag must not open the result.
  let dragged = false;
  // Why the row under the pointer cannot be dragged, shown as its tooltip.
  let refusal = $state<{ id: string; text: string } | null>(null);
  function checkDrag(i: Item) {
    if (!canDrag(i)) return;
    invoke<string | null>("drag_check", { id: i.id })
      .then((text) => (refusal = text ? { id: i.id, text } : null))
      .catch(() => {});
  }
  function pressRow(e: PointerEvent, i: Item) {
    dragged = false;
    press = null;
    // Shift extends a selection; touch scrolls; a button in the row is its own.
    if (e.button !== 0 || e.shiftKey || e.pointerType === "touch" || card || !canDrag(i)) return;
    // A button inside the row is its own; a row that is a button is the row.
    const button = (e.target as Element).closest("button");
    if (button && button !== e.currentTarget) return;
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
  // An empty Clipboard or Capture search says whether the add-on is off.
  const ADDONS: Record<number, [string, string]> = {
    2: ["com.lookout.clipboard", "Clipboard needs the Clipboard add-on. Turn it on in Settings > Advanced > Add-ons."],
    3: ["dev.lookout.screenshot", "Capture needs the Screenshot add-on. Turn it on in Settings > Advanced > Add-ons."],
  };
  // Apps may match "clip " too, so an empty history is judged by its own rows.
  const fromClipboard = $derived(all.some(isFromClipboard));
  const noClips = $derived(typedSection(query) === 2 && !fromClipboard);
  const emptySection = $derived(view === "results" && done && (all.length === 0 || noClips) ? typedSection(query) : 0);
  let addonOff = $state("");
  $effect(() => {
    const wanted = ADDONS[emptySection];
    addonOff = "";
    if (!wanted) return;
    let live = true;
    invoke<{ id: string; enabled: boolean }[]>("plugins_list")
      .then((list) => {
        if (live && !list.some((p) => p.id === wanted[0] && p.enabled)) addonOff = wanted[1];
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  });
  /** What an empty result list says: what happened and what to do next. */
  const emptyText = $derived.by(() => {
    const typed = query.trim();
    if (all.length > 0 && emptySection !== 2) return `Nothing under ${LANES[lane].name}. Press Tab to see other results.`;
    if (addonOff) return addonOff;
    const section = typedSection(query);
    const rest = typed.replace(/^\S+\s*/, "");
    if (isListCommand(query)) {
      if (/^win/i.test(typed)) return rest ? `No open window matches “${rest}”.` : "No other window is open.";
      return rest ? `Nothing saved matches “${rest}”.` : "Nothing saved yet. Choose Save in a file's or folder's menu.";
    }
    if (section === 2) {
      return rest ? `No clips match “${rest}”. Try fewer words.` : "Nothing copied yet. Copy some text and it appears here.";
    }
    if (noFolders) return `No results for “${typed}”.`;
    if (status?.indexing) return `No results for “${typed}” yet. Files are still being prepared, so try again shortly.`;
    return `No results for “${typed}”. Check the spelling or try fewer words.`;
  });
  /** "Choose folders" is offered for file searches only. */
  const offersFolders = $derived(noFolders && typedSection(query) === 0 && !isListCommand(query));
  /** Pause holds the clipboard history with all other background work. */
  function togglePause() {
    invoke("set_paused", { paused: !status?.paused })
      .then(refreshStatus)
      .catch((e) => (notice = errorText(e)));
    input?.focus();
  }
  // The tallest the screen allows: fixed-height panels shrink to fit at high Windows scaling.
  // The notice above the dock takes its own height (and the gap under it) from them.
  const showMoved = $derived(ui.disk_notice && !card && !showSetup);
  let movedHeight = $state(0);
  function fitScreen() {
    const taken = showMoved ? movedHeight + 8 : 0;
    document.documentElement.style.setProperty("--room", `${window.screen.availHeight - 32 - taken}px`);
  }
  $effect(fitScreen);

  const cardHides = $derived.by(() => card?.phase === "result" && !!card.hides);
  const hints = $derived(
    footerHints(view, {
      mode: ui.mode,
      hotkey: openHotkey,
      enter: defaultAction(cur) ? openLabel(cur) : null,
      path: PATHLIKE.test(query),
      clears: pinned && query !== "",
      paste: !!cur && !!rowKeys(cur).paste && verbOf(defaultAction(cur) ?? "") === "copy_clip",
      hides: view === "settings" ? settingsTyped : cardHides,
    }),
  );

  // One quiet line: the status only while something happens, then the keys.
  const footText = $derived([footerStatus(status), ...hints].filter(Boolean).join(" · "));

  const dur = (n: number) => (look.reduced ? 0 : n);
</script>

<svelte:window
  onkeydown={onKeydown}
  onresize={fitScreen}
  onpointerdown={(e) => {
    if (menu && !(e.target as Element).closest(".ctxmenu")) menu = null;
  }}
  onblur={() => !pinned && (visible = false)}
  onfocus={() => (visible = true)}
  onpointermove={dragMove}
  onpointerup={() => (press = null)}
/>
<svelte:document onvisibilitychange={() => (visible = document.visibilityState === "visible")} onselectionchange={trackCaret} />

{#snippet enterHint(label: string)}
  <span class="hint-enter"><kbd>Enter</kbd>{label}</span>
{/snippet}

{#snippet exactRow()}
  <li role="presentation" class="entry">
    <Row
      role="option"
      tabindex={-1}
      id={`result-${items.length}`}
      aria-selected={selected === items.length}
      selected={selected === items.length}
      data-index={items.length}
      onmousemove={() => (selected = items.length)}
      onclick={exactWords}
      oncontextmenu={(e) => rightClick(e, [{ label: "Search", run: exactWords }])}
    >
      {#snippet icon()}<span class="icon" aria-hidden="true"><Icon name="search" /></span>{/snippet}
      {#snippet title()}Search for the exact words instead{/snippet}
      {#snippet detail()}<span class="sub">“{query.trim()}”</span>{/snippet}
      {#snippet end()}{#if selected === items.length}{@render enterHint("Search")}{/if}{/snippet}
    </Row>
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
  {@const picture = clipThumbs[clipImageKey(it)]}
  {@const appPath = iconTarget(kindName(it.kind), it.uri)}
  {#if picture}
    <img class="thumb" src={picture} alt="" />
  {:else if clipParams(it).image}
    <span class="icon" aria-hidden="true"><Icon name="image" /></span>
  {:else if act}
    <span class="icon" aria-hidden="true">{@render glyph(act.icon ?? act.category)}</span>
  {:else if isRunning(it)}
    <span class="icon ring" aria-hidden="true">
      <svg viewBox="0 0 24 24" fill="none" stroke-width="2.4" stroke-linecap="round">
        <circle cx="12" cy="12" r="9" stroke="var(--hairline)" />
        <circle cx="12" cy="12" r="9" stroke="currentColor" transform="rotate(-90 12 12)" stroke-dasharray={RING} stroke-dashoffset={ringOffset(it)} />
      </svg>
    </span>
  {:else if appPath}
    <span class="icon" aria-hidden="true" use:watchIcon={appPath}>
      {#if appIcons[appPath]}<img class="appicon" src={appIcons[appPath]} alt="" />{:else}<Icon name="app" />{/if}
    </span>
  {:else if k}
    <span class="icon" aria-hidden="true"><Icon name={k} /></span>
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
          expandShortcut(query);
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
        tabindex={panel ? -1 : 0}
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

    {#if safeMode}
      <div class="problem glass" role="status">
        <span>Lookout started in safe mode after repeated crashes: no add-ons, no background indexing.</span>
        <button type="button" class="btn small primary" onclick={leaveSafeMode}>Turn everything back on</button>
      </div>
    {/if}
    {#each problems as p (p)}
      <div class="problem glass" role="alert">
        <span>{p}</span>
        <button type="button" class="btn small" onclick={() => (problems = problems.filter((x) => x !== p))}>Dismiss</button>
      </div>
    {/each}
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
                      <Row
                        tag="button"
                        extra={r.clip ? "draggable" : ""}
                        data-recent={n}
                        tooltip={refusal?.id === r.item.id ? refusal.text : undefined}
                        onpointerenter={() => checkDrag(r.item)}
                        onpointerdown={(e) => r.clip && pressRow(e, r.item)}
                        ondragstart={(e) => e.preventDefault()}
                        oncontextmenu={(e) => rightClick(e, rowMenu(r.item))}
                        onclick={() => {
                          if (!dragged) activate(r.item);
                          dragged = false;
                        }}
                      >
                        {#snippet icon()}
                          {#if n === 0 && thumbSrc}<img class="thumb" src={thumbSrc} alt="" />{:else}{@render tile(r.item)}{/if}
                        {/snippet}
                        {#snippet title()}<span class:code={isCode(r.item)}>{r.clip ? mask(r.item.title) : r.item.title}</span>{/snippet}
                        {#snippet detail()}
                          <span class="sub" class:code={!r.clip}
                            >{r.clip ? (r.fresh ? "Clip · just copied" : "Copied") : (r.item.uri ?? r.item.subtitle ?? "")}</span
                          >
                        {/snippet}
                        {#snippet end()}<span class="when">{ageLabel(r.ts, Math.floor(Date.now() / 1000))}</span>{/snippet}
                      </Row>
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
                      <Row
                        tag="button"
                        role="option"
                        selected={n === earlierAt}
                        aria-selected={n === earlierAt}
                        onfocus={() => (earlierAt = n)}
                        onmousemove={() => (earlierAt = n)}
                        oncontextmenu={(e) => {
                          earlierAt = n;
                          rightClick(e, searchMenu(h));
                        }}
                        onclick={() => setInput(h)}
                      >
                        {#snippet icon()}<span class="icon" aria-hidden="true">{@render glyph("Search")}</span>{/snippet}
                        {#snippet title()}{h}{/snippet}
                      </Row>
                    </li>
                  {/each}
                </ul>
                <!-- Focus stays in the search box, so the chosen line is announced. -->
                <span class="sr" aria-live="polite">{earlier[earlierAt]}</span>
              </div>
            {/if}
          </section>

          <section class="card glass side" aria-label={tips.length > 0 ? "Tips and files" : "Files"}>
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
                  {@render glyph(a.icon ?? a.category)}<span>{a.label ?? a.name}</span>
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
              <Row
                role="option"
                tabindex={-1}
                aria-selected="true"
                selected
                onclick={openDisk}
                oncontextmenu={(e) => rightClick(e, [{ label: "Open", run: openDisk }])}
              >
                {#snippet icon()}<span class="icon" aria-hidden="true"><Icon name="disk" /></span>{/snippet}
                {#snippet title()}Disk analyzer{/snippet}
                {#snippet detail()}<span class="sub">See what uses space on a drive or folder. Scans only when you press Scan.</span>{/snippet}
                {#snippet end()}{@render enterHint("Open")}{/snippet}
              </Row>
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
            {#if fromClipboard || (emptySection === 2 && !addonOff)}
              <p class="readline trust">
                <span>{status?.paused ? "Paused: nothing you copy is recorded." : "Clips are encrypted on this PC; text that looks like a secret is masked."}</span>
                <button type="button" class="btn small text" title="Pauses recording and all background work" onclick={togglePause}
                  >{status?.paused ? "Resume" : "Pause"}</button
                >
              </p>
            {/if}
            {#if status?.sizes_pending && typedSize()}<p class="readline muted" role="status">Sizes are still being collected</p>{/if}
            {#if opsShown && badOperator && offers.length === 0}<p class="readline" role="status">{OPERATOR_HINT}</p>{/if}
            {#if meant}<p class="readline" role="status">{meant}</p>{/if}
            {#if understoodLine}<p class="readline" role="status">{understoodLine}</p>{/if}
            {#if corrected}<p class="note">Showing results for <b>{corrected}</b></p>{/if}
            {#if items.length > 0 && emptySection === 2}<p class="empty" role="status">{emptyText}</p>{/if}
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
                    <Row
                      role="option"
                      tabindex={-1}
                      id={`result-${i}`}
                      aria-selected={i === selected}
                      selected={i === selected}
                      extra={canDrag(item) ? "draggable" : ""}
                      data-index={i}
                      tooltip={refusal?.id === item.id ? refusal.text : wraps(item) ? (item.subtitle ?? undefined) : undefined}
                      onmousemove={() => (selected = i)}
                      onpointerenter={() => checkDrag(item)}
                      onpointerdown={(e) => pressRow(e, item)}
                      ondragstart={(e) => e.preventDefault()}
                      oncontextmenu={(e) => {
                        selected = i;
                        rightClick(e, rowMenu(item));
                      }}
                      onclick={() => {
                        if (!dragged) activate(item);
                        dragged = false;
                      }}
                    >
                      {#snippet icon()}
                        {#if top && thumbSrc}<img class="thumb" src={thumbSrc} alt="" />{:else}{@render tile(item)}{/if}
                      {/snippet}
                      {#snippet title()}{#if top}<span class="tophit">Top hit</span>{/if}<span class:code={isCode(item)}
                          >{#each highlight(kindName(item.kind) === "clipboard" ? mask(item.title) : item.title) as part}{#if part.hit}<mark>{part.text}</mark
                              >{:else}{part.text}{/if}{/each}</span
                        >{/snippet}
                      {#snippet aside()}
                        <!-- Under a matched line, the path moves up beside the name. -->
                        {#if typeof item.metadata?.snippet === "string" && item.uri}<span class="meta code">{item.uri}</span>{/if}
                      {/snippet}
                      {#snippet detail()}
                        {#if typeof item.metadata?.snippet === "string"}
                          <span class="snippet"
                            >{#each snippetParts(item.metadata.snippet) as part}{#if part.hit}<mark>{part.text}</mark
                                >{:else}{part.text}{/if}{/each}</span
                          >
                        {:else if item.subtitle}
                          <span class="sub" class:code={isPath(item)}
                            >{#each highlight(kindName(item.kind) === "clipboard" ? mask(item.subtitle) : (subOf(item) ?? "")) as part}{#if part.hit}<mark>{part.text}</mark
                                >{:else}{part.text}{/if}{/each}</span
                          >
                        {/if}
                      {/snippet}
                      {#snippet end()}
                        {#if kindName(item.kind) === "clipboard" && clipTime(item)}
                          <span class="when">{ageLabel(clipTime(item), Math.floor(Date.now() / 1000))}</span>
                        {/if}
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
                        {#if i === selected && defaultAction(item)}{@render enterHint(openLabel(item))}{/if}
                      {/snippet}
                    </Row>
                  </li>
                {/each}
                {#if understood}{@render exactRow()}{/if}
              </ul>
            {:else}
              {#if understood}
                <ul id="results" class="results" role="listbox">{@render exactRow()}</ul>
              {/if}
              {#if done}
                <p class="empty" role="status">{emptyText}</p>
                {#if offersFolders && all.length === 0}
                  <div class="nofolders">
                    <span>Lookout has no folders to search yet.</span>
                    <button class="btn primary" onclick={chooseFolders}>Choose folders to search <kbd>Enter</kbd></button>
                  </div>
                {/if}
              {:else if slow}
                <p class="empty" role="status">Searching…</p>
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
              {:else if isNote(cur)}
                <div class="plain">
                  <p class="notetext">{mask(cur.title)}</p>
                  <span class="sub">{cur.subtitle}</span>
                </div>
              {:else}
                <div class="plain">
                  <strong class="nm" class:code={isCode(cur)}>{mask(cur.title)}</strong>
                  {#if kindName(cur.kind) === "clipboard" && clipTime(cur)}<span class="sub">Copied {ageLabel(clipTime(cur), Math.floor(Date.now() / 1000))}</span>{/if}
                  {#if cur.subtitle}<span class="sub wrap">{mask(cur.subtitle)}</span>{/if}
                  {#if cur.source === "action"}<span class="sub">Example: <span class="code">{cur.metadata?.example}</span></span>{/if}
                  <span class="sub">{groupOf(cur)}</span>
                </div>
              {/if}
              <div class="prevactions">
                {#if defaultAction(cur)}
                  <button class="btn small primary" onclick={() => activate(cur)}
                    >{openLabel(cur)}{#if rowKeys(cur).paste}&nbsp;<kbd>Enter</kbd>{/if}</button
                  >
                {/if}
                {#if rowKeys(cur).paste && verbOf(defaultAction(cur) ?? "") === "copy_clip"}
                  <button class="btn small" onclick={() => doAction(rowKeys(cur).paste ?? "", cur)}>Paste <kbd>Shift+Enter</kbd></button>
                {:else if kindName(cur.kind) === "clipboard" && verbOf(defaultAction(cur) ?? "") !== "copy_clip"}
                  <button class="btn small" onclick={() => doAction(rowKeys(cur).copy?.action ?? "", cur)}>Copy <kbd>Ctrl+C</kbd></button>
                {/if}
                {#if isFile(cur)}
                  <button class="btn small" title="Ctrl+E" onclick={() => doAction("reveal", cur)}>Show in folder</button>
                  <button class="btn small" title="Ctrl+C" onclick={() => doAction("copy_path", cur)}>Copy path</button>
                  <button class="btn small" title="Ctrl+Shift+C" onclick={() => doAction("copy_file", cur)}>Copy file</button>
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
    {#if showMoved}
      <div class="moved glass" role="status" bind:offsetHeight={movedHeight}>
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
  {#if menu}
    <div class="ctxmenu" role="menu" tabindex="-1" bind:this={menuEl} style:left={`${menu.x}px`} style:top={`${menu.y}px`}>
      {#each menu.entries as entry (entry.label)}
        <button
          type="button"
          role="menuitem"
          onclick={() => {
            closeMenu();
            entry.run();
          }}>{entry.label}</button
        >
      {/each}
    </div>
  {/if}
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
  .pin:hover {
    color: var(--text);
    border-color: var(--hover-border);
  }
  .pin:focus-visible {
    color: var(--text);
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .pin.on {
    color: var(--accent-text);
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
    color: var(--ghost);
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
  /* Always focused when the bar opens: the caret is the focus indicator. */
  .search:focus-visible {
    outline: none;
  }

  .problem {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 6px 8px 6px 16px;
    border-radius: var(--r-pill);
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
    color: var(--accent-on);
    font-weight: 600;
  }
  .chip:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .page {
    /* 212px: search row, dock, footer and the gaps around them */
    height: min(500px, calc(var(--room, 100vh) - 212px));
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
    /* Scrolls where the screen is too short; 256px is the room the rest takes. */
    grid-auto-rows: max-content;
    max-height: calc(var(--room, 100vh) - 256px);
    overflow-y: auto;
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
    color: var(--accent-text);
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
    max-height: min(440px, calc(var(--room, 100vh) - 256px));
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
    max-height: min(440px, calc(var(--room, 100vh) - 256px));
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
  .results {
    list-style: none;
    margin: 0;
    padding: 8px 10px;
    overflow-y: auto;
    flex: 1;
    min-height: 0;
  }
  .results.card {
    max-height: min(440px, calc(var(--room, 100vh) - 256px));
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
  .notetext {
    margin: 0;
    max-height: 260px;
    overflow-y: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    user-select: text;
  }
  .timerprev .big {
    font-family: var(--font-code);
    font-size: 40px;
    line-height: 1.1;
    font-weight: 500;
    font-variant-numeric: tabular-nums;
    color: var(--accent-text);
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
  .trust {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    color: var(--muted);
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
    color: var(--accent-text);
  }
</style>
