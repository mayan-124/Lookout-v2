//! Times one disk-usage scan: `disk_scan <folder> [max_seconds] [low|full] [chart_dir]`.
//! Prints seconds, files per second, average CPU, peak memory and where the
//! time went.
//! A run past `max_seconds` (default 300) is cancelled and said so.
#![allow(clippy::expect_used)]

use std::path::Path;
use std::time::{Duration, Instant};

use lookout_core::diskusage::{DiskService, Hints, ScanState};
use lookout_core::pause::PauseState;

fn cpu_seconds() -> f64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let z = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut c, mut e, mut k, mut u) = (z, z, z, z);
    // SAFETY: valid out-pointers for the current process.
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
    let t = |f: FILETIME| {
        ((u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime)) as f64 / 1e7
    };
    t(k) + t(u)
}

fn peak_mb() -> f64 {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: zeroed counters with cb set, as the API requires.
    unsafe {
        let mut m: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        m.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        K32GetProcessMemoryInfo(GetCurrentProcess(), &mut m, m.cb);
        m.PeakWorkingSetSize as f64 / 1_048_576.0
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = args.get(1).expect("folder");
    let cap: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(300.0);
    let low = args.get(3).is_some_and(|s| s == "low");
    // `fast:<path of lookout-fastscan.exe>` runs the Fast scan (Windows asks for permission).
    let fast = args.get(3).and_then(|s| s.strip_prefix("fast:"));
    let svc = DiskService::new(PauseState::new(), 0.25, None, None, Hints::default());
    let (cpu0, t0) = (cpu_seconds(), Instant::now());
    match fast {
        Some(h) => svc
            .start_fast(Path::new(root), Path::new(h))
            .expect("start"),
        None => svc.start(Path::new(root), low).expect("start"),
    }
    let mut stopped = false;
    let mut t_read = None;
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let p = svc.progress();
        if t_read.is_none() && p.current.starts_with("Reading") {
            t_read = Some(Instant::now());
        }
        if p.state != ScanState::Scanning {
            break;
        }
        if !stopped && t0.elapsed().as_secs_f64() > cap {
            svc.cancel();
            stopped = true;
        }
    }
    let wall = t0.elapsed().as_secs_f64();
    if fast.is_some() {
        println!(
            "fast: {} read+build={:.1}s (excludes the permission prompt) helper_peak={:.0}MB",
            svc.progress().message,
            t_read.map_or(0.0, |t| t.elapsed().as_secs_f64()),
            svc.helper_peak_bytes() as f64 / 1_048_576.0
        );
    }
    let cpu = cpu_seconds() - cpu0;
    let p = svc.progress();
    println!(
        "root={root} mode={} {} wall={wall:.1}s files={} folders={} files/s={:.0} cpu_avg={:.0}% peak_mem={:.0}MB",
        if low { "low" } else { "full" },
        if stopped { "STOPPED-at-cap" } else { "finished" },
        p.files,
        p.folders,
        p.files as f64 / wall,
        cpu / wall * 100.0,
        peak_mb()
    );
    if let Some(t) = svc.tree() {
        let (d, l, i) = t.total();
        println!(
            "nodes={} disk={d} logical={l} items={i} heap={}KB",
            t.len(),
            t.heap_bytes() / 1024
        );
    }
    println!("{}", svc.timings());
    if let Some(t) = svc.tree() {
        let w: Vec<u16> = format!("{}\\", &root[..2])
            .encode_utf16()
            .chain([0])
            .collect();
        let (mut _avail, mut total, mut free) = (0u64, 0u64, 0u64);
        // SAFETY: NUL-terminated drive root and valid out-pointers.
        unsafe {
            windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                w.as_ptr(),
                &mut _avail,
                &mut total,
                &mut free,
            )
        };
        if let Ok(f) = std::env::var("DUMP") {
            use lookout_core::diskusage::SortBy;
            let mut out = String::new();
            let mut stack = vec![(0u32, String::new(), 0u32)];
            while let Some((id, pre, d)) = stack.pop() {
                for r in t.children(id, SortBy::Size, true) {
                    let p = format!("{pre}/{}", r.name);
                    out.push_str(&format!(
                        "{p}	{}
",
                        r.size_disk
                    ));
                    if r.has_children && d < 3 {
                        stack.push((r.id, p, d + 1));
                    }
                }
            }
            std::fs::write(f, out).expect("dump");
        }
        if let Ok(path) = std::env::var("LIST") {
            use lookout_core::diskusage::SortBy;
            let mut id = 0u32;
            for part in path.split('/').filter(|x| !x.is_empty()) {
                if let Some(r) = t
                    .children(id, SortBy::Size, true)
                    .into_iter()
                    .find(|r| r.name == part)
                {
                    id = r.id;
                }
            }
            for r in t.children(id, SortBy::Size, true).iter().take(12) {
                println!("LIST {} disk={} logical={}", r.name, r.size_disk, r.logical);
            }
        }
        let st = &t.stats;
        println!(
            "MEASURE tree_disk={} volume_used={} hard_links={} hard_link_bytes={}",
            t.total().0,
            total - free,
            st.hard_links,
            st.hard_link_bytes
        );
    }
    // Optional: write the chart data (4 ring levels, 2 treemap levels) as JSON.
    if let (Some(dir), Some(t)) = (args.get(4), svc.tree()) {
        for (name, levels) in [("ring", 4), ("treemap", 2)] {
            let json = serde_json::to_string(&t.chart(0, levels)).expect("json");
            std::fs::write(Path::new(dir).join(format!("chart_{name}.json")), json).expect("write");
        }
    }
}
