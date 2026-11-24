# Lookout UI style

Tokens live in `ui/src/app.css`. Dark is the default. Settings > General > Appearance: Auto (follow Windows, default), Light or Dark; theme.ts sets `data-theme` on the root.
Never hard-code a color in a component; use a variable.

**Fonts** UI: Segoe UI Variable / Segoe UI / system-ui. Code font (`--font-code`: Cascadia Code, Cascadia Mono,
Consolas): paths, shortcuts, commands, file-type badges, status bar. Body 14px, small 13px/12px, section titles 20-22px,
section labels 12px uppercase with letter spacing.

**Shape** window 14px, cards 10px, buttons 8px, chips 6px. Hairline borders, no shadows.

**Colors** (dark / light)
- `--bg` #0e1013 / #fbf8f3, `--panel` #14171c / #fff, `--raised` #1b1f26 / #f3eee5
- `--border` #262b33 / #e7e0d4, `--border-strong` #3a4150 / #d9cfbe
- `--text` #e6e9ee / #1f1a14, `--text-soft` #c3c9d3 / #3d362c, `--muted` #8b93a1 / #6b6358
- `--accent` #f5a524 / #a34a08 (text, icons, focus). `--accent-bg` #f5a524 with `--accent-on` #1a1204 for buttons.
- `--match-text` #ffd28a / #7a3d05, `--match-bg` amber at 22% / 28%
- `--success` #3fb950 / #17722f, `--danger` #f85149 / #cf222e, `--toggle-off` #2f3541 / #c9bfae
- Chart hues `--c0..--c7`, `--c-grey`: color-blind-safe, none near amber.

**Contrast** all text pairs are at least 4.5:1. Two light values were darkened: accent #b45309 -> #a34a08 (4.35:1 on
raised) and success #1a7f37 -> #17722f (4.39:1 on raised).

**Components** (global classes in app.css)
- `.btn` secondary (transparent, strong border); `.btn.primary` amber; `.btn.danger` danger text; `.btn.small`; `.btn.text`.
- `kbd` key badge; `.tag` small code chip; `.switch` on/off toggle; `.warning` amber-edged note; `.note`/`.hint` muted text.
- Focus: 2px amber outline, 2px offset, on every focusable element (global `:focus-visible`).
