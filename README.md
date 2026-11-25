# Lookout

A local-first command bar for Windows: find things by what you remember about them, and run safe actions with undo. Everything stays on the device. See the project brief for the full specification.

## Install

1. Get `Lookout_<version>_x64-setup.exe` from the **Release build** workflow (Actions, artifact `lookout-installer`) or build it yourself (below).
2. Run it. It installs for your Windows account only, with no administrator rights, and bundles the clipboard and file-tools plugins and the semantic-search helper program.
3. Press `Alt+Space`. On first run Lookout asks which folders it may index; nothing is indexed until you choose.
4. `Ctrl+,` opens Settings: folders, exclusions, pause, idle time, rescan interval, plugins (each is off until you accept its permissions), "Delete my index" and "Delete my meaning index".

Uninstall from Windows Settings. Your data (index, key, settings, clipboard history) stays unless you tick the "delete the application data" box on the uninstall page.

**Semantic search ("By meaning") needs files that are not bundled**: the model, its tokenizer and `onnxruntime.dll`. Lookout works without them. `docs/semantic.md` says what to download, where to put it and which SHA-256 each must have. Results then appear in a separate "By meaning" group below the normal ones.

## What stays on the device

Everything. Your files, searches and index never leave your PC. Lookout connects to the internet only when you click a download button, "Check for updates" or "Download and install", and it only receives files. There is no telemetry. The name index, text index, usage history, undo log, clipboard history and meaning vectors are encrypted with a key only your Windows account can unlock (DPAPI). `settings.json` (folder names, your choices) is not encrypted. Secrets such as API keys and card numbers are masked before text is indexed. Deleting your index removes the data and the key; your files are never touched.

## Known limits

- Plugins are separate programs with your Windows permissions. Lookout does **not** sandbox their file or network access in v1; read `docs/plugins.md` before installing a third-party plugin.
- Text is read from `.docx`, `.xlsx`, `.pptx` and PDFs that have a text layer. Not supported: the old `.doc`, `.xls` and `.ppt` formats, scanned PDFs (no OCR on PDFs) and e-mail files; for those only names are searched.
- Screen capture uses GDI: some windows (protected or GPU-composited content) come out as black frames, and capture costs noticeable CPU at 1080p60 and above.
- Semantic ("By meaning") search needs a model download on first use; without it normal search still works.
- Windows only.
- No Resume (reopening what you were working on).
- Semantic search needs the extra files above, plus the Visual C++ 2015-2022 x64 runtime, and misses some queries (`docs/benchmarks.md`).
- Installer built by CI; not yet tested on a clean PC. Not code-signed, so Windows SmartScreen may warn.
- Measured numbers: `docs/benchmarks.md`.

## What works


- file and folder names from folders you choose (in-memory index, kept fresh by a file watcher)
- installed apps (Start Menu shortcuts)
- a calculator (`2+2*5`, `sqrt(16)`, `2^10`) and unit conversion (`10 km to miles`, `100 f in c`, `1 gib to mb`)
- frecency (files and apps you open rise in the ranking), recency, and a `kind:file|folder|app` filter

Actions go through a preview and, when they can hurt, an explicit confirmation, and every one is written to an undo log that survives restarts:

- quick actions: type `dark mode` (switches Windows dark/light, undoable), or `wifi`, `bluetooth`, `night light`, `display`, `sound`, `battery`, `storage`, `apps`, `windows update`, `settings` (opens that Settings page); results are labelled System
- type a web address (`apple.com`, `https://x.com/y`, `localhost:3000`) to open it in your default browser (http and https only), or a network path (`\\server\share`) to open it in Explorer
- `kill port 3000` (also `kill 3000`, `stop :3000`) shows the program, command line and folder that are using the port right now, asks you to confirm, stops it, and can start it again with `Ctrl+Z` (only if its command line and folder could be read)
- quick keys: give an app, file, folder, web address or system action a 2 to 8 character alias (select the result and click Set quick key, or Settings > General); type the alias and a space to put it on top (previews and confirmations still apply)
- timers: `timer 10 min`, `timer 90s`, `timer 1h30m tea`, `10 min timer`; `timers` lists and cancels them; a Windows notification (with sound) fires even when the window is hidden, and timers survive a restart
- opening a file is an action too: documents, folders and apps open at once, but `.exe .bat .cmd .js .vbs .msi .ps1 .scr .hta` (and similar types) ask first

Phase 3 adds search by what is *inside* things, with privacy controls:

- full-text search over the contents of text files (`txt md log csv json xml yaml toml html` and common source code; first 1 MiB of each), through a tantivy index
- text inside images and screenshots (`png jpg bmp gif tif webp`) read with the Windows built-in OCR; `screenshot with invoice 4821` finds the image
- time filters: `invoice yesterday`, `report last week`, `taxes in march`, `notes last 3 days`, or just `yesterday` for recently changed files
- first-run folder choice, per-folder exclusions, pause (tray item and `Ctrl+Alt+P`), a "Delete my index" button (`Ctrl+,` opens them)
- everything Lookout stores about your files is encrypted at rest with a key that only your Windows account can unwrap (DPAPI), and secrets (API keys, private keys, card numbers, `password = ...`) are masked before text is indexed

Phase 4 adds plugins: separate processes behind a JSON-RPC API (`docs/plugins.md`), run through the same preview, confirmation and undo as every other action. Two ship with Lookout, both **off until you accept their permissions** (`Ctrl+,`, Plugins):

- **Clipboard history** (type `clip`, `clip <words>`, `clip clear`): opt-in history of text you copy. Secrets are masked before anything is stored (the stored text is the masked text), history is encrypted with the same key as the index, nothing is recorded while Lookout is paused or when a program (password manager) asks not to be recorded, and the number of clips and retention time are settings.
- **File tools** (on files you find by name): `compress photo.png under 200 kb` (writes a JPEG copy, original untouched), `rename trip photos to Holiday {n}` (renames the files directly inside the folder; the result row previews the first renames as you type; undo puts the names back), `strip metadata photo.jpg` (writes a clean copy of a JPEG or PNG). No PDF tools yet.

Plugin limits are real: Lookout does not sandbox files or the network for plugins in v1. Read `docs/plugins.md` before installing a third-party plugin.

The tests that read real screenshots with Windows OCR skip themselves if the machine has no OCR language installed, so they may pass without checking anything on CI (see `docs/decisions.md`).

## Tested scale

File-name search was measured with up to about 1,000,000 names of mean length 19 characters, at about 49 bytes per name (idle memory 126 MB with that index, budget 150 MB). Larger or longer-named indexes use proportionally more memory and are not tested. A saved name snapshot with more than `snapshot_max_entries` entries (default 300,000) is **not loaded** at startup, to avoid holding two big indexes at once: above that size, file-name search starts empty after each launch and fills in as the startup crawl runs (about 40 s for 1M files at the default 10% duty). Below it, the snapshot answers immediately while the crawl refreshes it. Content search (text and OCR) has been measured on a few thousand documents, not at the million scale.

## Requirements

- Windows 10/11 x64
- Rust stable with the MSVC toolchain and the VS Build Tools "Desktop development with C++" workload
- Node.js 20.19+ (for the Svelte/Vite frontend)
- WebView2 runtime (preinstalled on Windows 11)

## Build, run, test

```bash
npm --prefix ui install
npm --prefix ui run tauri dev          # run with hot reload
npm --prefix ui run tauri build -- --no-bundle   # release exe: target/release/lookout.exe
# NSIS installer: build and stage the plugins first, then bundle them
cargo build --release -p lookout-clipboard -p lookout-files-tools -p lookout-semantic
powershell -File scripts/stage_plugins.ps1 release
npm --prefix ui run tauri build -- --config src-tauri/tauri.installer.conf.json
```

Dev machines: `powershell -File scripts\dev_update.ps1` closes Lookout, builds release, stages the plugins and fast-scan helper, copies them over the per-user install and restarts it.
It leaves settings, index and data alone, and prints the total time and version/commit; a failure names the step.

Plugins run from `plugins\<folder>\` next to `lookout.exe`; after building, `scripts\stage_plugins.ps1 [debug|release]` copies the plugin executables and manifests there.

```bash
cargo test --workspace   # includes kill-port tests against a harmless listener process (core/examples/test_listener)
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
npm --prefix ui run check
```

Tests and benchmarks never read your files: they use temporary folders and generated names.

## Using it

- `Alt+Space` shows or hides the bar. `Esc` or clicking elsewhere hides it. `Up`/`Down` choose, `Enter` runs the selected result's default action (open a file, folder or app; for a `kill port` result, show what would be stopped).
- A confirmation card has **Cancel** and **Confirm**; focus starts on Confirm (on Cancel for dangerous actions) and `Enter` only works on the focused button. `Esc` closes the card. After an action that can be undone the card offers **Undo**, and `Ctrl+Z` with an empty search box undoes the latest undoable action (it asks for confirmation too, since it starts a program). With text in the box `Ctrl+Z` is the text field's own undo.
- `Ctrl+,` opens **Privacy and folders**: the folders to index, folders to never index, pause, and **Delete my index**. On first run it opens by itself and nothing is indexed until you choose.
- Tray icon: **Pause indexing** and **Quit Lookout**. `Ctrl+Alt+P` (`pause_hotkey`) does the same pause. Pause stops the file crawl's updates, text extraction and OCR; work already running on one file finishes first.
- Reading file contents and OCR run only when you have been away from the keyboard and mouse for `idle_secs` (default 10 s), at about 10% of a core.
- Settings live in `%APPDATA%\dev.lookout.app\settings.json` (created on first run). Restart after editing.

  ```json
  {
    "hotkey": "Alt+Space",
    "index_roots": ["C:\\Users\\you\\Documents", "D:\\Projects"],
    "excluded_names": [],
    "crawl_duty": 0.1,
    "rescan_hours": 0,
    "ranking": { "w_match": 1.0, "w_frecency": 0.6, "w_recency": 0.2, "w_semantic": 0.0 }
  }
  ```

  Other Phase 3 settings: `snapshot_max_entries`, `excluded_folders` (never indexed: names, contents and OCR), `pause_hotkey`, `index_contents`, `ocr`, `idle_secs`, `first_run_done`.

  `rescan_hours` is how often the index is rebuilt from disk to repair anything the file watcher missed. It is 0 (off) by default because a rescan briefly holds two copies of the index in memory; set it to 24 to opt in. A watcher error triggers a rescan either way. With scheduled rescans off (the default), changes the file watcher misses without reporting an error are repaired only when Lookout is restarted (the index is rebuilt on every launch).

  **Indexing is opt-in.** `index_roots` is empty until you choose folders (the first-run chooser, or `Ctrl+,`), so file search finds nothing until you do. Apps work immediately. These names are never indexed, at any depth below a root: `node_modules`, `.git`, `AppData`, `Windows`, `Program Files`, `ProgramData`, recycle bin and system folders, browser `User Data`, and password-manager folders (`excluded_names` adds more). Symbolic links and junctions are skipped.
- If the hotkey is taken by another app (PowerToys Run and Flow Launcher both default to `Alt+Space`), Lookout falls back to `Alt+Space`, and if that is also taken it shows a message naming `settings.json`.

## Live check of the real app

```bash
npm --prefix ui run tauri build -- --no-bundle
cargo build -p lookout-core --examples
node scripts/live_e2e.mjs
```

Starts `target/release/lookout.exe` with `--data-dir` in a temp folder, drives its window through the WebView2 debugging port (port 9333) and checks search, the kill-port card, the confirmation refusals, undo after restarting Lookout, and opening files. Needs Node 22+. It stops only the harmless `test_listener` process it starts, and touches nothing of yours.

Where things are stored (`%APPDATA%\dev.lookout.app`, or `--data-dir`): `lookout.key` (DPAPI-wrapped key), `meta.db` (frecency and undo log, sealed), `names.idx` (file-name snapshot, sealed), `content\` (full-text index, encrypted). **Delete my index** removes all of them; `settings.json` stays.

## Benchmarks

```bash
cargo build --release -p lookout-bench
target\release\bench.exe latency --cycles 30      # hotkey to visible bar
target\release\bench.exe query                    # keystroke to first results, 1M synthetic names
target\release\bench.exe query --threads 4        # same, scans capped at 4 threads
target\release\bench.exe crawl --files 1000000    # real crawl of a generated temp tree (creates 1M empty files, ~5 min)
target\release\bench.exe locks                    # how long writers hold the index lock
target\release\bench.exe watch --files 100000     # watcher burst: how many new files are seen
target\release\bench.exe memory --runs 3          # idle memory of the real app, 120 s per run
target\release\bench.exe memory --runs 3 --app-arg --synthetic-index --app-arg 1000000   # ... with a 1M-name index loaded
target\release\bench.exe memory --runs 2 --fixture-text 3000 --fixture-images 60         # ... during a content + OCR batch (generated corpus, CPU trace)
```

Each exits non-zero when a budget is missed: hotkey p95 under 80 ms, first results p95 under 50 ms with no stale updates, idle private working set under 150 MB. Results and the exact definitions are in `docs/decisions.md`.

## Layout

```text
core/      lookout-core: item model, query parsing, ranking, name index, crawler and
           watcher, sources (calculator, units, apps, files, ports), frecency, query
           engine, action engine (gate, undo log, open, kill port)
ui/        Svelte + TypeScript frontend; ui/src-tauri is the thin Tauri 2 shell
bench/     budget benchmarks (latency, query, memory)
plugins/   clipboard, files-tools (plugin processes, built as separate crates); capture is a later phase
docs/      architecture notes and decisions
fixtures/  ranking regression cases (fixtures/ranking/cases.json)
```

## Privacy

See [PRIVACY.md](PRIVACY.md).

## License

[PolyForm Noncommercial 1.0.0](LICENSE): free for personal and other noncommercial use. Commercial use needs a separate license from the owner.
