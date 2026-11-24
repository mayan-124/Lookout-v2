//! Fast scan: the Disk page's tree built from an NTFS drive's file table.
//!
//! Lookout itself never runs as administrator. Reading a drive's file table
//! needs it, so a separate small read-only program (`lookout-fastscan.exe`, the
//! "helper") is started through Windows' permission prompt (UAC). It is given
//! only a drive letter and a random pipe name, and streams one record per
//! folder and per file name back over that private named pipe. This module
//! turns the stream into the same [`Tree`] the normal scan builds.
//!
//! Nothing touches the disk: the pipe is memory only. If the user says no to
//! the prompt, the scan fails with a clear message and the normal scan stays
//! available. Closing Lookout (or pressing Cancel) closes the pipe, which ends
//! the helper.

use std::collections::HashMap;
use std::io::Read;
use std::path::Component;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_CANCELLED, ERROR_NO_DATA, ERROR_PIPE_CONNECTED,
    ERROR_PIPE_LISTENING, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::Cryptography::{
    BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    GetDriveTypeW, GetVolumeInformationW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE,
    PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PeekNamedPipe, PIPE_NOWAIT, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
};
use windows_sys::Win32::System::Threading::WaitForSingleObject;
use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW};

use super::*;

const HELPER_NAME: &str = "lookout-fastscan.exe";

/// Whether Fast scan can be used for a place, and if not, why not.
#[derive(Debug, Clone, Serialize)]
pub struct FastInfo {
    pub available: bool,
    pub reason: String,
    pub letter: String,
}

fn drive_letter(path: &Path) -> Option<char> {
    match path.components().next()? {
        Component::Prefix(p) => match p.kind() {
            std::path::Prefix::Disk(b) | std::path::Prefix::VerbatimDisk(b) => {
                Some(char::from(b).to_ascii_uppercase())
            }
            _ => None,
        },
        _ => None,
    }
}

pub fn fast_info(path: &Path) -> FastInfo {
    let no = |reason: String| FastInfo {
        available: false,
        reason,
        letter: String::new(),
    };
    let Some(letter) = drive_letter(path) else {
        return no("Fast scan needs a drive with a letter, not a network location.".to_owned());
    };
    let root: Vec<u16> = format!("{letter}:\\")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: NUL-terminated drive root.
    let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
    if kind == 4 {
        return no(format!(
            "Fast scan can't read network drives ({letter}: is one); use the normal scan."
        ));
    }
    let mut fs = [0u16; 32];
    // SAFETY: NUL-terminated root and a 32-unit buffer for the file system name.
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs.as_mut_ptr(),
            fs.len() as u32,
        )
    };
    if ok == 0 {
        return no(format!("Drive {letter}: can't be read."));
    }
    let len = fs.iter().position(|&c| c == 0).unwrap_or(fs.len());
    let name = String::from_utf16_lossy(&fs[..len]);
    if !name.eq_ignore_ascii_case("NTFS") {
        return no(format!(
            "Fast scan needs an NTFS drive; {letter}: uses {name}. The normal scan works on it."
        ));
    }
    FastInfo {
        available: true,
        reason: String::new(),
        letter: letter.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Starting the helper and talking to it

#[derive(Debug)]
enum FastError {
    Cancelled,
    Declined,
    Failed(String),
}

impl FastError {
    fn message(&self) -> String {
        match self {
            FastError::Cancelled => String::new(),
            FastError::Declined => "Fast scan needs admin permission".to_owned(),
            FastError::Failed(m) => m.clone(),
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn random_hex() -> Option<String> {
    let mut b = [0u8; 16];
    // SAFETY: a 16-byte buffer and the system RNG flag (no algorithm handle).
    let st = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            b.as_mut_ptr(),
            b.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    (st == 0).then(|| b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Server end of the private pipe. Dropping it tells the helper to stop.
struct Link {
    pipe: HANDLE,
    proc: HANDLE,
    finished: bool,
}

impl Drop for Link {
    fn drop(&mut self) {
        // SAFETY: both handles are owned by this value and closed once.
        unsafe {
            if !self.finished {
                // Any byte means "stop"; closing the pipe does the same.
                let mut n = 0u32;
                WriteFile(self.pipe, b"x".as_ptr(), 1, &mut n, std::ptr::null_mut());
            }
            if self.pipe != INVALID_HANDLE_VALUE {
                CloseHandle(self.pipe);
            }
            if !self.proc.is_null() {
                if self.finished {
                    WaitForSingleObject(self.proc, 3000);
                }
                CloseHandle(self.proc);
            }
        }
    }
}

fn create_pipe(nonce: &str) -> Result<HANDLE, String> {
    let name = wide(&format!(r"\\.\pipe\lookout-fastscan-{nonce}"));
    // Owner (this user), Administrators and SYSTEM only: the elevated helper is
    // an Administrator, nobody else on the machine can connect.
    let sddl = wide("D:(A;;GA;;;OW)(A;;GA;;;BA)(A;;GA;;;SY)");
    let mut sd = std::ptr::null_mut();
    // SAFETY: NUL-terminated SDDL text; `sd` receives a LocalAlloc'd descriptor.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err("could not prepare the private connection".to_owned());
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    };
    // SAFETY: NUL-terminated name; `sa` and its descriptor live across the call.
    let h = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            1 << 20,
            1 << 20,
            0,
            &sa,
        )
    };
    // SAFETY: `sd` came from LocalAlloc above.
    unsafe { LocalFree(sd) };
    if h == INVALID_HANDLE_VALUE {
        return Err("could not open the private connection".to_owned());
    }
    Ok(h)
}

/// Starts the helper with the Windows permission prompt. Blocks until the user
/// has answered it.
fn launch_elevated(helper: &Path, letter: char, nonce: &str) -> Result<HANDLE, FastError> {
    let file = wide(&helper.to_string_lossy());
    let params = wide(&format!("{letter} {nonce}"));
    let verb = wide("runas");
    // SAFETY: a zeroed SHELLEXECUTEINFOW with the needed fields set below.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = 0x40; // SEE_MASK_NOCLOSEPROCESS
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = 0; // hidden
                    // SAFETY: all string pointers are NUL-terminated and outlive the call.
    let ok = unsafe { ShellExecuteExW(&mut info) };
    if ok == 0 {
        // SAFETY: reads the calling thread's last error.
        let e = unsafe { GetLastError() };
        return Err(if e == ERROR_CANCELLED {
            FastError::Declined
        } else {
            FastError::Failed(format!("Fast scan could not start (Windows error {e})."))
        });
    }
    Ok(info.hProcess)
}

impl Link {
    fn process_exited(&self) -> bool {
        // SAFETY: a live process handle (or null, which is checked first).
        !self.proc.is_null() && unsafe { WaitForSingleObject(self.proc, 0) } == WAIT_OBJECT_0
    }

    /// Waits for the helper to connect to the pipe.
    fn wait_connected(&self, progress: &Progress) -> Result<(), FastError> {
        let started = Instant::now();
        loop {
            if progress.cancel.load(Ordering::Relaxed) {
                return Err(FastError::Cancelled);
            }
            // SAFETY: a live pipe handle; non-blocking mode returns at once.
            let ok = unsafe { ConnectNamedPipe(self.pipe, std::ptr::null_mut()) };
            // SAFETY: reads the calling thread's last error.
            let e = unsafe { GetLastError() };
            if ok != 0 || e == ERROR_PIPE_CONNECTED || e == ERROR_NO_DATA {
                return Ok(());
            }
            if e != ERROR_PIPE_LISTENING {
                return Err(FastError::Failed("Fast scan could not connect.".to_owned()));
            }
            if self.process_exited() || started.elapsed() > Duration::from_secs(60) {
                return Err(FastError::Failed(
                    "Fast scan could not start the helper.".to_owned(),
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Reads the pipe without ever blocking for long, so Cancel and a helper that
/// died are noticed.
struct PipeReader<'a> {
    link: &'a Link,
    cancel: &'a AtomicBool,
}

impl Read for PipeReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut exited_once = false;
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
            }
            let mut avail = 0u32;
            // SAFETY: a live pipe handle and a valid out-pointer; nothing is consumed.
            let ok = unsafe {
                PeekNamedPipe(
                    self.link.pipe,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut avail,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Ok(0); // the helper closed its end
            }
            if avail > 0 {
                let want = avail.min(u32::try_from(buf.len()).unwrap_or(u32::MAX));
                let mut got = 0u32;
                // SAFETY: `buf` is valid for `want` bytes.
                let ok = unsafe {
                    ReadFile(
                        self.link.pipe,
                        buf.as_mut_ptr(),
                        want,
                        &mut got,
                        std::ptr::null_mut(),
                    )
                };
                if ok == 0 {
                    return Ok(0);
                }
                return Ok(got as usize);
            }
            if self.link.process_exited() {
                if exited_once {
                    return Ok(0);
                }
                exited_once = true; // look once more for bytes written just before exit
                continue;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

// ---------------------------------------------------------------------------
// Turning records into a tree

struct DirRec {
    rec: u64,
    parent: u64,
    mtime: u32,
    flags: u8,
    name_off: u32,
    name_len: u16,
}

/// What the files of one folder add up to (the same numbers a folder listing gives the normal scan).
#[derive(Default)]
struct Acc {
    files: u32,
    disk: u64,
    logical: u64,
    newest: u32,
    top: BinaryHeap<FileEntry>,
    rest_n: u32,
    rest_disk: u64,
    rest_logical: u64,
    rest_new: u32,
    cloud: u64,
    links: u64,
    link_bytes: u64,
}

type BigItem = Reverse<(u64, u64, u32, u64, String)>;

struct Builder {
    per_folder: usize,
    cluster: u32,
    dirs: Vec<DirRec>,
    names: Vec<u8>,
    accs: HashMap<u64, Acc>,
    big: BinaryHeap<BigItem>,
    big_overflow: bool,
    big_floor: u64,
    helper_peak: u64,
}

impl Builder {
    fn dir_name(&self, d: &DirRec) -> String {
        let s = d.name_off as usize;
        String::from_utf8_lossy(&self.names[s..s + usize::from(d.name_len)]).into_owned()
    }

    fn add_dir(&mut self, rec: u64, parent: u64, mtime: u32, flags: u8, name: &[u8]) {
        let name_off = self.names.len() as u32;
        self.names.extend_from_slice(name);
        self.dirs.push(DirRec {
            rec,
            parent,
            mtime,
            flags,
            name_off,
            name_len: name.len().min(usize::from(u16::MAX)) as u16,
        });
    }

    fn add_file(
        &mut self,
        parent: u64,
        mut disk: u64,
        mut logical: u64,
        mtime: u32,
        flags: u8,
        name: String,
    ) {
        let per = self.per_folder;
        let dup = flags & 2 != 0;
        let a = self.accs.entry(parent).or_default();
        if flags & 1 != 0 {
            a.cloud += 1;
        }
        if dup {
            a.links += 1;
            a.link_bytes += disk;
            (disk, logical) = (0, 0);
        }
        let big = !dup && disk >= self.big_floor;
        a.files += 1;
        a.disk += disk;
        a.logical += logical;
        a.newest = a.newest.max(mtime);
        let kept = per > 0 && (a.top.len() < per || a.top.peek().is_some_and(|s| disk > s.0 .0));
        if big {
            self.big
                .push(Reverse((disk, logical, mtime, parent, name.clone())));
            if self.big.len() > BIG_CAP {
                self.big_overflow = true;
                self.big.pop();
            }
            if self.big.len() >= BIG_CAP {
                self.big_floor = self.big.peek().map_or(BIG_MIN, |m| m.0 .0).max(BIG_MIN);
            }
        }
        let a = self.accs.entry(parent).or_default();
        if kept {
            a.top.push(Reverse((disk, logical, mtime, name)));
            if a.top.len() > per {
                if let Some(Reverse((d, s, m, _))) = a.top.pop() {
                    a.rest_n += 1;
                    a.rest_disk += d;
                    a.rest_logical += s;
                    a.rest_new = a.rest_new.max(m);
                }
            }
        } else {
            a.rest_n += 1;
            a.rest_disk += disk;
            a.rest_logical += logical;
            a.rest_new = a.rest_new.max(mtime);
        }
    }
}

fn rd_n<const N: usize>(r: &mut impl Read) -> Result<[u8; N], FastError> {
    let mut b = [0u8; N];
    match r.read_exact(&mut b) {
        Ok(()) => Ok(b),
        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Err(FastError::Cancelled),
        Err(_) => Err(FastError::Failed(
            "Fast scan stopped before it finished.".to_owned(),
        )),
    }
}

fn rd_name(r: &mut impl Read) -> Result<Vec<u8>, FastError> {
    let n = usize::from(u16::from_le_bytes(rd_n(r)?));
    let mut v = vec![0u8; n];
    match r.read_exact(&mut v) {
        Ok(()) => Ok(v),
        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Err(FastError::Cancelled),
        Err(_) => Err(FastError::Failed(
            "Fast scan stopped before it finished.".to_owned(),
        )),
    }
}

fn read_stream(
    r: &mut impl Read,
    opts: &ScanOptions,
    progress: &Progress,
) -> Result<Builder, FastError> {
    let head = rd_n::<5>(r)?;
    if &head != b"LKFS\x01" {
        return Err(FastError::Failed(
            "Fast scan got an unexpected answer from its helper.".to_owned(),
        ));
    }
    let cluster = u32::from_le_bytes(rd_n(r)?).max(512);
    let _records = u64::from_le_bytes(rd_n(r)?);
    let mut b = Builder {
        per_folder: opts.files_per_folder,
        cluster,
        dirs: Vec::new(),
        names: Vec::new(),
        accs: HashMap::new(),
        big: BinaryHeap::new(),
        big_overflow: false,
        big_floor: BIG_MIN,
        helper_peak: 0,
    };
    loop {
        let [tag] = rd_n::<1>(r)?;
        match tag {
            1 => {
                let rec = u64::from_le_bytes(rd_n(r)?);
                let parent = u64::from_le_bytes(rd_n(r)?);
                let mtime = u32::from_le_bytes(rd_n(r)?);
                let [flags] = rd_n::<1>(r)?;
                let name = rd_name(r)?;
                b.add_dir(rec, parent, mtime, flags, &name);
                progress.folders.fetch_add(1, Ordering::Relaxed);
            }
            2 => {
                let parent = u64::from_le_bytes(rd_n(r)?);
                let disk = u64::from_le_bytes(rd_n(r)?);
                let logical = u64::from_le_bytes(rd_n(r)?);
                let mtime = u32::from_le_bytes(rd_n(r)?);
                let [flags] = rd_n::<1>(r)?;
                let name = String::from_utf8_lossy(&rd_name(r)?).into_owned();
                progress.files.fetch_add(1, Ordering::Relaxed);
                if flags & 2 == 0 {
                    progress.bytes.fetch_add(disk, Ordering::Relaxed);
                }
                b.add_file(parent, disk, logical, mtime, flags, name);
            }
            3 => {
                let _records = u64::from_le_bytes(rd_n(r)?);
                b.helper_peak = u64::from_le_bytes(rd_n(r)?);
                return Ok(b);
            }
            4 => {
                let msg = String::from_utf8_lossy(&rd_name(r)?).into_owned();
                return Err(FastError::Failed(format!("Fast scan: {msg}")));
            }
            _ => {
                return Err(FastError::Failed(
                    "Fast scan got an unexpected answer from its helper.".to_owned(),
                ))
            }
        }
    }
}

fn build_tree(
    mut b: Builder,
    root: &Path,
    opts: &ScanOptions,
    progress: &Progress,
) -> Result<Tree, FastError> {
    *progress
        .current
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = "Building the folder tree…".to_owned();
    let dirs = std::mem::take(&mut b.dirs);
    let idx: HashMap<u64, u32> = dirs
        .iter()
        .enumerate()
        .map(|(i, d)| (d.rec, i as u32))
        .collect();
    let mut order: Vec<u32> = (0..dirs.len() as u32)
        .filter(|&i| dirs[i as usize].parent != dirs[i as usize].rec)
        .collect();
    order.sort_by_key(|&i| (dirs[i as usize].parent, dirs[i as usize].rec));
    let kids = |rec: u64| -> &[u32] {
        let lo = order.partition_point(|&i| dirs[i as usize].parent < rec);
        let hi = order.partition_point(|&i| dirs[i as usize].parent <= rec);
        &order[lo..hi]
    };

    // The scanned folder: the drive's root, or a folder found by walking its names.
    let mut root_rec = 5u64;
    // Long names (not 8.3 short ones or a mapped alias) are what the file table has.
    let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    for c in canon.components() {
        if let Component::Normal(n) = c {
            let want = n.to_string_lossy().to_lowercase();
            let hit = kids(root_rec)
                .iter()
                .find(|&&i| b.dir_name(&dirs[i as usize]).to_lowercase() == want);
            match hit {
                Some(&i) => root_rec = dirs[i as usize].rec,
                None => {
                    return Err(FastError::Failed(
                        "Fast scan could not find that folder in the drive's file table (it may be very new). Try again in a moment."
                            .to_owned(),
                    ))
                }
            }
        }
    }

    let mut tree = Tree {
        nodes: Vec::new(),
        names: Vec::new(),
        root: root.to_path_buf(),
        stats: TreeStats::default(),
        scanned_at: 0,
        cluster: b.cluster,
        big: Vec::new(),
        big_overflow: false,
        big_floor: BIG_MIN,
    };
    let rid = tree.add(&root.to_string_lossy(), 0, 0, NONE);
    if let Some(&i) = idx.get(&root_rec) {
        tree.nodes[rid as usize].mtime = dirs[i as usize].mtime;
    }
    // (folder record, its node or None when folded, nearest ancestor with a node)
    let mut stack: Vec<(u64, Option<u32>, u32)> = vec![(root_rec, Some(rid), rid)];
    while let Some((rec, node, anchor)) = stack.pop() {
        if progress.cancel.load(Ordering::Relaxed) {
            return Err(FastError::Cancelled);
        }
        let acc = b.accs.remove(&rec).unwrap_or_default();
        let sub_idx: Vec<u32> = kids(rec).to_vec();
        let subdirs: Vec<SubDir> = sub_idx
            .iter()
            .map(|&i| {
                let d = &dirs[i as usize];
                SubDir {
                    name: OsString::from(b.dir_name(d)),
                    mtime: d.mtime,
                    link: d.flags & 1 != 0,
                    cloud: d.flags & 2 != 0,
                }
            })
            .collect();
        let entered: Vec<u32> = sub_idx
            .iter()
            .zip(&subdirs)
            .filter(|(_, s)| !s.link)
            .map(|(&i, _)| i)
            .collect();
        tree.stats.folders += 1;
        tree.stats.files += u64::from(acc.files);
        tree.stats.cloud_files += acc.cloud;
        tree.stats.hard_links += acc.links;
        tree.stats.hard_link_bytes += acc.link_bytes;
        let listing = Listing {
            subdirs,
            files: acc.files,
            disk: acc.disk,
            logical: acc.logical,
            newest: acc.newest,
            top: acc.top.into_vec().into_iter().map(|Reverse(t)| t).collect(),
            rest_n: acc.rest_n,
            rest_disk: acc.rest_disk,
            rest_logical: acc.rest_logical,
            rest_new: acc.rest_new,
            ..Listing::default()
        };
        let job = Job {
            node,
            anchor,
            path: PathBuf::new(),
        };
        let mut new_jobs = Vec::new();
        apply(&mut tree, &job, listing, opts, &mut new_jobs);
        for (j, &di) in new_jobs.into_iter().zip(&entered) {
            stack.push((dirs[di as usize].rec, j.node, j.anchor));
        }
    }
    roll_up(&mut tree);
    tree.stats.volume_used = volume_used(root);

    // Large files: keep those inside the scanned folder and give them real paths.
    let mut big: Vec<BigFile> = Vec::new();
    for Reverse((disk, logical, mtime, parent, name)) in std::mem::take(&mut b.big) {
        let mut parts: Vec<String> = Vec::new();
        let mut cur = parent;
        let mut inside = false;
        for _ in 0..4096 {
            if cur == root_rec {
                inside = true;
                break;
            }
            let Some(&i) = idx.get(&cur) else { break };
            parts.push(b.dir_name(&dirs[i as usize]));
            cur = dirs[i as usize].parent;
        }
        if !inside {
            continue;
        }
        let mut path = root.to_path_buf();
        for p in parts.iter().rev() {
            path.push(p);
        }
        path.push(name);
        big.push(BigFile {
            path,
            size_disk: disk,
            logical,
            mtime,
        });
    }
    big.sort_by(|a, b| b.size_disk.cmp(&a.size_disk).then(a.path.cmp(&b.path)));
    tree.big = big;
    tree.big_overflow = b.big_overflow;
    tree.big_floor = b.big_floor;
    tree.scanned_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    tree.nodes.shrink_to_fit();
    tree.names.shrink_to_fit();
    progress.helper_peak.store(b.helper_peak, Ordering::Relaxed);
    Ok(tree)
}

fn run(
    root: &Path,
    helper: &Path,
    opts: &ScanOptions,
    progress: &Progress,
) -> Result<Tree, FastError> {
    let info = fast_info(root);
    if !info.available {
        return Err(FastError::Failed(info.reason));
    }
    let letter = info.letter.chars().next().unwrap_or('C');
    if helper.file_name().and_then(|n| n.to_str()) != Some(HELPER_NAME) || !helper.is_file() {
        return Err(FastError::Failed(
            "Fast scan is not available: its helper program is missing.".to_owned(),
        ));
    }
    let nonce =
        random_hex().ok_or_else(|| FastError::Failed("Fast scan could not start.".to_owned()))?;
    let pipe = create_pipe(&nonce).map_err(FastError::Failed)?;
    let mut link = Link {
        pipe,
        proc: std::ptr::null_mut(),
        finished: false,
    };
    *progress
        .current
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = "Waiting for Windows permission…".to_owned();
    link.proc = launch_elevated(helper, letter, &nonce)?;
    link.wait_connected(progress)?;
    *progress
        .current
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = "Reading the drive's file table…".to_owned();
    let builder = {
        let mut reader = std::io::BufReader::with_capacity(
            1 << 20,
            PipeReader {
                link: &link,
                cancel: &progress.cancel,
            },
        );
        read_stream(&mut reader, opts, progress)?
    };
    link.finished = true;
    drop(link);
    build_tree(builder, root, opts, progress)
}

/// The body of a fast-scan session (see `DiskService::start_fast`).
pub(super) fn scan_fast(
    root: &Path,
    helper: &Path,
    opts: &ScanOptions,
    progress: &Progress,
) -> Option<Tree> {
    match run(root, helper, opts, progress) {
        Ok(t) => Some(t),
        Err(e) => {
            if !matches!(e, FastError::Cancelled) {
                *progress
                    .message
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = e.message();
            }
            None
        }
    }
}
