# Architecture (Phase 4)

```text
keystroke -> frontend numbers it (generation N) -> invoke query(N, text)
                                                      |
                              Engine::submit(N)  (ignored unless N is newer than any earlier one)
                                |  parse -> spawn one thread per source
                                v
        +---------+  +-------+  +------+  +-------------------------+
        |calculator| | units |  | apps |  | files -> NameIndex.search |   (all Instant tier)
        +----+----+  +---+---+  +--+---+  +------------+------------+
             \           |         |                  /
              +--------- Msg::Batch{N, source, items} --------+
                                                             v
                         merger thread: drops anything not for the current generation,
                         coalesces, ranks, flushes (first update immediately, then <= 1 per 16 ms)
                                                             |
                      sink -> emit "query-results" {N, items, done} -> frontend drops N != its latest
```

## Staleness

Old results are discarded at four points: sources can poll `QueryContext::is_cancelled` and stop early (the index scan checks it every 65,536 entries); the merger ignores messages for any generation but the one it is building; it re-checks the live generation immediately before each delivery; and the frontend ignores any update whose generation is not its latest. Tests: `engine::tests::old_generation_results_are_discarded_even_from_a_source_that_ignores_cancellation`, `typing_quickly_delivers_only_the_final_query_after_the_last_keystroke`, and the bench's fast-typing check.

## Name index (`core/src/name_index.rs`)

All names live lowercase in one byte arena, `\0name\0name\0...`. A query runs a SIMD substring search (`memchr::memmem`) for its longest token over the arena (split across up to 8 threads above 200,000 entries), maps each hit to its entry with a galloping cursor, verifies the other tokens, scores the entry and keeps the best N in a heap. 20 bytes per entry plus the names; original-case names are stored only when they differ from lowercase. Removals set a tombstone flag; `compact()` rebuilds when more than 25% are dead.

## Background work

Lost watcher events cannot be detected on Windows (`notify` ignores `ReadDirectoryChangesW` overflow), so the event loop can rebuild the whole index and swap it in after a watcher error or on request, and every `rescan_hours` if the user opts in (default off: a rescan briefly holds two indexes); see `rescan` in `index_service.rs`.

`IndexService` crawls each root on one thread (duty-cycled to about 10% busy, paused by the shared `PauseState`) and applies debounced watcher events on another. Frecency lives in SQLite (`meta.db`) and is mirrored into the index as a one-byte level per entry after the first crawl.

## Actions (`core/src/actions`)

```text
Enter on a result -> prepare_action(action, result id)   plan against the live state -> preview + ticket
                       |- risk safe ----------------------> run_action(ticket) -> execute -> log
                       `- risk confirm / dangerous --> card shown -> present_action(ticket)
                                                       -> (>= 350 ms) confirm_action(ticket)
                                                       -> plan again, same fingerprint? -> log started -> execute -> log executed
```

`ActionEngine` (`gate.rs`) is the only owner of the actions; `Action::execute` needs an `Authorized` value only the gate can create. Tickets are single use and expire after 60 s. The log (`undo_log.rs`, table `undo_log` in `meta.db`) is written before execution. `open.rs` classifies the file on disk at that moment; `kill_port.rs` reads the TCP table and the process details through the `ProcessBackend` trait (`system.rs`, real implementation `WindowsSystem`; tests use a fake or the harmless `test_listener` example). Undo is a ticket as well: `prepare_undo` -> `present_action` -> `confirm_action`.

## Shell (`ui/src-tauri`)

Hotkey, tray, window, `query`, `index_status`, and the action commands `prepare_action`, `prepare_undo`, `present_action`, `cancel_action`, `run_action`, `confirm_action`, `can_undo` over `lookout_core::lookout::Lookout`. Action targets are looked up from the ids in the latest update the backend sent, so the webview cannot name a path or port of its own. File opening is handed to the `open` crate (ShellExecute) by a `Launcher` that only the action engine calls.

## Storage and encryption (Phase 3)

```text
lookout.key  DPAPI (CryptProtectData) wrapped 256-bit key         crypto.rs, storage.rs
meta.db      SQLite held in memory; sealed snapshot after writes  securedb.rs  (frecency, undo log)
names.idx    sealed snapshot of the name index                    names_store.rs
content/     tantivy files, each ChaCha20-encrypted               content/crypt_dir.rs
```

Sealed = ChaCha20-Poly1305 in 64 KiB authenticated chunks. The tantivy directory uses plain ChaCha20 (seekable) so it can be read at random offsets. `Storage::open` makes the key on first use and, if a key cannot be unwrapped, deletes what it protected and starts fresh.

## Content pipeline

```text
roots --> ContentService (one thread) --gate: not paused AND user idle >= idle_secs--> unit of work
            |  full scan at start; watcher paths via IndexService.on_path
            |  text file -> read (<= 1 MiB) -> secrets::mask -> ContentStore.upsert
            |  image     -> Windows OCR      -> secrets::mask -> ContentStore.upsert
            `  duty cycle ~10%; commit every 50 docs, when the gate closes, and at the end
ContentSource (Smart tier) --> tantivy query (all words, last word also a prefix, kind/time filters) --> items
```

Excluded folders, built-in excluded names, symlinks and sensitive file types (`.pem .key .env`, ...) are never opened. The writer exists only during a batch.

## Query parsing (Phase 3 additions)

`parse` removes a time phrase (`timefilter.rs`, local time zone, weeks start Monday) and a leading "screenshot with / image that says" phrase (kind = screenshot). The window filters both the name index (by modified time) and the content index; a time-only query lists the most recently changed names.

## Privacy controls

`Lookout::wipe_index` stops the services, switches `meta.db` writes off, deletes the files and key and empties the in-memory index. The shell (`ui/src-tauri`) swaps in a fresh `Lookout` when folders change or the index is deleted. Pause is one shared flag set by the tray item, `pause_hotkey` and the panel.

## Plugins (Phase 4; details in `plugins.md`)

```text
query "clip foo" / "compress x under 200 kb"
   -> PluginSource (Smart tier, one per manifest; routes by first word) -> PluginHost.query
        (starts the plugin on first use; per-call timeout; garbage/crash/hang => kill, error, restart on next use)
   -> plugin answers {items} or {select: text}; for file verbs the host searches names and hands back <= 8 candidates
   -> items come back as results whose action is plugin/<id>/<action>; Lookout::target_of tags them with their origin
Enter -> ActionEngine.prepare -> PluginActions (ActionProvider) -> PluginHost.plan: get_actions + preview
         risk = max(plugin's ask, manifest floor)  -> same ticket / present / confirm / undo log as open and kill_port
         execute / undo reach the plugin only through functions that need the gate's Authorized token
```

`core/src/plugins`: `manifest` (plugin.json, permissions, risk floor, approval key), `wire` (one process: stdio JSON-RPC, reader and writer threads, Job Object), `host` (registry, enable/approve state in `plugins.json`, lifecycle, supervisor thread, calls), `integration` (`PluginSource`, `PluginActions`), `sdk` (plugin side loop). Bundled plugins: `plugins/clipboard` (background; watcher, masked + encrypted store) and `plugins/files-tools` (on demand; WinRT image codecs, byte-level metadata stripping, two-phase rename).
