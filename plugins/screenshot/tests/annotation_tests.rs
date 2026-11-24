#![allow(clippy::unwrap_used, clippy::expect_used)]

use lookout_screenshot::annotation::{Annotation, AnnotationModel, Color};
use lookout_screenshot::geometry::{PointI, RectI};
use lookout_screenshot::image::ImageBuffer;

#[test]
fn test_annotation_stack_operations() {
    let mut model = AnnotationModel::new();
    let mut img = ImageBuffer::new(200, 200);

    // Initial state
    assert!(!model.can_undo());
    assert!(!model.can_redo());
    assert_eq!(model.next_step_number, 1);

    // Add rectangle
    model.push(Annotation::Rectangle {
        rect: RectI::new(10, 10, 50, 50),
        color: Color::AMBER,
        thickness: 2.0,
        fill: None,
    });

    // Add line
    model.push(Annotation::Line {
        start: PointI::new(5, 5),
        end: PointI::new(30, 30),
        color: Color::RED,
        thickness: 3.0,
    });

    // Add step markers 1 and 2
    model.push(Annotation::StepMarker {
        pos: PointI::new(20, 20),
        number: model.next_step_number,
        color: Color::GREEN,
        radius: 12.0,
    });
    assert_eq!(model.next_step_number, 2);

    model.push(Annotation::StepMarker {
        pos: PointI::new(40, 40),
        number: model.next_step_number,
        color: Color::GREEN,
        radius: 12.0,
    });
    assert_eq!(model.next_step_number, 3);
    assert_eq!(model.annotations.len(), 4);

    // Undo step 2
    model.undo(&mut img);
    assert_eq!(model.next_step_number, 2);
    assert_eq!(model.annotations.len(), 3);
    assert!(model.can_redo());

    // Undo step 1
    model.undo(&mut img);
    assert_eq!(model.next_step_number, 1);
    assert_eq!(model.annotations.len(), 2);

    // Redo step 1
    model.redo(&mut img);
    assert_eq!(model.next_step_number, 2);
    assert_eq!(model.annotations.len(), 3);

    // Pushing a new annotation must clear the redo stack
    model.push(Annotation::Arrow {
        start: PointI::new(1, 1),
        end: PointI::new(10, 10),
        color: Color::BLUE,
        thickness: 2.0,
    });
    assert!(!model.can_redo());
    assert_eq!(model.annotations.len(), 4);
}

#[test]
fn test_baked_black_text_on_white_image_has_no_color_fringes() {
    let w = 240u32;
    let h = 100u32;
    let mut img = ImageBuffer::new(w, h);
    // Fill with pure opaque white (r=255, g=255, b=255, a=255)
    for y in 0..h {
        for x in 0..w {
            img.set_pixel(x, y, [255, 255, 255, 255]);
        }
    }

    let mut model = AnnotationModel::new();
    model.push(Annotation::Text {
        pos: PointI::new(20, 25),
        text: "Grayscale AA Test 123!".to_string(),
        color: Color::rgb(0, 0, 0),
        size: 20,
        bold: false,
    });
    model.render_onto(&mut img);

    let mut text_pixels_found = 0;
    for y in 0..h {
        for x in 0..w {
            let p = img.get_pixel(x, y).expect("pixel within bounds");
            // Every pixel in the image must satisfy r == g == b (no color fringes!)
            assert_eq!(
                p[0], p[1],
                "Color fringe detected at ({x}, {y}): R({}) != G({})",
                p[0], p[1]
            );
            assert_eq!(
                p[1], p[2],
                "Color fringe detected at ({x}, {y}): G({}) != B({})",
                p[1], p[2]
            );
            if p[0] < 255 {
                text_pixels_found += 1;
            }
        }
    }

    // Verify text was actually rendered onto the image
    assert!(text_pixels_found > 0, "Text must produce non-white pixels");
}

#[test]
fn test_highlighter_semi_transparent_multiply_blend() {
    let w = 100u32;
    let h = 100u32;
    let mut img = ImageBuffer::new(w, h);
    // Fill with white (255, 255, 255)
    for y in 0..h {
        for x in 0..w {
            img.set_pixel(x, y, [255, 255, 255, 255]);
        }
    }

    let orig_pixel = [255u8, 255u8, 255u8, 255u8];
    let highlight_color = Color::AMBER; // (245, 165, 36)
    let opaque_pixel = [highlight_color.r, highlight_color.g, highlight_color.b, 255];

    let mut model = AnnotationModel::new();
    model.push(Annotation::Highlighter {
        points: vec![PointI::new(20, 50), PointI::new(80, 50)],
        color: highlight_color,
        thickness: 20.0,
    });
    model.render_onto(&mut img);

    // Pick a pixel directly under the center of the highlight stroke
    let p = img.get_pixel(50, 50).expect("pixel inside stroke");

    // 1. Pixel must differ from original white color
    assert_ne!(
        p, orig_pixel,
        "Pixel under highlight stroke must differ from original color"
    );

    // 2. Pixel must differ from opaque highlight color (proves semi-transparency)
    assert_ne!(
        p, opaque_pixel,
        "Pixel under highlight stroke must differ from opaque color (semi-transparent)"
    );

    // 3. Verify multiply-style blend on black text: black stays black (crisp and readable)
    let mut text_img = ImageBuffer::new(50, 50);
    // Fill text_img with black
    for y in 0..50 {
        for x in 0..50 {
            text_img.set_pixel(x, y, [0, 0, 0, 255]);
        }
    }
    let mut model_black = AnnotationModel::new();
    model_black.push(Annotation::Highlighter {
        points: vec![PointI::new(10, 25), PointI::new(40, 25)],
        color: highlight_color,
        thickness: 20.0,
    });
    model_black.render_onto(&mut text_img);
    let p_black = text_img.get_pixel(25, 25).expect("pixel inside stroke");
    assert_eq!(
        p_black,
        [0, 0, 0, 255],
        "Black text under multiply highlighter must remain crisp black"
    );
}
