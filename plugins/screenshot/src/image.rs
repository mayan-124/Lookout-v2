//! In-memory RGBA image representation, true pixel redaction (blur/pixelate),
//! cropping, and PNG / DIB serialization.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use crate::geometry::RectI;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageBuffer {
    pub width: u32,
    pub height: u32,
    /// 4 bytes per pixel: [R, G, B, A]
    pub pixels: Vec<u8>,
}

impl ImageBuffer {
    pub fn new(width: u32, height: u32) -> Self {
        let size = (width as usize)
            .checked_mul(height as usize)
            .and_then(|p| p.checked_mul(4))
            .unwrap_or(0);
        Self {
            width,
            height,
            pixels: vec![0; size],
        }
    }

    pub fn from_rgba(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, String> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|p| p.checked_mul(4))
            .ok_or_else(|| "Image dimensions overflow".to_string())?;
        if pixels.len() != expected {
            return Err(format!(
                "Pixel buffer length mismatch: expected {expected}, got {}",
                pixels.len()
            ));
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Converts 32-bit BGRA bytes (standard GDI BitBlt format) to RGBA.
    pub fn from_bgra(width: u32, height: u32, bgra: &[u8]) -> Result<Self, String> {
        let pixel_count = (width as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| "Image dimensions overflow".to_string())?;
        let expected = pixel_count
            .checked_mul(4)
            .ok_or_else(|| "Image byte size overflow".to_string())?;
        if bgra.len() < expected {
            return Err(format!(
                "BGRA buffer too small: expected {expected}, got {}",
                bgra.len()
            ));
        }

        let mut rgba = vec![0u8; expected];
        for i in 0..pixel_count {
            let src_idx = i * 4;
            let b = bgra[src_idx];
            let g = bgra[src_idx + 1];
            let r = bgra[src_idx + 2];
            let a = 255u8; // GDI capture alpha is usually 0, so set opaque 255

            let dst_idx = i * 4;
            rgba[dst_idx] = r;
            rgba[dst_idx + 1] = g;
            rgba[dst_idx + 2] = b;
            rgba[dst_idx + 3] = a;
        }

        Ok(Self {
            width,
            height,
            pixels: rgba,
        })
    }

    pub fn get_pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let idx = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        Some([
            self.pixels[idx],
            self.pixels[idx + 1],
            self.pixels[idx + 2],
            self.pixels[idx + 3],
        ])
    }

    pub fn set_pixel(&mut self, x: u32, y: u32, color: [u8; 4]) {
        if x >= self.width || y >= self.height {
            return;
        }
        let idx = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        self.pixels[idx..idx + 4].copy_from_slice(&color);
    }

    #[inline]
    pub fn row(&self, y: u32) -> &[u8] {
        if y >= self.height {
            return &[];
        }
        let stride = (self.width as usize) * 4;
        let start = (y as usize) * stride;
        let end = start + stride;
        &self.pixels[start..end]
    }

    /// Crops the image to `rect`. Coordinates are clamped to image bounds.
    pub fn crop(&self, rect: RectI) -> Result<Self, String> {
        let x0 = rect.left.clamp(0, self.width as i32) as u32;
        let y0 = rect.top.clamp(0, self.height as i32) as u32;
        let x1 = rect.right.clamp(0, self.width as i32) as u32;
        let y1 = rect.bottom.clamp(0, self.height as i32) as u32;

        let crop_w = x1.saturating_sub(x0);
        let crop_h = y1.saturating_sub(y0);

        if crop_w == 0 || crop_h == 0 {
            return Err("Crop rectangle is empty".to_string());
        }

        let mut cropped = Self::new(crop_w, crop_h);
        for row in 0..crop_h {
            let src_y = y0 + row;
            let src_start = ((src_y * self.width + x0) * 4) as usize;
            let src_end = src_start + (crop_w * 4) as usize;

            let dst_start = ((row * crop_w) * 4) as usize;
            let dst_end = dst_start + (crop_w * 4) as usize;

            cropped.pixels[dst_start..dst_end].copy_from_slice(&self.pixels[src_start..src_end]);
        }

        Ok(cropped)
    }

    /// Extracts pixels in a sub-rectangle (for undo state before a blur/redaction).
    pub fn get_rect_pixels(&self, rect: RectI) -> (RectI, Vec<u8>) {
        let x0 = rect.left.clamp(0, self.width as i32) as u32;
        let y0 = rect.top.clamp(0, self.height as i32) as u32;
        let x1 = rect.right.clamp(0, self.width as i32) as u32;
        let y1 = rect.bottom.clamp(0, self.height as i32) as u32;

        let w = x1.saturating_sub(x0);
        let h = y1.saturating_sub(y0);
        let valid_rect = RectI::new(x0 as i32, y0 as i32, x1 as i32, y1 as i32);

        if w == 0 || h == 0 {
            return (valid_rect, Vec::new());
        }

        let mut buf = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h {
            let src_y = y0 + row;
            let start = ((src_y * self.width + x0) * 4) as usize;
            let end = start + (w * 4) as usize;
            buf.extend_from_slice(&self.pixels[start..end]);
        }
        (valid_rect, buf)
    }

    /// Restores pixels in a sub-rectangle (for undoing a blur/redaction).
    pub fn set_rect_pixels(&mut self, rect: RectI, data: &[u8]) {
        let w = rect.width() as u32;
        let h = rect.height() as u32;
        if w == 0 || h == 0 || data.len() != (w * h * 4) as usize {
            return;
        }

        for row in 0..h {
            let dst_y = (rect.top as u32) + row;
            if dst_y >= self.height {
                break;
            }
            let dst_start = ((dst_y * self.width + (rect.left as u32)) * 4) as usize;
            let dst_end = dst_start + (w * 4) as usize;

            let src_start = (row * w * 4) as usize;
            let src_end = src_start + (w * 4) as usize;

            if dst_end <= self.pixels.len() && src_end <= data.len() {
                self.pixels[dst_start..dst_end].copy_from_slice(&data[src_start..src_end]);
            }
        }
    }

    /// Destructively pixelates the given rectangle in-place into mosaic blocks of `cell_size`.
    /// This is an irreversible true pixel redaction in the raster buffer.
    pub fn pixelate(&mut self, rect: RectI, cell_size: u32) {
        let cell_size = cell_size.max(2);
        let x0 = rect.left.clamp(0, self.width as i32) as u32;
        let y0 = rect.top.clamp(0, self.height as i32) as u32;
        let x1 = rect.right.clamp(0, self.width as i32) as u32;
        let y1 = rect.bottom.clamp(0, self.height as i32) as u32;

        let w = x1.saturating_sub(x0);
        let h = y1.saturating_sub(y0);
        if w == 0 || h == 0 {
            return;
        }

        for by in (y0..y1).step_by(cell_size as usize) {
            for bx in (x0..x1).step_by(cell_size as usize) {
                let cell_w = (x1 - bx).min(cell_size);
                let cell_h = (y1 - by).min(cell_size);

                // Compute average RGB of the block
                let mut sum_r = 0u64;
                let mut sum_g = 0u64;
                let mut sum_b = 0u64;
                let mut count = 0u64;

                for cy in 0..cell_h {
                    for cx in 0..cell_w {
                        let px = bx + cx;
                        let py = by + cy;
                        let idx = ((py * self.width + px) * 4) as usize;
                        sum_r += self.pixels[idx] as u64;
                        sum_g += self.pixels[idx + 1] as u64;
                        sum_b += self.pixels[idx + 2] as u64;
                        count += 1;
                    }
                }

                if count == 0 {
                    continue;
                }

                let avg_r = (sum_r / count) as u8;
                let avg_g = (sum_g / count) as u8;
                let avg_b = (sum_b / count) as u8;

                // Fill block with average color
                for cy in 0..cell_h {
                    for cx in 0..cell_w {
                        let px = bx + cx;
                        let py = by + cy;
                        let idx = ((py * self.width + px) * 4) as usize;
                        self.pixels[idx] = avg_r;
                        self.pixels[idx + 1] = avg_g;
                        self.pixels[idx + 2] = avg_b;
                        self.pixels[idx + 3] = 255;
                    }
                }
            }
        }
    }

    /// Destructively applies a true gaussian/box blur to the given rectangle in-place.
    /// Three passes of box blur approximate a gaussian blur.
    pub fn blur(&mut self, rect: RectI, radius: u32) {
        let r = radius.clamp(1, 40) as i32;
        let x0 = rect.left.clamp(0, self.width as i32) as usize;
        let y0 = rect.top.clamp(0, self.height as i32) as usize;
        let x1 = rect.right.clamp(0, self.width as i32) as usize;
        let y1 = rect.bottom.clamp(0, self.height as i32) as usize;

        let w = x1.saturating_sub(x0);
        let h = y1.saturating_sub(y0);
        if w == 0 || h == 0 {
            return;
        }

        // Extract sub-region to blur
        let mut sub = vec![0u8; w * h * 4];
        for row in 0..h {
            let src_y = y0 + row;
            let src_start = (src_y * (self.width as usize) + x0) * 4;
            let src_end = src_start + w * 4;
            let dst_start = row * w * 4;
            sub[dst_start..dst_start + w * 4].copy_from_slice(&self.pixels[src_start..src_end]);
        }

        // Apply 3 passes of box blur (horizontal + vertical)
        let mut temp = vec![0u8; w * h * 4];
        for _ in 0..3 {
            // Horizontal pass
            for y in 0..h {
                for x in 0..w {
                    let mut sum_r = 0i32;
                    let mut sum_g = 0i32;
                    let mut sum_b = 0i32;
                    let mut count = 0i32;

                    let k_start = (x as i32 - r).max(0) as usize;
                    let k_end = (x as i32 + r).min(w as i32 - 1) as usize;

                    for k in k_start..=k_end {
                        let idx = (y * w + k) * 4;
                        sum_r += sub[idx] as i32;
                        sum_g += sub[idx + 1] as i32;
                        sum_b += sub[idx + 2] as i32;
                        count += 1;
                    }

                    let dst_idx = (y * w + x) * 4;
                    temp[dst_idx] = (sum_r / count) as u8;
                    temp[dst_idx + 1] = (sum_g / count) as u8;
                    temp[dst_idx + 2] = (sum_b / count) as u8;
                    temp[dst_idx + 3] = sub[dst_idx + 3];
                }
            }

            // Vertical pass
            for x in 0..w {
                for y in 0..h {
                    let mut sum_r = 0i32;
                    let mut sum_g = 0i32;
                    let mut sum_b = 0i32;
                    let mut count = 0i32;

                    let k_start = (y as i32 - r).max(0) as usize;
                    let k_end = (y as i32 + r).min(h as i32 - 1) as usize;

                    for k in k_start..=k_end {
                        let idx = (k * w + x) * 4;
                        sum_r += temp[idx] as i32;
                        sum_g += temp[idx + 1] as i32;
                        sum_b += temp[idx + 2] as i32;
                        count += 1;
                    }

                    let dst_idx = (y * w + x) * 4;
                    sub[dst_idx] = (sum_r / count) as u8;
                    sub[dst_idx + 1] = (sum_g / count) as u8;
                    sub[dst_idx + 2] = (sum_b / count) as u8;
                    sub[dst_idx + 3] = temp[dst_idx + 3];
                }
            }
        }

        // Copy blurred sub-region back into main buffer
        for row in 0..h {
            let dst_y = y0 + row;
            let dst_start = (dst_y * (self.width as usize) + x0) * 4;
            let src_start = row * w * 4;
            self.pixels[dst_start..dst_start + w * 4]
                .copy_from_slice(&sub[src_start..src_start + w * 4]);
        }
    }

    /// Checks if the image or crop is essentially pure black (e.g. DRM protected content).
    pub fn is_black(&self, threshold_pct: f32) -> bool {
        if self.pixels.is_empty() {
            return false;
        }
        let total_pixels = (self.width * self.height) as usize;
        let mut black_count = 0usize;

        for i in 0..total_pixels {
            let idx = i * 4;
            if self.pixels[idx] == 0 && self.pixels[idx + 1] == 0 && self.pixels[idx + 2] == 0 {
                black_count += 1;
            }
        }

        let pct = (black_count as f32) / (total_pixels as f32);
        pct >= threshold_pct
    }

    /// Saves the image buffer as PNG to the given path.
    pub fn save_png(&self, path: &Path) -> Result<(), String> {
        let _ = std::fs::remove_file(path);
        let file = File::create(path).map_err(|e| format!("Could not create file: {e}"))?;
        let mut writer_buf = BufWriter::new(file);
        let w = &mut writer_buf;

        let mut encoder = png::Encoder::new(w, self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);

        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("PNG header write error: {e}"))?;
        writer
            .write_image_data(&self.pixels)
            .map_err(|e| format!("PNG pixel write error: {e}"))?;
        Ok(())
    }

    /// Loads an image buffer from a PNG file.
    pub fn load_png<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("Could not open PNG file: {e}"))?;
        let reader_file = std::io::BufReader::new(file);
        let decoder = png::Decoder::new(reader_file);
        let mut reader = decoder
            .read_info()
            .map_err(|e| format!("PNG read_info failed: {e}"))?;
        let buf_size = reader
            .output_buffer_size()
            .ok_or_else(|| "Could not determine PNG buffer size".to_string())?;
        let mut buf = vec![0; buf_size];
        let info = reader
            .next_frame(&mut buf)
            .map_err(|e| format!("PNG next_frame failed: {e}"))?;
        let bytes = &buf[..info.buffer_size()];

        let (w, h) = (info.width, info.height);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        match info.color_type {
            png::ColorType::Rgba => {
                rgba.copy_from_slice(bytes);
            }
            png::ColorType::Rgb => {
                for i in 0..(w * h) as usize {
                    rgba[i * 4] = bytes[i * 3];
                    rgba[i * 4 + 1] = bytes[i * 3 + 1];
                    rgba[i * 4 + 2] = bytes[i * 3 + 2];
                    rgba[i * 4 + 3] = 255;
                }
            }
            png::ColorType::Grayscale => {
                for i in 0..(w * h) as usize {
                    let v = bytes[i];
                    rgba[i * 4] = v;
                    rgba[i * 4 + 1] = v;
                    rgba[i * 4 + 2] = v;
                    rgba[i * 4 + 3] = 255;
                }
            }
            png::ColorType::GrayscaleAlpha => {
                for i in 0..(w * h) as usize {
                    let v = bytes[i * 2];
                    let a = bytes[i * 2 + 1];
                    rgba[i * 4] = v;
                    rgba[i * 4 + 1] = v;
                    rgba[i * 4 + 2] = v;
                    rgba[i * 4 + 3] = a;
                }
            }
            _ => return Err(format!("Unsupported PNG color type: {:?}", info.color_type)),
        }

        Ok(Self {
            width: w,
            height: h,
            pixels: rgba,
        })
    }

    /// Encodes the image buffer to PNG bytes in memory.
    pub fn encode_png(&self) -> Result<Vec<u8>, String> {
        let mut buf = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut buf, self.width, self.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_compression(png::Compression::Fast);

            let mut writer = encoder
                .write_header()
                .map_err(|e| format!("PNG header error: {e}"))?;
            writer
                .write_image_data(&self.pixels)
                .map_err(|e| format!("PNG write error: {e}"))?;
        }
        Ok(buf)
    }

    /// Converts image to standard Windows 32-bit CF_DIB memory structure (BITMAPINFOHEADER + BGRA pixels).
    pub fn to_dib_bytes(&self) -> Vec<u8> {
        // BITMAPINFOHEADER is 40 bytes.
        // biWidth = width, biHeight = -(height as i32) for top-down DIB.
        let header_size = 40usize;
        let pixel_bytes = (self.width as usize) * (self.height as usize) * 4;
        let total_size = header_size + pixel_bytes;

        let mut dib = vec![0u8; total_size];

        // Fill BITMAPINFOHEADER (little-endian)
        // biSize: 40
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        // biWidth: width
        dib[4..8].copy_from_slice(&(self.width as i32).to_le_bytes());
        // biHeight: -height (top-down)
        dib[8..12].copy_from_slice(&(-(self.height as i32)).to_le_bytes());
        // biPlanes: 1
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        // biBitCount: 32
        dib[14..16].copy_from_slice(&32u16.to_le_bytes());
        // biCompression: BI_RGB (0)
        dib[16..20].copy_from_slice(&0u32.to_le_bytes());
        // biSizeImage: pixel_bytes
        dib[20..24].copy_from_slice(&(pixel_bytes as u32).to_le_bytes());

        // Copy pixels converting RGBA to BGRA
        let mut dst = header_size;
        for i in 0..((self.width * self.height) as usize) {
            let src = i * 4;
            let r = self.pixels[src];
            let g = self.pixels[src + 1];
            let b = self.pixels[src + 2];
            let a = self.pixels[src + 3];

            dib[dst] = b;
            dib[dst + 1] = g;
            dib[dst + 2] = r;
            dib[dst + 3] = a;
            dst += 4;
        }

        dib
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_image_crop() {
        let mut img = ImageBuffer::new(100, 100);
        img.set_pixel(10, 10, [255, 0, 0, 255]);
        let cropped = img.crop(RectI::new(5, 5, 25, 25)).unwrap();
        assert_eq!(cropped.width, 20);
        assert_eq!(cropped.height, 20);
        assert_eq!(cropped.get_pixel(5, 5), Some([255, 0, 0, 255]));
    }

    #[test]
    fn test_pixelate_irreversible_true_pixel_change() {
        let mut img = ImageBuffer::new(20, 20);
        // Paint alternating black and white pixels
        for y in 0..20 {
            for x in 0..20 {
                let c = if (x + y) % 2 == 0 { 255 } else { 0 };
                img.set_pixel(x, y, [c, c, c, 255]);
            }
        }

        let orig_pixel_0_0 = img.get_pixel(0, 0).unwrap();
        let orig_pixel_1_0 = img.get_pixel(1, 0).unwrap();
        assert_ne!(orig_pixel_0_0, orig_pixel_1_0);

        // Apply pixelate
        img.pixelate(RectI::new(0, 0, 10, 10), 10);

        // Within the 10x10 cell, all pixels must now be equal to the average color (~127/128)
        let blurred_0_0 = img.get_pixel(0, 0).unwrap();
        let blurred_1_0 = img.get_pixel(1, 0).unwrap();
        assert_eq!(blurred_0_0, blurred_1_0);
        assert!(blurred_0_0[0] > 100 && blurred_0_0[0] < 155);

        // Pixels outside the redacted rect must remain unchanged
        assert_eq!(img.get_pixel(15, 15).unwrap(), [255, 255, 255, 255]);
    }

    #[test]
    fn test_blur_modifies_pixels() {
        let mut img = ImageBuffer::new(30, 30);
        // Put a bright red spot in the middle
        img.set_pixel(15, 15, [255, 0, 0, 255]);

        img.blur(RectI::new(10, 10, 20, 20), 4);

        // Center pixel is no longer 255 (energy has diffused)
        let center = img.get_pixel(15, 15).unwrap();
        assert!(center[0] < 250);
        assert!(center[0] > 0);

        // Adjacent pixels now have some red
        let neighbor = img.get_pixel(14, 15).unwrap();
        assert!(neighbor[0] > 0);
    }

    #[test]
    fn test_black_detection() {
        let mut img = ImageBuffer::new(50, 50);
        assert!(img.is_black(0.99));

        img.set_pixel(25, 25, [255, 255, 255, 255]);
        assert!(img.is_black(0.95));
        assert!(!img.is_black(1.0));
    }
}
