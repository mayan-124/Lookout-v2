//! The extractor on generated files: formats, limits, corrupt input.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::{docx, pptx, xlsx, zip_of};
use lookout_office_text::extract::{extract_office, Limits};

fn ok(bytes: &[u8], ext: &str) -> String {
    extract_office(bytes, ext, &Limits::default()).unwrap()
}

#[test]
fn finds_words_in_docx_xlsx_and_pptx() {
    let t = ok(
        &docx(&["The zanzibar migration & plan", "second paragraph"]),
        "docx",
    );
    assert!(t.contains("zanzibar migration & plan"), "{t}");
    assert!(t.contains("second paragraph"), "{t}");
    let t = ok(&xlsx(&["quokka budget", "Q3 totals"]), "xlsx");
    assert!(
        t.contains("quokka budget") && t.contains("Q3 totals"),
        "{t}"
    );
    let t = ok(&pptx(&["wombat roadmap", "slide two"]), "pptx");
    assert!(
        t.contains("wombat roadmap") && t.contains("slide two"),
        "{t}"
    );
}

#[test]
fn a_zip_bomb_is_rejected() {
    // 30 MB of one byte: a few KB on disk, over the 20 MB per-part limit.
    let big = vec![b' '; 30 * 1024 * 1024];
    let bomb = zip_of(&[("word/document.xml", big)]);
    assert!(
        bomb.len() < 200 * 1024,
        "the sample must be tiny: {}",
        bomb.len()
    );
    let why = extract_office(&bomb, "docx", &Limits::default()).unwrap_err();
    assert!(why.contains("limit"), "{why}");
    // Many parts that are each fine but together over the total limit.
    let part = vec![b' '; 500 * 1024];
    let parts: Vec<(String, Vec<u8>)> = (0..4)
        .map(|i| (format!("word/header{i}.xml"), part.clone()))
        .collect();
    let refs: Vec<(&str, Vec<u8>)> = parts.iter().map(|(n, d)| (n.as_str(), d.clone())).collect();
    let many = zip_of(&refs);
    let small = Limits {
        max_total: 1024 * 1024,
        max_part: 600 * 1024,
        ..Limits::default()
    };
    let why = extract_office(&many, "docx", &small).unwrap_err();
    assert!(why.contains("zip bomb") || why.contains("limit"), "{why}");
}

#[test]
fn corrupt_encrypted_and_oversized_files_are_skipped_not_crashed() {
    let good = docx(&["hello world"]);
    // Truncated, garbage, empty, an OLE header (encrypted Office files look like this).
    let cases: Vec<Vec<u8>> = vec![
        good[..good.len() / 2].to_vec(),
        vec![0xAB; 5000],
        Vec::new(),
        b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1 rest of an OLE container".to_vec(),
        zip_of(&[("word/document.xml", b"<w:document><w:p><w:t>oops".to_vec())]),
        zip_of(&[("word/document.xml", b"<a><b></a>".to_vec())]),
    ];
    for c in cases {
        // Either skipped with a reason, or some text; never a panic.
        let _ = extract_office(&c, "docx", &Limits::default());
    }
    assert!(extract_office(&good[..good.len() / 2], "docx", &Limits::default()).is_err());
    assert!(
        extract_office(&[0xD0, 0xCF, 0x11, 0xE0], "docx", &Limits::default())
            .unwrap_err()
            .contains("encrypted or corrupt")
    );
    let tiny = Limits {
        max_file: 10,
        ..Limits::default()
    };
    assert!(extract_office(&good, "docx", &tiny)
        .unwrap_err()
        .contains("50 MB"));
    assert!(extract_office(&good, "pdf", &Limits::default()).is_err());
}

#[test]
fn the_time_limit_stops_a_file() {
    let zero = Limits {
        time: Duration::ZERO,
        ..Limits::default()
    };
    let why = extract_office(&docx(&["hello"]), "docx", &zero).unwrap_err();
    assert!(why.contains("time limit"), "{why}");
}

#[test]
fn output_is_capped_at_one_megabyte() {
    let para = "lorem ipsum dolor sit amet ".repeat(100);
    let refs: Vec<&str> = (0..2000).map(|_| para.as_str()).collect();
    let t = ok(&docx(&refs), "docx");
    assert!(t.len() <= 1 << 20, "{}", t.len());
    assert!(t.len() > 1000 * 1000);
}

#[test]
fn a_text_pdf_is_found_by_a_word_inside() {
    let t = ok(
        &common::pdf(&["The zanzibar (migration) plan", "second line"]),
        "pdf",
    );
    assert!(t.contains("zanzibar (migration) plan"), "{t}");
    assert!(t.contains("second line"), "{t}");
    let t = ok(&common::pdf(&["page one wombat", "page two quokka"]), "pdf");
    assert!(t.contains("wombat") && t.contains("quokka"), "{t}");
}

#[test]
fn an_encrypted_pdf_is_skipped() {
    let why = extract_office(
        &common::pdf_encrypted("secret platypus"),
        "pdf",
        &Limits::default(),
    )
    .unwrap_err();
    assert!(why.contains("encrypted"), "{why}");
}

#[test]
fn corrupt_and_truncated_pdfs_are_skipped_without_a_crash() {
    let good = common::pdf(&["some words here"]);
    let l = Limits::default();
    for bytes in [
        good[..good.len() / 2].to_vec(),
        good[..20].to_vec(),
        b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 9 0 R >> endobj\ntrailer << /Root 1 0 R >>"
            .to_vec(),
        b"not a pdf at all".to_vec(),
        Vec::new(),
        [b"%PDF-1.7\n".as_slice(), &[0xff; 4000]].concat(),
    ] {
        // Err or Ok are both fine; the point is that this returns.
        let _ = extract_office(&bytes, "pdf", &l);
    }
    assert!(extract_office(b"not a pdf at all", "pdf", &l).is_err());
}

#[test]
fn a_scanned_pdf_has_no_text_layer() {
    let why = extract_office(&common::pdf_scan(), "pdf", &Limits::default()).unwrap_err();
    assert_eq!(why, "no text layer");
}

#[test]
fn pdf_limits_are_enforced() {
    let pages: Vec<String> = (0..6).map(|i| format!("page {i} words")).collect();
    let refs: Vec<&str> = pages.iter().map(String::as_str).collect();
    let bytes = common::pdf(&refs);
    let few = Limits {
        max_pages: 5,
        ..Limits::default()
    };
    assert!(extract_office(&bytes, "pdf", &few)
        .unwrap_err()
        .contains("pages"));
    let tiny = Limits {
        max_text: 20,
        ..Limits::default()
    };
    assert!(extract_office(&bytes, "pdf", &tiny).unwrap().len() <= 20);
    let late = Limits {
        time: Duration::ZERO,
        ..Limits::default()
    };
    assert!(extract_office(&bytes, "pdf", &late).is_err());
    let small = Limits {
        max_file: 100,
        ..Limits::default()
    };
    assert!(extract_office(&bytes, "pdf", &small).is_err());
}
