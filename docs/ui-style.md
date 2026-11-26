# Lookout UI style

Tokens live in `ui/src/tokens.css`.
Never hard-code a color in a component; use a variable.

**Fonts** UI: Segoe UI Variable / Segoe UI / system-ui. Code font (`--font-code`: Cascadia Code, Cascadia Mono,
Consolas): paths, shortcuts, commands, file-type badges, status bar. Body 14px, small 13px/12px, section titles 20-22px,
section labels 12px uppercase with letter spacing.

**Shape** window 14px, cards 10px, buttons 8px, chips 6px. Hairline borders, no shadows.

**Colors** palette "Graphite", always dark. All values are in `ui/src/tokens.css`.
- Panel base #111214 at .96 with tints #495057 and #868E96; glass off: flat #181A1D.
- Accent default #FFD43B (also mint, blue, violet); text on the accent #111214.
- Text #FFFFFF titles, #E3E8F5 body, #9AA3BD secondary.
- Chart hues `--c0..--c7`, `--c-grey`: distinct, softened for the graphite base.

**Contrast** text pairs are at least 4.5:1 on plain panels, except danger red toward the lightest gradient corner.
Secondary text is lightened per theme so it also passes on cards. Danger red, and Synthwave's accent used as text,
fall below on raised surfaces toward that corner.

**Components** (global classes in app.css)
- `.btn` secondary (transparent, strong border); `.btn.primary` accent; `.btn.danger` danger text; `.btn.small`; `.btn.text`.
- `kbd` key badge; `.tag` small code chip; `.switch` on/off toggle; `.warning` accent-edged note; `.note`/`.hint` muted text.
- Focus: 2px accent outline, 2px offset, on every focusable element (global `:focus-visible`).
