//! Fast scan against the normal scan on a generated tree, and the files Windows
//! keeps locked. Fast scan needs administrator rights, so this only runs when
//! the test process is elevated (CI runners are) or `LOOKOUT_FASTSCAN_UAC` is
//! set to accept a Windows permission prompt; otherwise it says so and passes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use lookout_core::diskusage::{DiskService, Hints, ScanState, SortBy, Tree};
use lookout_core::pause::PauseState;
use windows_sys::Win32::System::Ioctl::{FSCTL_SET_COMPRESSION, FSCTL_SET_SPARSE};
use windows_sys::Win32::System::IO::DeviceIoControl;

fn elevated() -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    // SAFETY: standard token query on the current process; handle closed after.
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut n = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            std::ptr::addr_of_mut!(e).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut n,
        );
        CloseHandle(token);
        ok != 0 && e.TokenIsElevated != 0
    }
}

fn ioctl(f: &fs::File, code: u32, input: Option<u16>) {
    let mut n = 0u32;
    let (p, len) = input
        .as_ref()
        .map_or((std::ptr::null(), 0), |v| (std::ptr::from_ref(v).cast(), 2));
    // SAFETY: valid handle; the input (if any) is a 2-byte value that outlives the call.
    let ok = unsafe {
        DeviceIoControl(
            f.as_raw_handle(),
            code,
            p,
            len,
            std::ptr::null_mut(),
            0,
            &mut n,
            std::ptr::null_mut(),
        )
    };
    assert!(ok != 0, "ioctl {code:#x} failed");
}

fn put(p: &Path, n: usize) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    let mut f = fs::File::create(p).unwrap();
    f.write_all(&vec![b'x'; n]).unwrap();
    f.sync_all().unwrap();
}

fn plain(p: &Path) -> String {
    p.to_string_lossy().trim_start_matches(r"\\?\").to_owned()
}

fn build(r: &Path) {
    put(&r.join("a/small.txt"), 100); // lives inside the file table record
    put(&r.join("a/b/mid.bin"), 300_000);
    put(&r.join("a/b/c/big.bin"), 3_000_000);
    fs::hard_link(r.join("a/b/c/big.bin"), r.join("a/big_link.bin")).unwrap();
    for i in 0..120 {
        put(&r.join(format!("many/f{i}.dat")), 1000 + i);
    }
    put(&r.join("target_dir/inside.bin"), 2_000_000);
    let st = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(plain(&r.join("junc")))
        .arg(plain(&r.join("target_dir")))
        .output()
        .unwrap();
    assert!(st.status.success(), "mklink failed");
    // Sparse: 16 MB long, only 64 KB written.
    let mut f = fs::File::create(r.join("sparse.bin")).unwrap();
    ioctl(&f, FSCTL_SET_SPARSE, None);
    f.set_len(16 << 20).unwrap();
    f.seek(SeekFrom::Start(4 << 20)).unwrap();
    f.write_all(&[7u8; 65536]).unwrap();
    f.sync_all().unwrap();
    // Compressed: 4 MB of zeros shrink to almost nothing.
    let mut f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(r.join("comp.bin"))
        .unwrap();
    ioctl(&f, FSCTL_SET_COMPRESSION, Some(1));
    f.write_all(&vec![0u8; 4 << 20]).unwrap();
    f.sync_all().unwrap();
    // A path well past 260 characters.
    let mut deep = r.to_path_buf();
    for _ in 0..8 {
        deep.push("x".repeat(50));
    }
    put(&deep.join("deep.bin"), 500_000);
}

fn wait_done(d: &DiskService) -> ScanState {
    for _ in 0..12_000 {
        let p = d.progress();
        if matches!(
            p.state,
            ScanState::Done | ScanState::Failed | ScanState::Cancelled
        ) {
            return p.state;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("scan did not finish");
}

fn service() -> DiskService {
    DiskService::new(PauseState::new(), 1.0, None, None, Hints::default())
}

fn helper() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lookout-fastscan"))
}

fn row(t: &Tree, name: &str) -> (u64, u64) {
    let r = t
        .children(0, SortBy::Name, false)
        .into_iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("no row {name}"));
    (r.size_disk, r.logical)
}

fn compare(normal: &Tree, fast: &Tree) -> Result<(), String> {
    let (nd, nl, ni) = normal.total();
    let (fd, fl, fi) = fast.total();
    let mut bad = Vec::new();
    if nl != fl {
        bad.push(format!("logical {nl} vs {fl}"));
    }
    if ni != fi {
        bad.push(format!("items {ni} vs {fi}"));
    }
    if nd.abs_diff(fd) > 65536 {
        bad.push(format!("size on disk {nd} vs {fd}"));
    }
    for name in ["sparse.bin", "comp.bin", "target_dir", "junc"] {
        let (a, b) = (row(normal, name), row(fast, name));
        if a.1 != b.1 || a.0.abs_diff(b.0) > 8192 {
            bad.push(format!("{name}: {a:?} vs {b:?}"));
        }
    }
    let (s, c) = (row(fast, "sparse.bin"), row(fast, "comp.bin"));
    if s.0 > 1 << 20 || c.0 > 1 << 20 {
        bad.push(format!("sparse/compressed use too much: {s:?} {c:?}"));
    }
    let (ns, fs_) = (normal.stats, fast.stats);
    for (what, a, b) in [
        ("files", ns.files, fs_.files),
        ("folders", ns.folders, fs_.folders),
        ("links_skipped", ns.links_skipped, fs_.links_skipped),
        ("hard_links", ns.hard_links, fs_.hard_links),
    ] {
        if a != b {
            bad.push(format!("{what} {a} vs {b}"));
        }
    }
    let list = |t: &Tree| {
        // Sizes only: which of two hard links carries the bytes is not fixed.
        let mut v: Vec<u64> = t
            .big_files(0, None, 1000)
            .into_iter()
            .map(|r| r.size_disk)
            .collect();
        v.sort();
        v
    };
    if list(normal) != list(fast) {
        bad.push(format!(
            "large files {:?} vs {:?}",
            list(normal),
            list(fast)
        ));
    }
    if bad.is_empty() {
        Ok(())
    } else {
        Err(bad.join("; "))
    }
}

#[test]
fn fast_scan_matches_normal_scan_and_sees_locked_files() {
    if !elevated() && std::env::var_os("LOOKOUT_FASTSCAN_UAC").is_none() {
        eprintln!("skipped: needs an elevated test run (or LOOKOUT_FASTSCAN_UAC=1 to accept the Windows prompt)");
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(base.path()).unwrap().join("tree");
    fs::create_dir_all(&root).unwrap();
    build(&root);

    let normal = service();
    normal.start(&root, false).unwrap();
    assert_eq!(wait_done(&normal), ScanState::Done);
    let normal_tree = normal.tree().unwrap();

    // The file table is written lazily: give it a moment, then try a few times.
    let fast = service();
    let mut last = String::new();
    let mut ok = false;
    std::thread::sleep(Duration::from_secs(5));
    for attempt in 0..4 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_secs(6));
        }
        fast.start_fast(&root, &helper()).unwrap();
        let st = wait_done(&fast);
        assert_eq!(st, ScanState::Done, "{}", fast.progress().message);
        match compare(&normal_tree, &fast.tree().unwrap()) {
            Ok(()) => {
                ok = true;
                break;
            }
            Err(e) => last = e,
        }
    }
    assert!(ok, "fast scan differs from the normal scan: {last}");

    // Files Windows keeps locked show up with their real sizes.
    let drive = PathBuf::from(format!("{}\\", &plain(&root)[..2]));
    let whole = service();
    whole.start_fast(&drive, &helper()).unwrap();
    assert_eq!(
        wait_done(&whole),
        ScanState::Done,
        "{}",
        whole.progress().message
    );
    let t = whole.tree().unwrap();
    let mut seen = 0;
    for name in ["pagefile.sys", "hiberfil.sys", "swapfile.sys"] {
        let Some(entry) = fs::read_dir(&drive)
            .unwrap()
            .flatten()
            .find(|e| e.file_name().eq_ignore_ascii_case(name))
        else {
            continue;
        };
        let Ok(md) = entry.metadata() else { continue };
        let r = row(&t, name);
        assert_eq!(r.1, md.len(), "{name} logical size");
        assert!(r.0 > 0, "{name} has a size on disk");
        seen += 1;
    }
    eprintln!("locked files checked: {seen}");
}
