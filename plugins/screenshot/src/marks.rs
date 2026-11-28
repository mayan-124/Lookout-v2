//! Drawing on the capture overlay: the marks made with the bar's tools. The marks are the editor's own annotations, kept in the frozen
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

const PEN_COLORS: [Color; 7] = [
    Color::RED,
    Color::AMBER,
    Color::GREEN,
    Color::BLUE,
    Color::PURPLE,
    Color::WHITE,
    Color::BLACK,
];

const MARKER_COLORS: [Color; 5] = [
    Color::rgb(250, 204, 21),
    Color::rgb(74, 222, 128),
    Color::rgb(56, 189, 248),
    Color::rgb(244, 114, 182),
    Color::rgb(251, 146, 60),
];

/// The colours a tool can draw in, the first being its own; none for `Hide`.
pub fn colors(tool: Tool) -> &'static [Color] {
    match tool {
        Tool::Marker => &MARKER_COLORS,
        Tool::Hide => &[],
        _ => &PEN_COLORS,
    }
}

/// Thin, medium and thick at 100%: a line's width, or the height of text.
fn widths(tool: Tool) -> [f32; 3] {
    match tool {
        Tool::Marker => [12.0, 18.0, 28.0],
        Tool::Text => [14.0, 20.0, 30.0],
        _ => [2.0, 3.0, 6.0],
    }
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
    color: Color,
    /// The chosen width, scaled.
    width: f32,
}

impl Stroke {
    /// The annotation as drawn so far; `None` while it is too small to keep.
    fn annotation(&self) -> Option<Annotation> {
        let (first, last) = (*self.points.first()?, *self.points.last()?);
        let (color, thickness) = (self.color, self.width);
        let moved = (first.x - last.x).abs() >= 3 || (first.y - last.y).abs() >= 3;
        let rect = RectI::from_points(first, last);
        let solid = rect.width() >= 3 && rect.height() >= 3;
        match self.tool {
            Tool::Pen => (self.points.len() > 1).then(|| Annotation::Pen {
                points: self.points.clone(),
                color,
                thickness,
            }),
            Tool::Marker => (self.points.len() > 1).then(|| Annotation::Highlighter {
                points: self.points.clone(),
                color,
                thickness,
            }),
            Tool::Arrow => moved.then_some(Annotation::Arrow {
                start: first,
                end: last,
                color,
                thickness,
            }),
            Tool::Box => solid.then_some(Annotation::Rectangle {
                rect,
                color,
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
    color: Color,
}

impl Typing {
    fn annotation(&self, caret: &str) -> Annotation {
        Annotation::Text {
            pos: self.at,
            text: format!("{}{caret}", self.text),
            color: self.color,
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
    /// Chosen colour and thickness of each tool; none means the tool's own.
    chosen_color: [Option<usize>; 6],
    chosen_level: [Option<usize>; 6],
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

    pub fn color(&self, tool: Tool) -> Color {
        let chosen = self.chosen_color[tool as usize].unwrap_or(0);
        colors(tool).get(chosen).copied().unwrap_or(Color::RED)
    }

    /// Thin 0, medium 1, thick 2.
    pub fn level(&self, tool: Tool) -> usize {
        self.chosen_level[tool as usize].unwrap_or(1)
    }

    pub fn set_color(&mut self, tool: Tool, index: usize) {
        self.chosen_color[tool as usize] = Some(index);
    }

    pub fn set_level(&mut self, tool: Tool, level: usize) {
        self.chosen_level[tool as usize] = Some(level.min(2));
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
        let (color, width) = (self.color(tool), widths(tool)[self.level(tool)] * scale);
        if tool == Tool::Text {
            self.typing = Some(Typing {
                at: p,
                text: String::new(),
                size: width.round() as u32,
                color,
            });
        } else {
            self.stroke = Some(Stroke {
                tool,
                points: vec![p],
                scale,
                color,
                width,
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
