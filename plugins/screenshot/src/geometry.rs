//! Geometry primitives for multi-monitor, mixed-DPI coordinate handling and selection math.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn distance_to(self, other: Point) -> f32 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PointI {
    pub x: i32,
    pub y: i32,
}

impl PointI {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RectI {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl RectI {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn from_points(p1: PointI, p2: PointI) -> Self {
        Self {
            left: p1.x.min(p2.x),
            top: p1.y.min(p2.y),
            right: p1.x.max(p2.x),
            bottom: p1.y.max(p2.y),
        }
    }

    pub fn width(&self) -> i32 {
        (self.right - self.left).max(0)
    }

    pub fn height(&self) -> i32 {
        (self.bottom - self.top).max(0)
    }

    pub fn center_x(&self) -> i32 {
        (self.left + self.right) / 2
    }

    pub fn center_y(&self) -> i32 {
        (self.top + self.bottom) / 2
    }

    pub fn is_empty(&self) -> bool {
        self.width() == 0 || self.height() == 0
    }

    pub fn contains(&self, p: PointI) -> bool {
        p.x >= self.left && p.x < self.right && p.y >= self.top && p.y < self.bottom
    }

    pub fn offset(&self, dx: i32, dy: i32) -> Self {
        Self {
            left: self.left + dx,
            top: self.top + dy,
            right: self.right + dx,
            bottom: self.bottom + dy,
        }
    }

    pub fn clamp_to(&self, bounds: RectI) -> Self {
        Self {
            left: self.left.clamp(bounds.left, bounds.right),
            top: self.top.clamp(bounds.top, bounds.bottom),
            right: self.right.clamp(bounds.left, bounds.right),
            bottom: self.bottom.clamp(bounds.top, bounds.bottom),
        }
    }

    pub fn intersect(&self, other: RectI) -> Option<RectI> {
        let left = self.left.max(other.left);
        let top = self.top.max(other.top);
        let right = self.right.min(other.right);
        let bottom = self.bottom.min(other.bottom);
        if right > left && bottom > top {
            Some(RectI::new(left, top, right, bottom))
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectF {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl RectF {
    pub const fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn from_points(p1: Point, p2: Point) -> Self {
        Self {
            left: p1.x.min(p2.x),
            top: p1.y.min(p2.y),
            right: p1.x.max(p2.x),
            bottom: p1.y.max(p2.y),
        }
    }

    pub fn width(&self) -> f32 {
        (self.right - self.left).max(0.0)
    }

    pub fn height(&self) -> f32 {
        (self.bottom - self.top).max(0.0)
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.left && p.x <= self.right && p.y >= self.top && p.y <= self.bottom
    }

    pub fn to_rect_i(&self) -> RectI {
        RectI {
            left: self.left.round() as i32,
            top: self.top.round() as i32,
            right: self.right.round() as i32,
            bottom: self.bottom.round() as i32,
        }
    }
}

/// Translates a local overlay rectangle to screen coordinates given the virtual screen origin.
/// The virtual screen origin can be negative if secondary monitors are to the left or top.
pub fn overlay_to_screen(local: RectI, origin: PointI) -> RectI {
    local.offset(origin.x, origin.y)
}

/// Translates a screen coordinate to overlay local coordinates.
pub fn screen_to_overlay(screen: RectI, origin: PointI) -> RectI {
    screen.offset(-origin.x, -origin.y)
}

/// Computes the bounding box of multiple monitor rectangles.
pub fn virtual_desktop_bounds(monitors: &[RectI]) -> RectI {
    if monitors.is_empty() {
        return RectI::default();
    }
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;

    for m in monitors {
        min_x = min_x.min(m.left);
        min_y = min_y.min(m.top);
        max_x = max_x.max(m.right);
        max_y = max_y.max(m.bottom);
    }

    RectI::new(min_x, min_y, max_x, max_y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rect_normalization_and_dimensions() {
        let p1 = PointI::new(300, 400);
        let p2 = PointI::new(100, 200);
        let r = RectI::from_points(p1, p2);
        assert_eq!(r.left, 100);
        assert_eq!(r.top, 200);
        assert_eq!(r.right, 300);
        assert_eq!(r.bottom, 400);
        assert_eq!(r.width(), 200);
        assert_eq!(r.height(), 200);
    }

    #[test]
    fn test_multi_monitor_negative_coordinates() {
        // Monitor 1 (left): -1920, 0 to 0, 1080
        // Monitor 2 (primary): 0, 0 to 1920, 1080
        let m1 = RectI::new(-1920, 0, 0, 1080);
        let m2 = RectI::new(0, 0, 1920, 1080);
        let bounds = virtual_desktop_bounds(&[m1, m2]);
        assert_eq!(bounds.left, -1920);
        assert_eq!(bounds.top, 0);
        assert_eq!(bounds.right, 1920);
        assert_eq!(bounds.bottom, 1080);
        assert_eq!(bounds.width(), 3840);
        assert_eq!(bounds.height(), 1080);

        let origin = PointI::new(bounds.left, bounds.top);
        // Local selection on left monitor from (100, 100) to (500, 500)
        let local_sel = RectI::new(100, 100, 500, 500);
        let screen_sel = overlay_to_screen(local_sel, origin);
        assert_eq!(screen_sel.left, -1820);
        assert_eq!(screen_sel.top, 100);
        assert_eq!(screen_sel.right, -1420);
        assert_eq!(screen_sel.bottom, 500);

        let back_to_local = screen_to_overlay(screen_sel, origin);
        assert_eq!(back_to_local, local_sel);
    }

    #[test]
    fn test_clamping() {
        let bounds = RectI::new(0, 0, 1000, 800);
        let sel = RectI::new(-50, 100, 1200, 900);
        let clamped = sel.clamp_to(bounds);
        assert_eq!(clamped.left, 0);
        assert_eq!(clamped.top, 100);
        assert_eq!(clamped.right, 1000);
        assert_eq!(clamped.bottom, 800);
    }
}
