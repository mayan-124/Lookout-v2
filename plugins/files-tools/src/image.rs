//! Shrinking a picture to a target file size with Windows' own image codecs
//! (WinRT `BitmapDecoder` / `BitmapEncoder`), so no image library is added.
//!
//! The result is always a JPEG: quality is searched for the best that fits; if
//! even the lowest quality is too big the picture is scaled down in steps.

use std::path::Path;

use windows::core::HSTRING;
use windows::Foundation::PropertyValue;
use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapDecoder, BitmapEncoder, BitmapPixelFormat, BitmapPropertySet,
    BitmapTransform, BitmapTypedValue, ColorManagementMode, ExifOrientationMode,
};
use windows::Storage::Streams::{DataReader, InMemoryRandomAccessStream};
use windows::Storage::{FileAccessMode, StorageFile};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

const SCALES: &[f64] = &[1.0, 0.85, 0.7, 0.55, 0.4, 0.3, 0.2, 0.12];
const QUALITY_STEPS: u32 = 7;

pub struct Shrunk {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub quality: f32,
    pub scale: f64,
}

fn err(e: windows::core::Error) -> String {
    e.message()
}

fn encode(pixels: &[u8], w: u32, h: u32, quality: f32) -> Result<Vec<u8>, String> {
    let stream = InMemoryRandomAccessStream::new().map_err(err)?;
    let options = BitmapPropertySet::new().map_err(err)?;
    let value = PropertyValue::CreateSingle(quality).map_err(err)?;
    let typed =
        BitmapTypedValue::Create(&value, windows::Foundation::PropertyType::Single).map_err(err)?;
    options
        .Insert(&HSTRING::from("ImageQuality"), &typed)
        .map_err(err)?;
    let encoder = BitmapEncoder::CreateWithEncodingOptionsAsync(
        BitmapEncoder::JpegEncoderId().map_err(err)?,
        &stream,
        &options,
    )
    .and_then(|op| op.join())
    .map_err(err)?;
    encoder
        .SetPixelData(
            BitmapPixelFormat::Bgra8,
            BitmapAlphaMode::Ignore,
            w,
            h,
            96.0,
            96.0,
            pixels,
        )
        .map_err(err)?;
    encoder.FlushAsync().and_then(|op| op.join()).map_err(err)?;
    let size = stream.Size().map_err(err)?;
    let reader =
        DataReader::CreateDataReader(&stream.GetInputStreamAt(0).map_err(err)?).map_err(err)?;
    reader
        .LoadAsync(u32::try_from(size).map_err(|_| "result too large".to_owned())?)
        .and_then(|op| op.join())
        .map_err(err)?;
    let mut out = vec![0u8; size as usize];
    reader.ReadBytes(&mut out).map_err(err)?;
    Ok(out)
}

/// Largest JPEG of `src` (by quality, then by scale) that is at most `target` bytes.
pub fn shrink(src: &Path, target: u64) -> Result<Shrunk, String> {
    // SAFETY: plain COM initialisation for this thread; a repeat call is harmless.
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    let full = std::fs::canonicalize(src)
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_owned();
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(full))
        .and_then(|op| op.join())
        .map_err(err)?;
    let stream = file
        .OpenAsync(FileAccessMode::Read)
        .and_then(|op| op.join())
        .map_err(err)?;
    let decoder = BitmapDecoder::CreateAsync(&stream)
        .and_then(|op| op.join())
        .map_err(|_| "this is not a picture Windows can read".to_owned())?;
    let (w, h) = (
        decoder.PixelWidth().map_err(err)?,
        decoder.PixelHeight().map_err(err)?,
    );

    for &scale in SCALES {
        let (sw, sh) = (
            ((f64::from(w) * scale).round() as u32).max(1),
            ((f64::from(h) * scale).round() as u32).max(1),
        );
        let transform = BitmapTransform::new().map_err(err)?;
        transform.SetScaledWidth(sw).map_err(err)?;
        transform.SetScaledHeight(sh).map_err(err)?;
        let data = decoder
            .GetPixelDataTransformedAsync(
                BitmapPixelFormat::Bgra8,
                BitmapAlphaMode::Ignore,
                &transform,
                ExifOrientationMode::RespectExifOrientation,
                ColorManagementMode::DoNotColorManage,
            )
            .and_then(|op| op.join())
            .map_err(err)?;
        let pixels = data.DetachPixelData().map_err(err)?;
        // Quality 0.05 first: if even that is too big, go to a smaller scale.
        let worst = encode(&pixels, sw, sh, 0.05)?;
        if worst.len() as u64 > target {
            continue;
        }
        let (mut lo, mut hi) = (0.05f32, 1.0f32);
        let mut best = (worst, 0.05f32);
        for _ in 0..QUALITY_STEPS {
            let mid = (lo + hi) / 2.0;
            let bytes = encode(&pixels, sw, sh, mid)?;
            if bytes.len() as u64 <= target {
                best = (bytes, mid);
                lo = mid;
            } else {
                hi = mid;
            }
        }
        return Ok(Shrunk {
            bytes: best.0,
            width: sw,
            height: sh,
            quality: best.1,
            scale,
        });
    }
    Err("could not get the picture that small, even at a fraction of its size".into())
}
