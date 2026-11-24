//! One-click setup of the semantic search files (model, tokenizer, runtime).
//!
//! The only network code in Lookout. It runs only when the user clicks the
//! button (never at startup, never in the background), over HTTPS, to the
//! pinned hosts only, and it only receives files: a plain GET with no cookies,
//! no credentials and no headers of our own. Every file is hashed while it is
//! written and compared with its pin BEFORE it is used; the three finished
//! files are moved into the plugin's assets folder only when all of them passed.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use sha2::{Digest, Sha256};

pub const MODEL_FILE: &str = "model.onnx";
pub const TOKENIZER_FILE: &str = "tokenizer.json";
pub const RUNTIME_FILE: &str = "onnxruntime.dll";
const ASSET_FILES: [&str; 3] = [MODEL_FILE, TOKENIZER_FILE, RUNTIME_FILE];

/// Where the runtime sits inside the official wheel.
const WHEEL_DLL: &str = "onnxruntime/capi/onnxruntime.dll";
/// Upper bound for the extracted runtime (the real one is far smaller).
const MAX_DLL_BYTES: u64 = 128 * 1024 * 1024;
/// Room for the extracted runtime and a little slack, on top of the downloads.
const EXTRA_SPACE: u64 = 64 * 1024 * 1024;
#[cfg(all(windows, feature = "download"))]
const MAX_REDIRECTS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupError {
    Offline,
    Unreachable(String),
    Http(String, u32),
    Secure(String),
    HashMismatch(String),
    DiskFull,
    NotEnoughSpace { need: u64, have: u64 },
    Cancelled,
    Other(String),
}

fn mb(n: u64) -> u64 {
    n.div_ceil(1_000_000)
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Offline => write!(f, "You appear to be offline. Connect to the internet and try again. Nothing was installed."),
            Self::Unreachable(h) => write!(f, "Could not reach {h}. Try again later. Nothing was installed."),
            Self::Http(h, c) => write!(f, "{h} answered with error {c}. Try again later. Nothing was installed."),
            Self::Secure(h) => write!(f, "The secure connection to {h} could not be verified, so nothing was downloaded."),
            Self::HashMismatch(n) => write!(f, "The downloaded {n} did not match its expected SHA-256, so it was thrown away and not used. Nothing was installed."),
            Self::DiskFull => write!(f, "The disk is full. Free some space and try again. Nothing was installed."),
            Self::NotEnoughSpace { need, have } => write!(f, "Not enough free disk space: about {} MB needed, {} MB free. Nothing was downloaded.", mb(*need), have / 1_000_000),
            Self::Cancelled => write!(f, "Cancelled. Nothing was installed."),
            Self::Other(m) => write!(f, "Setup failed: {m}. Nothing was installed."),
        }
    }
}

fn io_err(e: &std::io::Error) -> SetupError {
    // ERROR_DISK_FULL, ERROR_HANDLE_DISK_FULL
    if matches!(e.raw_os_error(), Some(112 | 39)) {
        SetupError::DiskFull
    } else {
        SetupError::Other(e.to_string())
    }
}

/// One file to receive: where from, its exact size and its SHA-256.
#[derive(Debug, Clone)]
pub struct Download {
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

/// What is fetched and what must come out of it.
#[derive(Debug, Clone)]
pub struct Plan {
    pub model: Download,
    pub tokenizer: Download,
    /// The official `onnxruntime` wheel (a zip); only one dll is taken from it.
    pub wheel: Download,
    pub runtime_sha256: String,
}

impl Plan {
    /// The pins. Kept equal to `plugins/semantic/src/assets.rs` (a test checks it).
    pub fn pinned() -> Self {
        Self {
            model: Download {
                url: "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/ea104dacec62c0de699686887e3f920caeb4f3e3/onnx/model_quantized.onnx".into(),
                size: 34_014_426,
                sha256: "6c9c6101a956d62dfb5e7190c538226c0c5bb9cb27b651234b6df063ee7dbfe4".into(),
            },
            tokenizer: Download {
                url: "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/ea104dacec62c0de699686887e3f920caeb4f3e3/tokenizer.json".into(),
                size: 711_396,
                sha256: "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66".into(),
            },
            wheel: Download {
                url: "https://files.pythonhosted.org/packages/a9/3e/27039badc39a1b01628b6b40b1ff1bf4a886e6dfb2f311c483606b7c86f7/onnxruntime-1.32.0-cp312-cp312-win_amd64.whl".into(),
                size: 16_698_767,
                sha256: "efe7456952c243a05a53d5b8732953625144642c2f3989094277f6217f0c0c40".into(),
            },
            runtime_sha256: "443c0065fcdba17618ddef05050308f4b471893c5c7da0337d6957c9073ad8a7".into(),
        }
    }

    pub fn total_bytes(&self) -> u64 {
        self.model.size + self.tokenizer.size + self.wheel.size
    }
}

/// Hosts a download may touch: the pinned ones, plus the CDN names Hugging Face
/// redirects to (`*.hf.co`). Everything must be `https://`.
pub fn host_allowed(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    h == "huggingface.co"
        || h == "files.pythonhosted.org"
        || h.ends_with(".hf.co")
        || h.ends_with(".huggingface.co")
}

/// Splits an `https://host/path` URL; refuses anything else.
pub fn check_url(url: &str) -> Result<(String, String), SetupError> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| SetupError::Other("refusing a non-HTTPS address".into()))?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let plain = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    if !plain || !host_allowed(host) {
        return Err(SetupError::Other(format!("refusing the host {host:?}")));
    }
    Ok((host.to_ascii_lowercase(), path.to_owned()))
}

/// Receives a file. Implementations follow redirects only through
/// [`check_url`] and hand every chunk to `sink`; an error from `sink` stops the transfer.
pub trait Transport: Send + Sync {
    fn get(
        &self,
        url: &str,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), SetupError>,
    ) -> Result<(), SetupError>;
}

/// The data folder's assets folder of a plugin (what the helper reads from).
pub fn assets_dir(data_root: &Path, plugin_id: &str) -> PathBuf {
    data_root.join("plugins").join(plugin_id).join("assets")
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    Idle,
    Downloading,
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupPhase {
    NotSetUp,
    Downloading,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetupStatus {
    pub phase: SetupPhase,
    pub message: Option<String>,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub current: Option<String>,
}

/// What the confirmation dialog shows before anything is downloaded.
#[derive(Debug, Clone, Serialize)]
pub struct SetupInfo {
    pub total_bytes: u64,
    pub files: Vec<(String, u64)>,
    pub hosts: Vec<String>,
}

pub fn info() -> SetupInfo {
    let p = Plan::pinned();
    SetupInfo {
        total_bytes: p.total_bytes(),
        files: vec![
            ("Model (bge-small-en-v1.5, int8)".into(), p.model.size),
            ("Tokenizer".into(), p.tokenizer.size),
            (
                "ONNX Runtime 1.32.0 (official package)".into(),
                p.wheel.size,
            ),
        ],
        hosts: vec!["huggingface.co".into(), "files.pythonhosted.org".into()],
    }
}

pub struct SemanticSetup {
    assets: PathBuf,
    temp: PathBuf,
    phase: Mutex<Phase>,
    cancel: AtomicBool,
    done: AtomicU64,
    total: AtomicU64,
    current: Mutex<Option<String>>,
}

impl SemanticSetup {
    /// `data_root` is Lookout's data folder; the temp folder lives inside it.
    pub fn new(data_root: &Path, plugin_id: &str) -> Self {
        Self {
            assets: assets_dir(data_root, plugin_id),
            temp: data_root.join("semantic-download"),
            phase: Mutex::new(Phase::Idle),
            cancel: AtomicBool::new(false),
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            current: Mutex::new(None),
        }
    }

    fn files_present(&self) -> bool {
        ASSET_FILES.iter().all(|f| self.assets.join(f).is_file())
    }

    pub fn status(&self) -> SetupStatus {
        let phase = self
            .phase
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let (p, message) = match phase {
            Phase::Downloading => (SetupPhase::Downloading, None),
            Phase::Failed(m) => (SetupPhase::Failed, Some(m)),
            Phase::Cancelled => (
                SetupPhase::NotSetUp,
                Some(SetupError::Cancelled.to_string()),
            ),
            Phase::Idle if self.files_present() => (SetupPhase::Ready, None),
            Phase::Idle => (SetupPhase::NotSetUp, None),
        };
        SetupStatus {
            phase: p,
            message,
            done_bytes: self.done.load(Ordering::Relaxed),
            total_bytes: self.total.load(Ordering::Relaxed),
            current: self
                .current
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    fn set_phase(&self, p: Phase) {
        *self.phase.lock().unwrap_or_else(PoisonError::into_inner) = p;
    }

    /// Starts the download on its own thread (called only from the button).
    pub fn start(
        self: &Arc<Self>,
        plan: Plan,
        transport: Arc<dyn Transport>,
    ) -> Result<(), String> {
        {
            let mut p = self.phase.lock().unwrap_or_else(PoisonError::into_inner);
            if *p == Phase::Downloading {
                return Err("a download is already running".into());
            }
            *p = Phase::Downloading;
        }
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            me.run_blocking(&plan, transport.as_ref(), &free_space);
        });
        Ok(())
    }

    /// The whole job, with the state changes. Tests call this directly.
    pub fn run_blocking(
        &self,
        plan: &Plan,
        transport: &dyn Transport,
        free: &dyn Fn(&Path) -> std::io::Result<u64>,
    ) {
        self.set_phase(Phase::Downloading);
        self.cancel.store(false, Ordering::Release);
        self.done.store(0, Ordering::Relaxed);
        self.total.store(plan.total_bytes(), Ordering::Relaxed);
        let result = self.run_inner(plan, transport, free);
        let _ = std::fs::remove_dir_all(&self.temp);
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.set_phase(match result {
            Ok(()) => Phase::Idle,
            Err(SetupError::Cancelled) => Phase::Cancelled,
            Err(e) => Phase::Failed(e.to_string()),
        });
    }

    fn check_cancel(&self) -> Result<(), SetupError> {
        if self.cancel.load(Ordering::Acquire) {
            Err(SetupError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn run_inner(
        &self,
        plan: &Plan,
        transport: &dyn Transport,
        free: &dyn Fn(&Path) -> std::io::Result<u64>,
    ) -> Result<(), SetupError> {
        let _ = std::fs::remove_dir_all(&self.temp);
        std::fs::create_dir_all(&self.temp).map_err(|e| io_err(&e))?;
        let need = plan.total_bytes() + EXTRA_SPACE;
        if let Ok(have) = free(&self.temp) {
            if have < need {
                return Err(SetupError::NotEnoughSpace { need, have });
            }
        }
        self.fetch(transport, &plan.model, MODEL_FILE, MODEL_FILE)?;
        self.fetch(transport, &plan.tokenizer, TOKENIZER_FILE, TOKENIZER_FILE)?;
        self.fetch(transport, &plan.wheel, "runtime.whl", RUNTIME_FILE)?;
        self.check_cancel()?;
        self.extract_runtime(&plan.runtime_sha256)?;
        self.check_cancel()?;
        self.install()
    }

    /// Downloads into the temp folder, hashing as it writes; the file is kept
    /// only if size and SHA-256 match their pins.
    fn fetch(
        &self,
        transport: &dyn Transport,
        d: &Download,
        temp_name: &str,
        label: &str,
    ) -> Result<(), SetupError> {
        self.check_cancel()?;
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = Some(label.to_owned());
        let path = self.temp.join(temp_name);
        let mut file = std::fs::File::create(&path).map_err(|e| io_err(&e))?;
        let mut hasher = Sha256::new();
        let mut got = 0u64;
        let r = transport.get(&d.url, &mut |chunk| {
            self.check_cancel()?;
            got += chunk.len() as u64;
            if got > d.size {
                return Err(SetupError::HashMismatch(label.to_owned()));
            }
            hasher.update(chunk);
            file.write_all(chunk).map_err(|e| io_err(&e))?;
            self.done.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            Ok(())
        });
        drop(file);
        if let Err(e) = r {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
        if got != d.size || hex(&hasher.finalize()) != d.sha256.to_ascii_lowercase() {
            let _ = std::fs::remove_file(&path);
            return Err(SetupError::HashMismatch(label.to_owned()));
        }
        Ok(())
    }

    /// Takes the one dll out of the verified wheel and checks it against its own pin.
    fn extract_runtime(&self, pin: &str) -> Result<(), SetupError> {
        let wheel = self.temp.join("runtime.whl");
        let out = self.temp.join(RUNTIME_FILE);
        let f = std::fs::File::open(&wheel).map_err(|e| io_err(&e))?;
        let mut zip = zip::ZipArchive::new(f)
            .map_err(|e| SetupError::Other(format!("the runtime package is not readable: {e}")))?;
        let entry = zip
            .by_name(WHEEL_DLL)
            .map_err(|_| SetupError::Other("the runtime package has no onnxruntime.dll".into()))?;
        let mut limited = entry.take(MAX_DLL_BYTES + 1);
        let mut dst = std::fs::File::create(&out).map_err(|e| io_err(&e))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let n = limited
                .read(&mut buf)
                .map_err(|e| SetupError::Other(format!("could not unpack onnxruntime.dll: {e}")))?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > MAX_DLL_BYTES {
                let _ = std::fs::remove_file(&out);
                return Err(SetupError::HashMismatch(RUNTIME_FILE.into()));
            }
            hasher.update(&buf[..n]);
            dst.write_all(&buf[..n]).map_err(|e| io_err(&e))?;
        }
        drop(dst);
        if hex(&hasher.finalize()) != pin.to_ascii_lowercase() {
            let _ = std::fs::remove_file(&out);
            return Err(SetupError::HashMismatch(RUNTIME_FILE.into()));
        }
        let _ = std::fs::remove_file(&wheel);
        Ok(())
    }

    /// Moves the three verified files into the assets folder. Files that were
    /// there are set aside first and put back if anything fails.
    fn install(&self) -> Result<(), SetupError> {
        std::fs::create_dir_all(&self.assets).map_err(|e| io_err(&e))?;
        let backup = self.temp.join("previous");
        std::fs::create_dir_all(&backup).map_err(|e| io_err(&e))?;
        let mut moved_old: Vec<&str> = Vec::new();
        let mut moved_new: Vec<&str> = Vec::new();
        let result = (|| {
            for name in ASSET_FILES {
                let dest = self.assets.join(name);
                if dest.exists() {
                    std::fs::rename(&dest, backup.join(name)).map_err(|e| io_err(&e))?;
                    moved_old.push(name);
                }
            }
            for name in ASSET_FILES {
                std::fs::rename(self.temp.join(name), self.assets.join(name))
                    .map_err(|e| io_err(&e))?;
                moved_new.push(name);
            }
            Ok(())
        })();
        if result.is_err() {
            for name in &moved_new {
                let _ = std::fs::remove_file(self.assets.join(name));
            }
            for name in &moved_old {
                let _ = std::fs::rename(backup.join(name), self.assets.join(name));
            }
        }
        result
    }

    /// "Remove semantic files": deletes only the three downloaded files.
    /// The helper must not be running (the caller turns the plugin off first).
    pub fn remove_files(&self) -> Result<(), String> {
        if *self.phase.lock().unwrap_or_else(PoisonError::into_inner) == Phase::Downloading {
            return Err("a download is running; cancel it first".into());
        }
        for name in ASSET_FILES {
            match std::fs::remove_file(self.assets.join(name)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("could not remove {name}: {e}")),
            }
        }
        let _ = std::fs::remove_dir_all(&self.temp);
        self.set_phase(Phase::Idle);
        Ok(())
    }
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Free bytes for the current user on the volume that holds `path`.
#[cfg(windows)]
pub fn free_space(path: &Path) -> std::io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut free = 0u64;
    // SAFETY: `wide` is NUL-terminated and outlives the call; the out pointers are valid.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(free)
    }
}

#[cfg(not(windows))]
pub fn free_space(_path: &Path) -> std::io::Result<u64> {
    Ok(u64::MAX)
}

// ------------------------------------------------------------------ WinHTTP

/// HTTPS GET through the Windows HTTP stack (system proxy and certificates).
/// Cookies, redirects and authentication are switched off; redirects are
/// followed here, one hop at a time, each checked by [`check_url`].
#[cfg(all(windows, feature = "download"))]
pub struct WinHttp;

#[cfg(all(windows, feature = "download"))]
mod winhttp {
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Networking::WinHttp::{
        WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
        WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption,
        WinHttpSetTimeouts,
    };

    use super::{check_url, SetupError, Transport, WinHttp, MAX_REDIRECTS};

    const ACCESS_AUTOMATIC_PROXY: u32 = 4;
    const FLAG_SECURE: u32 = 0x0080_0000;
    const OPTION_DISABLE_FEATURE: u32 = 63;
    const OPTION_SECURE_PROTOCOLS: u32 = 84;
    const DISABLE_COOKIES_REDIRECTS_AUTH: u32 = 1 | 2 | 4;
    const TLS_1_2: u32 = 0x0800;
    const TLS_1_3: u32 = 0x2000;
    const QUERY_STATUS_CODE_NUMBER: u32 = 19 | 0x2000_0000;
    const QUERY_LOCATION: u32 = 33;

    struct Handle(*mut c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle came from a WinHttp* open call and is closed once.
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    fn last_error(host: &str) -> SetupError {
        match std::io::Error::last_os_error().raw_os_error() {
            // NAME_NOT_RESOLVED, INTERNET_DISCONNECTED
            Some(12007 | 12163) => SetupError::Offline,
            // SECURE_FAILURE and the certificate errors
            Some(12175 | 12037 | 12038 | 12044 | 12045) => SetupError::Secure(host.to_owned()),
            _ => SetupError::Unreachable(host.to_owned()),
        }
    }

    enum Next {
        Done,
        Redirect(String),
    }

    impl Transport for WinHttp {
        fn get(
            &self,
            url: &str,
            sink: &mut dyn FnMut(&[u8]) -> Result<(), SetupError>,
        ) -> Result<(), SetupError> {
            let mut current = url.to_owned();
            for _ in 0..=MAX_REDIRECTS {
                let (host, path) = check_url(&current)?;
                match once(&host, &path, sink)? {
                    Next::Done => return Ok(()),
                    Next::Redirect(loc) => {
                        current = if loc.starts_with('/') {
                            format!("https://{host}{loc}")
                        } else {
                            loc
                        };
                    }
                }
            }
            Err(SetupError::Other("too many redirects".into()))
        }
    }

    fn once(
        host: &str,
        path: &str,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), SetupError>,
    ) -> Result<Next, SetupError> {
        // SAFETY: every pointer passed below refers to a live, NUL-terminated
        // or correctly sized buffer owned by this function; handles are closed by `Handle`.
        unsafe {
            let agent = wide("Lookout");
            let session = Handle(WinHttpOpen(
                agent.as_ptr(),
                ACCESS_AUTOMATIC_PROXY,
                null(),
                null(),
                0,
            ));
            if session.0.is_null() {
                return Err(last_error(host));
            }
            WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 15_000);
            for protos in [TLS_1_2 | TLS_1_3, TLS_1_2] {
                if WinHttpSetOption(
                    session.0,
                    OPTION_SECURE_PROTOCOLS,
                    std::ptr::from_ref(&protos).cast(),
                    4,
                ) != 0
                {
                    break;
                }
            }
            let host_w = wide(host);
            let conn = Handle(WinHttpConnect(session.0, host_w.as_ptr(), 443, 0));
            if conn.0.is_null() {
                return Err(last_error(host));
            }
            let verb = wide("GET");
            let path_w = wide(path);
            let req = Handle(WinHttpOpenRequest(
                conn.0,
                verb.as_ptr(),
                path_w.as_ptr(),
                null(),
                null(),
                null_mut(),
                FLAG_SECURE,
            ));
            if req.0.is_null() {
                return Err(last_error(host));
            }
            let off = DISABLE_COOKIES_REDIRECTS_AUTH;
            WinHttpSetOption(
                req.0,
                OPTION_DISABLE_FEATURE,
                std::ptr::from_ref(&off).cast(),
                4,
            );
            if WinHttpSendRequest(req.0, null(), 0, null(), 0, 0, 0) == 0
                || WinHttpReceiveResponse(req.0, null_mut()) == 0
            {
                return Err(last_error(host));
            }
            let mut code = 0u32;
            let mut len = 4u32;
            if WinHttpQueryHeaders(
                req.0,
                QUERY_STATUS_CODE_NUMBER,
                null(),
                std::ptr::from_mut(&mut code).cast(),
                &mut len,
                null_mut(),
            ) == 0
            {
                return Err(last_error(host));
            }
            match code {
                200 => {}
                301 | 302 | 303 | 307 | 308 => {
                    let mut buf = vec![0u16; 4096];
                    let mut blen = (buf.len() * 2) as u32;
                    if WinHttpQueryHeaders(
                        req.0,
                        QUERY_LOCATION,
                        null(),
                        buf.as_mut_ptr().cast(),
                        &mut blen,
                        null_mut(),
                    ) == 0
                    {
                        return Err(SetupError::Http(host.to_owned(), code));
                    }
                    let n = (blen as usize / 2).min(buf.len());
                    return Ok(Next::Redirect(String::from_utf16_lossy(&buf[..n])));
                }
                other => return Err(SetupError::Http(host.to_owned(), other)),
            }
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let mut read = 0u32;
                if WinHttpReadData(req.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut read) == 0
                {
                    return Err(last_error(host));
                }
                if read == 0 {
                    return Ok(Next::Done);
                }
                sink(&buf[..read as usize])?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::AtomicUsize;

    fn sha(b: &[u8]) -> String {
        hex(&Sha256::digest(b))
    }

    /// A wheel-shaped zip holding the runtime under its real inner path.
    fn wheel_with(dll: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut z = zip::ZipWriter::new(std::io::Cursor::new(&mut out));
            let o = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            z.start_file("onnxruntime/__init__.py", o).unwrap();
            z.write_all(b"# not needed").unwrap();
            z.start_file(WHEEL_DLL, o).unwrap();
            z.write_all(dll).unwrap();
            z.finish().unwrap();
        }
        out
    }

    struct Fixture {
        plan: Plan,
        files: HashMap<String, Vec<u8>>,
    }

    fn fixture() -> Fixture {
        let (m, t, dll) = (vec![1u8; 5000], vec![2u8; 300], vec![3u8; 4000]);
        let wheel = wheel_with(&dll);
        let d = |u: &str, b: &[u8]| Download {
            url: format!("https://huggingface.co/{u}"),
            size: b.len() as u64,
            sha256: sha(b),
        };
        let plan = Plan {
            model: d("m", &m),
            tokenizer: d("t", &t),
            wheel: d("w", &wheel),
            runtime_sha256: sha(&dll),
        };
        let files = HashMap::from([
            (plan.model.url.clone(), m),
            (plan.tokenizer.url.clone(), t),
            (plan.wheel.url.clone(), wheel),
        ]);
        Fixture { plan, files }
    }

    /// In-memory transport. `fail` makes every call fail; `cancel_after` calls
    /// `cancel()` once that many chunks went out.
    struct Fake {
        files: HashMap<String, Vec<u8>>,
        fail: Option<SetupError>,
        calls: AtomicUsize,
        cancel_on: Option<(Arc<SemanticSetup>, usize)>,
        seen_status: Mutex<Vec<SetupPhase>>,
        probe: Option<Arc<SemanticSetup>>,
    }

    impl Fake {
        fn new(files: HashMap<String, Vec<u8>>) -> Self {
            Self {
                files,
                fail: None,
                calls: AtomicUsize::new(0),
                cancel_on: None,
                seen_status: Mutex::new(Vec::new()),
                probe: None,
            }
        }
    }

    impl Transport for Fake {
        fn get(
            &self,
            url: &str,
            sink: &mut dyn FnMut(&[u8]) -> Result<(), SetupError>,
        ) -> Result<(), SetupError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(p) = &self.probe {
                self.seen_status.lock().unwrap().push(p.status().phase);
            }
            if let Some(e) = &self.fail {
                return Err(e.clone());
            }
            let body = self
                .files
                .get(url)
                .ok_or(SetupError::Http("x".into(), 404))?;
            for (i, c) in body.chunks(1000).enumerate() {
                if let Some((s, n)) = &self.cancel_on {
                    if i == *n {
                        s.cancel();
                    }
                }
                sink(c)?;
            }
            Ok(())
        }
    }

    fn plenty(_: &Path) -> std::io::Result<u64> {
        Ok(u64::MAX)
    }

    fn setup_in(d: &Path) -> Arc<SemanticSetup> {
        Arc::new(SemanticSetup::new(d, "com.lookout.semantic"))
    }

    fn assets_empty(s: &SemanticSetup) -> bool {
        !s.assets.exists() || std::fs::read_dir(&s.assets).unwrap().next().is_none()
    }

    #[test]
    fn good_files_are_verified_installed_and_status_goes_ready() {
        let d = tempfile::tempdir().unwrap();
        let s = setup_in(d.path());
        let fx = fixture();
        assert_eq!(s.status().phase, SetupPhase::NotSetUp);
        let mut t = Fake::new(fx.files.clone());
        t.probe = Some(s.clone());
        s.run_blocking(&fx.plan, &t, &plenty);
        assert_eq!(s.status().phase, SetupPhase::Ready);
        assert_eq!(
            *t.seen_status.lock().unwrap(),
            vec![SetupPhase::Downloading; 3]
        );
        assert_eq!(
            sha(&std::fs::read(s.assets.join(MODEL_FILE)).unwrap()),
            fx.plan.model.sha256
        );
        assert_eq!(
            sha(&std::fs::read(s.assets.join(RUNTIME_FILE)).unwrap()),
            fx.plan.runtime_sha256
        );
        assert!(!s.temp.exists(), "temp folder is removed");
        assert_eq!(s.status().done_bytes, fx.plan.total_bytes());
    }

    #[test]
    fn a_hash_mismatch_is_rejected_and_nothing_is_installed() {
        for which in ["model", "wheel"] {
            let d = tempfile::tempdir().unwrap();
            let s = setup_in(d.path());
            let mut fx = fixture();
            let url = if which == "model" {
                &fx.plan.model.url
            } else {
                &fx.plan.wheel.url
            };
            let body = fx.files.get_mut(url).unwrap();
            let n = body.len();
            body[n / 2] ^= 0xff; // same size, different bytes
            s.run_blocking(&fx.plan, &Fake::new(fx.files.clone()), &plenty);
            let st = s.status();
            assert_eq!(st.phase, SetupPhase::Failed, "{which}");
            assert!(st.message.unwrap().contains("SHA-256"));
            assert!(assets_empty(&s) && !s.temp.exists(), "{which}");
        }
    }

    #[test]
    fn a_runtime_that_does_not_match_its_own_pin_is_rejected() {
        let d = tempfile::tempdir().unwrap();
        let s = setup_in(d.path());
        let mut fx = fixture();
        fx.plan.runtime_sha256 = sha(b"some other dll");
        s.run_blocking(&fx.plan, &Fake::new(fx.files.clone()), &plenty);
        assert_eq!(s.status().phase, SetupPhase::Failed);
        assert!(assets_empty(&s) && !s.temp.exists());
    }

    #[test]
    fn cancelling_leaves_no_files() {
        let d = tempfile::tempdir().unwrap();
        let s = setup_in(d.path());
        let fx = fixture();
        let mut t = Fake::new(fx.files.clone());
        t.cancel_on = Some((s.clone(), 2));
        s.run_blocking(&fx.plan, &t, &plenty);
        let st = s.status();
        assert_eq!(st.phase, SetupPhase::NotSetUp);
        assert!(st.message.unwrap().contains("Cancelled"));
        assert!(assets_empty(&s) && !s.temp.exists());
    }

    #[test]
    fn offline_and_unreachable_fail_cleanly_with_clear_messages() {
        for (err, word) in [
            (SetupError::Offline, "offline"),
            (
                SetupError::Unreachable("huggingface.co".into()),
                "Could not reach huggingface.co",
            ),
        ] {
            let d = tempfile::tempdir().unwrap();
            let s = setup_in(d.path());
            let fx = fixture();
            let mut t = Fake::new(fx.files.clone());
            t.fail = Some(err);
            s.run_blocking(&fx.plan, &t, &plenty);
            let st = s.status();
            assert_eq!(st.phase, SetupPhase::Failed);
            assert!(st.message.unwrap().contains(word));
            assert!(assets_empty(&s) && !s.temp.exists());
        }
    }

    #[test]
    fn too_little_disk_space_is_reported_before_any_download() {
        let d = tempfile::tempdir().unwrap();
        let s = setup_in(d.path());
        let fx = fixture();
        let t = Fake::new(fx.files.clone());
        s.run_blocking(&fx.plan, &t, &|_| Ok(1000));
        assert_eq!(t.calls.load(Ordering::SeqCst), 0);
        let st = s.status();
        assert_eq!(st.phase, SetupPhase::Failed);
        assert!(st.message.unwrap().contains("Not enough free disk space"));
        assert!(SetupError::DiskFull.to_string().contains("disk is full"));
        assert_eq!(
            io_err(&std::io::Error::from_raw_os_error(112)),
            SetupError::DiskFull
        );
    }

    #[test]
    fn remove_deletes_only_the_downloaded_files() {
        let d = tempfile::tempdir().unwrap();
        let s = setup_in(d.path());
        let fx = fixture();
        s.run_blocking(&fx.plan, &Fake::new(fx.files.clone()), &plenty);
        let vectors = d.path().join("plugins/com.lookout.semantic/vectors.log");
        std::fs::write(&vectors, b"v").unwrap();
        std::fs::write(s.assets.join("notes.txt"), b"mine").unwrap();
        s.remove_files().unwrap();
        assert_eq!(s.status().phase, SetupPhase::NotSetUp);
        assert!(vectors.exists() && s.assets.join("notes.txt").exists());
        assert!(!s.assets.join(MODEL_FILE).exists());
    }

    #[test]
    fn only_https_to_pinned_hosts_is_accepted() {
        assert!(check_url("https://huggingface.co/a/b").is_ok());
        assert!(check_url("https://us.aws.cdn.hf.co/x?y=1").is_ok());
        assert!(check_url("https://files.pythonhosted.org/p.whl").is_ok());
        for bad in [
            "http://huggingface.co/a",
            "https://evilhf.co/a",
            "https://huggingface.co.evil.com/a",
            "https://user@huggingface.co/a",
            "https://example.com/a",
            "ftp://huggingface.co/a",
        ] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn pins_equal_the_ones_the_helper_checks() {
        let src = include_str!("../../../plugins/semantic/src/assets.rs");
        let p = Plan::pinned();
        for h in [&p.model.sha256, &p.tokenizer.sha256, &p.runtime_sha256] {
            assert!(src.contains(h.as_str()), "{h} is not in the helper's pins");
        }
    }

    /// Real network. Downloads about 51 MB from the pinned hosts into a
    /// temporary folder and checks the pins. Run by hand:
    /// `cargo test -p lookout-core --features download --lib semantic::setup::tests::real_download -- --ignored --nocapture`
    #[test]
    #[ignore = "uses the real network (about 51 MB)"]
    #[cfg(all(windows, feature = "download"))]
    fn real_download() {
        let d = tempfile::tempdir().unwrap();
        let s = setup_in(d.path());
        s.run_blocking(&Plan::pinned(), &WinHttp, &free_space);
        let st = s.status();
        assert_eq!(st.phase, SetupPhase::Ready, "{:?}", st.message);
        for f in ASSET_FILES {
            println!(
                "{f}: {} bytes",
                std::fs::metadata(s.assets.join(f)).unwrap().len()
            );
        }
    }
}
