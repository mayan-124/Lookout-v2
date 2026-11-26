//! Thumbnails and file icons from the Windows shell, for the file preview.
//!
//! The pictures come from whatever thumbnail handler is installed for a type:
//! third-party code. Each request runs on its own worker thread and is waited
//! for at most [`TIMEOUT`]. A handler cannot be stopped from outside, so a
//! cancelled or late worker is abandoned and its answer dropped; no new one is
//! started while [`MAX_WORKERS`] are still running. Callers check first that the
//! file may be previewed ([`crate::preview::may_thumbnail`]). Nothing is written
//! to disk here (Windows keeps its own thumbnail cache), and the few pictures
//! kept in memory are dropped when the bar has been hidden for a while.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use crate::preview::base64;

pub const TIMEOUT: Duration = Duration::from_secs(2);
/// Largest side asked of a handler, in pixels.
pub const MAX_SIDE: u32 = 1024;
const MIN_SIDE: u32 = 64;
const MAX_WORKERS: usize = 3;
const CACHE_ITEMS: usize = 8;
const CACHE_BYTES: usize = 12 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Shot {
    /// `data:` URL of the picture.
    pub image: String,
    /// The file type's icon: there was no thumbnail.
    pub icon: bool,
}

/// Makes the pictures of a file, the quick one (its icon) first. The last
/// argument turns true once nobody waits any more.
pub type Extract =
    Arc<dyn Fn(&Path, u32, &dyn Fn(Shot), &dyn Fn() -> bool) + Send + Sync + 'static>;

type Key = (PathBuf, u32, u64, Option<SystemTime>);

pub struct Thumbnails {
    extract: Extract,
    timeout: Duration,
    running: Arc<AtomicUsize>,
    /// Requests numbered up to this one are no longer wanted.
    cancelled: Arc<AtomicU64>,
    /// Oldest first.
    cache: Mutex<Vec<(Key, Shot)>>,
}

impl Default for Thumbnails {
    fn default() -> Self {
        Self::new(Arc::new(shell), TIMEOUT)
    }
}

struct Running(Arc<AtomicUsize>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Thumbnails {
    pub fn new(extract: Extract, timeout: Duration) -> Self {
        Self {
            extract,
            timeout,
            running: Arc::default(),
            cancelled: Arc::default(),
            cache: Mutex::default(),
        }
    }

    /// The selection moved on: stop waiting for `request` and older ones.
    pub fn cancel(&self, request: u64) {
        self.cancelled.fetch_max(request, Ordering::AcqRel);
    }

    /// Drops the pictures kept in memory.
    pub fn clear(&self) {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        *cache = Vec::new();
    }

    /// A thumbnail of `path`, else its icon, else `None`. Blocks for at most
    /// the timeout. Requests are numbered upwards by the caller.
    pub fn get(&self, path: &Path, side: u32, request: u64) -> Option<Shot> {
        let side = side.clamp(MIN_SIDE, MAX_SIDE);
        let meta = std::fs::metadata(path).ok()?;
        let key: Key = (path.to_path_buf(), side, meta.len(), meta.modified().ok());
        if let Some(shot) = self.cached(&key) {
            return Some(shot);
        }
        let floor = self.cancelled.clone();
        let unwanted = move || floor.load(Ordering::Acquire) >= request;
        if unwanted() {
            return None;
        }
        if self.running.fetch_add(1, Ordering::AcqRel) >= MAX_WORKERS {
            self.running.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        let guard = Running(self.running.clone());
        let (tx, rx) = mpsc::channel::<Shot>();
        let (extract, path, gone) = (self.extract.clone(), path.to_path_buf(), unwanted.clone());
        let deadline = Instant::now() + self.timeout;
        std::thread::Builder::new()
            .name("lookout-thumb".into())
            .spawn(move || {
                let _guard = guard;
                let send = |shot| {
                    let _ = tx.send(shot);
                };
                // Late work stops between the icon and the thumbnail.
                let stop = || gone() || Instant::now() >= deadline;
                extract(&path, side, &send, &stop);
            })
            .ok()?;
        let mut best: Option<Shot> = None;
        loop {
            if unwanted() {
                return None;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match rx.recv_timeout(left.min(POLL)) {
                Ok(shot) => {
                    let finished = !shot.icon;
                    best = Some(shot);
                    if finished {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        // An icon that stood in for a slow handler is asked for again next time.
        if let Some(shot) = best.as_ref().filter(|s| !s.icon) {
            self.keep(key, shot.clone());
        }
        best
    }

    fn cached(&self, key: &Key) -> Option<Shot> {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        let at = cache.iter().position(|(k, _)| k == key)?;
        let hit = cache.remove(at);
        let shot = hit.1.clone();
        cache.push(hit);
        Some(shot)
    }

    fn keep(&self, key: Key, shot: Shot) {
        if shot.image.len() > CACHE_BYTES / 2 {
            return;
        }
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache.push((key, shot));
        while cache.len() > CACHE_ITEMS
            || cache.iter().map(|(_, s)| s.image.len()).sum::<usize>() > CACHE_BYTES
        {
            cache.remove(0);
        }
    }
}

/// A 32-bit BMP with an alpha channel from bottom-up BGRA pixels as the shell
/// gives them: colours multiplied by alpha, or no alpha at all.
fn bmp(width: u32, height: u32, mut pixels: Vec<u8>) -> Vec<u8> {
    if pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
        for p in pixels.as_chunks_mut::<4>().0 {
            p[3] = 255;
        }
    } else {
        for p in pixels.as_chunks_mut::<4>().0 {
            let a = u32::from(p[3]);
            if a > 0 && a < 255 {
                for c in &mut p[..3] {
                    *c = (u32::from(*c) * 255 / a).min(255) as u8;
                }
            }
        }
    }
    // File header, then a version 4 info header naming the channel masks.
    const HEADERS: u32 = 14 + 108;
    let size = pixels.len() as u32;
    let mut out = Vec::with_capacity(HEADERS as usize + pixels.len());
    out.extend_from_slice(b"BM");
    for v in [HEADERS + size, 0, HEADERS, 108, width, height] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    // One plane, 32 bits a pixel.
    out.extend_from_slice(&[1, 0, 32, 0]);
    // BI_BITFIELDS, image size, resolution, colour counts, masks, "sRGB".
    for v in [
        3,
        size,
        2835,
        2835,
        0,
        0,
        0x00FF_0000,
        0x0000_FF00,
        0x0000_00FF,
        0xFF00_0000,
        0x7352_4742,
    ] {
        out.extend_from_slice(&u32::to_le_bytes(v));
    }
    out.resize(HEADERS as usize, 0);
    out.extend_from_slice(&pixels);
    out
}

fn data_url(width: u32, height: u32, pixels: Vec<u8>) -> String {
    format!(
        "data:image/bmp;base64,{}",
        base64(&bmp(width, height, pixels))
    )
}

#[cfg(not(windows))]
fn shell(_: &Path, _: u32, _: &dyn Fn(Shot), _: &dyn Fn() -> bool) {}

/// The shell's icon for the file, then the installed handler's thumbnail.
#[cfg(windows)]
fn shell(path: &Path, side: u32, send: &dyn Fn(Shot), stop: &dyn Fn() -> bool) {
    use windows::core::PCWSTR;
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{
        IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_ICONONLY, SIIGBF_THUMBNAILONLY,
    };
    /// Side of the icon, in pixels.
    const ICON: u32 = 96;

    let text: Vec<u16> = path
        .to_string_lossy()
        .replace('/', "\\")
        .encode_utf16()
        .chain([0])
        .collect();
    // SAFETY: this thread is new; paired with CoUninitialize below.
    if unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_err() {
        return;
    }
    {
        // SAFETY: `text` is NUL-terminated and outlives the call.
        let item: windows::core::Result<IShellItemImageFactory> =
            unsafe { SHCreateItemFromParsingName(PCWSTR(text.as_ptr()), None) };
        if let Ok(factory) = item {
            for (side, flags, icon) in [
                (ICON, SIIGBF_ICONONLY, true),
                (side, SIIGBF_THUMBNAILONLY, false),
            ] {
                if stop() {
                    break;
                }
                let size = windows::Win32::Foundation::SIZE {
                    cx: side as i32,
                    cy: side as i32,
                };
                // SAFETY: a plain COM call; `pixels` takes ownership of the bitmap.
                let Ok(bitmap) = (unsafe { factory.GetImage(size, flags) }) else {
                    continue;
                };
                if let Some((w, h, px)) = pixels(bitmap) {
                    send(Shot {
                        image: data_url(w, h, px),
                        icon,
                    });
                }
            }
        }
    }
    // SAFETY: every COM object above is released by now.
    unsafe { CoUninitialize() };
}

/// The bitmap's size and bottom-up BGRA pixels. Deletes the bitmap.
#[cfg(windows)]
fn pixels(bitmap: windows::Win32::Graphics::Gdi::HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
    use windows::Win32::Graphics::Gdi::{
        DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    };
    let read = || {
        let mut info = BITMAP::default();
        let want = std::mem::size_of::<BITMAP>() as i32;
        // SAFETY: `info` is exactly `want` bytes and `bitmap` is live.
        let got = unsafe { GetObjectW(bitmap.into(), want, Some((&raw mut info).cast())) };
        let (w, h) = (info.bmWidth, info.bmHeight.unsigned_abs() as i32);
        let limit = 2 * MAX_SIDE as i32;
        if got != want || !(1..=limit).contains(&w) || !(1..=limit).contains(&h) {
            return None;
        }
        let mut header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u8; w as usize * h as usize * 4];
        // SAFETY: the screen context is released right after; `px` holds `h`
        // rows of `w` 32-bit pixels, as the header asks.
        let rows = unsafe {
            let dc = GetDC(None);
            let rows = GetDIBits(
                dc,
                bitmap,
                0,
                h as u32,
                Some(px.as_mut_ptr().cast()),
                &mut header,
                DIB_RGB_COLORS,
            );
            ReleaseDC(None, dc);
            rows
        };
        (rows == h).then_some((w as u32, h as u32, px))
    };
    let out = read();
    // SAFETY: the bitmap is ours and no longer used.
    unsafe {
        let _ = DeleteObject(bitmap.into());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(name: &str, icon: bool) -> Shot {
        Shot {
            image: name.into(),
            icon,
        }
    }

    fn file() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.docx");
        std::fs::write(&path, "x").unwrap();
        (dir, path)
    }

    /// Sends the icon, then waits `delay` before the thumbnail.
    fn slow(delay: Duration, calls: Arc<AtomicUsize>) -> Extract {
        Arc::new(move |_, _, send, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            send(shot("icon", true));
            std::thread::sleep(delay);
            send(shot("thumb", false));
        })
    }

    #[test]
    fn a_thumbnail_is_returned_and_kept_until_cleared() {
        let (_d, path) = file();
        let calls = Arc::new(AtomicUsize::new(0));
        let t = Thumbnails::new(slow(Duration::ZERO, calls.clone()), TIMEOUT);
        assert_eq!(t.get(&path, 512, 1), Some(shot("thumb", false)));
        assert_eq!(t.get(&path, 512, 2), Some(shot("thumb", false)));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "the second was cached");
        t.clear();
        t.get(&path, 512, 3);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_handler_that_hangs_leaves_the_icon_after_the_timeout() {
        let (_d, path) = file();
        let calls = Arc::new(AtomicUsize::new(0));
        let t = Thumbnails::new(
            slow(Duration::from_secs(5), calls.clone()),
            Duration::from_millis(100),
        );
        let started = Instant::now();
        assert_eq!(t.get(&path, 512, 1), Some(shot("icon", true)));
        assert!(started.elapsed() < Duration::from_secs(3));
        // Nothing but the icon came, so it is not cached; a hung worker counts
        // until there are too many, and then no more are started.
        for n in 2..=3 {
            assert!(t.get(&path, 512, n).is_some());
        }
        assert_eq!(t.get(&path, 512, 4), None);
        assert_eq!(calls.load(Ordering::SeqCst), MAX_WORKERS);
    }

    #[test]
    fn a_handler_that_gives_nothing_or_panics_gives_nothing() {
        let (_d, path) = file();
        let t = Thumbnails::new(Arc::new(|_, _, _, _| {}), TIMEOUT);
        assert_eq!(t.get(&path, 512, 1), None);
        let t = Thumbnails::new(Arc::new(|_, _, _, _| panic!("bad handler")), TIMEOUT);
        assert_eq!(t.get(&path, 512, 1), None);
        assert_eq!(t.get(&path.with_extension("gone"), 512, 2), None);
    }

    #[test]
    fn cancelling_stops_the_wait_and_old_requests_do_not_start() {
        let (_d, path) = file();
        let calls = Arc::new(AtomicUsize::new(0));
        let t = Arc::new(Thumbnails::new(
            slow(Duration::from_secs(5), calls.clone()),
            Duration::from_secs(30),
        ));
        let waiting = {
            let (t, path) = (t.clone(), path.clone());
            std::thread::spawn(move || t.get(&path, 512, 7))
        };
        while calls.load(Ordering::SeqCst) == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
        let started = Instant::now();
        t.cancel(7);
        assert_eq!(waiting.join().unwrap(), None);
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(t.get(&path, 512, 6), None);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn shell_pixels_become_a_bmp_with_straight_alpha() {
        // One half-transparent premultiplied pixel, one opaque.
        let out = bmp(2, 1, vec![64, 0, 0, 128, 1, 2, 3, 255]);
        assert_eq!(&out[..2], b"BM");
        assert_eq!(out.len(), 122 + 8);
        assert_eq!(&out[10..14], &122u32.to_le_bytes());
        assert_eq!(&out[122..], &[127, 0, 0, 128, 1, 2, 3, 255]);
        // No alpha channel at all: opaque, not invisible.
        let out = bmp(1, 1, vec![9, 8, 7, 0]);
        assert_eq!(&out[122..], &[9, 8, 7, 255]);
    }

    #[cfg(windows)]
    #[test]
    fn windows_gives_a_picture_of_a_real_file() {
        let (_d, path) = file();
        let t = Thumbnails::new(Arc::new(shell), Duration::from_secs(30));
        let got = t.get(&path, 256, 1).expect("at least the icon");
        assert!(got.image.starts_with("data:image/bmp;base64,Qk"));
    }
}
