# Project Brief: Lookout (working name)

A local-first command bar for Windows that finds things by memory, runs safe actions with undo, and grows through modules. This document is the full specification. Read all of it before writing code, then follow the "How to work" section.

---

## 1. One-line pitch

One hotkey opens a bar that **finds anything on your computer by what you remember about it** (name, content, screenshot text, meaning, time), and **acts on it safely** (preview, confirm, undo). Everything stays on the device.

## 2. The problem

- People lose files, screenshots and clipboard items and cannot remember names. Existing launchers (Flow Launcher, PowerToys Run/Command Palette, Raycast) mostly search by name and run commands.
- Quick actions ("kill port 3000", "compress this PDF") are done by googling commands or opening heavy apps, and many are risky with no undo.
- Memory-style tools can feel invasive. Lookout is opt-in, local and pausable.

## 3. Target users

- **Student:** finds notes by topic, compresses PDFs to upload limits, resumes a project.
- **Developer:** frees ports, restarts dev servers, undoes git mistakes, searches logs.
- **Office/small business:** merges PDFs, batch renames, masks secrets in clipboard.
- **Creator:** captures and redacts screenshots, resizes images, sends files to a phone.

## 4. Core principles

1. **Speed is a feature.** Fast results are never blocked by slow sources.
2. **Everything is a typed item with actions** (see section 7). No feature needs its own big window.
3. **Safe by default.** Every action has a preview, a risk level and, where possible, undo.
4. **Local and private.** No accounts, no cloud, no telemetry.
5. **Modules over monolith.** Every feature beyond the core is a module built on the same plugin API.

## 5. Non-goals for v1

- macOS/Linux support, cloud sync, accounts, telemetry.
- Browser tab search (needs an extension; later phase).
- Cloud AI. Any AI is optional, small and local.
- A general-purpose automation platform.

## 6. Platform and technology (defaults; deviate only with a written reason)

- **OS:** Windows 10/11 (x64).
- **Core language:** Rust (stable).
- **UI:** Tauri 2 with a lightweight frontend (Svelte or React + TypeScript). Create the window at startup and keep it hidden, then show it on hotkey. Never create it on demand.
- **Global hotkey:** configurable, default `Alt+Space`.
- **Name index:** in-memory sorted structure (FST or trie) built by a background crawl, kept fresh with the `notify` crate. Optional later: elevated helper reading the NTFS change journal for faster updates (requires admin; do not require it in v1).
- **Full-text search:** `tantivy`.
- **Semantic search:** small embedding model (bge-small or MiniLM class) via ONNX Runtime (`ort`); vectors stored via `usearch` or SQLite with `sqlite-vec`.
- **OCR:** Windows built-in OCR (`Windows.Media.Ocr`) through the `windows` crate; Tesseract as fallback.
- **Metadata store:** SQLite (action undo log, frecency, settings, item metadata).
- **Plugins:** separate processes using JSON-RPC 2.0 over stdio.
- **Installer:** MSI or NSIS via Tauri bundler.

## 7. Architecture

### 7.1 Processes

1. **Daemon (Rust core):** hotkey, indexers, query engine, action engine, plugin host, settings, undo log. Runs at login, tray icon.
2. **UI (Tauri window):** renders the bar, results, previews and action cards. Talks to the daemon over in-process commands/events.
3. **Plugin processes:** one per plugin, started lazily, killed on idle or crash.

### 7.2 Item model

```text
Item {
  id: string               // stable, e.g. "file:C:\docs\a.pdf"
  kind: enum               // file | folder | app | clipboard | screenshot | process | port | project | snippet | custom(string)
  title: string
  subtitle: string?        // path, description, source
  uri: string?             // path or URL for default open
  source: string          // which source/plugin produced it
  modified_at, created_at: timestamp?
  metadata: map<string, value>   // kind-specific (size, mime, pid, port, text_preview, ocr_text...)
  score_features: { match: f32, frecency: f32, recency: f32, semantic: f32? }
}
```

### 7.3 Action model

```text
Action {
  id: string
  label: string
  applies_to: [kind]            // which item kinds it works on
  risk: safe | confirm | dangerous
  preview(items, params) -> Preview     // text/diff/list shown BEFORE execution
  execute(items, params) -> Result       // may return undo_token
  undo(undo_token) -> Result            // optional; if missing, UI shows "cannot be undone"
}
```

Rules:
- `risk=confirm|dangerous` actions must show `preview` and require explicit confirmation (Enter on a visible confirm button).
- Every `execute` is written to the **undo log** (SQLite): timestamp, action id, items, params, undo_token, status.
- A global "Undo last action" command (`Ctrl+Z` inside the bar) reverts the latest undoable entry.

### 7.4 Source interface

A source is anything that produces items:
```text
Source {
  id, name
  tier: instant | smart       // instant sources must answer from memory in < 20 ms
  query(parsed_query, cancel_token) -> stream<Item>
  on_index_change(...)        // optional
}
```
Built-in sources: Apps, Files (names), Content (full-text + OCR), Semantic, Clipboard, Processes/Ports.

### 7.5 Query pipeline (progressive results)

1. On each keystroke, assign a new `query_generation` and cancel older work.
2. Parse input into: free text, filters (`kind:`, `in:`, time phrases like "last week", "yesterday", "march"), and an optional action verb ("compress", "kill", "convert").
3. Fan out to all sources concurrently. Instant sources respond first.
4. Merge into one ranked list and push updates to the UI at most every ~16 ms. Results appear immediately and are refined as smart sources respond. The list never blocks waiting for slow sources.
5. Discard results from stale generations.

### 7.6 Ranking

`score = w_match * match + w_frecency * frecency + w_recency * recency(kind) + w_semantic * semantic`
- Start with simple weights in a config file. Frecency = decayed open count.
- Add a test fixture to compare ranking changes against known queries.

## 8. Performance budgets (must be measured, not assumed)

- Hotkey to visible bar: < 80 ms (warm).
- Keystroke to first results (instant tier, 1M indexed names): < 50 ms.
- Idle RAM: < 150 MB (without semantic model loaded); semantic model loads lazily.
- Background indexing: runs only when idle, capped around 10% CPU, pausable.
- Cold start to ready: < 3 s.

Provide a `bench/` project that measures these and prints a table. CI or a script must fail if budgets regress badly.

## 9. Privacy and security requirements

- **Opt-in indexing:** first run asks which folders to index. Defaults exclude: system folders, `AppData` (except explicit allow), `node_modules`, `.git`, browser profiles, password-manager directories.
- **Controls:** pause indexing from tray and hotkey, per-folder exclusions, "delete my index" button.
- **At rest:** index and database encrypted with a key protected by Windows DPAPI.
- **Clipboard:** detect and mask secrets (API-key patterns, high-entropy tokens, card numbers via Luhn, private keys). Masked items are never indexed in full.
- **Network:** Your files, searches and index never leave your PC. Lookout connects to the internet only when you click a download button, and it only receives files. No background or startup connections.
- **Plugins:** manifest declares permissions (file read paths, network hosts, spawning processes). The host enforces what it can in v1 (job objects, no network by default) and shows permissions on install. Document known limits honestly.
- **No telemetry.**

## 10. Plugin API

### Manifest (`plugin.json`)
```json
{
  "id": "com.example.files-tools",
  "name": "File Tools",
  "version": "0.1.0",
  "entry": "plugin.exe",
  "triggers": ["compress", "convert", "merge"],
  "kinds": ["file"],
  "permissions": { "fs_read": ["user_selected"], "fs_write": ["user_selected"], "network": [], "spawn": false }
}
```

### JSON-RPC methods (host to plugin)
`initialize`, `query(parsed_query)`, `get_actions(items)`, `preview(action_id, items, params)`, `execute(action_id, items, params)`, `undo(undo_token)`, `shutdown`.
Plugins may also send notifications such as `items_changed`.

**Acceptance test for the API:** at least two built-in modules (Clipboard and Files verbs) must be implemented through this same interface.

## 11. UI/UX specification

- Single borderless window, centered upper third, rounded, dark/light follows system.
- Input at top; results below (max ~8 visible), each row: icon, title, subtitle, kind badge, shortcut hint.
- Keyboard: `Up/Down` select, `Enter` default action, `Tab` open action list for the selected item, `Ctrl+Enter` reveal in folder, `Ctrl+Z` undo last action, `Esc` close, `Ctrl+,` settings.
- **Preview pane** (right side) for the selected item: file preview, OCR text, clipboard content (masked), process details.
- **Action card:** shows what will happen (for example "Stop `node.exe` (PID 4821) using port 3000 in `C:\proj\app`"), a risk badge, and Confirm/Cancel. After execution show result and an Undo button when available.
- Empty state shows tips and recent items. Never show a blank screen.
- Fully usable with keyboard only; respect system accessibility settings.

## 12. Modules and priorities

**P0: core (must ship first)**
- **Find:** apps, file names, calculator, unit and currency conversion (currency rates optional/offline cache).
- **Do (dev basics):** `kill port 3000` with process lookup, preview, confirm, best-effort restart as undo (only when command line and working directory were captured).

**P1: daily use**
- **Content search:** full-text + OCR of images/screenshots; time filters.
- **Capture:** screenshot hotkey with OCR, auto-blur of emails/tokens, auto-index.
- **Clipboard:** history, secret masking, smart paste (web table to clean tabular text, strip tracking from links).
- **Files verbs:** on selected files: compress image to target size ("under 200 kb"), merge/split PDF, batch rename with live preview, strip metadata.

**P2: depth**
- **Semantic search** (local embeddings).
- **Resume:** save and restore a work session (files, folders, terminals, tabs when available).
- **Share:** send a file to a phone over LAN (QR code); expiring local link.
- **Snippets:** text expander with fields, dates, per-app rules.

**P3: later**
- Dev tools (git "undo anything", env manager, log viewer), Study tools, Cleanup (duplicates, "free up space" with explanations and undo), Sensitive-file finder, Plain-language rules ("when I download an invoice, file it in Invoices/2026", always shown as a preview before approval), Timeline, browser tab search via an extension.

## 13. Phased plan and acceptance criteria

**Phase 0 (setup):** repo, CI, project structure, hidden prewarmed window, hotkey toggles bar, tray icon. *Done when:* hotkey shows/hides the bar in < 80 ms; `cargo test` and lint pass.

**Phase 1 (core find):** app launch, calculator, units, file name index with watcher, frecency ranking, progressive result streaming with cancellation. *Done when:* 1M-name benchmark meets the < 50 ms budget; stale results never flash.

**Phase 2 (safe actions):** action engine, preview/confirm UI, undo log, `kill port` action. *Done when:* action cannot run without confirmation; undo works for restart case; log survives restart.

**Phase 3 (content):** tantivy index, OCR pipeline, time filters, privacy controls (first-run folder choice, pause, exclusions, delete index). *Done when:* "screenshot with <text>" finds the image; pause really stops indexing.

**Phase 4 (capture, clipboard, files verbs):** built as plugins through the plugin API. *Done when:* both are separate processes and can be disabled without affecting the core.

**Phase 5 (semantic + resume):** local embeddings, find-by-description evaluation, session restore. *Done when:* the evaluation set (section 14) reaches the target.

**Phase 6 (polish and release):** installer, settings UI, docs, benchmark report, public README with GIF demos.

## 14. Testing and benchmarks

- **Unit tests:** parser (time phrases, filters), ranking, action preview/undo, secret masking.
- **Integration tests:** end-to-end query on a fixture folder tree.
- **Performance suite (`bench/`):** hotkey latency, keystroke-to-first-result, memory, indexing throughput.
- **Find-by-description evaluation:** create 50 realistic queries over a fixture corpus (documents, screenshots, notes) with expected results; report top-1 and top-5 accuracy and compare with Windows Search as a baseline. Target: clearly better than baseline on content/meaning queries.
- **Safety tests:** every `confirm`/`dangerous` action must be impossible to run without the confirm step.

## 15. Suggested repository layout

```text
lookout/
  core/            # Rust daemon crate(s): index, query, actions, plugin host
  ui/              # Tauri frontend
  plugins/
    clipboard/
    files-tools/
    capture/
  bench/
  docs/            # architecture, plugin API, privacy notes
  fixtures/        # sample corpus for tests and evaluation
  README.md
```

## 16. How to work (instructions to the AI building this)

1. **Build in phases.** Complete one phase fully, with tests and a runnable build, then stop and summarize what works, what is missing and what to test manually. Do not start the next phase until told to.
2. **Stay inside the scope** of the current phase. Do not add features, dependencies or abstractions that are not required. If something seems missing from the spec, ask first or state your assumption clearly.
3. **Prefer the defaults** in section 6. If you deviate, explain why in `docs/decisions.md`.
4. **Measure, don't guess.** Use the `bench/` tooling to verify budgets and report real numbers, including failures.
5. **Be honest about limits.** If something cannot be done as specified (for example needs admin rights, or a Windows API limitation), say so and propose the closest alternative.
6. **Write clear code:** small modules, typed errors, no unwrap in non-test code, comments only where intent is not obvious.
7. **Safety first:** never run destructive actions during development on the user's real files. Use fixtures and temp directories.
8. **Document as you go:** keep `README.md` and `docs/` current with how to build, run, test and benchmark.

## 17. First task for the AI (copy this as the opening message after the brief)

> Read the full brief. Start **Phase 0** only. Create the repository structure from section 15, set up the Rust workspace and a Tauri 2 app with a pre-created hidden window, implement the global hotkey (default `Alt+Space`, configurable) that shows/hides the bar, add a tray icon with Pause and Quit, and add CI for build, test and lint. Include a small latency measurement for hotkey-to-visible. When done, summarize the result, list how to run it, report the measured latency, and wait for my approval before Phase 1.
