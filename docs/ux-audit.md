# Lookout UX audit

Method: release build, isolated data folder, sample files outside the repo. Light/dark was switched by emulating the OS theme (the app has no theme switch). Screenshots are in `C:\lkt_shots` (not committed). Not captured: "By meaning" (needs a model download), Plugins permission card, Disk "Largest/old" tabs.

## 1. Screens and how to reach them
- Search bar: hotkey (default Alt+Space). Shows tips when empty, results, or "No results".
- First-start setup: appears automatically on first show (Settings in setup mode).
- Confirm / result card: Enter on a risky result (e.g. "kill port 3000"), Ctrl+Z on an empty box.
- Settings (folders, pause, hotkey, indexing, delete, Health, Plugins): Ctrl+, only. No visible button.
- Disk usage (Folders / Largest 100 / Large not modified, ring or treemap): type "disk" or footer link.

## 2. Settings
Basic: Folders to index, Never index, Start/Save, Pause. Advanced: Grab-text hotkey, Idle seconds, Rescan hours, Indexing speed, Max index size, Delete index, Health, Plugins.
Confusing: "Idle seconds before reading file contents"; "Rescan every (hours, 0 = never)"; "Max content index size (MB)"; speed note quotes CPU percentages; "Never index (names, contents and screenshots text)"; the second "Save" button (tuning) sits apart from the first one, so users may think one saves both.

## 3. Technical wording -> plain replacement
- "index / indexing / Delete my index" -> "search list / preparing your files for search / Forget my files"
- "Plugins" -> "Extras"; "This plugin asks to:" -> "This extra wants to:"; "run as separate programs with your Windows permissions; see docs/plugins.md" -> remove path, say "It can see what you can see."
- "Semantic / By meaning / meaning index" -> "Smart search"; "SHA-256 ... hosts" -> "checked for safety".
- "Screenshot reading / OCR / Grab text from screen" -> "Read text in pictures".
- Health lines: "up to date", "Last error: none", "files found so far", "Index now" -> "Ready", hide when none, "Check files now".
- Footer "144 apps · files · folders" and "meaning index 3/8" -> "Ready to search" with details on hover.
- "kind:folder", "kill port 3000", "Ctrl+Z (empty box)" in tips; "badge" words `calc`, `convert`, `port`.
- Disk: "Size on disk vs logical", "hard-linked", "limited detail", "Large, not modified", "Low impact" -> "Space used", remove, "Old big files", "Gentle".
- Card label "Needs confirmation" is fine; "Dangerous" could say what is lost (it does in the body).

## 4. Inconsistencies and gaps
- Buttons: three separate `.btn` copies (App, Privacy, Plugins) with different padding/sizes (7x16 vs 6x14 vs 4x10); Disk uses a different blue (#2f6fed) than the bar (#3b82f6); tabs and ring/treemap toggles use a third style; footer "Disk usage" is plain text, not button-like.
- Hard-coded colors ignoring the theme: orange #d9822b warnings, red #c0392b, blue #3b82f6 in App/Privacy/Plugins; Disk has #b9770e/#c0392b; Chart.svelte has many fixed hexes (#fff, #000, #666, #ccc) for labels and borders, so the chart does not follow light/dark. Disk and Chart need theme tokens.
- Spacing: settings is one long 2000px scroll (about 7 screens at 325px) with no sections, headings differ in rhythm; number inputs fixed at 110px next to long labels; Disk uses a wider window, the bar jumps in size.
- Missing states: Health and Plugins show no spinner while loading ("Search is not running." on failure); the setup download has text-only progress, no bar; scan errors use "that is not a folder Lookout can read" with no next step; results list has no "searching…" state; "Search unavailable" footer gives no help.
- Saving: Save closes Settings silently; "Saved." message appears at the very bottom, out of sight after scrolling.

## 5. First run
- A new user sees the folder chooser first, which is good, but nothing tells them the hotkey (Alt+Space). The tips screen never mentions it, and Settings only shows the Pause hotkey.
- No welcome text, no hint where to find Settings later except a tip line saying Ctrl+, in a long tip list. Closing the setup with Esc hides the bar and leaves nothing indexed.
- Empty tips list has 6 lines mixing basics and expert items (ports, undo, disk).

## 6. Keyboard and accessibility
- Good: focus ring on buttons/inputs (#3b82f6), combobox/listbox roles, alertdialog on cards, Esc everywhere.
- Gaps: results list uses mouse hover to change selection and `onkeydown` stubs; list rows not focusable; input has aria-controls but `#results` missing in some states; Settings and Disk checkboxes/selects have no visible focus style (Plugins config inputs rely on default); Chart segments hide the outline (`outline: none`).
- Contrast: muted text (#6b6b78 on near-white, 12px) is around 4.5:1 at best; footer, hints and `.note` are 11-12px; orange #d9822b on light is under 3:1; the white-on-blue buttons are about 3.7:1. Many hint lines are 11px.
- Readonly hotkey box: click-then-press has no visible "listening" state. Screen readers hear "Remove" without the path only through aria-label (ok), but Disk "Expand" has no row name.

## 7. Five changes with the biggest effect (in order)
1. Show the hotkey and a short welcome on first start and in the empty-bar tips; put a visible Settings button (gear) in the footer next to Disk usage.
2. Rewrite the jargon (section 3), especially index, plugins, semantic, OCR, Health; hide "Last error: none".
3. Split Settings into short sections with a Basic part first and Advanced collapsed; one Save at the bottom; show "Saved" next to the button.
4. Make all colors theme tokens in app.css (buttons, warning, danger, accent, chart labels) and share one button style across App, Privacy, Plugins and Disk.
5. Fix contrast and focus: darker muted text, 13px minimum for hints, visible focus on Chart segments, selects and checkboxes; add loading and error-with-next-step states for Health, Plugins, setup download and scans.
