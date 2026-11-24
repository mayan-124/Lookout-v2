//! Small .docx/.xlsx/.pptx files built in memory. Nothing here reads user files.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

pub fn zip_of(parts: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut w = ZipWriter::new(Cursor::new(Vec::new()));
    let opt = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, data) in parts {
        w.start_file(*name, opt).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;")
}

pub fn docx(paragraphs: &[&str]) -> Vec<u8> {
    let mut x = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><w:document xmlns:w=\"urn:w\"><w:body>",
    );
    for p in paragraphs {
        // Split into two runs: words must survive a run boundary.
        let mid = p.len() / 2;
        let mid = (0..=mid).rev().find(|i| p.is_char_boundary(*i)).unwrap();
        x.push_str(&format!(
            "<w:p><w:r><w:t>{}</w:t></w:r><w:r><w:t>{}</w:t></w:r></w:p>",
            esc(&p[..mid]),
            esc(&p[mid..])
        ));
    }
    x.push_str("</w:body></w:document>");
    zip_of(&[("word/document.xml", x.into_bytes())])
}

pub fn xlsx(cells: &[&str]) -> Vec<u8> {
    let mut sst = String::from("<sst xmlns=\"urn:s\">");
    let mut sheet = String::from("<worksheet><sheetData><row>");
    for (i, c) in cells.iter().enumerate() {
        sst.push_str(&format!("<si><t>{}</t></si>", esc(c)));
        sheet.push_str(&format!("<c t=\"s\"><v>{i}</v></c>"));
    }
    sst.push_str("</sst>");
    sheet.push_str("</row></sheetData></worksheet>");
    zip_of(&[
        ("xl/sharedStrings.xml", sst.into_bytes()),
        ("xl/worksheets/sheet1.xml", sheet.into_bytes()),
    ])
}

pub fn pptx(slides: &[&str]) -> Vec<u8> {
    let parts: Vec<(String, Vec<u8>)> = slides
        .iter()
        .enumerate()
        .map(|(i, s)| {
            (
                format!("ppt/slides/slide{}.xml", i + 1),
                format!(
                    "<p:sld xmlns:p=\"urn:p\" xmlns:a=\"urn:a\"><a:p><a:r><a:t>{}</a:t></a:r></a:p></p:sld>",
                    esc(s)
                )
                .into_bytes(),
            )
        })
        .collect();
    let refs: Vec<(&str, Vec<u8>)> = parts.iter().map(|(n, d)| (n.as_str(), d.clone())).collect();
    zip_of(&refs)
}

/// A PDF from object bodies (object n is `objs[n-1]`), with a correct xref table.
fn pdf_of(objs: &[Vec<u8>], trailer_extra: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).bytes());
        out.extend(body);
        out.extend(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R {trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .bytes(),
    );
    out
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut v = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    v.extend(data);
    v.extend(b"\nendstream");
    v
}

/// One page per entry, each drawn as lines of Helvetica text.
pub fn pdf(pages: &[&str]) -> Vec<u8> {
    pdf_with(pages, "")
}

pub fn pdf_with(pages: &[&str], trailer_extra: &str) -> Vec<u8> {
    let kids: String = (0..pages.len())
        .map(|i| format!("{} 0 R ", 4 + 2 * i))
        .collect();
    let mut objs = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!("<< /Type /Pages /Kids [{kids}] /Count {} >>", pages.len()).into_bytes(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    ];
    for (i, text) in pages.iter().enumerate() {
        objs.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >>",
                5 + 2 * i
            )
            .into_bytes(),
        );
        let mut c = String::from("BT /F1 12 Tf 14 TL 50 740 Td ");
        for line in text.lines() {
            let e = line
                .replace('\\', r"\\")
                .replace('(', r"\(")
                .replace(')', r"\)");
            c.push_str(&format!("({e}) Tj T* "));
        }
        c.push_str("ET");
        objs.push(stream("", c.as_bytes()));
    }
    pdf_of(&objs, trailer_extra)
}

/// A scan: one page that is only an image, no text operators.
pub fn pdf_scan() -> Vec<u8> {
    let objs = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /XObject << /Im0 5 0 R >> >> >>".to_vec(),
        stream("", b"q 200 0 0 200 0 0 cm /Im0 Do Q"),
        stream(
            "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8",
            &[0, 255, 255, 0],
        ),
    ];
    pdf_of(&objs, "")
}

/// Marked encrypted: the trailer points at an /Encrypt dictionary.
pub fn pdf_encrypted(text: &str) -> Vec<u8> {
    pdf_with(&[text], "/Encrypt << /Filter /Standard /V 1 /R 2 /O (aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa) /U (bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb) /P -4 >>")
}
