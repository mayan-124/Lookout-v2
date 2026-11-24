use lookout_screenshot::annotation::measure_system_text;
use lookout_screenshot::editor::{
    get_button_rect_scaled, get_toolbar_rect_scaled, measure_redraw_time_3840x2160, BUTTONS,
};

#[test]
fn test_4k_redraw_under_8ms() {
    let avg_ms = measure_redraw_time_3840x2160();
    println!("Measured 3840x2160 redraw time: {avg_ms:.3} ms per frame");
    assert!(avg_ms < 8.0, "Redraw time {avg_ms} ms exceeded 8 ms budget");
}

#[test]
fn test_toolbar_layout_at_various_dpi_scales() {
    let scales = [1.0f32, 1.25, 1.5, 1.75, 2.0];
    for &scale in &scales {
        // 1. Single-row mode (wide window)
        let win_w = (1400.0 * scale).round() as i32;
        let tb = get_toolbar_rect_scaled(win_w, scale);

        let mut button_rects = Vec::new();
        for btn in BUTTONS {
            let br = get_button_rect_scaled(tb, btn.id, scale);
            // Assert button is completely inside the toolbar frame
            assert!(
                br.left >= tb.left,
                "Scale {scale}: Button {} left ({}) is outside toolbar left ({})",
                btn.id,
                br.left,
                tb.left
            );
            assert!(
                br.right <= tb.right,
                "Scale {scale}: Button {} right ({}) is outside toolbar right ({})",
                btn.id,
                br.right,
                tb.right
            );
            assert!(
                br.top >= tb.top,
                "Scale {scale}: Button {} top ({}) is outside toolbar top ({})",
                btn.id,
                br.top,
                tb.top
            );
            assert!(
                br.bottom <= tb.bottom,
                "Scale {scale}: Button {} bottom ({}) is outside toolbar bottom ({})",
                btn.id,
                br.bottom,
                tb.bottom
            );

            // Assert positive dimensions
            assert!(br.width() > 0 && br.height() > 0);
            button_rects.push((btn.id, br));
        }

        // Assert no two buttons overlap
        for i in 0..button_rects.len() {
            for j in (i + 1)..button_rects.len() {
                let (id_a, rect_a) = button_rects[i];
                let (id_b, rect_b) = button_rects[j];
                assert!(
                    rect_a.intersect(rect_b).is_none(),
                    "Scale {scale}: Buttons {id_a} and {id_b} overlap: {rect_a:?} vs {rect_b:?}"
                );
            }
        }

        // Assert Copy and Save are fully visible and wide enough for their text
        let copy_rect = get_button_rect_scaled(tb, 13, scale);
        let save_rect = get_button_rect_scaled(tb, 14, scale);
        assert!(copy_rect.intersect(save_rect).is_none());
        assert!(
            copy_rect.right <= save_rect.left,
            "Scale {scale}: Copy must precede Save without overlap"
        );

        let text_font_size = (13.0 * scale).round() as u32;
        let (copy_tw, _) = measure_system_text("Copy", text_font_size, false);
        let (save_tw, _) = measure_system_text("Save", text_font_size, true);
        assert!(
            copy_rect.width() >= copy_tw + (6.0 * scale).round() as i32,
            "Scale {scale}: Copy button width {} too small for text width {}",
            copy_rect.width(),
            copy_tw
        );
        assert!(
            save_rect.width() >= save_tw + (6.0 * scale).round() as i32,
            "Scale {scale}: Save button width {} too small for text width {}",
            save_rect.width(),
            save_tw
        );

        // 2. Multi-row mode (narrow window, e.g. 700 px at 100% scale, scaled with DPI)
        let narrow_win_w = (700.0 * scale).round() as i32;
        let tb_narrow = get_toolbar_rect_scaled(narrow_win_w, scale);
        assert!(tb_narrow.left >= 0 && tb_narrow.right <= narrow_win_w);

        let mut narrow_button_rects = Vec::new();
        for btn in BUTTONS {
            let br = get_button_rect_scaled(tb_narrow, btn.id, scale);
            assert!(
                br.left >= tb_narrow.left && br.right <= tb_narrow.right,
                "Scale {scale}, btn {}: br={:?}, tb_narrow={:?}",
                btn.id,
                br,
                tb_narrow
            );
            assert!(br.top >= tb_narrow.top && br.bottom <= tb_narrow.bottom);
            narrow_button_rects.push((btn.id, br));
        }

        for i in 0..narrow_button_rects.len() {
            for j in (i + 1)..narrow_button_rects.len() {
                let (id_a, rect_a) = narrow_button_rects[i];
                let (id_b, rect_b) = narrow_button_rects[j];
                assert!(
                    rect_a.intersect(rect_b).is_none(),
                    "Scale {scale} (multi-row): Buttons {id_a} and {id_b} overlap: {rect_a:?} vs {rect_b:?}"
                );
            }
        }
    }

    // Explicitly test narrow widths (500, 600, 700, 900 px) at scales 1.0, 1.5, 2.0
    for &test_scale in &[1.0f32, 1.5, 2.0] {
        for &narrow_w in &[500, 600, 700, 900] {
            let tb = get_toolbar_rect_scaled(narrow_w, test_scale);
            let mut btn_rects = Vec::new();
            for btn in BUTTONS {
                let br = get_button_rect_scaled(tb, btn.id, test_scale);
                // Assert no button crosses the frame edge
                assert!(
                    br.left >= tb.left,
                    "Scale {test_scale}, width {narrow_w}: Button {} left ({}) crosses frame left ({})",
                    btn.id,
                    br.left,
                    tb.left
                );
                assert!(
                    br.right <= tb.right,
                    "Scale {test_scale}, width {narrow_w}: Button {} right ({}) crosses frame right ({})",
                    btn.id,
                    br.right,
                    tb.right
                );
                assert!(
                    br.top >= tb.top,
                    "Scale {test_scale}, width {narrow_w}: Button {} top ({}) crosses frame top ({})",
                    btn.id,
                    br.top,
                    tb.top
                );
                assert!(
                    br.bottom <= tb.bottom,
                    "Scale {test_scale}, width {narrow_w}: Button {} bottom ({}) crosses frame bottom ({})",
                    btn.id,
                    br.bottom,
                    tb.bottom
                );
                btn_rects.push((btn.id, br));
            }

            // Assert no two buttons overlap
            for i in 0..btn_rects.len() {
                for j in (i + 1)..btn_rects.len() {
                    let (id_a, rect_a) = btn_rects[i];
                    let (id_b, rect_b) = btn_rects[j];
                    assert!(
                        rect_a.intersect(rect_b).is_none(),
                        "Scale {test_scale}, width {narrow_w}: Buttons {id_a} and {id_b} overlap: {rect_a:?} vs {rect_b:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn test_selection_handles_are_8_times_scale_px() {
    use lookout_screenshot::overlay::get_selection_handle_size;
    for &scale in &[1.0f32, 1.25, 1.5, 1.75, 2.0] {
        let handle_size = get_selection_handle_size(scale);
        let expected = (8.0 * scale).round() as i32;
        assert_eq!(
            handle_size, expected,
            "Handle size for scale {scale} was {handle_size}, expected {expected}"
        );
    }
}
