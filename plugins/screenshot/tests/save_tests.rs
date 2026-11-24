#![allow(clippy::unwrap_used, clippy::expect_used)]

use lookout_screenshot::image::ImageBuffer;
use lookout_screenshot::save::{next_available_path, resolve_save_folder, save_screenshot};
use std::fs;

#[test]
fn test_resolve_save_folder() {
    let custom = resolve_save_folder("C:\\MyScreenshots");
    assert_eq!(custom, std::path::PathBuf::from("C:\\MyScreenshots"));

    let default_folder = resolve_save_folder("");
    assert!(default_folder
        .to_string_lossy()
        .contains("Lookout Screenshots"));
}

#[test]
fn test_png_collision_free_suffixes() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let ts = "2026-11-21 123456";

    let mut img = ImageBuffer::new(10, 10);
    img.set_pixel(0, 0, [255, 255, 255, 255]);

    let f1 = dir.join(format!("Screenshot {ts}.png"));
    fs::write(&f1, b"existing1").unwrap();

    let f2 = dir.join(format!("Screenshot {ts} (1).png"));
    fs::write(&f2, b"existing2").unwrap();

    let next = next_available_path(dir, ts);
    assert_eq!(
        next.file_name().unwrap().to_string_lossy(),
        format!("Screenshot {ts} (2).png")
    );
}

#[test]
fn test_save_screenshot_creates_dir_and_saves() {
    let temp = tempfile::tempdir().unwrap();
    let nested = temp.path().join("sub").join("folder");

    let img = ImageBuffer::new(20, 20);
    let saved_path = save_screenshot(&img, &nested).unwrap();

    assert!(saved_path.exists());
    assert!(saved_path.to_string_lossy().ends_with(".png"));
    assert!(fs::metadata(&saved_path).unwrap().len() > 0);
}

#[test]
fn test_saved_image_contains_only_clean_pixels_no_overlay_border() {
    use lookout_screenshot::geometry::RectI;

    let w = 200u32;
    let h = 200u32;
    let mut desktop = ImageBuffer::new(w, h);
    for y in 0..h {
        for x in 0..w {
            desktop.set_pixel(x, y, [50, 100, 150, 255]);
        }
    }

    let sel = RectI::new(20, 20, 120, 120);
    let cropped = desktop.crop(sel).expect("crop should succeed");
    assert_eq!(cropped.width, 100);
    assert_eq!(cropped.height, 100);

    for y in 0..cropped.height {
        for x in 0..cropped.width {
            let px = cropped.get_pixel(x, y).unwrap();
            assert_eq!(
                px,
                [50, 100, 150, 255],
                "Pixel at ({x}, {y}) was altered by overlay/border!"
            );
        }
    }
}
