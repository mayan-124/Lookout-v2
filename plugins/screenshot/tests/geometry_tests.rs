#![allow(clippy::unwrap_used, clippy::expect_used)]

use lookout_screenshot::geometry::{
    overlay_to_screen, screen_to_overlay, virtual_desktop_bounds, PointI, RectI,
};

#[test]
fn test_complex_multi_monitor_layout() {
    // 3 monitors:
    // Left portrait monitor: -1200, -200 to 0, 1720 (1200x1920)
    // Center primary monitor: 0, 0 to 2560, 1440 (2560x1440)
    // Right 4K monitor: 2560, -500 to 6400, 1660 (3840x2160)
    let left = RectI::new(-1200, -200, 0, 1720);
    let center = RectI::new(0, 0, 2560, 1440);
    let right = RectI::new(2560, -500, 6400, 1660);

    let virtual_bounds = virtual_desktop_bounds(&[left, center, right]);
    assert_eq!(virtual_bounds.left, -1200);
    assert_eq!(virtual_bounds.top, -500);
    assert_eq!(virtual_bounds.right, 6400);
    assert_eq!(virtual_bounds.bottom, 1720);
    assert_eq!(virtual_bounds.width(), 7600);
    assert_eq!(virtual_bounds.height(), 2220);

    let origin = PointI::new(virtual_bounds.left, virtual_bounds.top);

    // Selection spanning across left and center monitors
    let local_rect = RectI::new(1000, 400, 1600, 900);
    let screen_rect = overlay_to_screen(local_rect, origin);
    assert_eq!(screen_rect.left, -200);
    assert_eq!(screen_rect.top, -100);
    assert_eq!(screen_rect.right, 400);
    assert_eq!(screen_rect.bottom, 400);

    let round_trip = screen_to_overlay(screen_rect, origin);
    assert_eq!(round_trip, local_rect);
}

#[test]
fn test_mixed_dpi_scaling_bounds() {
    // Monitor at 100% scale (1920x1080)
    let m100 = RectI::new(0, 0, 1920, 1080);
    // Monitor at 150% scale (3840x2160 physical pixels) positioned to the right
    let m150 = RectI::new(1920, 0, 5760, 2160);

    let bounds = virtual_desktop_bounds(&[m100, m150]);
    assert_eq!(bounds.left, 0);
    assert_eq!(bounds.top, 0);
    assert_eq!(bounds.width(), 5760);
    assert_eq!(bounds.height(), 2160);
}
