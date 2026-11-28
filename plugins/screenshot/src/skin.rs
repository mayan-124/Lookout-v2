//! The look of the capture overlay: the window's theme colors, smooth shapes and
//! the 20 px line icons.

use std::sync::Mutex;

use crate::annotation::Color;
use crate::geometry::RectI;
use crate::image::ImageBuffer;

/// Id, panel, text, muted and accent of each theme, as in `ui/src/theme.ts`.
const THEMES: [(&str, u32, u32, u32, u32); 6] = [
    ("carbon", 0x1A1A1D, 0xF2F2F3, 0xA0A0A8, 0xFF6363),
    ("slate", 0x1C1F26, 0xE8EAED, 0x9AA3AE, 0x4C8DFF),
    ("ink", 0x16161D, 0xEDEDF4, 0xA3A3B5, 0x8B7CF6),
    ("mint", 0x13201C, 0xE8F3EF, 0x98B3A9, 0x3DD6A3),
    ("amber", 0x1E1A14, 0xF4EEE4, 0xB3A68F, 0xF5A623),
    ("graphite", 0x1E1E1E, 0xF0F0F0, 0xA0A0A0, 0x0A84FF),
];

const ON_DARK: Color = Color::rgb(0x11, 0x12, 0x14);

#[derive(Debug, Clone, Copy)]
pub struct Skin {
    /// Bars, menus and tags: nearly opaque.
    pub panel: Color,
    pub border: Color,
    pub text: Color,
    pub muted: Color,
    /// A button that has nothing to do.
    pub dim: Color,
    pub hover: Color,
    /// Fill of the one main button, the frame and the handles.
    pub accent: Color,
    pub accent_on: Color,
    /// Behind the tool in hand, with `accent_text` on it.
    pub soft: Color,
    pub accent_text: Color,
}

/// `amount` (0 to 1) of `over` on top of `under`, in sRGB.
pub fn mix(under: Color, over: Color, amount: f32) -> Color {
    let m = |u: u8, o: u8| (f32::from(u) + (f32::from(o) - f32::from(u)) * amount).round() as u8;
    Color::rgb(m(under.r, over.r), m(under.g, over.g), m(under.b, over.b))
}

/// WCAG contrast ratio, 1 to 21.
pub fn contrast(a: Color, b: Color) -> f32 {
    let luminance = |c: Color| {
        let channel = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
    };
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn lighten_until(color: Color, ok: impl Fn(Color) -> bool) -> Color {
    (0..50)
        .map(|step| mix(color, Color::WHITE, step as f32 / 50.0))
        .find(|c| ok(*c))
        .unwrap_or(Color::WHITE)
}

fn rgb(v: u32) -> Color {
    Color::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// `#RRGGBB`; anything else is no choice.
fn parse_hex(s: &str) -> Option<Color> {
    let hex = s.strip_prefix('#').filter(|h| h.len() == 6)?;
    u32::from_str_radix(hex, 16).ok().map(rgb)
}

impl Skin {
    /// An unknown theme is the default one, as in the window.
    pub fn new(theme: &str, accent: Option<&str>) -> Self {
        let &(_, base, text, muted, own) =
            THEMES.iter().find(|t| t.0 == theme).unwrap_or(&THEMES[0]);
        let (base, text, muted) = (rgb(base), rgb(text), rgb(muted));
        let on = |a: Color| {
            if contrast(a, ON_DARK) >= contrast(a, Color::WHITE) {
                ON_DARK
            } else {
                Color::WHITE
            }
        };
        let chosen = accent.and_then(parse_hex).unwrap_or(rgb(own));
        let accent = lighten_until(chosen, |c| {
            contrast(c, base) >= 3.0 && contrast(c, on(c)) >= 4.5
        });
        let panel = mix(base, Color::WHITE, 0.04);
        let hover = mix(panel, text, 0.13);
        let soft = mix(panel, accent, 0.18);
        let readable = |c: Color| {
            lighten_until(c, |c| {
                [panel, hover, soft]
                    .iter()
                    .all(|under| contrast(c, *under) >= 4.5)
            })
        };
        Self {
            panel: Color { a: 245, ..panel },
            border: mix(panel, text, 0.16),
            text: readable(text),
            muted: readable(muted),
            dim: mix(panel, text, 0.32),
            hover,
            accent,
            accent_on: on(accent),
            soft,
            accent_text: readable(accent),
        }
    }
}

static CHOSEN: Mutex<Option<(String, Option<String>)>> = Mutex::new(None);

/// Remembers the window's theme, for overlays opened from now on.
pub fn choose(appearance: &serde_json::Value) {
    if let Some(theme) = appearance["theme"].as_str() {
        let accent = appearance["accent"].as_str().map(str::to_owned);
        *CHOSEN.lock().unwrap_or_else(|e| e.into_inner()) = Some((theme.to_owned(), accent));
    }
}

pub fn current() -> Skin {
    match &*CHOSEN.lock().unwrap_or_else(|e| e.into_inner()) {
        Some((theme, accent)) => Skin::new(theme, accent.as_deref()),
        None => Skin::new(THEMES[0].0, None),
    }
}

// ---------------------------------------------------------------- shapes

/// Lays `color` over one pixel; `cover` is how much of the pixel it fills.
pub fn blend(img: &mut ImageBuffer, x: i32, y: i32, color: Color, cover: f32) {
    if x < 0 || y < 0 || x >= img.width as i32 || y >= img.height as i32 {
        return;
    }
    let a = (cover.clamp(0.0, 1.0) * f32::from(color.a)).round() as u32;
    if a == 0 {
        return;
    }
    let at = (y as usize * img.width as usize + x as usize) * 4;
    for (k, c) in [color.r, color.g, color.b].into_iter().enumerate() {
        let under = u32::from(img.pixels[at + k]);
        img.pixels[at + k] = ((under * (255 - a) + u32::from(c) * a + 127) / 255) as u8;
    }
}

/// Distance from a point to a rounded rectangle's edge; negative inside.
fn rounded(px: f32, py: f32, (l, t, r, b): (f32, f32, f32, f32), radius: f32) -> f32 {
    let radius = radius.min((r - l) / 2.0).min((b - t) / 2.0).max(0.0);
    let dx = (px - (l + r) / 2.0).abs() - ((r - l) / 2.0 - radius);
    let dy = (py - (t + b) / 2.0).abs() - ((b - t) / 2.0 - radius);
    dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - radius
}

/// A smooth rounded rectangle, filled, with a one pixel `edge` if given.
pub fn panel(img: &mut ImageBuffer, r: RectI, radius: f32, fill: Color, edge: Option<Color>) {
    let shape = (r.left as f32, r.top as f32, r.right as f32, r.bottom as f32);
    for y in r.top..r.bottom {
        for x in r.left..r.right {
            let d = rounded(x as f32 + 0.5, y as f32 + 0.5, shape, radius);
            blend(img, x, y, fill, 0.5 - d);
            if let Some(edge) = edge {
                blend(img, x, y, edge, 1.0 - (d + 0.5).abs());
            }
        }
    }
}

// ---------------------------------------------------------------- icons

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Pen,
    Marker,
    Arrow,
    Box,
    Text,
    Hide,
    Undo,
    Redo,
    Copy,
    Save,
    More,
    Close,
    Shot,
    Record,
    Scroll,
    ReadText,
    Editor,
    Timer,
    Pointer,
    Target,
    Chevron,
    PutDown,
}

/// Drawn on a 20 by 20 grid.
enum Mark {
    Line(&'static [(f32, f32)]),
    Ring(f32, f32, f32),
    Dot(f32, f32, f32),
    /// Left, top, right, bottom, corner radius.
    Frame(f32, f32, f32, f32, f32),
    Block(f32, f32, f32, f32),
}

fn marks_of(icon: Icon) -> &'static [Mark] {
    use Mark::{Block, Dot, Frame, Line, Ring};
    match icon {
        Icon::Pen => &[
            Line(&[
                (3.5, 16.5),
                (4.5, 12.5),
                (13.5, 3.5),
                (16.5, 6.5),
                (7.5, 15.5),
                (3.5, 16.5),
            ]),
            Line(&[(11.5, 5.5), (14.5, 8.5)]),
        ],
        Icon::Marker => &[
            Line(&[(7.0, 10.0), (13.0, 3.5), (16.5, 7.0), (10.0, 13.0)]),
            Line(&[
                (7.0, 10.0),
                (10.0, 13.0),
                (8.0, 14.0),
                (6.0, 12.0),
                (7.0, 10.0),
            ]),
            Line(&[(3.5, 17.0), (16.5, 17.0)]),
        ],
        Icon::Arrow => &[
            Line(&[(4.5, 15.5), (15.0, 5.0)]),
            Line(&[(8.0, 4.5), (15.5, 4.5), (15.5, 12.0)]),
        ],
        Icon::Box => &[Frame(3.5, 4.5, 16.5, 15.5, 2.0)],
        Icon::Text => &[
            Line(&[(4.5, 6.5), (4.5, 4.0), (15.5, 4.0), (15.5, 6.5)]),
            Line(&[(10.0, 4.0), (10.0, 16.0)]),
            Line(&[(7.5, 16.0), (12.5, 16.0)]),
        ],
        Icon::Hide => &[
            Frame(3.0, 3.0, 17.0, 17.0, 2.0),
            Block(5.5, 5.5, 8.5, 8.5),
            Block(11.5, 5.5, 14.5, 8.5),
            Block(8.5, 8.5, 11.5, 11.5),
            Block(5.5, 11.5, 8.5, 14.5),
            Block(11.5, 11.5, 14.5, 14.5),
        ],
        Icon::Undo => &[
            Line(&[(7.5, 4.0), (4.0, 7.5), (7.5, 11.0)]),
            Line(&[
                (4.0, 7.5),
                (12.5, 7.5),
                (14.6, 8.2),
                (16.0, 10.0),
                (16.3, 12.0),
                (15.6, 14.0),
                (14.0, 15.4),
                (12.0, 16.0),
                (8.0, 16.0),
            ]),
        ],
        Icon::Redo => &[
            Line(&[(12.5, 4.0), (16.0, 7.5), (12.5, 11.0)]),
            Line(&[
                (16.0, 7.5),
                (7.5, 7.5),
                (5.4, 8.2),
                (4.0, 10.0),
                (3.7, 12.0),
                (4.4, 14.0),
                (6.0, 15.4),
                (8.0, 16.0),
                (12.0, 16.0),
            ]),
        ],
        Icon::Copy => &[
            Frame(7.0, 7.0, 16.5, 16.5, 2.0),
            Line(&[
                (4.0, 13.0),
                (3.5, 12.0),
                (3.5, 5.0),
                (5.0, 3.5),
                (12.0, 3.5),
                (13.0, 4.0),
            ]),
        ],
        Icon::Save => &[
            Line(&[(10.0, 3.0), (10.0, 12.0)]),
            Line(&[(6.0, 8.5), (10.0, 12.5), (14.0, 8.5)]),
            Line(&[(3.5, 13.0), (3.5, 16.5), (16.5, 16.5), (16.5, 13.0)]),
        ],
        Icon::More => &[
            Dot(4.5, 10.0, 1.5),
            Dot(10.0, 10.0, 1.5),
            Dot(15.5, 10.0, 1.5),
        ],
        Icon::Close => &[
            Line(&[(5.0, 5.0), (15.0, 15.0)]),
            Line(&[(15.0, 5.0), (5.0, 15.0)]),
        ],
        Icon::Shot => &[
            Frame(2.5, 5.5, 17.5, 16.5, 2.5),
            Line(&[(7.0, 5.5), (8.0, 3.5), (12.0, 3.5), (13.0, 5.5)]),
            Ring(10.0, 11.0, 3.0),
        ],
        Icon::Record => &[Ring(10.0, 10.0, 7.0), Dot(10.0, 10.0, 3.2)],
        Icon::Scroll => &[
            Frame(4.5, 2.5, 15.5, 17.5, 3.0),
            Line(&[(10.0, 6.0), (10.0, 13.5)]),
            Line(&[(7.5, 11.0), (10.0, 13.5), (12.5, 11.0)]),
        ],
        Icon::ReadText => &[
            Line(&[(3.0, 7.0), (3.0, 3.0), (7.0, 3.0)]),
            Line(&[(13.0, 3.0), (17.0, 3.0), (17.0, 7.0)]),
            Line(&[(17.0, 13.0), (17.0, 17.0), (13.0, 17.0)]),
            Line(&[(7.0, 17.0), (3.0, 17.0), (3.0, 13.0)]),
            Line(&[(7.0, 7.5), (13.0, 7.5)]),
            Line(&[(10.0, 7.5), (10.0, 13.0)]),
        ],
        Icon::Editor => &[
            Line(&[
                (8.5, 4.0),
                (4.0, 4.0),
                (4.0, 16.0),
                (16.0, 16.0),
                (16.0, 11.5),
            ]),
            Line(&[(11.5, 3.5), (16.5, 3.5), (16.5, 8.5)]),
            Line(&[(16.0, 4.0), (9.5, 10.5)]),
        ],
        Icon::Timer => &[
            Ring(10.0, 11.0, 6.0),
            Line(&[(10.0, 11.0), (10.0, 8.0)]),
            Line(&[(8.0, 2.5), (12.0, 2.5)]),
        ],
        Icon::Pointer => &[Line(&[
            (5.0, 3.0),
            (15.5, 11.0),
            (10.8, 11.8),
            (13.2, 16.2),
            (10.8, 17.4),
            (8.5, 13.0),
            (5.0, 16.0),
            (5.0, 3.0),
        ])],
        Icon::Target => &[
            Frame(4.5, 4.0, 15.5, 17.0, 2.0),
            Line(&[(7.5, 4.0), (7.5, 2.5), (12.5, 2.5), (12.5, 4.0)]),
            Line(&[(7.5, 9.0), (12.5, 9.0)]),
            Line(&[(7.5, 12.5), (10.5, 12.5)]),
        ],
        Icon::Chevron => &[Line(&[(6.0, 8.0), (10.0, 12.0), (14.0, 8.0)])],
        Icon::PutDown => &[
            Line(&[(10.0, 3.0), (10.0, 17.0)]),
            Line(&[(3.0, 10.0), (17.0, 10.0)]),
            Line(&[(8.0, 5.0), (10.0, 3.0), (12.0, 5.0)]),
            Line(&[(8.0, 15.0), (10.0, 17.0), (12.0, 15.0)]),
            Line(&[(5.0, 8.0), (3.0, 10.0), (5.0, 12.0)]),
            Line(&[(15.0, 8.0), (17.0, 10.0), (15.0, 12.0)]),
        ],
    }
}

fn to_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = dx * dx + dy * dy;
    let t = if len > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p.0 - a.0 - t * dx).hypot(p.1 - a.1 - t * dy)
}

/// Draws `icon` centered on (`cx`, `cy`), `size` pixels square.
pub fn icon(img: &mut ImageBuffer, icon: Icon, cx: f32, cy: f32, size: f32, color: Color) {
    let unit = size / 20.0;
    let half = (1.5 * unit).max(1.2) / 2.0;
    let (left, top) = ((cx - size / 2.0).round(), (cy - size / 2.0).round());
    let span = size.ceil() as i32;
    for y in -1..=span {
        for x in -1..=span {
            // The pixel's center, on the icon's own grid.
            let p = ((x as f32 + 0.5) / unit, (y as f32 + 0.5) / unit);
            let stroke = |d: f32| half + 0.5 - d * unit;
            let fill = |d: f32| 0.5 - d * unit;
            let cover = marks_of(icon).iter().fold(0.0f32, |best, mark| {
                best.max(match *mark {
                    Mark::Line(points) => points
                        .windows(2)
                        .map(|w| stroke(to_segment(p, w[0], w[1])))
                        .fold(0.0, f32::max),
                    Mark::Ring(x, y, r) => stroke(((p.0 - x).hypot(p.1 - y) - r).abs()),
                    Mark::Dot(x, y, r) => fill((p.0 - x).hypot(p.1 - y) - r),
                    Mark::Frame(l, t, r, b, radius) => {
                        stroke(rounded(p.0, p.1, (l, t, r, b), radius).abs())
                    }
                    Mark::Block(l, t, r, b) => fill(rounded(p.0, p.1, (l, t, r, b), 0.0)),
                })
            });
            blend(img, left as i32 + x, top as i32 + y, color, cover);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_reads_at_4_5_to_1_in_every_theme() {
        for (id, ..) in THEMES {
            for accent in [None, Some("#202020"), Some("#FFD43B")] {
                let s = Skin::new(id, accent);
                let panel = Color { a: 255, ..s.panel };
                for ink in [s.text, s.muted] {
                    assert!(contrast(ink, panel) >= 4.5, "{id} {ink:?}");
                    assert!(contrast(ink, s.hover) >= 4.5, "{id} {ink:?} hover");
                }
                assert!(contrast(s.accent_text, s.soft) >= 4.5, "{id} soft");
                assert!(contrast(s.accent_on, s.accent) >= 4.5, "{id} accent");
            }
        }
    }

    #[test]
    fn the_themes_are_the_windows_themes() {
        let ts = include_str!("../../../ui/src/theme.ts");
        assert_eq!(ts.matches("  theme(\"").count(), THEMES.len());
        for (id, panel, text, muted, accent) in THEMES {
            let colors = [panel, text, muted, accent].map(|c| format!("\"#{c:06X}\""));
            let tail = colors.join(", ");
            assert!(
                ts.lines()
                    .any(|l| l.contains(&format!("theme(\"{id}\"")) && l.contains(&tail)),
                "{id} differs from theme.ts"
            );
        }
    }
}
