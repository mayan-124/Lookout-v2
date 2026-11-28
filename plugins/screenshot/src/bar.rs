//! The overlay's one floating bar, its small lists and its tooltips: where each
//! thing is, for both drawing and clicking, and how it looks.

use crate::annotation::{draw_system_text_rgba, measure_system_text, Color};
use crate::geometry::{PointI, RectI};
use crate::image::ImageBuffer;
use crate::marks::{self, Marks, Monitor, Tool};
use crate::overlay::{CaptureMode, DelayTimer, Destination, SelectionState};
use crate::skin::{self, Icon, Skin};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Modes,
    Tool(Tool),
    Undo,
    Redo,
    Copy,
    Save,
    More,
    Close,
    Mode(CaptureMode),
    Editor,
    Record,
    Scroll,
    ReadText,
    Timer,
    Pointer,
    Target,
    Color(usize),
    Size(usize),
    PutDown,
}

/// What is open beside the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Popup {
    Modes,
    More,
    /// Colour and thickness of the tool in hand.
    Style(Tool),
}

impl Popup {
    fn anchor(self) -> Button {
        match self {
            Self::Modes => Button::Modes,
            Self::More => Button::More,
            Self::Style(tool) => Button::Tool(tool),
        }
    }
}

/// A bar or a popup: its outline and everything on it.
#[derive(Debug, Clone)]
pub struct Panel {
    pub rect: RectI,
    pub items: Vec<(Button, RectI)>,
    /// Thin dividing lines.
    pub lines: Vec<RectI>,
    pub scale: f32,
    monitor: RectI,
    /// Lists and tooltips open downwards first.
    down: bool,
}

impl Panel {
    pub fn at(&self, p: PointI) -> Option<Button> {
        self.items
            .iter()
            .find(|(_, r)| r.contains(p))
            .map(|(b, _)| *b)
    }
}

// Sizes at 100%.
const HEIGHT: f32 = 40.0;
const BUTTON: f32 = 32.0;
const MODE_BUTTON: f32 = 44.0;
const PAD: f32 = 4.0;
const SPACE: f32 = 2.0;
const DIVIDER: f32 = 9.0;
/// The widest bar, for very narrow screens.
const WIDEST: f32 = 428.0;
const TOOLS: [Tool; 6] = [
    Tool::Pen,
    Tool::Marker,
    Tool::Arrow,
    Tool::Box,
    Tool::Text,
    Tool::Hide,
];

fn px(v: f32, s: f32) -> i32 {
    (v * s).round() as i32
}

/// The monitor's scale, less on a very narrow one.
pub fn scale_on(monitor: Monitor) -> f32 {
    monitor
        .scale
        .min(monitor.rect.width() as f32 / (WIDEST + 20.0))
}

/// Groups of buttons; a divider goes between two groups.
fn groups(sel: bool, mode: CaptureMode) -> Vec<Vec<Button>> {
    if !sel {
        return vec![vec![Button::Modes, Button::Close]];
    }
    let actions = vec![Button::Copy, Button::Save, Button::More, Button::Close];
    // Drawing is for screenshots only.
    if mode == CaptureMode::Screenshot {
        vec![
            TOOLS.map(Button::Tool).to_vec(),
            vec![Button::Undo, Button::Redo],
            actions,
        ]
    } else {
        vec![vec![Button::Modes], actions]
    }
}

/// Where a bar of this size goes: below the box, above it, beside it; inside its
/// bottom edge only when the box leaves no room at all. Always on the monitor.
fn place(sel: RectI, m: RectI, w: i32, h: i32, s: f32) -> PointI {
    let (gap, margin) = (px(10.0, s), px(8.0, s));
    // The size tag sits just above the box.
    let tag = px(34.0, s);
    let x = ((sel.left + sel.right - w) / 2)
        .min(m.right - w - margin)
        .max(m.left + margin);
    let low = (sel.bottom - h)
        .min(m.bottom - h - margin)
        .max(m.top + margin);
    let below = sel.bottom + gap;
    let above = sel.top - tag - h;
    if below + h <= m.bottom - margin && below >= m.top + margin {
        PointI::new(x, below)
    } else if above >= m.top + margin && above + h <= m.bottom - margin {
        PointI::new(x, above)
    } else if sel.right + gap + w <= m.right - margin && sel.right + gap >= m.left + margin {
        PointI::new(sel.right + gap, low)
    } else if sel.left - gap - w >= m.left + margin && sel.left - gap <= m.right - margin {
        PointI::new(sel.left - gap - w, low)
    } else {
        let y = (sel.bottom - h - gap)
            .min(m.bottom - h - margin)
            .max(m.top + margin);
        PointI::new(x, y)
    }
}

/// The bar for a box (below or above it), or without one at the top of `monitor`.
pub fn layout(sel: Option<RectI>, monitor: Monitor, mode: CaptureMode) -> Panel {
    let s = scale_on(monitor);
    let m = monitor.rect;
    let mut items = Vec::new();
    let mut lines = Vec::new();
    // Laid out from (0, 0), then moved into place.
    let top = px(PAD, s);
    let mut x = PAD;
    for (n, group) in groups(sel.is_some(), mode).iter().enumerate() {
        if n > 0 {
            let at = px(x + DIVIDER / 2.0 - SPACE, s);
            lines.push(RectI::new(at, px(11.0, s), at + 1, px(HEIGHT - 11.0, s)));
            x += DIVIDER - SPACE;
        }
        for button in group {
            let wide = if *button == Button::Modes {
                MODE_BUTTON
            } else {
                BUTTON
            };
            let r = RectI::new(px(x, s), top, px(x + wide, s), top + px(BUTTON, s));
            items.push((*button, r));
            x += wide + SPACE;
        }
    }
    let (w, h) = (px(x - SPACE + PAD, s), px(HEIGHT, s));
    let at = match sel {
        Some(sel) => place(sel, m, w, h, s),
        None => PointI::new((m.left + m.right - w) / 2, m.top + px(12.0, s)),
    };
    let moved = |r: RectI| RectI::new(r.left + at.x, r.top + at.y, r.right + at.x, r.bottom + at.y);
    Panel {
        rect: RectI::new(at.x, at.y, at.x + w, at.y + h),
        items: items.into_iter().map(|(b, r)| (b, moved(r))).collect(),
        lines: lines.into_iter().map(moved).collect(),
        scale: s,
        monitor: m,
        down: sel.is_none_or(|sel| at.y >= sel.top),
    }
}

/// A `w` by `h` thing beside the bar, in line with `anchor`.
fn beside(bar: &Panel, anchor: RectI, w: i32, h: i32) -> RectI {
    let (m, gap) = (bar.monitor, px(6.0, bar.scale));
    let x = ((anchor.left + anchor.right - w) / 2)
        .min(m.right - w - gap)
        .max(m.left + gap);
    let (down, up) = (bar.rect.bottom + gap, bar.rect.top - gap - h);
    let fits_down = down + h <= m.bottom;
    let fits_up = up >= m.top;
    let y = if fits_down && (bar.down || !fits_up) {
        down
    } else {
        up.max(m.top)
    };
    RectI::new(x, y, x + w, y + h)
}

/// The open list or popover, if its button is on the bar.
pub fn popup(bar: &Panel, open: Popup) -> Option<Panel> {
    let anchor = bar.items.iter().find(|(b, _)| *b == open.anchor())?.1;
    let s = bar.scale;
    let mut items = Vec::new();
    let mut lines = Vec::new();
    let (w, h) = match open {
        Popup::Modes | Popup::More => {
            let rows: &[Option<Button>] = if open == Popup::Modes {
                &[
                    Some(Button::Mode(CaptureMode::Screenshot)),
                    Some(Button::Mode(CaptureMode::RecordVideo)),
                    Some(Button::Mode(CaptureMode::ScrollCapture)),
                    Some(Button::Mode(CaptureMode::TextExtract)),
                ]
            } else {
                &[
                    Some(Button::Editor),
                    Some(Button::Record),
                    Some(Button::Scroll),
                    Some(Button::ReadText),
                    None,
                    Some(Button::Timer),
                    Some(Button::Pointer),
                    Some(Button::Target),
                ]
            };
            let w = if open == Popup::Modes { 168.0 } else { 244.0 };
            let mut y = 6.0;
            for row in rows {
                match row {
                    Some(b) => {
                        let r = RectI::new(px(6.0, s), px(y, s), px(w - 6.0, s), px(y + 30.0, s));
                        items.push((*b, r));
                        y += 30.0;
                    }
                    None => {
                        let at = px(y + 4.0, s);
                        lines.push(RectI::new(px(12.0, s), at, px(w - 12.0, s), at + 1));
                        y += 9.0;
                    }
                }
            }
            (px(w, s), px(y + 6.0, s))
        }
        Popup::Style(tool) => {
            let colors = marks::colors(tool).len();
            let width = 12.0 + colors as f32 * 28.0;
            let width = width.max(12.0 + 4.0 * BUTTON + DIVIDER);
            for k in 0..colors {
                let left = 6.0 + k as f32 * 28.0;
                let r = RectI::new(px(left, s), px(6.0, s), px(left + 28.0, s), px(34.0, s));
                items.push((Button::Color(k), r));
            }
            let row =
                |left: f32| RectI::new(px(left, s), px(38.0, s), px(left + BUTTON, s), px(70.0, s));
            for k in 0..3 {
                items.push((Button::Size(k), row(6.0 + k as f32 * BUTTON)));
            }
            let at = px(6.0 + 3.0 * BUTTON + 4.0, s);
            lines.push(RectI::new(at, px(45.0, s), at + 1, px(63.0, s)));
            items.push((Button::PutDown, row(width - 6.0 - BUTTON)));
            (px(width, s), px(76.0, s))
        }
    };
    let rect = beside(bar, anchor, w, h);
    let moved = |r: RectI| {
        RectI::new(
            r.left + rect.left,
            r.top + rect.top,
            r.right + rect.left,
            r.bottom + rect.top,
        )
    };
    Some(Panel {
        rect,
        items: items.into_iter().map(|(b, r)| (b, moved(r))).collect(),
        lines: lines.into_iter().map(moved).collect(),
        scale: s,
        monitor: bar.monitor,
        down: bar.down,
    })
}

// ---------------------------------------------------------------- drawing

fn mode_look(mode: CaptureMode) -> (Icon, &'static str, &'static str) {
    match mode {
        CaptureMode::Screenshot => (Icon::Shot, "Shot", "S"),
        CaptureMode::RecordVideo => (Icon::Record, "Record", "R"),
        CaptureMode::ScrollCapture => (Icon::Scroll, "Scroll", "L"),
        CaptureMode::TextExtract => (Icon::ReadText, "Text", "T"),
    }
}

/// The icon, name and key of a button. `Enter` goes to what the mode confirms.
fn look(button: Button, state: &SelectionState) -> (Icon, String, &'static str) {
    let enter = |mode| {
        if state.capture_mode == mode {
            "Enter"
        } else {
            ""
        }
    };
    let (icon, name, key) = match button {
        Button::Modes => {
            let (icon, name, key) = mode_look(state.capture_mode);
            return (icon, format!("Mode: {name}"), key);
        }
        Button::Mode(mode) => mode_look(mode),
        Button::Tool(Tool::Pen) => (Icon::Pen, "Pen", ""),
        Button::Tool(Tool::Marker) => (Icon::Marker, "Marker", ""),
        Button::Tool(Tool::Arrow) => (Icon::Arrow, "Arrow", ""),
        Button::Tool(Tool::Box) => (Icon::Box, "Box", ""),
        Button::Tool(Tool::Text) => (Icon::Text, "Text", ""),
        Button::Tool(Tool::Hide) => (Icon::Hide, "Hide", ""),
        Button::Undo => (Icon::Undo, "Undo", "Ctrl+Z"),
        Button::Redo => (Icon::Redo, "Redo", "Ctrl+Y"),
        Button::Copy => (Icon::Copy, "Copy", "Ctrl+C"),
        Button::Save => (Icon::Save, "Save", "Ctrl+S"),
        Button::More => (Icon::More, "More", ""),
        Button::Close => (Icon::Close, "Close", "Esc"),
        Button::Editor => (Icon::Editor, "Editor", enter(CaptureMode::Screenshot)),
        Button::Record => (Icon::Record, "Record", enter(CaptureMode::RecordVideo)),
        Button::Scroll => (Icon::Scroll, "Scroll", enter(CaptureMode::ScrollCapture)),
        Button::ReadText => (Icon::ReadText, "Read text", enter(CaptureMode::TextExtract)),
        Button::Timer => {
            let wait = match state.delay_timer {
                DelayTimer::Off => "Timer: off",
                DelayTimer::Sec3 => "Timer: 3 s",
                DelayTimer::Sec5 => "Timer: 5 s",
            };
            (Icon::Timer, wait, "")
        }
        Button::Pointer => {
            let shown = if state.include_cursor {
                "Mouse pointer: on"
            } else {
                "Mouse pointer: off"
            };
            (Icon::Pointer, shown, "")
        }
        Button::Target => {
            let to = match state.destination {
                Destination::Clipboard => "Copy to: Clipboard",
                Destination::File => "Copy to: File",
                Destination::Both => "Copy to: Both",
            };
            (Icon::Target, to, "")
        }
        Button::PutDown => (Icon::PutDown, "Put tool down", ""),
        Button::Color(_) => (Icon::Box, "Colour", ""),
        Button::Size(0) => (Icon::Box, "Thin", ""),
        Button::Size(1) => (Icon::Box, "Medium", ""),
        Button::Size(_) => (Icon::Box, "Thick", ""),
    };
    (icon, name.to_owned(), key)
}

fn font(s: f32) -> u32 {
    (12.0 * s).round().max(11.0) as u32
}

/// Text, vertically centered in `r`, starting at `x`.
fn text(img: &mut ImageBuffer, x: i32, r: RectI, words: &str, color: Color, s: f32) {
    let (_, h) = measure_system_text(words, font(s), false);
    let at = PointI::new(x, r.top + (r.height() - h) / 2);
    draw_system_text_rgba(img, at, words, color, font(s), false);
}

fn background(img: &mut ImageBuffer, panel: &Panel, radius: f32, skin: &Skin) {
    // A soft shadow lifts it off a light picture.
    let r = panel.rect;
    for spread in 1..=3 {
        let grown = RectI::new(
            r.left - spread,
            r.top - spread + 2,
            r.right + spread,
            r.bottom + spread + 2,
        );
        let shade = Color::rgba(0, 0, 0, 16);
        skin::panel(img, grown, radius + spread as f32, shade, None);
    }
    skin::panel(img, r, radius, skin.panel, Some(skin.border));
    for line in &panel.lines {
        skin::panel(img, *line, 0.0, skin.border, None);
    }
}

fn center(r: RectI) -> (f32, f32) {
    (
        (r.left + r.right) as f32 / 2.0,
        (r.top + r.bottom) as f32 / 2.0,
    )
}

pub fn draw_bar(
    img: &mut ImageBuffer,
    bar: &Panel,
    state: &SelectionState,
    held: &Marks,
    open: Option<Popup>,
    skin: &Skin,
) {
    let s = bar.scale;
    let size = 20.0 * s;
    background(img, bar, bar.rect.height() as f32 / 2.0, skin);
    let hovered = bar.at(state.current_cursor);
    for &(button, r) in &bar.items {
        let active = matches!(button, Button::Tool(t) if held.tool == Some(t))
            || open.is_some_and(|o| o.anchor() == button);
        let dim = (button == Button::Undo && !held.model.can_undo())
            || (button == Button::Redo && !held.model.can_redo());
        let hover = hovered == Some(button) && !dim;
        let (back, ink) = if button == Button::Copy {
            let back = if hover {
                skin::mix(skin.accent, Color::WHITE, 0.18)
            } else {
                skin.accent
            };
            (Some(back), skin.accent_on)
        } else if active {
            (Some(skin.soft), skin.accent_text)
        } else if dim {
            (None, skin.dim)
        } else {
            (hover.then_some(skin.hover), skin.text)
        };
        if let Some(back) = back {
            skin::panel(img, r, r.height() as f32 / 2.0, back, None);
        }
        let (cx, cy) = center(r);
        let icon = look(button, state).0;
        if button == Button::Modes {
            skin::icon(img, icon, cx - 6.0 * s, cy, size, ink);
            skin::icon(img, Icon::Chevron, cx + 11.0 * s, cy, 12.0 * s, ink);
        } else {
            skin::icon(img, icon, cx, cy, size, ink);
        }
    }
}

pub fn draw_popup(
    img: &mut ImageBuffer,
    panel: &Panel,
    open: Popup,
    state: &SelectionState,
    held: &Marks,
    skin: &Skin,
) {
    let s = panel.scale;
    background(img, panel, 10.0 * s, skin);
    let hovered = panel.at(state.current_cursor);
    for &(button, r) in &panel.items {
        let hover = hovered == Some(button);
        let chosen = match (button, open) {
            (Button::Mode(m), _) => m == state.capture_mode,
            (Button::Size(k), Popup::Style(t)) => k == held.level(t),
            _ => false,
        };
        if chosen || hover {
            let back = if chosen { skin.soft } else { skin.hover };
            skin::panel(img, r, 7.0 * s, back, None);
        }
        let ink = if chosen { skin.accent_text } else { skin.text };
        let (cx, cy) = center(r);
        match (button, open) {
            (Button::Color(k), Popup::Style(t)) => {
                let color = marks::colors(t)[k];
                let ring = |img: &mut ImageBuffer, half: f32, fill: Color, edge: Color| {
                    let r = RectI::new(
                        (cx - half).round() as i32,
                        (cy - half).round() as i32,
                        (cx + half).round() as i32,
                        (cy + half).round() as i32,
                    );
                    skin::panel(img, r, half, fill, Some(edge));
                };
                if color == held.color(t) {
                    ring(img, 12.0 * s, skin.panel, skin.accent);
                }
                ring(img, 8.0 * s, color, skin.dim);
            }
            (Button::Size(k), _) => {
                let half = [2.0, 4.0, 6.5][k.min(2)] * s;
                let r = RectI::new(
                    (cx - half).round() as i32,
                    (cy - half).round() as i32,
                    (cx + half).round() as i32,
                    (cy + half).round() as i32,
                );
                skin::panel(img, r, half, ink, None);
            }
            (Button::PutDown, _) => skin::icon(img, Icon::PutDown, cx, cy, 20.0 * s, ink),
            _ => {
                let (icon, name, key) = look(button, state);
                let ix = r.left as f32 + 17.0 * s;
                skin::icon(img, icon, ix, cy, 18.0 * s, ink);
                text(img, r.left + px(34.0, s), r, &name, ink, s);
                if !key.is_empty() {
                    let (w, _) = measure_system_text(key, font(s), false);
                    text(img, r.right - w - px(10.0, s), r, key, skin.muted, s);
                }
            }
        }
    }
}

/// Name and key of the button under the pointer.
pub fn draw_tooltip(img: &mut ImageBuffer, over: &Panel, state: &SelectionState, skin: &Skin) {
    let Some(&(button, anchor)) = over
        .items
        .iter()
        .find(|(_, r)| r.contains(state.current_cursor))
    else {
        return;
    };
    let s = over.scale;
    let (_, name, key) = look(button, state);
    let (nw, h) = measure_system_text(&name, font(s), false);
    let kw = if key.is_empty() {
        0
    } else {
        measure_system_text(key, font(s), false).0 + px(10.0, s)
    };
    let r = beside(over, anchor, nw + kw + px(20.0, s), h + px(10.0, s));
    let tip = Panel {
        rect: r,
        items: Vec::new(),
        lines: Vec::new(),
        scale: s,
        monitor: over.monitor,
        down: over.down,
    };
    background(img, &tip, 7.0 * s, skin);
    text(img, r.left + px(10.0, s), r, &name, skin.text, s);
    if !key.is_empty() {
        text(img, r.left + px(20.0, s) + nw, r, key, skin.muted, s);
    }
}

/// The box's size, at its top left corner; inside it when there is no room above.
pub fn draw_size_tag(img: &mut ImageBuffer, sel: RectI, monitor: Monitor, skin: &Skin) {
    let s = scale_on(monitor);
    let m = monitor.rect;
    let words = format!("{} × {}", sel.width(), sel.height());
    let (tw, th) = measure_system_text(&words, font(s), false);
    let (w, h) = (tw + px(16.0, s), th + px(6.0, s));
    let (mut x, mut y) = (sel.left, sel.top - h - px(6.0, s));
    if y < m.top + 2 {
        (x, y) = (sel.left + px(6.0, s), sel.top + px(6.0, s));
    }
    let x = x.min(m.right - w - 2).max(m.left + 2);
    let r = RectI::new(x, y, x + w, y + h);
    skin::panel(img, r, 6.0 * s, skin.panel, Some(skin.border));
    text(img, r.left + px(8.0, s), r, &words, skin.text, s);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(left: i32, width: i32, height: i32, scale: f32) -> Monitor {
        Monitor {
            rect: RectI::new(left, 0, left + width, height),
            scale,
        }
    }

    fn within(inner: RectI, outer: RectI) -> bool {
        inner.left >= outer.left
            && inner.top >= outer.top
            && inner.right <= outer.right
            && inner.bottom <= outer.bottom
    }

    fn apart(a: RectI, b: RectI) -> bool {
        a.right <= b.left || b.right <= a.left || a.bottom <= b.top || b.bottom <= a.top
    }

    const MODES: [CaptureMode; 2] = [CaptureMode::Screenshot, CaptureMode::RecordVideo];

    #[test]
    fn the_bar_and_its_lists_stay_on_the_boxs_monitor_at_every_scale() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let screens = [
                monitor(0, 1920, 1080, 1.0),
                monitor(1920, 2560, 1440, scale),
            ];
            for sel in [
                RectI::new(1925, 5, 1990, 60),
                RectI::new(4400, 1380, 4480, 1440),
                RectI::new(1920, 0, 4480, 1440),
                RectI::new(1800, 900, 2900, 1430),
                RectI::new(0, 1000, 40, 1080),
            ] {
                let m = marks::monitor_for(&screens, sel, screens[0]);
                for mode in MODES {
                    for sel in [Some(sel), None] {
                        let bar = layout(sel, m, mode);
                        assert!(within(bar.rect, m.rect), "{scale} {sel:?} {bar:?}");
                        for (_, b) in &bar.items {
                            assert!(within(*b, bar.rect), "{scale} {b:?}");
                        }
                        for open in [Popup::Modes, Popup::More, Popup::Style(Tool::Pen)] {
                            if let Some(list) = popup(&bar, open) {
                                assert!(within(list.rect, m.rect), "{scale} {list:?}");
                                for (_, b) in &list.items {
                                    assert!(within(*b, list.rect), "{scale} {b:?}");
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_bar_never_covers_a_box_that_leaves_room() {
        for scale in [1.0, 1.5] {
            let m = monitor(0, 1920, 1080, scale);
            for sel in [
                RectI::new(500, 200, 900, 500),
                RectI::new(500, 700, 900, 1080),
                RectI::new(0, 0, 400, 300),
                // Full height: only the sides are free.
                RectI::new(0, 0, 900, 1080),
                RectI::new(1000, 0, 1920, 1080),
            ] {
                for mode in MODES {
                    let bar = layout(Some(sel), m, mode);
                    assert!(apart(bar.rect, sel), "{scale} {sel:?} {:?}", bar.rect);
                }
            }
        }
    }

    #[test]
    fn the_bar_sits_below_then_above_the_box() {
        let m = monitor(0, 1920, 1080, 1.0);
        let sel = RectI::new(500, 200, 900, 500);
        let bar = layout(Some(sel), m, CaptureMode::Screenshot);
        assert!(bar.rect.top >= sel.bottom);
        assert_eq!((bar.rect.width(), bar.rect.height()), (428, 40));
        let low = RectI::new(500, 700, 900, 1070);
        assert!(layout(Some(low), m, CaptureMode::Screenshot).rect.bottom <= low.top);
    }
}
