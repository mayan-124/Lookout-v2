//! Drawing on the capture overlay: the toolbar beside the box and the marks made
//! with it. The marks are the editor's own annotations, kept in the frozen
//! picture's coordinates, so they stay on the picture when the box is moved or
//! resized. Nothing here writes to disk.

use crate::annotation::{Annotation, AnnotationModel, Color};
use crate::geometry::{PointI, RectI};
use crate::image::ImageBuffer;

/// A monitor in overlay pixels, with its scale (1.0 = 100%).
#[derive(Debug, Clone, Copy)]
pub struct Monitor {
    pub rect: RectI,
    pub scale: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Pen,
    Marker,
    Arrow,
    Box,
    Text,
    Hide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Tool(Tool),
    Undo,
    Redo,
    Copy,
    Save,
    Editor,
    Record,
    Scroll,
    ReadText,
    Close,
}

const DRAW_ROW: [(Button, &str); 8] = [
    (Button::Tool(Tool::Pen), "Pen"),
    (Button::Tool(Tool::Marker), "Marker"),
    (Button::Tool(Tool::Arrow), "Arrow"),
    (Button::Tool(Tool::Box), "Box"),
    (Button::Tool(Tool::Text), "Text"),
    (Button::Tool(Tool::Hide), "Hide"),
    (Button::Undo, "Undo"),
    (Button::Redo, "Redo"),
];

const ACTION_ROW: [(Button, &str); 7] = [
    (Button::Copy, "Copy"),
    (Button::Save, "Save"),
    (Button::Editor, "Editor"),
    (Button::Record, "Record"),
    (Button::Scroll, "Scroll"),
    (Button::ReadText, "Read text"),
    (Button::Close, "Close"),
];

const WIDTH: f32 = 448.0;
const ROW: f32 = 32.0;
const EDGE: f32 = 4.0;

/// The monitor holding most of `sel`; the first one if none touches it.
pub fn monitor_for(monitors: &[Monitor], sel: RectI, fallback: Monitor) -> Monitor {
    let shared = |m: &Monitor| {
        let w = sel.right.min(m.rect.right) - sel.left.max(m.rect.left);
        let h = sel.bottom.min(m.rect.bottom) - sel.top.max(m.rect.top);
        i64::from(w.max(0)) * i64::from(h.max(0))
    };
    monitors
        .iter()
        .filter(|m| shared(m) > 0)
        .max_by_key(|m| shared(m))
        .or_else(|| monitors.first())
        .copied()
        .unwrap_or(fallback)
}

/// The scale the toolbar is drawn at: the monitor's, less on a very narrow one.
pub fn toolbar_scale(monitor: Monitor) -> f32 {
    monitor
        .scale
        .min(monitor.rect.width() as f32 / (WIDTH + 20.0))
}

/// Below the box; above it when there is no room below; else inside its bottom
/// edge. Always wholly on the one monitor.
pub fn toolbar_rect(sel: RectI, monitor: Monitor, draw: bool) -> RectI {
    let s = toolbar_scale(monitor);
    let px = |v: f32| (v * s).round() as i32;
    let rows = if draw { 2.0 } else { 1.0 };
    let (w, h) = (px(WIDTH), px(rows * ROW + 2.0 * EDGE));
    let (gap, margin) = (px(12.0), px(8.0));
    let m = monitor.rect;
    let x = ((sel.left + sel.right - w) / 2)
        .min(m.right - w - margin)
        .max(m.left + margin);
    let below = sel.bottom + gap;
    // The size label sits just above the box.
    let above = sel.top - px(40.0) - h;
    let y = if below + h <= m.bottom - margin && below >= m.top + margin {
        below
    } else if above >= m.top + px(60.0) && above + h <= m.bottom - margin {
        above
    } else {
        (sel.bottom - h - gap)
            .min(m.bottom - h - margin)
            .max(m.top + margin)
    };
    RectI::new(x, y, x + w, y + h)
}

/// Every button with its place, for both drawing and clicking.
pub fn buttons(toolbar: RectI, draw: bool) -> Vec<(Button, &'static str, RectI)> {
    let s = toolbar.width() as f32 / WIDTH;
    let px = |v: f32| (v * s).round() as i32;
    let rows: &[&[(Button, &str)]] = if draw {
        &[&DRAW_ROW, &ACTION_ROW]
    } else {
        &[&ACTION_ROW]
    };
    let mut all = Vec::new();
    for (n, row) in rows.iter().enumerate() {
        let each = (toolbar.width() - 2 * px(EDGE)) as f32 / row.len() as f32;
        let top = toolbar.top + px(EDGE) + px(n as f32 * ROW);
        for (k, (button, label)) in row.iter().enumerate() {
            let left = toolbar.left + px(EDGE) + (k as f32 * each).round() as i32;
            let right = toolbar.left + px(EDGE) + ((k + 1) as f32 * each).round() as i32;
            all.push((
                *button,
                *label,
                RectI::new(left + 1, top + 1, right - 1, top + px(ROW) - 1),
            ));
        }
    }
    all
}

/// The point, kept inside the box.
fn inside(p: PointI, r: RectI) -> PointI {
    PointI::new(
        p.x.clamp(r.left, (r.right - 1).max(r.left)),
        p.y.clamp(r.top, (r.bottom - 1).max(r.top)),
    )
}

struct Stroke {
    tool: Tool,
    points: Vec<PointI>,
    scale: f32,
}

impl Stroke {
    /// The annotation as drawn so far; `None` while it is too small to keep.
    fn annotation(&self) -> Option<Annotation> {
        let (first, last) = (*self.points.first()?, *self.points.last()?);
        let thickness = 3.0 * self.scale;
        let moved = (first.x - last.x).abs() >= 3 || (first.y - last.y).abs() >= 3;
        let rect = RectI::from_points(first, last);
        let solid = rect.width() >= 3 && rect.height() >= 3;
        match self.tool {
            Tool::Pen => (self.points.len() > 1).then(|| Annotation::Pen {
                points: self.points.clone(),
                color: Color::RED,
                thickness,
            }),
            Tool::Marker => (self.points.len() > 1).then(|| Annotation::Highlighter {
                points: self.points.clone(),
                color: Color::rgb(250, 204, 21),
                thickness: 18.0 * self.scale,
            }),
            Tool::Arrow => moved.then_some(Annotation::Arrow {
                start: first,
                end: last,
                color: Color::RED,
                thickness,
            }),
            Tool::Box => solid.then_some(Annotation::Rectangle {
                rect,
                color: Color::RED,
                thickness,
                fill: None,
            }),
            Tool::Hide => solid.then(|| Annotation::Blur {
                rect,
                cell_size: (14.0 * self.scale).round() as u32,
                // Marks are redrawn from the untouched picture: nothing to keep.
                saved_pixels: (rect, Vec::new()),
            }),
            Tool::Text => None,
        }
    }
}

struct Typing {
    at: PointI,
    text: String,
    size: u32,
}

impl Typing {
    fn annotation(&self, caret: &str) -> Annotation {
        Annotation::Text {
            pos: self.at,
            text: format!("{}{caret}", self.text),
            color: Color::RED,
            size: self.size,
            bold: true,
        }
    }
}

#[derive(Default)]
pub struct Marks {
    pub model: AnnotationModel,
    /// The tool in hand; none means a drag moves the box.
    pub tool: Option<Tool>,
    stroke: Option<Stroke>,
    typing: Option<Typing>,
    /// The frozen picture with the finished marks on it, while there are any.
    painted: Option<ImageBuffer>,
}

impl Marks {
    /// The picture as it is now: the frozen one, or the marked copy.
    pub fn picture<'a>(&'a self, frozen: &'a ImageBuffer) -> &'a ImageBuffer {
        self.painted.as_ref().unwrap_or(frozen)
    }

    /// What is being drawn or typed and not finished yet.
    pub fn draft(&self) -> Option<Annotation> {
        match &self.typing {
            Some(t) => Some(t.annotation("|")),
            None => self.stroke.as_ref().and_then(Stroke::annotation),
        }
    }

    pub fn drawing(&self) -> bool {
        self.stroke.is_some()
    }

    pub fn typing(&self) -> bool {
        self.typing.is_some()
    }

    fn repaint(&mut self, frozen: &ImageBuffer) {
        self.painted = (!self.model.annotations.is_empty()).then(|| {
            let mut picture = self.painted.take().unwrap_or_else(|| frozen.clone());
            picture.pixels.copy_from_slice(&frozen.pixels);
            self.model.render_onto(&mut picture);
            picture
        });
    }

    /// Picking the tool in hand puts it down.
    pub fn pick(&mut self, tool: Tool) {
        self.tool = (self.tool != Some(tool)).then_some(tool);
    }

    /// A press inside the box with a tool in hand.
    pub fn press(&mut self, p: PointI, sel: RectI, scale: f32) {
        let Some(tool) = self.tool else {
            return;
        };
        let p = inside(p, sel);
        if tool == Tool::Text {
            self.typing = Some(Typing {
                at: p,
                text: String::new(),
                size: (20.0 * scale).round() as u32,
            });
        } else {
            self.stroke = Some(Stroke {
                tool,
                points: vec![p],
                scale,
            });
        }
    }

    pub fn drag(&mut self, p: PointI, sel: RectI) {
        if let Some(stroke) = &mut self.stroke {
            let p = inside(p, sel);
            let freehand = matches!(stroke.tool, Tool::Pen | Tool::Marker);
            if !freehand {
                stroke.points.truncate(1);
            }
            if !freehand || stroke.points.last() != Some(&p) {
                stroke.points.push(p);
            }
        }
    }

    pub fn release(&mut self, frozen: &ImageBuffer) {
        if let Some(done) = self.stroke.take().and_then(|s| s.annotation()) {
            self.model.push(done);
            self.repaint(frozen);
        }
    }

    pub fn type_char(&mut self, c: char) {
        if let Some(t) = &mut self.typing {
            if c == '\u{8}' {
                t.text.pop();
            } else if !c.is_control() && t.text.chars().count() < 200 {
                t.text.push(c);
            }
        }
    }

    /// Keeps the typed text, if there is any.
    pub fn finish_text(&mut self, frozen: &ImageBuffer) {
        if let Some(t) = self.typing.take() {
            if !t.text.trim().is_empty() {
                self.model.push(t.annotation(""));
                self.repaint(frozen);
            }
        }
    }

    pub fn drop_text(&mut self) {
        self.typing = None;
    }

    pub fn undo(&mut self, frozen: &ImageBuffer) {
        // The picture is rebuilt from the frozen one, so there is nothing to restore.
        if self.model.undo(&mut ImageBuffer::new(1, 1)).is_some() {
            self.repaint(frozen);
        }
    }

    pub fn redo(&mut self, frozen: &ImageBuffer) {
        if self.model.redo(&mut ImageBuffer::new(1, 1)).is_some() {
            self.repaint(frozen);
        }
    }
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

    #[test]
    fn the_toolbar_stays_on_the_boxs_monitor_at_every_scale() {
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
                let m = monitor_for(&screens, sel, screens[0]);
                for draw in [true, false] {
                    let bar = toolbar_rect(sel, m, draw);
                    assert!(within(bar, m.rect), "{scale} {sel:?} -> {bar:?}");
                    for (_, _, b) in buttons(bar, draw) {
                        assert!(within(b, bar), "{scale} {b:?} outside {bar:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_toolbar_sits_below_then_above_the_box() {
        let m = monitor(0, 1920, 1080, 1.0);
        let sel = RectI::new(500, 200, 900, 500);
        assert!(toolbar_rect(sel, m, true).top >= sel.bottom);
        let low = RectI::new(500, 700, 900, 1070);
        assert!(toolbar_rect(low, m, true).bottom <= low.top);
    }

    #[test]
    fn marks_stay_on_the_picture_and_undo_restores_it() {
        let frozen = ImageBuffer::new(200, 200);
        let sel = RectI::new(10, 10, 150, 150);
        let mut marks = Marks::default();
        marks.pick(Tool::Arrow);
        marks.press(PointI::new(20, 20), sel, 1.0);
        // A drag past the box ends at its edge.
        marks.drag(PointI::new(500, 100), sel);
        marks.release(&frozen);
        let Annotation::Arrow { end, .. } = &marks.model.annotations[0] else {
            panic!("an arrow");
        };
        assert_eq!(*end, PointI::new(149, 100));
        assert_ne!(marks.picture(&frozen).pixels, frozen.pixels);
        marks.undo(&frozen);
        assert_eq!(marks.picture(&frozen).pixels, frozen.pixels);
        marks.redo(&frozen);
        assert_ne!(marks.picture(&frozen).pixels, frozen.pixels);
    }

    #[test]
    fn text_is_typed_then_kept_and_a_click_alone_draws_nothing() {
        let frozen = ImageBuffer::new(200, 100);
        let sel = RectI::new(0, 0, 200, 100);
        let mut marks = Marks::default();
        marks.pick(Tool::Box);
        marks.press(PointI::new(5, 5), sel, 1.0);
        marks.release(&frozen);
        assert!(marks.model.annotations.is_empty());
        marks.pick(Tool::Text);
        marks.press(PointI::new(5, 5), sel, 1.0);
        for c in ['h', 'o', '\u{8}', 'i', '\r'] {
            marks.type_char(c);
        }
        marks.finish_text(&frozen);
        assert!(matches!(
            &marks.model.annotations[0],
            Annotation::Text { text, .. } if text == "hi"
        ));
    }
}
