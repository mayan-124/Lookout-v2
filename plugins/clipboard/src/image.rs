//! Pictures for the clipboard history: PNG in and out, the small thumbnail, and
//! the device-independent bitmap older programs paste. Nothing is cached: every
//! decoded picture is dropped before the call returns.

use std::io::Cursor;

/// Most pixels a picture may have to be looked at (about an 8K screen). Above
/// this it is refused before anything that size is allocated.
pub const MAX_PIXELS: u64 = 40_000_000;
/// Longest side of a thumbnail, in pixels.
pub const THUMB_SIDE: u32 = 64;

/// A decoded picture, 8-bit RGBA, rows top down.
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

fn fits(width: u32, height: u32) -> bool {
    let pixels = u64::from(width) * u64::from(height);
    pixels > 0 && pixels <= MAX_PIXELS
}

/// Encodes RGB (3 bytes a pixel) or RGBA (4) rows.
pub fn encode_png(width: u32, height: u32, alpha: bool, data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, width, height);
    enc.set_color(if alpha {
        png::ColorType::Rgba
    } else {
        png::ColorType::Rgb
    });
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let mut writer = enc.write_header().ok()?;
    writer.write_image_data(data).ok()?;
    writer.finish().ok()?;
    Some(out)
}

/// The bytes up to the end of the PNG (a clipboard block may be longer).
pub fn trim_png(bytes: &[u8]) -> Option<&[u8]> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    let mut at = 8usize;
    loop {
        let head = bytes.get(at..at.checked_add(8)?)?;
        let len = u32::from_be_bytes(head[..4].try_into().ok()?) as usize;
        let end = at.checked_add(12)?.checked_add(len)?;
        if end > bytes.len() {
            return None;
        }
        if &head[4..] == b"IEND" {
            return Some(&bytes[..end]);
        }
        at = end;
    }
}

/// Width and height from the header, without decoding.
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let reader = png::Decoder::new(Cursor::new(bytes)).read_info().ok()?;
    let info = reader.info();
    Some((info.width, info.height))
}

pub fn decode(bytes: &[u8]) -> Option<Rgba> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(
        png::Transformations::EXPAND | png::Transformations::STRIP_16 | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().ok()?;
    let (width, height) = (reader.info().width, reader.info().height);
    if !fits(width, height) {
        return None;
    }
    let mut buf = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buf).ok()?;
    buf.truncate(frame.buffer_size());
    let data = match frame.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        _ => return None,
    };
    (data.len() as u64 == u64::from(width) * u64::from(height) * 4).then_some(Rgba {
        width,
        height,
        data,
    })
}

/// A PNG no larger than [`THUMB_SIDE`] either way (never enlarged), each pixel
/// the average of the ones it stands for.
pub fn thumbnail(bytes: &[u8]) -> Option<Vec<u8>> {
    let full = decode(bytes)?;
    let (w, h) = (u64::from(full.width), u64::from(full.height));
    let longest = w.max(h);
    let side = u64::from(THUMB_SIDE).min(longest);
    let (tw, th) = ((w * side / longest).max(1), (h * side / longest).max(1));
    let mut out = Vec::with_capacity((tw * th * 4) as usize);
    for y in 0..th {
        let rows = y * h / th..((y + 1) * h / th);
        for x in 0..tw {
            let cols = x * w / tw..((x + 1) * w / tw);
            let mut sum = [0u64; 4];
            let mut n = 0u64;
            for sy in rows.clone() {
                let from = ((sy * w + cols.start) * 4) as usize;
                let to = ((sy * w + cols.end) * 4) as usize;
                for p in full.data[from..to].as_chunks::<4>().0 {
                    for (s, v) in sum.iter_mut().zip(p) {
                        *s += u64::from(*v);
                    }
                    n += 1;
                }
            }
            out.extend(sum.map(|s| (s / n.max(1)) as u8));
        }
    }
    encode_png(u32::try_from(tw).ok()?, u32::try_from(th).ok()?, true, &out)
}

/// A `CF_DIB` block: a 40-byte header, then 32-bit BGRX rows bottom up.
/// Transparent parts are laid over white, for programs that ignore alpha.
pub fn dib(image: &Rgba) -> Vec<u8> {
    let row = image.width as usize * 4;
    let size = row * image.height as usize;
    let mut out = Vec::with_capacity(40 + size);
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&image.width.to_le_bytes());
    out.extend_from_slice(&image.height.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&u32::try_from(size).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    for line in image.data.chunks_exact(row.max(4)).rev() {
        for p in line.as_chunks::<4>().0 {
            let a = u32::from(p[3]);
            let over = |c: u8| ((u32::from(c) * a + 255 * (255 - a)) / 255) as u8;
            out.extend_from_slice(&[over(p[2]), over(p[1]), over(p[0]), 255]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_survives_a_round_trip_and_trailing_bytes_are_cut() {
        let png = encode_png(2, 1, false, &[10, 20, 30, 40, 50, 60]).unwrap();
        let mut padded = png.clone();
        padded.extend_from_slice(&[0; 16]);
        assert_eq!(trim_png(&padded), Some(png.as_slice()));
        assert_eq!(trim_png(&png[..png.len() - 1]), None);
        assert_eq!(trim_png(b"not a picture"), None);
        assert_eq!(dimensions(&png), Some((2, 1)));
        let back = decode(&png).unwrap();
        assert_eq!(back.data, [10, 20, 30, 255, 40, 50, 60, 255]);
        let dib = dib(&back);
        assert_eq!(dib.len(), 48);
        assert_eq!(&dib[40..44], &[30, 20, 10, 255]);
    }

    #[test]
    fn a_thumbnail_is_small_keeps_the_shape_and_is_never_enlarged() {
        let big = encode_png(640, 320, false, &vec![200; 640 * 320 * 3]).unwrap();
        let t = thumbnail(&big).unwrap();
        assert_eq!(dimensions(&t), Some((64, 32)));
        assert_eq!(&decode(&t).unwrap().data[..4], &[200, 200, 200, 255]);
        let small = encode_png(3, 5, false, &[1; 45]).unwrap();
        assert_eq!(dimensions(&thumbnail(&small).unwrap()), Some((3, 5)));
        assert!(thumbnail(b"junk").is_none());
    }
}
