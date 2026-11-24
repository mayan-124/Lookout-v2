# Plugins

A plugin is a separate program that Lookout starts, talks to over JSON-RPC 2.0 on stdio, and stops again. Clipboard history and the file tools are plugins written against exactly this interface.

## Layout and manifest

`plugins/<folder>/plugin.json` next to `lookout.exe` (the folder name is free; the `id` inside is what counts):

```json
{
  "id": "com.example.files-tools", "name": "File Tools", "version": "0.1.0",
  "entry": "plugin.exe", "args": [],
  "triggers": ["compress", "rename"], "kinds": ["file"],
  "permissions": { "fs_read": ["user_selected"], "fs_write": ["user_selected"],
                   "network": [], "spawn": false, "clipboard": "none" },
  "background": false,
  "config_defaults": {}
}
```

`entry` must be a relative path inside the plugin folder. `triggers` are the first words of a query that are sent to the plugin. `background: true` keeps the plugin running while it is turned on (needed to watch the clipboard); every other plugin is started by the first query or action that needs it and stopped after 60 s without a call. `config_defaults` lists settings (numbers or text) that the plugin panel lets the user change; they are passed to `initialize`.

## Framing and methods

One JSON object per line (UTF-8, `\n`), at most 4 MiB. Host to plugin (requests, answered with `result` or `error`):

| method | params | result |
|---|---|---|
| `initialize` | `protocol`, `data_dir` (the plugin's own folder), `config`, `paused`, `key` (64 hex characters, or null) | any |
| `query` | `raw`, `verb`, `rest`, `candidates` (`[{path, kind}]`) | `{items}` or `{select: {text}}` (ask the host to find files named `text`, then query again with `candidates`) |
| `get_actions` | `items` | `{actions: [{id, label, risk}]}` |
| `preview` | `action_id`, `items`, `params` | `{title, lines, warnings, undoable, risk?}` |
| `execute` | `action_id`, `items`, `params` | `{message, undo_token?}` |
| `undo` | `undo_token`, `dry_run` (true: describe only, change nothing) | `{title, lines, warnings}` or `{message}` |
| `shutdown` | none | `null` |

Host to plugin notification: `pause_changed {paused}`. Plugin to host notification: `items_changed`. A plugin item is `{id, title, kind, subtitle?, uri?, action, params?, score?}`; `action` is the action a press of Enter starts.

`undo` with `dry_run` is an addition to the brief's method list: the gate has to show what an undo will do before it asks for confirmation.

## Index API (semantic search)

Besides the methods above, the **core** (not the webview, not another plugin) may call these on a plugin through `PluginHost::service_call`, which refuses any other method name. They never run actions and bypass nothing in the gate; they exist so the core can feed and query the semantic helper (`docs/semantic.md`). Same framing, timeouts set per call (batch 30 s, search 8 s, others 10 s).

| method | params | result |
|---|---|---|
| `index_needs` | `items: [{path, mtime}]` | `{need: [path]}` the files with no vector for that mtime |
| `index_batch` | `docs: [{path, name, mtime, text}]` (masked text; empty text means "no text, remember it") | `{stored}` |
| `index_remove` | `paths` | `{removed}` |
| `index_list` | `after`, `limit` | `{paths}` in ascending order |
| `index_status` | none | `{ready, reason, count}` |
| `index_search` | `text`, `limit` | `{hits: [{path, score}]}` best first, score = cosine similarity |
| `index_delete` | none | `{deleted}` |

The host waits for a busy plugin but gives up when the caller's cancel flag is set (the next keystroke).

## What the host decides

* **Every plugin action goes through the action gate and the undo log**, like `open` and `kill port`: prepare (live preview), present, confirm (or run, only for risk `safe`), then execute. The gate plans again before executing and refuses if the preview changed. `execute` is a method the host calls; the host's function that calls it needs the gate's `Authorized` token, so no other code path in Lookout can reach it.
* **Risk.** The effective risk is the highest of: what the plugin asked for in `get_actions`, what it put in `preview`, and the floor implied by its manifest. Floor: `fs_write` listing anything but narrow entries (`*`, `any`, `all`) is `dangerous`; any other `fs_write`, `spawn: true` or a non-empty `network` list is `confirm`; otherwise `safe`. A plugin can only raise the risk.
* **A plugin cannot execute anything itself.** Messages it sends other than the `items_changed` notification (a request named `execute`, a notification named `run_action`, anything else) get an error response or are dropped, and are counted. Only an item that plugin produced can be the target of its actions, and the webview cannot name a different action or path.
* **Paths.** For plugins with `fs_read: ["user_selected"]` the host searches the file-name index for the text the plugin asked for and hands over at most 8 candidates; an item whose `uri` is not one of those paths is dropped.
* **Permissions are shown before first use.** A plugin is off until the user opens the plugin panel (Ctrl+, then Plugins), reads its permissions and accepts. The accepted permissions are remembered with a fingerprint of the manifest; if a later version asks for more, it must be accepted again.
* **Failure handling.** Every call has a timeout (query 1.5 s, preview 5 s, initialize 5 s, execute 60 s, undo 30 s). A timeout, a crash, invalid JSON, a line over 4 MiB or a JSON-RPC error that is not well formed kills the plugin process and fails that one call; the next use starts a fresh process. After three failures in a row the host waits (up to a minute) before trying again. Queries to a busy plugin are skipped, never queued.
* Turning a plugin off stops its process at once; it is never started again until turned on.

## What is and is not enforced (v1)

Enforced by Windows through a Job Object around every plugin process:

* the process and everything it started die when Lookout exits, even if Lookout crashes (kill-on-close);
* a plugin whose manifest says `spawn: false` cannot create child processes (active process limit 1);
* a plugin without clipboard permission cannot read or write the clipboard (job UI limits);
* a memory cap of 1 GiB for the job.

Not enforced, so the manifest is a declaration and a prompt for the user, not a sandbox:

* **Files.** A plugin runs with the user's Windows rights and can read or write any file the user can. `fs_read`/`fs_write` are not enforced by the operating system in v1 (that would need an AppContainer or a restricted token plus ACL changes). The host only limits what it passes in and what it accepts back (candidates, `uri` checks) and shows a preview of what the plugin says it will do. **A plugin could change things during `preview`, or do more than it said during `execute`; Lookout cannot prevent or detect that.**
* **Network.** Not blocked. Blocking per program needs a firewall rule (administrator) or an AppContainer without the network capability; neither is done. The bundled plugins never connect anywhere themselves (they depend on no networking crate; the one-click download lives in the Lookout app, and the plugin executables link `lookout-core` but never call it); a third-party plugin could connect anywhere. `network` in the manifest is only shown to the user and raises the risk floor.
* The small window between starting a plugin and putting it into its job (the process is created, then assigned) is not closed: a plugin that spawns a child in its first microseconds could escape the "no children" rule.
* **A plugin can unwrap `lookout.key` itself.** The master key is protected with DPAPI for the current Windows user, and a plugin runs as that user, so it can read the file and unwrap it just as Lookout does. Lookout gives each plugin only a key of its own (derived from the master key and the plugin id with HKDF-Expand/HMAC-SHA256, sent over the pipe at `initialize`, never written to disk), which stops an honest plugin from holding the master key and keeps one plugin's data unreadable to another, but it does not stop a hostile plugin until there is a sandbox.
* **The preview is written by the plugin, and the host cannot verify that `execute` does what it described.** The gate compares two previews of the same request (so the plugin cannot change its story between preview and confirm), not a preview with an outcome. The card shows a line written by Lookout, "From plugin: <name>", above everything the plugin wrote, so the user can see whose words they are reading.
* **Rename is the only action that changes originals.** Compress and strip write a new file next to the original and undo deletes only that file. Rename moves files within one folder and its undo moves them back (and refuses if an old name is taken again).
* Plugins talk to the host over their own pipes; other programs run by the same user can still read the plugin's files (including its encrypted data, whose key is the same DPAPI-protected key as the rest of the index).

## Writing a plugin

Implement the methods above on stdin/stdout, answering each request with one line. Rust plugins can use `lookout_core::plugins::sdk::serve` with a `Handler`; `core/examples/test_plugin.rs` is a small one. Do not print anything else on stdout. Anything on stderr is discarded.
