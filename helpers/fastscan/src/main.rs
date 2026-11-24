//! Lookout fast-scan helper: reads an NTFS volume's file table (the $MFT) and
//! streams one record per folder and per file name to Lookout over a named pipe.
//!
//! Safety properties, all of which the code below keeps:
//! * Its only inputs are a drive letter and a 32-hex-digit pipe nonce. Nothing
//!   else on the command line is read, and no path is ever accepted.
//! * It is read-only: the volume is opened for reading, the pipe is the only
//!   thing written, and no file is created, changed or deleted.
//! * It has no network code, starts no other program and exits when done.
//! * It stops as soon as Lookout closes its end of the pipe or sends any byte
//!   (so closing Lookout, or pressing Cancel, ends the helper).
//!
//! Wire format (little endian): `LKFS`, version u8, cluster u32, records u64;
//! then records: 1 = folder, 2 = file name, 3 = end, 4 = error (see `Out`).
#![cfg_attr(not(test), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::process::ExitCode;

use windows_sys::Win32::Storage::FileSystem::{
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};
use windows_sys::Win32::System::Ioctl::{FSCTL_GET_NTFS_VOLUME_DATA, NTFS_VOLUME_DATA_BUFFER};
use windows_sys::Win32::System::Pipes::PeekNamedPipe;
use windows_sys::Win32::System::IO::DeviceIoControl;

const ATTR_REPARSE: u32 = 0x400;
const ATTR_OFFLINE: u32 = 0x1000;
const ATTR_RECALL_ON_OPEN: u32 = 0x4_0000;
const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
/// "Not stored here" as Windows reports it. In the file table 0x40000 also means
/// "has extended attributes" (Windows hides that), so it only counts together
/// with the reparse flag that real cloud placeholders carry.
fn is_cloud(attrs: u32) -> bool {
    attrs & (ATTR_OFFLINE | ATTR_RECALL_ON_DATA_ACCESS) != 0
        || (attrs & ATTR_RECALL_ON_OPEN != 0 && attrs & ATTR_REPARSE != 0)
}
/// Records below this number are NTFS's own metadata files ($MFT, $LogFile ...).
const FIRST_USER_RECORD: u64 = 24;
const REC_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
const CHUNK: usize = 4 << 20;

fn le16(b: &[u8], o: usize) -> u16 {
    b.get(o..o + 2)
        .map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
}
fn le32(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4)
        .map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn le64(b: &[u8], o: usize) -> u64 {
    b.get(o..o + 8).map_or(0, |s| {
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        u64::from_le_bytes(a)
    })
}

fn unix_secs(ft: u64) -> u32 {
    u32::try_from((ft / 10_000_000).saturating_sub(11_644_473_600)).unwrap_or(u32::MAX)
}

// ---------------------------------------------------------------------------
// Output

struct Out {
    pipe: File,
    buf: Vec<u8>,
}

impl Out {
    fn new(pipe: File) -> Self {
        Self {
            pipe,
            buf: Vec::with_capacity(1 << 20),
        }
    }

    /// True once Lookout closed the pipe or sent anything (a cancel).
    fn cancelled(&self) -> bool {
        let mut avail = 0u32;
        // SAFETY: a live pipe handle and a valid out-pointer; nothing is read.
        let ok = unsafe {
            PeekNamedPipe(
                self.pipe.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut avail,
                std::ptr::null_mut(),
            )
        };
        ok == 0 || avail > 0
    }

    fn flush(&mut self) -> Result<(), String> {
        if !self.buf.is_empty() {
            self.pipe
                .write_all(&self.buf)
                .map_err(|_| "Lookout closed the connection".to_owned())?;
            self.buf.clear();
        }
        Ok(())
    }

    fn name(&mut self, name: &str) {
        let n = name.len().min(usize::from(u16::MAX));
        self.buf.extend_from_slice(&(n as u16).to_le_bytes());
        self.buf.extend_from_slice(&name.as_bytes()[..n]);
    }

    fn folder(&mut self, rec: u64, parent: u64, mtime: u32, flags: u8, name: &str) {
        self.buf.push(1);
        self.buf.extend_from_slice(&rec.to_le_bytes());
        self.buf.extend_from_slice(&parent.to_le_bytes());
        self.buf.extend_from_slice(&mtime.to_le_bytes());
        self.buf.push(flags);
        self.name(name);
    }

    #[allow(clippy::too_many_arguments)]
    fn file(&mut self, parent: u64, disk: u64, logical: u64, mtime: u32, flags: u8, name: &str) {
        self.buf.push(2);
        self.buf.extend_from_slice(&parent.to_le_bytes());
        self.buf.extend_from_slice(&disk.to_le_bytes());
        self.buf.extend_from_slice(&logical.to_le_bytes());
        self.buf.extend_from_slice(&mtime.to_le_bytes());
        self.buf.push(flags);
        self.name(name);
    }

    fn end(&mut self, records: u64) {
        self.buf.push(3);
        self.buf.extend_from_slice(&records.to_le_bytes());
        self.buf
            .extend_from_slice(&peak_working_set().to_le_bytes());
    }

    fn error(&mut self, msg: &str) {
        self.buf.push(4);
        self.name(msg);
        let _ = self.flush();
    }
}

fn peak_working_set() -> u64 {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: zeroed counters with `cb` set, as the API requires.
    unsafe {
        let mut m: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        m.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        K32GetProcessMemoryInfo(GetCurrentProcess(), &mut m, m.cb);
        m.PeakWorkingSetSize as u64
    }
}

// ---------------------------------------------------------------------------
// File records

#[derive(Clone)]
struct OName {
    parent: u64,
    ns: u8,
    name: String,
}

#[derive(Clone, Copy)]
struct PData {
    logical: u64,
    alloc: u64,
    resident: bool,
}

/// What one MFT record says.
#[derive(Default)]
struct Parsed {
    base: u64,
    dir: bool,
    has_si: bool,
    attrs: u32,
    mtime: u64,
    has_list: bool,
    names: Vec<OName>,
    data: Option<PData>,
}

/// Undoes the update-sequence protection of a record. False if it is damaged.
fn fixup(rec: &mut [u8]) -> bool {
    if rec.get(0..4) != Some(b"FILE") {
        return false;
    }
    let usa = usize::from(le16(rec, 4));
    let count = usize::from(le16(rec, 6));
    if count < 2 || usa + count * 2 > rec.len() {
        return false;
    }
    let check = [rec[usa], rec[usa + 1]];
    for i in 1..count {
        let end = i * 512;
        if end > rec.len() || rec[end - 2..end] != check {
            return false;
        }
        rec[end - 2] = rec[usa + 2 * i];
        rec[end - 1] = rec[usa + 2 * i + 1];
    }
    true
}

/// Walks the attributes of a (fixed-up) record.
fn parse_record(rec: &[u8], p: &mut Parsed) {
    *p = Parsed::default();
    let flags = le16(rec, 22);
    p.dir = flags & 2 != 0;
    p.base = le64(rec, 32) & REC_MASK;
    let used = (le32(rec, 24) as usize).min(rec.len());
    let mut off = usize::from(le16(rec, 20));
    while off + 8 <= used {
        let ty = le32(rec, off);
        if ty == 0xFFFF_FFFF {
            break;
        }
        let alen = le32(rec, off + 4) as usize;
        if alen < 16 || off + alen > used {
            break;
        }
        let attr = &rec[off..off + alen];
        let non_res = attr[8] != 0;
        let named = attr[9] != 0;
        match ty {
            0x10 if !non_res => {
                let (co, cl) = (usize::from(le16(attr, 20)), le32(attr, 16) as usize);
                if let Some(c) = attr.get(co..co + cl.min(alen)) {
                    p.has_si = true;
                    p.mtime = le64(c, 8);
                    p.attrs = le32(c, 32);
                }
            }
            0x20 => p.has_list = true,
            0x30 if !non_res => {
                let (co, cl) = (usize::from(le16(attr, 20)), le32(attr, 16) as usize);
                if let Some(c) = attr.get(co..co + cl.min(alen)) {
                    let chars = usize::from(c.get(64).copied().unwrap_or(0));
                    let ns = c.get(65).copied().unwrap_or(1);
                    let units: Vec<u16> = (0..chars).map(|i| le16(c, 66 + i * 2)).collect();
                    p.names.push(OName {
                        parent: le64(c, 0) & REC_MASK,
                        ns,
                        name: String::from_utf16_lossy(&units),
                    });
                }
            }
            0x80 if !named => {
                if !non_res {
                    p.data = Some(PData {
                        logical: u64::from(le32(attr, 16)),
                        alloc: 0,
                        resident: true,
                    });
                } else if le64(attr, 16) == 0 {
                    // Only the first extent of a stream carries its sizes.
                    let comp_unit = le16(attr, 34);
                    let alloc = if comp_unit != 0 && alen >= 0x48 {
                        le64(attr, 64)
                    } else {
                        le64(attr, 40)
                    };
                    p.data = Some(PData {
                        logical: le64(attr, 48),
                        alloc,
                        resident: false,
                    });
                }
            }
            _ => {}
        }
        off += alen;
    }
}

/// What is carried from a record that has an attribute list until its
/// extension records have been seen.
#[derive(Default)]
struct Pending {
    dir: bool,
    has_si: bool,
    attrs: u32,
    mtime: u64,
    names: Vec<OName>,
    data: Option<PData>,
}

impl Pending {
    fn merge(&mut self, p: Parsed) {
        if p.has_si {
            self.has_si = true;
            self.attrs = p.attrs;
            self.mtime = p.mtime;
        }
        if p.dir {
            self.dir = true;
        }
        self.names.extend(p.names);
        if self.data.is_none() {
            self.data = p.data;
        }
    }
}

struct Ctx {
    cluster: u64,
}

fn emit(out: &mut Out, ctx: &Ctx, rec_no: u64, e: &Pending) {
    if rec_no < FIRST_USER_RECORD {
        return; // NTFS metadata files and the root itself
    }
    let mut names: Vec<&OName> = e.names.iter().filter(|n| n.ns != 2).collect();
    if names.is_empty() {
        names = e.names.iter().collect();
    }
    let Some(first) = names.first() else {
        return;
    };
    let mtime = unix_secs(e.mtime);
    let cloud = is_cloud(e.attrs);
    if e.dir {
        let mut flags = 0u8;
        if e.attrs & ATTR_REPARSE != 0 || cloud {
            flags |= 1;
        }
        if cloud {
            flags |= 2;
        }
        out.folder(rec_no, first.parent, mtime, flags, &first.name);
        return;
    }
    let (logical, disk) = match e.data {
        None => (0, 0),
        Some(d) if cloud => (d.logical, 0),
        Some(d) if d.resident => (d.logical, d.logical.div_ceil(ctx.cluster) * ctx.cluster),
        Some(d) => (d.logical, d.alloc),
    };
    for (i, n) in names.iter().enumerate() {
        let mut flags = u8::from(cloud);
        if i > 0 {
            flags |= 2; // another path to the same file: its bytes count once
        }
        out.file(n.parent, disk, logical, mtime, flags, &n.name);
    }
}

// ---------------------------------------------------------------------------
// Reading the volume

fn read_exact_at(f: &File, buf: &mut [u8], mut off: u64) -> Result<(), String> {
    let mut done = 0;
    while done < buf.len() {
        let n = f
            .seek_read(&mut buf[done..], off)
            .map_err(|e| format!("reading the drive failed ({e})"))?;
        if n == 0 {
            return Err("the drive ended early".to_owned());
        }
        done += n;
        off += n as u64;
    }
    Ok(())
}

/// The data runs of $MFT, from its own record. `None` when the table is so
/// fragmented that it needs an attribute list (not supported).
fn mft_runs(rec0: &[u8]) -> Option<Vec<(u64, u64)>> {
    let used = (le32(rec0, 24) as usize).min(rec0.len());
    let mut off = usize::from(le16(rec0, 20));
    let mut runs = None;
    while off + 8 <= used {
        let ty = le32(rec0, off);
        if ty == 0xFFFF_FFFF {
            break;
        }
        let alen = le32(rec0, off + 4) as usize;
        if alen < 16 || off + alen > used {
            return None;
        }
        let attr = &rec0[off..off + alen];
        if ty == 0x20 {
            return None;
        }
        if ty == 0x80 && attr[9] == 0 && attr[8] != 0 {
            if le64(attr, 16) != 0 {
                return None;
            }
            let mut r = Vec::new();
            let mut p = usize::from(le16(attr, 32));
            let mut lcn: i64 = 0;
            while let Some(&h) = attr.get(p) {
                if h == 0 {
                    break;
                }
                let (ls, os) = (usize::from(h & 15), usize::from(h >> 4));
                let mut len = 0u64;
                for i in 0..ls {
                    len |= u64::from(*attr.get(p + 1 + i)?) << (8 * i);
                }
                let mut delta = 0i64;
                for i in 0..os {
                    delta |= i64::from(*attr.get(p + 1 + ls + i)?) << (8 * i);
                }
                if os > 0 && os < 8 && delta >> (8 * os - 1) & 1 == 1 {
                    delta -= 1i64 << (8 * os);
                }
                if os == 0 {
                    return None; // a sparse piece of $MFT is not something we understand
                }
                lcn += delta;
                r.push((u64::try_from(lcn).ok()?, len));
                p += 1 + ls + os;
            }
            runs = Some(r);
        }
        off += alen;
    }
    runs
}

fn run(letter: char, out: &mut Out) -> Result<(), String> {
    let vol = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(format!(r"\\.\{letter}:"))
        .map_err(|e| format!("could not open drive {letter}: ({e})"))?;
    // SAFETY: zeroed output buffer of the documented size for this control code.
    let vd: NTFS_VOLUME_DATA_BUFFER = unsafe {
        let mut vd: NTFS_VOLUME_DATA_BUFFER = std::mem::zeroed();
        let mut got = 0u32;
        let ok = DeviceIoControl(
            vol.as_raw_handle(),
            FSCTL_GET_NTFS_VOLUME_DATA,
            std::ptr::null(),
            0,
            std::ptr::addr_of_mut!(vd).cast(),
            std::mem::size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
            &mut got,
            std::ptr::null_mut(),
        );
        if ok == 0 {
            return Err(format!("drive {letter}: is not NTFS"));
        }
        vd
    };
    let rs = vd.BytesPerFileRecordSegment as usize;
    let cluster = u64::from(vd.BytesPerCluster);
    if !(512..=65536).contains(&rs) || !rs.is_power_of_two() || cluster == 0 {
        return Err("unexpected NTFS record size".to_owned());
    }
    let valid = u64::try_from(vd.MftValidDataLength).unwrap_or(0);
    let start = u64::try_from(vd.MftStartLcn).unwrap_or(0) * cluster;
    let mut rec0 = vec![0u8; rs.max(4096)];
    read_exact_at(&vol, &mut rec0, start)?;
    rec0.truncate(rs);
    if !fixup(&mut rec0) {
        return Err("the file table's first record is damaged".to_owned());
    }
    let runs = mft_runs(&rec0).ok_or_else(|| {
        "this drive's file table is too fragmented for Fast scan; use the normal scan".to_owned()
    })?;

    out.buf.extend_from_slice(b"LKFS\x01");
    out.buf.extend_from_slice(&(cluster as u32).to_le_bytes());
    out.buf
        .extend_from_slice(&(valid / rs as u64).to_le_bytes());

    let ctx = Ctx { cluster };
    let mut chunk = vec![0u8; CHUNK - CHUNK % rs];
    let mut rec_no = 0u64;
    let mut left_total = valid;
    let mut parsed = Parsed::default();
    let mut deferred: HashMap<u64, Pending> = HashMap::new();
    let mut ext: HashMap<u64, Pending> = HashMap::new();
    'runs: for (lcn, clusters) in runs {
        let mut off = lcn * cluster;
        let mut left = clusters * cluster;
        if left % rs as u64 != 0 {
            return Err("unexpected file table layout".to_owned());
        }
        while left > 0 && left_total > 0 {
            if out.cancelled() {
                return Ok(());
            }
            let n = (chunk.len() as u64).min(left).min(left_total) as usize;
            read_exact_at(&vol, &mut chunk[..n], off)?;
            off += n as u64;
            left -= n as u64;
            left_total -= n as u64;
            for rec in chunk[..n].chunks_exact_mut(rs) {
                let no = rec_no;
                rec_no += 1;
                if rec.get(0..4) != Some(b"FILE") || le16(rec, 22) & 1 == 0 || !fixup(rec) {
                    continue;
                }
                parse_record(rec, &mut parsed);
                if parsed.base != 0 {
                    ext.entry(parsed.base)
                        .or_default()
                        .merge(std::mem::take(&mut parsed));
                } else if parsed.has_list {
                    let mut d = Pending::default();
                    d.merge(std::mem::take(&mut parsed));
                    deferred.insert(no, d);
                } else {
                    let e = Pending {
                        dir: parsed.dir,
                        has_si: parsed.has_si,
                        attrs: parsed.attrs,
                        mtime: parsed.mtime,
                        names: std::mem::take(&mut parsed.names),
                        data: parsed.data,
                    };
                    emit(out, &ctx, no, &e);
                }
            }
            if out.buf.len() >= 1 << 20 {
                out.flush()?;
            }
            if left_total == 0 {
                break 'runs;
            }
        }
    }
    for (no, mut e) in deferred {
        if let Some(x) = ext.remove(&no) {
            e.names.extend(x.names);
            if e.data.is_none() {
                e.data = x.data;
            }
            if !e.has_si && x.has_si {
                e.attrs = x.attrs;
                e.mtime = x.mtime;
            }
            e.dir |= x.dir;
        }
        emit(out, &ctx, no, &e);
    }
    out.end(rec_no);
    out.flush()
}

fn parse_args(args: &[String]) -> Option<(char, String)> {
    let [letter, nonce] = args else {
        return None;
    };
    let mut c = letter.chars();
    let l = c.next()?;
    if c.next().is_some() || !l.is_ascii_alphabetic() {
        return None;
    }
    if nonce.len() != 32 || !nonce.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some((l.to_ascii_uppercase(), nonce.clone()))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((letter, nonce)) = parse_args(&args) else {
        return ExitCode::from(2);
    };
    let name = format!(r"\\.\pipe\lookout-fastscan-{nonce}");
    let mut pipe = None;
    for _ in 0..50 {
        match OpenOptions::new().read(true).write(true).open(&name) {
            Ok(f) => {
                pipe = Some(f);
                break;
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
    let Some(pipe) = pipe else {
        return ExitCode::from(3);
    };
    let mut out = Out::new(pipe);
    match run(letter, &mut out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            out.error(&e);
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In the file table 0x40000 means "has extended attributes". It once made
    /// folders such as Temp look cloud-only, so the scan skipped them.
    #[test]
    fn extended_attributes_flag_is_not_cloud() {
        assert!(!is_cloud(0x40000));
        assert!(!is_cloud(0x40000 | 0x10 | 0x2000_0000));
        assert!(!is_cloud(0));
        // Real placeholders: offline, recall on data access, or recall on open with a reparse point.
        assert!(is_cloud(ATTR_OFFLINE));
        assert!(is_cloud(ATTR_RECALL_ON_DATA_ACCESS | ATTR_REPARSE));
        assert!(is_cloud(ATTR_RECALL_ON_OPEN | ATTR_REPARSE));
    }
}
