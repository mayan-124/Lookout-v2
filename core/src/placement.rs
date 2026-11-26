//! Where the bar appears: on the monitor under the cursor, centred, near the
//! upper third of its work area, or where the user last dragged it there.
//! Positions are physical pixels of the whole desktop.

use crate::settings::position_on_monitor;

/// How far down the work area the top of the bar sits.
const TOP: f64 = 0.18;
/// The usual tallest bar, in logical pixels. Room is kept for it, so the search
/// box does not move while results make the window grow and shrink.
const ROOM: f64 = 640.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen {
    /// The whole monitor.
    pub bounds: Rect,
    /// The monitor without the taskbar.
    pub work: Rect,
    /// Physical pixels per logical pixel (1.5 at 150%).
    pub scale: f64,
}

/// The monitor containing the point, else the nearest one.
pub fn screen_at(screens: &[Screen], x: i32, y: i32) -> Option<usize> {
    let away = |s: &Screen| {
        let r = s.bounds;
        let dx = i64::from((r.x - x).max(x - (r.x + r.w - 1)).max(0));
        let dy = i64::from((r.y - y).max(y - (r.y + r.h - 1)).max(0));
        dx * dx + dy * dy
    };
    (0..screens.len()).min_by_key(|&i| away(&screens[i]))
}

/// Top-left corner for a bar of `width` x `height` logical pixels. `saved` is
/// used while it still leaves the bar grabbable inside the work area.
pub fn place(screen: &Screen, width: f64, height: f64, saved: Option<(i32, i32)>) -> (i32, i32) {
    let px = |v: f64| (v * screen.scale).round() as i32;
    let (w, work) = (px(width), screen.work);
    if let Some(p) = saved {
        if position_on_monitor(p, w, (work.x, work.y, work.w, work.h)) {
            return p;
        }
    }
    let x = work.x + ((work.w - w) / 2).max(0);
    let y = work.y + (f64::from(work.h) * TOP).round() as i32;
    let lowest = work.y + work.h - px(height.max(ROOM));
    (x, y.min(lowest).max(work.y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(x: i32, y: i32, w: i32, h: i32, taskbar: i32, scale: f64) -> Screen {
        Screen {
            bounds: Rect { x, y, w, h },
            work: Rect {
                x,
                y,
                w,
                h: h - taskbar,
            },
            scale,
        }
    }

    /// A 100% primary, a 150% monitor to its left and a 200% one below it.
    fn desk() -> [Screen; 3] {
        [
            screen(0, 0, 1920, 1080, 48, 1.0),
            screen(-2560, 0, 2560, 1440, 72, 1.5),
            screen(0, 1080, 3840, 2160, 96, 2.0),
        ]
    }

    #[test]
    fn the_cursor_picks_its_monitor() {
        let d = desk();
        assert_eq!(screen_at(&d, 5, 5), Some(0));
        assert_eq!(screen_at(&d, 1919, 1079), Some(0));
        assert_eq!(screen_at(&d, -1, 500), Some(1));
        assert_eq!(screen_at(&d, -2560, 0), Some(1));
        assert_eq!(screen_at(&d, 100, 1080), Some(2));
        // In a gap between monitors: the nearest.
        assert_eq!(screen_at(&d, 2000, 100), Some(0));
        assert_eq!(screen_at(&[], 0, 0), None);
    }

    #[test]
    fn the_default_is_centred_near_the_upper_third_at_any_scale() {
        let d = desk();
        assert_eq!(place(&d[0], 940.0, 200.0, None), (490, 186));
        // 940 logical is 1410 physical at 150% and 1880 at 200%.
        assert_eq!(
            place(&d[1], 940.0, 200.0, None),
            (-2560 + (2560 - 1410) / 2, 246)
        );
        assert_eq!(
            place(&d[2], 940.0, 200.0, None),
            ((3840 - 1880) / 2, 1080 + 372)
        );
        for s in d {
            let (x, y) = place(&s, 940.0, 640.0, None);
            assert!(x >= s.work.x && y >= s.work.y);
            assert!(y + (640.0 * s.scale) as i32 <= s.work.y + s.work.h);
        }
    }

    #[test]
    fn a_small_screen_keeps_the_bar_above_the_taskbar_and_still() {
        // 1366x768 at 100% with a taskbar: 18% down would end below the work area.
        let s = screen(0, 0, 1366, 768, 48, 1.0);
        assert_eq!(place(&s, 940.0, 150.0, None), (213, 80));
        assert_eq!(place(&s, 940.0, 640.0, None), (213, 80));
        // Taller than the room: as high as needed, never above the work area.
        assert_eq!(place(&s, 940.0, 700.0, None).1, 20);
        assert_eq!(place(&s, 940.0, 2000.0, None).1, 0);
        // A taskbar at the top moves the work area down.
        let top = Screen {
            work: Rect {
                x: 0,
                y: 48,
                w: 1920,
                h: 1032,
            },
            ..desk()[0]
        };
        assert_eq!(place(&top, 940.0, 200.0, None), (490, 48 + 186));
    }

    #[test]
    fn a_saved_position_is_reused_only_while_it_is_on_screen() {
        let d = desk();
        let usual = place(&d[0], 940.0, 200.0, None);
        assert_eq!(place(&d[0], 940.0, 200.0, Some((40, 600))), (40, 600));
        assert_eq!(place(&d[1], 940.0, 200.0, Some((-2000, 300))), (-2000, 300));
        // Another monitor's place, a gone monitor, or under the taskbar.
        for gone in [(-2000, 300), (5000, 200), (300, 1000), (300, -50)] {
            assert_eq!(
                place(&d[0], 940.0, 200.0, Some(gone)),
                usual,
                "{gone:?} falls back"
            );
        }
    }
}
