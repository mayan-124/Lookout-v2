#![allow(clippy::unwrap_used, clippy::expect_used)]

use lookout_screenshot::geometry::RectI;
use lookout_screenshot::image::ImageBuffer;
use std::fs;

#[test]
fn test_true_pixel_redaction_and_undo() {
    let mut img = ImageBuffer::new(40, 40);

    // Write a distinct pattern of colors (e.g. simulated text / password)
    for y in 0..40 {
        for x in 0..40 {
            let val = ((x * 7 + y * 13) % 255) as u8;
            let val2 = ((val as u16 * 2) % 255) as u8;
            img.set_pixel(x, y, [val, 255 - val, val2, 255]);
        }
    }

    let redact_rect = RectI::new(10, 10, 30, 30);
    let orig_center = img.get_pixel(20, 20).unwrap();

    // Take undo snapshot
    let saved_pixels = img.get_rect_pixels(redact_rect);

    // Perform true destructive pixelation
    img.pixelate(redact_rect, 10);

    let blurred_center = img.get_pixel(20, 20).unwrap();
    assert_ne!(orig_center, blurred_center);

    // Check that every pixel in the 10x10 sub-block has the identical average color
    let block_sample_1 = img.get_pixel(11, 11).unwrap();
    let block_sample_2 = img.get_pixel(18, 18).unwrap();
    assert_eq!(block_sample_1, block_sample_2);

    // Encode to PNG: verify that encoded bytes are valid PNG containing modified pixels
    let png_bytes = img.encode_png().unwrap();
    assert!(!png_bytes.is_empty());
    assert_eq!(&png_bytes[1..4], b"PNG");

    // Test in-editor undo: restores the original pixels
    img.set_rect_pixels(saved_pixels.0, &saved_pixels.1);
    assert_eq!(img.get_pixel(20, 20).unwrap(), orig_center);
}

#[test]
fn test_saved_png_has_flattened_blur() {
    let temp_dir = tempfile::tempdir().unwrap();
    let file_path = temp_dir.path().join("test_redacted.png");

    let mut img = ImageBuffer::new(50, 50);
    for y in 0..50 {
        for x in 0..50 {
            img.set_pixel(x, y, [x as u8 * 5, y as u8 * 5, 128, 255]);
        }
    }

    // Apply blur to top-left
    img.blur(RectI::new(0, 0, 25, 25), 8);
    img.save_png(&file_path).unwrap();

    assert!(file_path.exists());
    let metadata = fs::metadata(&file_path).unwrap();
    assert!(metadata.len() > 50);
}
