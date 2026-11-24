//! Reading the text in images and screenshots with the Windows built-in OCR
//! engine (`Windows.Media.Ocr`). It runs locally; no image leaves the machine.
//! Tesseract is not used: see docs/decisions.md.

use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum OcrError {
    #[error("Windows OCR is not available (no OCR language installed?)")]
    Unavailable,
    #[error("image is too large for the OCR engine ({0} px)")]
    TooLarge(u32),
    #[error("OCR failed: {0}")]
    Failed(String),
}

/// Largest side after upscaling; the OCR engine's own limit is higher.
pub const MAX_UPSCALED: usize = 4000;

/// Windows OCR reads nothing at all from small text (measured: 0% at 14 px and
/// below, 100% from 12 px once doubled), so when a plain read finds little text
/// the picture is doubled with smooth
/// (bilinear) interpolation before OCR. `None` when that would exceed
/// [`MAX_UPSCALED`] (a 2560x1440 screenshot is passed on as it is). Works on any
/// 4-byte-per-pixel buffer, top-down or bottom-up.
pub fn upscale_2x(w: usize, h: usize, px: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    if w == 0 || h == 0 || px.len() < w * h * 4 || w.max(h) * 2 > MAX_UPSCALED {
        return None;
    }
    let (w2, h2) = (w * 2, h * 2);
    let mut out = vec![0u8; w2 * h2 * 4];
    for y in 0..h2 {
        let fy = ((y as f32 + 0.5) / 2.0 - 0.5).clamp(0.0, (h - 1) as f32);
        let (y0, ty) = (fy as usize, fy.fract());
        let y1 = (y0 + 1).min(h - 1);
        for x in 0..w2 {
            let fx = ((x as f32 + 0.5) / 2.0 - 0.5).clamp(0.0, (w - 1) as f32);
            let (x0, tx) = (fx as usize, fx.fract());
            let x1 = (x0 + 1).min(w - 1);
            for c in 0..4 {
                let at = |xx: usize, yy: usize| f32::from(px[(yy * w + xx) * 4 + c]);
                let top = at(x0, y0) * (1.0 - tx) + at(x1, y0) * tx;
                let bottom = at(x0, y1) * (1.0 - tx) + at(x1, y1) * tx;
                out[(y * w2 + x) * 4 + c] = (top * (1.0 - ty) + bottom * ty + 0.5) as u8;
            }
        }
    }
    Some((w2, h2, out))
}

pub trait Ocr: Send + Sync {
    /// The text found in the image, or an error.
    fn recognize(&self, path: &Path) -> Result<String, OcrError>;

    /// Whether the engine can run right now (an OCR language is installed).
    fn available(&self) -> bool {
        true
    }
}

/// OCR that always fails as unavailable (used when OCR is switched off).
pub struct NoOcr;

impl Ocr for NoOcr {
    fn recognize(&self, _: &Path) -> Result<String, OcrError> {
        Err(OcrError::Unavailable)
    }

    fn available(&self) -> bool {
        false
    }
}

pub struct WindowsOcr;

#[cfg(windows)]
impl Ocr for WindowsOcr {
    fn available(&self) -> bool {
        use windows::Media::Ocr::OcrEngine;
        OcrEngine::TryCreateFromUserProfileLanguages().is_ok()
    }

    fn recognize(&self, path: &Path) -> Result<String, OcrError> {
        self.recognize_path(path, true)
    }
}

#[cfg(windows)]
impl WindowsOcr {
    /// `upscale` is only ever false in the tests that measure its effect.
    pub fn recognize_path(&self, path: &Path, upscale: bool) -> Result<String, OcrError> {
        use windows::core::HSTRING;
        use windows::Media::Ocr::OcrEngine;
        use windows::Storage::{FileAccessMode, StorageFile};
        use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

        let fail = |e: windows::core::Error| OcrError::Failed(e.message());
        // SAFETY: plain COM initialisation for this thread; a second call (or a
        // thread already initialised differently) returns an error we can ignore.
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        let engine =
            OcrEngine::TryCreateFromUserProfileLanguages().map_err(|_| OcrError::Unavailable)?;
        let full = std::fs::canonicalize(path)
            .map_err(|e| OcrError::Failed(e.to_string()))?
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_owned();
        let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(full))
            .and_then(|op| op.join())
            .map_err(fail)?;
        let stream = file
            .OpenAsync(FileAccessMode::Read)
            .and_then(|op| op.join())
            .map_err(fail)?;
        recognize_stream(&engine, &stream, upscale)
    }
}

/// OCR of an encoded image (BMP/PNG...) held in memory; nothing touches disk.
#[cfg(windows)]
impl WindowsOcr {
    pub fn recognize_bytes(&self, bytes: &[u8]) -> Result<String, OcrError> {
        self.recognize_bytes_with(bytes, true)
    }

    /// `upscale` is only ever false in the tests that measure its effect.
    pub fn recognize_bytes_with(&self, bytes: &[u8], upscale: bool) -> Result<String, OcrError> {
        use windows::Media::Ocr::OcrEngine;
        use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
        use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

        let fail = |e: windows::core::Error| OcrError::Failed(e.message());
        // SAFETY: plain COM initialisation; an error from a repeat call is ignored.
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        let engine =
            OcrEngine::TryCreateFromUserProfileLanguages().map_err(|_| OcrError::Unavailable)?;
        let stream = InMemoryRandomAccessStream::new().map_err(fail)?;
        let writer = DataWriter::CreateDataWriter(&stream).map_err(fail)?;
        writer.WriteBytes(bytes).map_err(fail)?;
        writer.StoreAsync().and_then(|op| op.join()).map_err(fail)?;
        writer.FlushAsync().and_then(|op| op.join()).map_err(fail)?;
        stream.Seek(0).map_err(fail)?;
        let stream: windows::Storage::Streams::IRandomAccessStream = stream.into();
        recognize_stream(&engine, &stream, upscale)
    }
}

#[cfg(windows)]
fn recognize_stream(
    engine: &windows::Media::Ocr::OcrEngine,
    stream: &windows::Storage::Streams::IRandomAccessStream,
    upscale: bool,
) -> Result<String, OcrError> {
    use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapDecoder, BitmapPixelFormat};

    let fail = |e: windows::core::Error| OcrError::Failed(e.message());
    let decoder = BitmapDecoder::CreateAsync(stream)
        .and_then(|op| op.join())
        .map_err(fail)?;
    let (w, h) = (
        decoder.PixelWidth().map_err(fail)?,
        decoder.PixelHeight().map_err(fail)?,
    );
    let area = w as usize * h as usize;
    if area > MAX_PIXELS {
        return Err(OcrError::TooLarge(w.max(h)));
    }
    // Memory: the job waits until its estimated bytes fit in the shared budget.
    let _held = BUDGET.acquire(if area > TILED_ABOVE {
        TILE_JOB_BYTES
    } else {
        area * BYTES_PER_PIXEL
    });
    if area > TILED_ABOVE {
        return recognize_tiled(engine, &decoder, w as usize, h as usize, upscale);
    }
    let bitmap = decoder
        .GetSoftwareBitmapConvertedAsync(BitmapPixelFormat::Bgra8, BitmapAlphaMode::Premultiplied)
        .and_then(|op| op.join())
        .map_err(fail)?;
    let first = read_bitmap(engine, &bitmap)?;
    // Retry rule: a plain read with fewer than 1 letter per 20,000 pixels missed
    // small text (a big headline alone does not make it "enough"). The retry
    // (doubled picture, or doubled tiles if a side is over 2000 px) replaces
    // the plain read only if it found more text.
    if !upscale || area > RETRY_PIXELS || letters(&first) >= (area / PIXELS_PER_LETTER).max(1) {
        return Ok(first);
    }
    let copied = pixels(&bitmap);
    drop(bitmap);
    let Some((pw, ph, px)) = copied else {
        return Ok(first);
    };
    let second = if pw.max(ph) > TILE {
        let mut parts = Vec::new();
        for (x, y, tw, th) in tile_rects(pw, ph) {
            let mut crop = Vec::with_capacity(tw * th * 4);
            for row in y..y + th {
                crop.extend_from_slice(&px[(row * pw + x) * 4..(row * pw + x + tw) * 4]);
            }
            let doubled = upscale_2x(tw, th, &crop);
            drop(crop);
            let Some((w2, h2, big)) = doubled else {
                continue;
            };
            let tile = bitmap_from(w2, h2, &big);
            drop(big);
            let Some(tile) = tile else {
                continue;
            };
            parts.push(read_bitmap(engine, &tile)?);
        }
        merge_lines(&parts)
    } else {
        let Some((w2, h2, big)) = upscale_2x(pw, ph, &px) else {
            return Ok(first);
        };
        match bitmap_from(w2, h2, &big) {
            Some(b) => read_bitmap(engine, &b)?,
            None => return Ok(first),
        }
    };
    Ok(if letters(&second) > letters(&first) {
        second
    } else {
        first
    })
}

/// Pictures over this many pixels (100 MP) are skipped as absurd.
const MAX_PIXELS: usize = 100_000_000;
/// Pictures over this are decoded and read one tile at a time (never whole).
const TILED_ABOVE: usize = 4_000_000;
/// The doubled retry is only tried up to this size (4000x3000).
const RETRY_PIXELS: usize = 12_000_000;
/// Estimated peak bytes per pixel of a picture read whole (decoded + copy +
/// doubled copies, measured about 24).
const BYTES_PER_PIXEL: usize = 24;
/// Peak bytes of one tile job (a 2000x2000 tile, its crop and doubled copy).
const TILE_JOB_BYTES: usize = 160_000_000;
/// Pixel memory allowed in flight across all OCR jobs.
const BUDGET_BYTES: usize = 600_000_000;

/// Counting budget: small jobs run side by side, big ones wait for room.
/// Strictly first come, first served, so a big job is never starved by a
/// stream of small ones. A job needing more than the whole budget is clamped
/// to it and runs alone.
struct MemBudget {
    limit: usize,
    state: std::sync::Mutex<BudgetState>,
    freed: std::sync::Condvar,
}

struct BudgetState {
    used: usize,
    next_ticket: u64,
    serving: u64,
}

/// Bytes held; given back when dropped (on success, error, panic or cancel).
struct Held<'a> {
    budget: &'a MemBudget,
    bytes: usize,
}

static BUDGET: MemBudget = MemBudget::new(BUDGET_BYTES);

impl MemBudget {
    const fn new(limit: usize) -> Self {
        Self {
            limit,
            state: std::sync::Mutex::new(BudgetState {
                used: 0,
                next_ticket: 0,
                serving: 0,
            }),
            freed: std::sync::Condvar::new(),
        }
    }

    fn acquire(&self, bytes: usize) -> Held<'_> {
        let bytes = bytes.min(self.limit);
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let ticket = s.next_ticket;
        s.next_ticket += 1;
        while s.serving != ticket || (s.used != 0 && s.used + bytes > self.limit) {
            s = self.freed.wait(s).unwrap_or_else(|e| e.into_inner());
        }
        s.used += bytes;
        s.serving += 1;
        self.freed.notify_all();
        Held {
            budget: self,
            bytes,
        }
    }

    #[cfg(test)]
    fn used(&self) -> usize {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).used
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        let mut s = self.budget.state.lock().unwrap_or_else(|e| e.into_inner());
        s.used -= self.bytes;
        self.budget.freed.notify_all();
    }
}

/// A big picture read tile by tile: each tile is decoded on its own, so no
/// full-size bitmap or copy exists. Tiles are read plain; if the whole picture
/// gave too little text (and is not over [`RETRY_PIXELS`]) each tile is read
/// again doubled, and the retry replaces the plain read only if it found more.
#[cfg(windows)]
fn recognize_tiled(
    engine: &windows::Media::Ocr::OcrEngine,
    decoder: &windows::Graphics::Imaging::BitmapDecoder,
    w: usize,
    h: usize,
    upscale: bool,
) -> Result<String, OcrError> {
    use windows::Graphics::Imaging::{
        BitmapAlphaMode, BitmapBounds, BitmapPixelFormat, BitmapTransform, ColorManagementMode,
        ExifOrientationMode,
    };

    let fail = |e: windows::core::Error| OcrError::Failed(e.message());
    let tile_at = |(x, y, tw, th): (usize, usize, usize, usize)| {
        let transform = BitmapTransform::new().map_err(fail)?;
        transform
            .SetBounds(BitmapBounds {
                X: x as u32,
                Y: y as u32,
                Width: tw as u32,
                Height: th as u32,
            })
            .map_err(fail)?;
        decoder
            .GetSoftwareBitmapTransformedAsync(
                BitmapPixelFormat::Bgra8,
                BitmapAlphaMode::Premultiplied,
                &transform,
                ExifOrientationMode::IgnoreExifOrientation,
                ColorManagementMode::DoNotColorManage,
            )
            .and_then(|op| op.join())
            .map_err(fail)
    };
    let rects = tile_rects(w, h);
    let mut first = Vec::new();
    for &r in &rects {
        first.push(read_bitmap(engine, &tile_at(r)?)?);
    }
    let plain = merge_lines(&first);
    let area = w * h;
    if !upscale || area > RETRY_PIXELS || letters(&plain) >= (area / PIXELS_PER_LETTER).max(1) {
        return Ok(plain);
    }
    let mut second = Vec::new();
    for (i, &r) in rects.iter().enumerate() {
        let doubled = pixels(&tile_at(r)?).and_then(|(tw, th, px)| upscale_2x(tw, th, &px));
        let text = match doubled.and_then(|(w2, h2, big)| bitmap_from(w2, h2, &big)) {
            Some(b) => read_bitmap(engine, &b)?,
            None => first[i].clone(),
        };
        second.push(text);
    }
    let second = merge_lines(&second);
    Ok(if letters(&second) > letters(&plain) {
        second
    } else {
        plain
    })
}

/// A plain read must find at least one letter per this many pixels.
const PIXELS_PER_LETTER: usize = 20_000;
/// Images with a side over this are read in tiles of at most this size...
const TILE: usize = 2000;
/// ...overlapping by this many pixels: more than a line of text is tall and
/// than a word is wide, so every word lies whole inside at least one tile.
const OVERLAP: usize = 300;

/// Tiles `(x, y, w, h)` covering a `w` x `h` picture, at most [`TILE`] on a
/// side, neighbours overlapping by [`OVERLAP`].
fn tile_rects(w: usize, h: usize) -> Vec<(usize, usize, usize, usize)> {
    let spans = |len: usize| {
        let mut v = Vec::new();
        let mut at = 0;
        loop {
            let size = TILE.min(len - at);
            v.push((at, size));
            if at + size >= len {
                return v;
            }
            at += TILE - OVERLAP;
        }
    };
    let (xs, ys) = (spans(w), spans(h));
    ys.iter()
        .flat_map(|&(y, th)| xs.iter().map(move |&(x, tw)| (x, y, tw, th)))
        .collect()
}

/// The texts joined line by line, a line already seen (the overlap read twice)
/// dropped.
fn merge_lines(parts: &[String]) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for line in parts.iter().flat_map(|p| p.lines()) {
        let key = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !key.is_empty() && seen.insert(key) {
            out.push(line);
        }
    }
    out.join("\n")
}

fn letters(text: &str) -> usize {
    text.chars().filter(|c| c.is_alphanumeric()).count()
}

#[cfg(windows)]
fn read_bitmap(
    engine: &windows::Media::Ocr::OcrEngine,
    bitmap: &windows::Graphics::Imaging::SoftwareBitmap,
) -> Result<String, OcrError> {
    let fail = |e: windows::core::Error| OcrError::Failed(e.message());
    let result = engine
        .RecognizeAsync(bitmap)
        .and_then(|op| op.join())
        .map_err(fail)?;
    Ok(result.Text().map_err(fail)?.to_string())
}

/// Width, height and BGRA bytes of a bitmap.
#[cfg(windows)]
fn pixels(bitmap: &windows::Graphics::Imaging::SoftwareBitmap) -> Option<(usize, usize, Vec<u8>)> {
    use windows::Storage::Streams::{Buffer, DataReader, IBuffer};

    let (w, h) = (
        usize::try_from(bitmap.PixelWidth().ok()?).ok()?,
        usize::try_from(bitmap.PixelHeight().ok()?).ok()?,
    );
    let len = u32::try_from(w * h * 4).ok()?;
    let buffer = Buffer::Create(len).ok()?;
    let as_ibuffer: IBuffer = buffer.clone().into();
    bitmap.CopyToBuffer(&as_ibuffer).ok()?;
    buffer.SetLength(len).ok()?;
    let mut px = vec![0u8; w * h * 4];
    DataReader::FromBuffer(&as_ibuffer)
        .ok()?
        .ReadBytes(&mut px)
        .ok()?;
    Some((w, h, px))
}

#[cfg(windows)]
fn bitmap_from(
    w: usize,
    h: usize,
    px: &[u8],
) -> Option<windows::Graphics::Imaging::SoftwareBitmap> {
    use windows::core::Interface;
    use windows::Graphics::Imaging::{
        BitmapAlphaMode, BitmapBufferAccessMode, BitmapPixelFormat, SoftwareBitmap,
    };
    use windows::Win32::System::WinRT::IMemoryBufferByteAccess;

    // The bitmap is written in place: no second and third copy of the bytes.
    let out = SoftwareBitmap::CreateWithAlpha(
        BitmapPixelFormat::Bgra8,
        i32::try_from(w).ok()?,
        i32::try_from(h).ok()?,
        BitmapAlphaMode::Premultiplied,
    )
    .ok()?;
    {
        let buffer = out.LockBuffer(BitmapBufferAccessMode::Write).ok()?;
        let reference = buffer.CreateReference().ok()?;
        let access: IMemoryBufferByteAccess = reference.cast().ok()?;
        let (mut ptr, mut len) = (std::ptr::null_mut::<u8>(), 0u32);
        // SAFETY: `ptr`/`len` describe the locked buffer, valid while `reference`
        // lives; we copy at most `len` bytes into it.
        unsafe {
            access.GetBuffer(&mut ptr, &mut len).ok()?;
            let n = px.len().min(len as usize);
            std::ptr::copy_nonoverlapping(px.as_ptr(), ptr, n);
        }
    }
    Some(out)
}

#[cfg(not(windows))]
impl Ocr for WindowsOcr {
    fn recognize(&self, _: &Path) -> Result<String, OcrError> {
        Err(OcrError::Unavailable)
    }

    fn available(&self) -> bool {
        false
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::fixture_image;

    fn normalised(text: &str) -> String {
        text.to_uppercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Runs OCR on a generated image. Returns `None` (test skipped, with a
    /// message) if this machine has no Windows OCR language installed.
    fn read(path: &Path) -> Option<String> {
        match WindowsOcr.recognize(path) {
            Ok(t) => {
                eprintln!("OCR RAN on {}: read {:?}", path.display(), normalised(&t));
                Some(normalised(&t))
            }
            Err(OcrError::Unavailable) => {
                eprintln!("SKIPPED: Windows OCR is not available on this machine");
                None
            }
            Err(e) => panic!("OCR failed: {e}"),
        }
    }

    #[test]
    fn windows_ocr_reads_text_from_in_memory_image_bytes() {
        let bmp = fixture_image::bmp("INVOICE 4821 PAID", 8);
        match WindowsOcr.recognize_bytes(&bmp) {
            Ok(t) => {
                let t = normalised(&t);
                assert!(
                    t.contains("INVOICE") && t.contains("4821"),
                    "OCR read {t:?}"
                );
            }
            Err(OcrError::Unavailable) => eprintln!("SKIPPED: Windows OCR is not available"),
            Err(e) => panic!("OCR failed: {e}"),
        }
    }

    #[test]
    fn windows_ocr_reads_a_generated_bmp_screenshot() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("shot.bmp");
        std::fs::write(&p, fixture_image::bmp("INVOICE 4821 PAID", 8)).unwrap();
        if let Some(t) = read(&p) {
            assert!(
                t.contains("INVOICE") && t.contains("4821"),
                "OCR read {t:?}"
            );
        }
    }

    #[test]
    fn windows_ocr_reads_a_generated_png_screenshot() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("shot.png");
        let r = fixture_image::render("MEETING AT NOON", 8);
        let file = std::fs::File::create(&p).unwrap();
        let mut enc = png::Encoder::new(
            std::io::BufWriter::new(file),
            r.width as u32,
            r.height as u32,
        );
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .unwrap()
            .write_image_data(&r.pixels)
            .unwrap();
        if let Some(t) = read(&p) {
            assert!(
                t.contains("MEETING") && t.contains("NOON"),
                "OCR read {t:?}"
            );
        }
    }

    /// Writes grey pixels (0 = black) as a 24-bit top-down-flipped BMP.
    fn grey_bmp(w: usize, h: usize, px: &[u8]) -> Vec<u8> {
        let row = (w * 3).div_ceil(4) * 4;
        let mut out = Vec::with_capacity(54 + row * h);
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&((54 + row * h) as u32).to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&(w as i32).to_le_bytes());
        out.extend_from_slice(&(h as i32).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&24u16.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&((row * h) as u32).to_le_bytes());
        out.extend_from_slice(&[0; 16]);
        for y in (0..h).rev() {
            let mut r = Vec::with_capacity(row);
            for x in 0..w {
                r.extend_from_slice(&[px[y * w + x]; 3]);
            }
            r.resize(row, 0);
            out.extend_from_slice(&r);
        }
        out
    }

    /// A 5.5 MP picture (read in tiles) with one word straddling the first
    /// vertical tile edge (x = 2000) and one straddling the first horizontal
    /// edge (y = 2000). Both must be found whole.
    #[test]
    fn a_word_crossing_a_tile_boundary_is_still_found() {
        let (w, h) = (2600usize, 2100usize);
        let mut px = vec![255u8; w * h];
        let mut put = |text: &str, x: usize, y: usize| {
            let r = fixture_image::render(text, 8);
            for row in 0..r.height {
                px[(y + row) * w + x..(y + row) * w + x + r.width]
                    .copy_from_slice(&r.pixels[row * r.width..(row + 1) * r.width]);
            }
            r
        };
        let total = put("NOON", 1900, 400); // ink about x 1932..2118
        put("MOON", 500, 1960); // ink about y 1992..2048
                                // The overlap is more than the height of the text and the width of a word.
        assert!(
            OVERLAP >= total.height && OVERLAP >= total.width - 64,
            "overlap too small"
        );
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("tiles.bmp");
        std::fs::write(&p, grey_bmp(w, h, &px)).unwrap();
        // Plain read (no doubled retry): this tests the tile edges only.
        match WindowsOcr.recognize_path(&p, false) {
            Err(OcrError::Unavailable) => eprintln!("SKIPPED: Windows OCR is not available"),
            Err(e) => panic!("OCR failed: {e}"),
            Ok(t) => {
                let t = normalised(&t);
                assert!(t.contains("NOON"), "word across x edge lost: {t:?}");
                assert!(t.contains("MOON"), "word across y edge lost: {t:?}");
            }
        }
    }

    #[test]
    fn a_blank_or_broken_image_is_an_error_or_empty_not_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.png");
        std::fs::write(&p, b"not an image").unwrap();
        assert!(WindowsOcr.recognize(&p).is_err());
        let blank = dir.path().join("blank.bmp");
        std::fs::write(&blank, fixture_image::bmp("   ", 8)).unwrap();
        if let Some(t) = read(&blank) {
            assert!(t.is_empty(), "{t:?}");
        }
    }

    /// Indexer path (`recognize` on a file) with and without the upscale.
    /// Run: cargo test -p lookout-core --lib content::ocr -- --ignored --nocapture
    /// (set LOOKOUT_OCR_TIMING_IMAGE to an image path to also time it once each).
    #[test]
    #[ignore = "needs a desktop session (font rendering) and a Windows OCR language"]
    fn indexer_ocr_small_text_with_and_without_upscale() {
        use crate::screengrab::bmp_file;
        use crate::screengrab::tests::ocr_sweep::{accuracy, shot, SENTENCE};
        let dir = tempfile::tempdir().unwrap();
        for px in [10, 12, 14] {
            let (w, h, pixels) = shot(SENTENCE, px, false);
            let p = dir.path().join(format!("s{px}.bmp"));
            std::fs::write(&p, bmp_file(w, h, &pixels)).unwrap();
            let acc = |up| {
                accuracy(
                    SENTENCE,
                    &WindowsOcr.recognize_path(&p, up).unwrap_or_default(),
                )
            };
            eprintln!(
                "{px}px indexer OCR: plain {:.1}%  upscaled {:.1}%",
                acc(false),
                acc(true)
            );
        }
        // Fixtures on a 2560x1440 canvas: before (plain read, as the old code
        // did for images over 2000 px) against the tiled retry.
        type Part = (usize, usize, (usize, usize, Vec<u8>));
        let on_canvas = |parts: &[Part]| {
            let (cw, ch) = (2560usize, 1440usize);
            let mut px = vec![255u8; cw * ch * 4];
            for (x, top, (w, h, p)) in parts {
                // Rows are bottom-up, so a distance from the top flips.
                for r in 0..*h {
                    let dst = ((ch - top - h + r) * cw + x) * 4;
                    px[dst..dst + w * 4].copy_from_slice(&p[r * w * 4..(r + 1) * w * 4]);
                }
            }
            bmp_file(cw, ch, &px)
        };
        let headline = "QUARTERLY SALES REPORT";
        let mut cases: Vec<(String, String, Vec<u8>)> = [10, 12, 14]
            .iter()
            .map(|&px| {
                (
                    format!("{px}px body"),
                    SENTENCE.to_owned(),
                    on_canvas(&[(300, 700, shot(SENTENCE, px, false))]),
                )
            })
            .collect();
        cases.push((
            "40px headline + 12px body".into(),
            format!("{headline} {SENTENCE}"),
            on_canvas(&[
                (300, 300, shot(headline, 40, false)),
                (300, 450, shot(SENTENCE, 12, false)),
            ]),
        ));
        for (name, want, bmp) in cases {
            let p = dir.path().join("canvas.bmp");
            std::fs::write(&p, bmp).unwrap();
            let acc = |up| {
                accuracy(
                    &want,
                    &WindowsOcr.recognize_path(&p, up).unwrap_or_default(),
                )
            };
            eprintln!(
                "2560x1440 {name}: before {:.1}%  after {:.1}%",
                acc(false),
                acc(true)
            );
        }
        if let Some(img) = std::env::var_os("LOOKOUT_OCR_TIMING_IMAGE") {
            let img = std::path::PathBuf::from(img);
            for up in [false, true] {
                let t = std::time::Instant::now();
                let n = WindowsOcr.recognize_path(&img, up).map(|t| t.len());
                eprintln!(
                    "upscale={up}: {:?} ms, {n:?} chars",
                    t.elapsed().as_millis()
                );
            }
        }
    }
}

#[cfg(test)]
mod upscale_tests {
    use super::*;

    #[test]
    fn small_pictures_are_doubled_smoothly_and_huge_ones_left_alone() {
        let px = [0, 0, 0, 255, 255, 255, 255, 255]; // 2x1: black, white
        let (w, h, out) = upscale_2x(2, 1, &px).unwrap();
        assert_eq!((w, h, out.len()), (4, 2, 4 * 2 * 4));
        let row: Vec<u8> = (0..4).map(|x| out[x * 4]).collect();
        assert_eq!((row[0], row[3]), (0, 255));
        assert!(row[1] > 0 && row[1] < row[2]);
        // Anything over 2000 on a side is tiled by the caller, never doubled whole.
        assert!(upscale_2x(2560, 1440, &vec![0; 2560 * 1440 * 4]).is_none());
        assert!(upscale_2x(1920, 1080, &vec![0; 1920 * 1080 * 4]).is_some());
    }

    #[test]
    fn tiles_cover_the_picture_overlap_and_stay_within_2000() {
        assert_eq!(
            tile_rects(2560, 1440),
            vec![(0, 0, 2000, 1440), (1700, 0, 860, 1440)]
        );
        let t = tile_rects(4000, 2100);
        assert!(t.iter().all(|&(_, _, w, h)| w <= 2000 && h <= 2000));
        assert_eq!(t.iter().map(|t| t.0 + t.2).max(), Some(4000));
        assert_eq!(t.iter().map(|t| t.1 + t.3).max(), Some(2100));
        assert_eq!(tile_rects(2000, 2000).len(), 1);
        // Neighbouring tiles share at least OVERLAP pixels on both axes.
        let wide = tile_rects(5000, 5000);
        for a in &wide {
            for b in &wide {
                if b.0 > a.0 && b.0 < a.0 + a.2 {
                    assert!(a.0 + a.2 - b.0 >= OVERLAP);
                }
                if b.1 > a.1 && b.1 < a.1 + a.3 {
                    assert!(a.1 + a.3 - b.1 >= OVERLAP);
                }
            }
        }
    }

    #[test]
    fn merging_tiles_drops_lines_read_twice() {
        let parts = vec![
            "alpha beta\nshared  line".to_owned(),
            "shared line\ngamma".to_owned(),
        ];
        assert_eq!(merge_lines(&parts), "alpha beta\nshared  line\ngamma");
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(5);
    const SHORT: Duration = Duration::from_millis(300);

    fn budget(limit: usize) -> Arc<MemBudget> {
        Arc::new(MemBudget::new(limit))
    }

    #[test]
    fn small_jobs_share_the_budget_in_parallel() {
        let b = budget(100);
        let (a, c, d) = (b.acquire(30), b.acquire(30), b.acquire(30));
        assert_eq!(b.used(), 90);
        drop((a, c, d));
        assert_eq!(b.used(), 0);
    }

    #[test]
    fn a_job_over_the_whole_budget_still_runs_alone() {
        let b = budget(100);
        let big = b.acquire(10_000);
        assert_eq!(b.used(), 100); // clamped, not refused
        let (tx, rx) = mpsc::channel();
        let b2 = b.clone();
        let t = std::thread::spawn(move || {
            let _h = b2.acquire(1);
            tx.send(()).unwrap();
        });
        assert!(rx.recv_timeout(SHORT).is_err(), "ran beside the big job");
        drop(big);
        rx.recv_timeout(WAIT).expect("small job never got in");
        t.join().unwrap();
        // And a big job waiting behind small ones also gets in once they leave.
        let small = b.acquire(10);
        let b3 = b.clone();
        let (tx, rx) = mpsc::channel();
        let t = std::thread::spawn(move || {
            let _h = b3.acquire(10_000);
            tx.send(()).unwrap();
        });
        assert!(rx.recv_timeout(SHORT).is_err());
        drop(small);
        rx.recv_timeout(WAIT).expect("big job never ran");
        t.join().unwrap();
        assert_eq!(b.used(), 0);
    }

    #[test]
    fn a_big_job_is_not_starved_by_a_stream_of_small_ones() {
        let b = budget(100);
        let first = b.acquire(50);
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let spawn = |name: &'static str, bytes: usize| {
            let (b, order) = (b.clone(), order.clone());
            std::thread::spawn(move || {
                let _h = b.acquire(bytes);
                order.lock().unwrap().push(name);
                std::thread::sleep(Duration::from_millis(50));
            })
        };
        let big = spawn("big", 100);
        std::thread::sleep(SHORT); // the big one is queued first
        let small = spawn("small", 10); // would fit beside `first`, must still wait
        std::thread::sleep(SHORT);
        assert!(order.lock().unwrap().is_empty());
        drop(first);
        big.join().unwrap();
        small.join().unwrap();
        assert_eq!(*order.lock().unwrap(), vec!["big", "small"]);
        assert_eq!(b.used(), 0);
    }

    #[test]
    fn memory_is_given_back_on_error_cancel_and_panic() {
        let b = budget(100);
        let failing = |b: &MemBudget| -> Result<(), OcrError> {
            let _h = b.acquire(60);
            Err(OcrError::Failed("boom".into()))?; // error: early return
            Ok(())
        };
        assert!(failing(&b).is_err());
        assert_eq!(b.used(), 0);
        let cancelled = |b: &MemBudget, cancel: bool| {
            let _h = b.acquire(60);
            if cancel {
                return None; // cancel: the job stops after it was admitted
            }
            Some(())
        };
        assert!(cancelled(&b, true).is_none());
        assert_eq!(b.used(), 0);
        let b2 = b.clone();
        let r = std::thread::spawn(move || {
            let _h = b2.acquire(60);
            panic!("job panicked");
        })
        .join();
        assert!(r.is_err());
        assert_eq!(b.used(), 0);
        // The budget still works after a panic.
        let _h = b.acquire(100);
        assert_eq!(b.used(), 100);
    }
}
