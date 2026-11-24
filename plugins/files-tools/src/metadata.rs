//! Removing metadata (EXIF with GPS and camera data, XMP, IPTC, comments, text
//! chunks) without decoding or re-encoding the picture: the image data is
//! copied byte for byte. Only JPEG and PNG.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Png,
}

pub fn detect(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Format::Jpeg)
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Format::Png)
    } else {
        None
    }
}

/// The cleaned file and how many bytes were dropped.
pub fn strip(bytes: &[u8]) -> Result<(Vec<u8>, usize), String> {
    let out = match detect(bytes) {
        Some(Format::Jpeg) => strip_jpeg(bytes)?,
        Some(Format::Png) => strip_png(bytes)?,
        None => return Err("only JPEG and PNG files are supported".into()),
    };
    let removed = bytes.len() - out.len();
    Ok((out, removed))
}

fn strip_jpeg(b: &[u8]) -> Result<Vec<u8>, String> {
    let bad = || "the JPEG file is damaged".to_owned();
    let mut out = Vec::with_capacity(b.len());
    out.extend_from_slice(&b[..2]);
    let mut i = 2;
    loop {
        // Find the next marker (0xFF, then a non-0xFF, non-0 byte).
        if i >= b.len() || b[i] != 0xFF {
            return Err(bad());
        }
        while i < b.len() && b[i] == 0xFF {
            i += 1;
        }
        let marker = *b.get(i).ok_or_else(bad)?;
        i += 1;
        match marker {
            0xD9 => {
                out.extend_from_slice(&[0xFF, 0xD9]);
                out.extend_from_slice(&b[i..]);
                return Ok(out);
            }
            0x01 | 0xD0..=0xD7 | 0xD8 => out.extend_from_slice(&[0xFF, marker]),
            _ => {
                let len = usize::from(u16::from_be_bytes([
                    *b.get(i).ok_or_else(bad)?,
                    *b.get(i + 1).ok_or_else(bad)?,
                ]));
                if len < 2 || i + len > b.len() {
                    return Err(bad());
                }
                let body = &b[i + 2..i + len];
                let keep = match marker {
                    0xE1 | 0xE3..=0xE9 | 0xEA..=0xED | 0xEF | 0xFE => false,
                    0xEE => true, // Adobe: colour transform, needed to show the picture right
                    0xE2 => body.starts_with(b"ICC_PROFILE\0"),
                    _ => true,
                };
                if keep {
                    out.extend_from_slice(&[0xFF, marker]);
                    out.extend_from_slice(&b[i..i + len]);
                }
                i += len;
                if marker == 0xDA {
                    // Start of scan: the picture data runs to the end; copy it as it is.
                    out.extend_from_slice(&b[i..]);
                    return Ok(out);
                }
            }
        }
    }
}

fn strip_png(b: &[u8]) -> Result<Vec<u8>, String> {
    const KEEP: &[&[u8; 4]] = &[
        b"IHDR", b"PLTE", b"IDAT", b"IEND", b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"iCCP", b"sBIT",
        b"bKGD", b"hIST", b"pHYs", b"sPLT", b"acTL", b"fcTL", b"fdAT",
    ];
    let bad = || "the PNG file is damaged".to_owned();
    let mut out = Vec::with_capacity(b.len());
    out.extend_from_slice(&b[..8]);
    let mut i = 8;
    while i < b.len() {
        let head = b.get(i..i + 8).ok_or_else(bad)?;
        let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let end = i + 12 + len;
        if end > b.len() {
            return Err(bad());
        }
        if KEEP.iter().any(|k| &head[4..8] == *k) {
            out.extend_from_slice(&b[i..end]);
        }
        i = end;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(marker: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![0xFF, marker];
        v.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
        v.extend_from_slice(body);
        v
    }

    fn fake_jpeg() -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8];
        v.extend(seg(0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0"));
        v.extend(seg(0xE1, b"Exif\0\0GPS 51.5N 0.1W Canon EOS"));
        v.extend(seg(0xE1, b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta/>"));
        v.extend(seg(0xED, b"Photoshop 3.0\0IPTC caption"));
        v.extend(seg(0xFE, b"made by someone"));
        v.extend(seg(0xE2, b"ICC_PROFILE\0profile"));
        v.extend(seg(0xDB, &[0; 65]));
        v.extend(seg(0xDA, &[1, 1, 0, 0, 0x3F, 0]));
        v.extend_from_slice(&[0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD9]);
        v
    }

    #[test]
    fn jpeg_loses_exif_xmp_iptc_and_comments_but_keeps_the_picture() {
        let src = fake_jpeg();
        let (out, removed) = strip(&src).unwrap();
        let text = String::from_utf8_lossy(&out);
        for gone in ["Exif", "GPS", "Canon", "xmpmeta", "IPTC", "someone"] {
            assert!(!text.contains(gone), "{gone} survived");
        }
        assert!(text.contains("JFIF") && text.contains("ICC_PROFILE"));
        assert!(
            out.ends_with(&[0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD9]),
            "image data changed"
        );
        assert_eq!(removed, src.len() - out.len());
        assert!(removed > 80);
    }

    #[test]
    fn png_loses_text_and_exif_chunks() {
        fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
            let mut v = (data.len() as u32).to_be_bytes().to_vec();
            v.extend_from_slice(kind);
            v.extend_from_slice(data);
            v.extend_from_slice(&[0, 0, 0, 0]); // crc is copied or dropped as a whole
            v
        }
        let mut src = b"\x89PNG\r\n\x1a\n".to_vec();
        src.extend(chunk(b"IHDR", &[0; 13]));
        src.extend(chunk(b"tEXt", b"Author\0Jane Doe"));
        src.extend(chunk(b"eXIf", b"GPS data"));
        src.extend(chunk(b"tIME", &[0; 7]));
        src.extend(chunk(b"IDAT", b"pixels"));
        src.extend(chunk(b"IEND", b""));
        let (out, _) = strip(&src).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(!text.contains("Jane") && !text.contains("GPS") && !text.contains("tIME"));
        assert!(text.contains("IHDR") && text.contains("pixels") && text.contains("IEND"));
    }

    #[test]
    fn other_or_damaged_files_are_refused() {
        assert!(strip(b"GIF89a....").is_err());
        assert!(strip(&[0xFF, 0xD8, 0xFF, 0xE1, 0xFF, 0xFF]).is_err());
        assert!(strip(b"\x89PNG\r\n\x1a\n\0\0\xff\xffIHDR").is_err());
    }
}
