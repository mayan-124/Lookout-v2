//! Generates screenshot-like fixture images with known text, so OCR can be
//! tested and benchmarked without touching any real image. Uppercase letters,
//! digits and spaces are drawn from a 5x7 pixel font, black on white.

const GLYPHS: &[(char, &str)] = &[
    ('A', "01110/10001/10001/11111/10001/10001/10001"),
    ('B', "11110/10001/10001/11110/10001/10001/11110"),
    ('C', "01110/10001/10000/10000/10000/10001/01110"),
    ('D', "11110/10001/10001/10001/10001/10001/11110"),
    ('E', "11111/10000/10000/11110/10000/10000/11111"),
    ('F', "11111/10000/10000/11110/10000/10000/10000"),
    ('G', "01110/10001/10000/10111/10001/10001/01111"),
    ('H', "10001/10001/10001/11111/10001/10001/10001"),
    ('I', "01110/00100/00100/00100/00100/00100/01110"),
    ('J', "00111/00010/00010/00010/00010/10010/01100"),
    ('K', "10001/10010/10100/11000/10100/10010/10001"),
    ('L', "10000/10000/10000/10000/10000/10000/11111"),
    ('M', "10001/11011/10101/10101/10001/10001/10001"),
    ('N', "10001/11001/10101/10011/10001/10001/10001"),
    ('O', "01110/10001/10001/10001/10001/10001/01110"),
    ('P', "11110/10001/10001/11110/10000/10000/10000"),
    ('Q', "01110/10001/10001/10001/10101/10010/01101"),
    ('R', "11110/10001/10001/11110/10100/10010/10001"),
    ('S', "01111/10000/10000/01110/00001/00001/11110"),
    ('T', "11111/00100/00100/00100/00100/00100/00100"),
    ('U', "10001/10001/10001/10001/10001/10001/01110"),
    ('V', "10001/10001/10001/10001/10001/01010/00100"),
    ('W', "10001/10001/10001/10101/10101/11011/10001"),
    ('X', "10001/10001/01010/00100/01010/10001/10001"),
    ('Y', "10001/10001/01010/00100/00100/00100/00100"),
    ('Z', "11111/00001/00010/00100/01000/10000/11111"),
    ('0', "01110/10001/10011/10101/11001/10001/01110"),
    ('1', "00100/01100/00100/00100/00100/00100/01110"),
    ('2', "01110/10001/00001/00010/00100/01000/11111"),
    ('3', "11110/00001/00001/01110/00001/00001/11110"),
    ('4', "00010/00110/01010/10010/11111/00010/00010"),
    ('5', "11111/10000/11110/00001/00001/10001/01110"),
    ('6', "00110/01000/10000/11110/10001/10001/01110"),
    ('7', "11111/00001/00010/00100/01000/01000/01000"),
    ('8', "01110/10001/10001/01110/10001/10001/01110"),
    ('9', "01110/10001/10001/01111/00001/00010/01100"),
];

/// Grey pixels (0 = black, 255 = white), row-major, with their size.
pub struct Raster {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

fn glyph(c: char) -> Option<Vec<[bool; 5]>> {
    let (_, rows) = GLYPHS.iter().find(|(g, _)| *g == c.to_ascii_uppercase())?;
    Some(
        rows.split('/')
            .map(|r| {
                let mut row = [false; 5];
                for (i, b) in r.bytes().take(5).enumerate() {
                    row[i] = b == b'1';
                }
                row
            })
            .collect(),
    )
}

/// Draws `text` (letters, digits, spaces; anything else becomes a space).
pub fn render(text: &str, scale: usize) -> Raster {
    let margin = 4 * scale;
    let chars: Vec<char> = text.chars().collect();
    let width = margin * 2 + chars.len() * 6 * scale;
    let height = margin * 2 + 7 * scale;
    let mut pixels = vec![255u8; width * height];
    for (i, c) in chars.iter().enumerate() {
        let Some(rows) = glyph(*c) else { continue };
        for (y, row) in rows.iter().enumerate() {
            for (x, on) in row.iter().enumerate() {
                if !on {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let px = margin + i * 6 * scale + x * scale + dx;
                        let py = margin + y * scale + dy;
                        pixels[py * width + px] = 0;
                    }
                }
            }
        }
    }
    Raster {
        width,
        height,
        pixels,
    }
}

/// A 24-bit BMP file of the rendered text.
pub fn bmp(text: &str, scale: usize) -> Vec<u8> {
    let r = render(text, scale);
    let row_bytes = (r.width * 3).div_ceil(4) * 4;
    let data_len = row_bytes * r.height;
    let mut out = Vec::with_capacity(54 + data_len);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((54 + data_len) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(r.width as i32).to_le_bytes());
    out.extend_from_slice(&(r.height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 4]); // no compression
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    for y in (0..r.height).rev() {
        let mut row = Vec::with_capacity(row_bytes);
        for x in 0..r.width {
            let g = r.pixels[y * r.width + x];
            row.extend_from_slice(&[g, g, g]);
        }
        row.resize(row_bytes, 0);
        out.extend_from_slice(&row);
    }
    out
}
