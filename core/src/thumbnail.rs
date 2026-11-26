//! Thumbnails and file icons from the Windows shell, for the file preview.
//!
//! The pictures come from whatever thumbnail handler is installed for a type:
//! third-party code. It never runs in Lookout's process. Each request starts
//! one helper (this same program with [`HELPER_ARG`]), which reads the request
//! from its input, checks the skip rules again, writes the file's icon and then
//! its thumbnail as PNG, and exits. The helper is killed after [`TIMEOUT`] or
//! when the selection moves on; after [`MAX_CRASHES`] crashes in a row only
//! icons are asked for, until Lookout restarts. No helper runs otherwise.
//! Nothing is written to disk (Windows keeps its own thumbnail cache), and the
//! few pictures kept in memory are dropped when the bar has been hidden a while.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::exclude::Excludes;
use crate::preview::{self, base64};
use crate::share::Rules;

/// The argument that makes the program a thumbnail helper.
pub const HELPER_ARG: &str = "--thumbnail-helper";
pub const TIMEOUT: Duration = Duration::from_secs(2);
/// Largest side asked of a handler, in pixels.
pub const MAX_SIDE: u32 = 1024;
const MIN_SIDE: u32 = 64;
/// Crashes in a row after which thumbnails stop for the rest of the session.
pub const MAX_CRASHES: u32 = 3;
const CACHE_ITEMS: usize = 8;
const CACHE_BYTES: usize = 12 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(20);
/// A helper whose parent is gone ends itself after this long.
const WATCHDOG: Duration = Duration::from_secs(10);
const MAX_REQUEST: u64 = 1024 * 1024;
const MAX_PICTURE: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Shot {
    /// `data:` URL of the picture.
    pub image: String,
    /// The file type's icon: there was no thumbnail.
    pub icon: bool,
}

/// What the helper is told, as JSON on its input.
#[derive(Serialize, Deserialize)]
struct Request {
    path: PathBuf,
    side: u32,
    icon_only: bool,
    /// The skip rules: indexed folders, and what the user excluded.
    roots: Vec<PathBuf>,
    names: Vec<String>,
    folders: Vec<PathBuf>,
}

/// The command that starts a helper.
pub type Launch = Arc<dyn Fn() -> std::io::Result<Command> + Send + Sync>;

type Key = (PathBuf, u32, u64, Option<SystemTime>);

pub struct Thumbnails {
    launch: Launch,
    timeout: Duration,
    /// Helpers that crashed since the last one that did not.
    crashes: AtomicU32,
    /// Requests numbered up to this one are no longer wanted.
    cancelled: AtomicU64,
    /// Oldest first.
    cache: Mutex<Vec<(Key, Shot)>>,
}

impl Default for Thumbnails {
    fn default() -> Self {
        let this_program = || {
            let mut cmd = Command::new(std::env::current_exe()?);
            cmd.arg(HELPER_ARG);
            Ok(cmd)
        };
        Self::new(Arc::new(this_program), TIMEOUT)
    }
}

/// Kills the helper and collects it, however the request ends.
struct Helper(Child);

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Thumbnails {
    pub fn new(launch: Launch, timeout: Duration) -> Self {
        Self {
            launch,
            timeout,
            crashes: AtomicU32::new(0),
            cancelled: AtomicU64::new(0),
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

    /// Whether too many helpers crashed and only icons are asked for.
    pub fn icons_only(&self) -> bool {
        self.crashes.load(Ordering::Acquire) >= MAX_CRASHES
    }

    /// A thumbnail of `path`, else its icon, else `None`. Blocks for at most
    /// the timeout. Requests are numbered upwards by the caller.
    pub fn get(&self, path: &Path, side: u32, request: u64, rules: &Rules) -> Option<Shot> {
        let side = side.clamp(MIN_SIDE, MAX_SIDE);
        let meta = std::fs::metadata(path).ok()?;
        let key: Key = (path.to_path_buf(), side, meta.len(), meta.modified().ok());
        if let Some(shot) = self.cached(&key) {
            return Some(shot);
        }
        let unwanted = || self.cancelled.load(Ordering::Acquire) >= request;
        if unwanted() {
            return None;
        }
        let icon_only = self.icons_only();
        let (names, folders) = rules.excludes.lists();
        let message = serde_json::to_vec(&Request {
            path: path.to_path_buf(),
            side,
            icon_only,
            roots: rules.roots.clone(),
            names,
            folders,
        })
        .ok()?;

        let deadline = Instant::now() + self.timeout;
        let mut command = (self.launch)().ok()?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut helper = Helper(command.spawn().ok()?);
        let (mut input, output) = (helper.0.stdin.take()?, helper.0.stdout.take()?);
        let (tx, rx) = mpsc::channel::<Shot>();
        // Writing and reading both wait on the helper, so they have their own thread.
        std::thread::Builder::new()
            .name("lookout-thumb".into())
            .spawn(move || {
                if input.write_all(&message).is_err() {
                    return;
                }
                drop(input);
                let mut output = output;
                while let Some(shot) = read_frame(&mut output) {
                    if tx.send(shot).is_err() {
                        break;
                    }
                }
            })
            .ok()?;

        let mut best: Option<Shot> = None;
        let mut ended = false;
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
                Err(RecvTimeoutError::Disconnected) => {
                    ended = true;
                    break;
                }
            }
        }
        let thumbnail = best.as_ref().is_some_and(|s| !s.icon);
        // Only an exit of its own tells a crash: a late helper is just killed.
        let crashed = !thumbnail
            && ended
            && loop {
                match helper.0.try_wait() {
                    Ok(Some(status)) => break !status.success(),
                    Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
                    _ => return best,
                }
            };
        if crashed {
            self.crashes.fetch_add(1, Ordering::AcqRel);
        } else if !icon_only && (thumbnail || ended) {
            self.crashes.store(0, Ordering::Release);
        }
        // An icon that stood in for a thumbnail is asked for again next time.
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

/// One picture from the helper: 1 for an icon or 0 for a thumbnail, the PNG's
/// length, the PNG. `None` at the end, or on anything else.
fn read_frame(from: &mut impl Read) -> Option<Shot> {
    let mut head = [0u8; 5];
    from.read_exact(&mut head).ok()?;
    let [kind, a, b, c, d] = head;
    let len = u32::from_le_bytes([a, b, c, d]) as usize;
    if kind > 1 || len == 0 || len > MAX_PICTURE {
        return None;
    }
    let mut png = vec![0u8; len];
    from.read_exact(&mut png).ok()?;
    Some(Shot {
        image: format!("data:image/png;base64,{}", base64(&png)),
        icon: kind == 1,
    })
}

fn write_frame(to: &mut impl Write, icon: bool, png: &[u8]) -> std::io::Result<()> {
    let len = u32::try_from(png.len()).unwrap_or(u32::MAX);
    to.write_all(&[u8::from(icon)])?;
    to.write_all(&len.to_le_bytes())?;
    to.write_all(png)?;
    to.flush()
}

/// The whole life of a helper: one request from the input, its pictures to the
/// output. `main` returns right after.
pub fn helper_main() {
    serve(shell);
}

/// [`helper_main`] with the pictures made by `make` (tests give their own):
/// `make(path, side, icon_only, send)` sends the icon first.
pub fn serve(make: impl FnOnce(&Path, u32, bool, &dyn Fn(bool, Vec<u8>))) {
    quiet_crashes();
    let _ = std::thread::Builder::new().spawn(|| {
        std::thread::sleep(WATCHDOG);
        std::process::exit(0);
    });
    let mut message = Vec::new();
    if std::io::stdin()
        .lock()
        .take(MAX_REQUEST)
        .read_to_end(&mut message)
        .is_err()
    {
        return;
    }
    let Ok(r) = serde_json::from_slice::<Request>(&message) else {
        return;
    };
    // Checked again here, with the built-in rules always in force.
    let rules = Rules {
        excludes: Excludes::default()
            .with_extra(r.names)
            .with_folders(r.folders),
        roots: r.roots,
    };
    if !preview::may_thumbnail(&r.path, &rules) {
        return;
    }
    let side = r.side.clamp(MIN_SIDE, MAX_SIDE);
    make(&r.path, side, r.icon_only, &|icon, png| {
        let _ = write_frame(&mut std::io::stdout().lock(), icon, &png);
    });
}

/// A crashing handler ends the helper without a Windows error box.
#[cfg(windows)]
fn quiet_crashes() {
    #[link(name = "kernel32")]
    extern "system" {
        fn SetErrorMode(mode: u32) -> u32;
    }
    // SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX
    // SAFETY: sets a flag of this process only.
    unsafe {
        SetErrorMode(0x0001 | 0x0002 | 0x8000);
    }
}

#[cfg(not(windows))]
fn quiet_crashes() {}

#[cfg(not(windows))]
pub fn shell(_: &Path, _: u32, _: bool, _: &dyn Fn(bool, Vec<u8>)) {}

/// Top-down BGRA pixels.
#[cfg(windows)]
struct Pixels {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

/// The shell's icon for the file, then the installed handler's thumbnail. The
/// handlers run on a thread of their own (they expect a single-threaded
/// apartment); PNG encoding stays on this one.
#[cfg(windows)]
pub fn shell(path: &Path, side: u32, icon_only: bool, send: &dyn Fn(bool, Vec<u8>)) {
    use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

    let (tx, rx) = mpsc::channel::<(bool, Pixels)>();
    let path = path.to_path_buf();
    let handlers = std::thread::Builder::new().spawn(move || pictures(&path, side, icon_only, &tx));
    if handlers.is_err() {
        return;
    }
    // SAFETY: plain initialisation of this thread; an error only means it has
    // an apartment already.
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    for (icon, pixels) in rx {
        if let Ok(png) = png(pixels) {
            send(icon, png);
        }
    }
}

#[cfg(windows)]
fn pictures(path: &Path, side: u32, icon_only: bool, send: &mpsc::Sender<(bool, Pixels)>) {
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
            let wanted = [
                (ICON, SIIGBF_ICONONLY, true),
                (side, SIIGBF_THUMBNAILONLY, false),
            ];
            for (side, flags, icon) in wanted.into_iter().take(if icon_only { 1 } else { 2 }) {
                let size = windows::Win32::Foundation::SIZE {
                    cx: side as i32,
                    cy: side as i32,
                };
                // SAFETY: a plain COM call; `pixels` takes ownership of the bitmap.
                let Ok(bitmap) = (unsafe { factory.GetImage(size, flags) }) else {
                    continue;
                };
                if let Some(picture) = pixels(bitmap) {
                    let _ = send.send((icon, picture));
                }
            }
        }
    }
    // SAFETY: every COM object above is released by now.
    unsafe { CoUninitialize() };
}

/// The bitmap's pixels, top row first. Deletes the bitmap.
#[cfg(windows)]
fn pixels(bitmap: windows::Win32::Graphics::Gdi::HBITMAP) -> Option<Pixels> {
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
                // Negative: the top row first.
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut data = vec![0u8; w as usize * h as usize * 4];
        // SAFETY: the screen context is released right after; `data` holds `h`
        // rows of `w` 32-bit pixels, as the header asks.
        let rows = unsafe {
            let dc = GetDC(None);
            let rows = GetDIBits(
                dc,
                bitmap,
                0,
                h as u32,
                Some(data.as_mut_ptr().cast()),
                &mut header,
                DIB_RGB_COLORS,
            );
            ReleaseDC(None, dc);
            rows
        };
        (rows == h).then_some(Pixels {
            width: w as u32,
            height: h as u32,
            data,
        })
    };
    let out = read();
    // SAFETY: the bitmap is ours and no longer used.
    unsafe {
        let _ = DeleteObject(bitmap.into());
    }
    out
}

/// A bitmap without any alpha is opaque, not invisible.
#[cfg(windows)]
fn opaque_if_no_alpha(data: &mut [u8]) {
    if data.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
        for p in data.as_chunks_mut::<4>().0 {
            p[3] = 255;
        }
    }
}

/// PNG through the Windows encoder, in memory. The shell's colours come
/// multiplied by alpha.
#[cfg(windows)]
fn png(mut picture: Pixels) -> windows::core::Result<Vec<u8>> {
    use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapEncoder, BitmapPixelFormat};
    use windows::Storage::Streams::{DataReader, InMemoryRandomAccessStream};

    opaque_if_no_alpha(&mut picture.data);
    let stream = InMemoryRandomAccessStream::new()?;
    let encoder = BitmapEncoder::CreateAsync(BitmapEncoder::PngEncoderId()?, &stream)?.join()?;
    encoder.SetPixelData(
        BitmapPixelFormat::Bgra8,
        BitmapAlphaMode::Premultiplied,
        picture.width,
        picture.height,
        96.0,
        96.0,
        &picture.data,
    )?;
    encoder.FlushAsync()?.join()?;
    let len = u32::try_from(stream.Size()?).unwrap_or(u32::MAX);
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
    reader.LoadAsync(len)?.join()?;
    let mut out = vec![0u8; len as usize];
    reader.ReadBytes(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_carry_pictures_and_anything_else_ends_the_reading() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, true, &[1, 2, 3]).unwrap();
        write_frame(&mut bytes, false, &[4]).unwrap();
        let mut from = bytes.as_slice();
        let icon = read_frame(&mut from).unwrap();
        assert!(icon.icon && icon.image == "data:image/png;base64,AQID");
        assert!(!read_frame(&mut from).unwrap().icon);
        assert_eq!(read_frame(&mut from), None);
        for bad in [
            &[7, 1, 0, 0, 0, 9][..],
            &[0, 0, 0, 0, 0],
            &[0, 255, 255, 255, 255],
            &[0, 9, 0, 0, 0, 1],
        ] {
            assert_eq!(read_frame(&mut { bad }), None, "{bad:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn shell_pixels_become_a_png() {
        let mut data = vec![9, 8, 7, 0];
        opaque_if_no_alpha(&mut data);
        assert_eq!(data, [9, 8, 7, 255]);
        let mut data = vec![9, 8, 7, 0, 1, 1, 1, 128];
        opaque_if_no_alpha(&mut data);
        assert_eq!(data[3], 0, "some alpha: left as it is");
        let out = png(Pixels {
            width: 2,
            height: 1,
            data,
        })
        .unwrap();
        assert_eq!(&out[..8], b"\x89PNG\r\n\x1a\n");
    }
}
