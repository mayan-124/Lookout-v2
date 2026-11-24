//! Duplicate finder: group by size, then by a hash of the first 64 KB, then by
//! a hash of the whole file. Hard links to one file are one file, not duplicates.
//! Nothing is removed here; see `cleanup::Batch` and the batch action.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::cleanup::{
    is_cloud, is_protected, is_reparse, mtime_secs, Batch, Entry, Env, Mode, Task, Throttle,
};
use crate::pause::PauseState;

pub const DEFAULT_MIN_SIZE: u64 = 1 << 20;
const PARTIAL: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct DupFile {
    pub path: String,
    pub mtime: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Group {
    pub id: u32,
    pub size: u64,
    pub files: Vec<DupFile>,
    /// Bytes freed by keeping one copy: `size * (copies - 1)`.
    pub wasted: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    Newest,
    Oldest,
    ShortestPath,
    /// The file at this position in the group.
    Index(usize),
}

impl Keep {
    pub fn parse(s: &str) -> Keep {
        match s {
            "oldest" => Keep::Oldest,
            "shortest" => Keep::ShortestPath,
            _ => Keep::Newest,
        }
    }
}

/// Folders the finder never enters: system and program folders, per-user
/// application data, the recycle bin, and version-control internals.
fn skip_dir(env: &Env, path: &Path, name: &str) -> bool {
    let l = name.to_lowercase();
    matches!(
        l.as_str(),
        "appdata" | "$recycle.bin" | "system volume information" | ".git" | "windowsapps"
    ) || env
        .system_dirs
        .iter()
        .any(|d| !d.as_os_str().is_empty() && path.starts_with(d))
}

/// (volume serial, file index) of a file: two paths with the same identity are
/// hard links to one file.
fn identity(path: &Path) -> Option<(u32, u64)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let f = std::fs::File::open(path).ok()?;
    // SAFETY: a live handle and a zeroed output struct.
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        (GetFileInformationByHandle(f.as_raw_handle(), &mut info) != 0).then(|| {
            (
                info.dwVolumeSerialNumber,
                (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            )
        })
    }
}

/// Hash of the first `limit` bytes (or the whole file when `limit` is `None`).
/// `None` when the file cannot be read or the task was cancelled.
fn hash_file<T>(path: &Path, limit: Option<usize>, task: &Task<T>) -> Option<[u8; 32]> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut left = limit.unwrap_or(usize::MAX);
    while left > 0 {
        if task.cancelled() {
            return None;
        }
        let want = buf.len().min(left);
        let n = f.read(&mut buf[..want]).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        left -= n;
        task.bytes.fetch_add(n as u64, Ordering::Relaxed);
    }
    Some(h.finalize().into())
}

/// Finds groups of identical files below `root`.
pub fn find(
    env: &Env,
    root: &Path,
    min_size: u64,
    task: &Task<Vec<Group>>,
    pause: &PauseState,
    gentle: bool,
) -> Vec<Group> {
    let mut th = Throttle::new(gentle, pause.clone());

    // 1. Walk, grouping by size.
    task.set_phase("Listing files");
    let mut by_size: HashMap<u64, Vec<(PathBuf, u64)>> = HashMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            if !th.tick(task) {
                return Vec::new();
            }
            let Ok(md) = e.metadata() else { continue };
            if is_reparse(&md) || is_cloud(&md) {
                continue; // links and cloud-only placeholders
            }
            let path = e.path();
            if md.is_dir() {
                if !skip_dir(env, &path, &e.file_name().to_string_lossy()) {
                    stack.push(path);
                }
            } else if md.len() >= min_size.max(1) && !is_protected(env, &path) {
                task.files.fetch_add(1, Ordering::Relaxed);
                by_size
                    .entry(md.len())
                    .or_default()
                    .push((path, mtime_secs(&md)));
            }
        }
    }
    by_size.retain(|_, v| v.len() > 1);

    // 2. Same size: drop extra paths to one file (hard links).
    task.set_phase("Ignoring hard links");
    let mut sets: Vec<(u64, Vec<(PathBuf, u64)>)> = Vec::new();
    for (size, files) in by_size {
        let mut seen: HashMap<(u32, u64), ()> = HashMap::new();
        let mut uniq = Vec::new();
        for (p, m) in files {
            if !th.tick(task) {
                return Vec::new();
            }
            // A file that cannot be opened (locked, no access) is left out.
            if let Some(id) = identity(&p) {
                if seen.insert(id, ()).is_none() {
                    uniq.push((p, m));
                }
            }
        }
        if uniq.len() > 1 {
            sets.push((size, uniq));
        }
    }

    // 3. First 64 KB, then everything.
    let mut groups: Vec<Group> = Vec::new();
    task.set_phase("Comparing the start of each file");
    let mut partial_sets: Vec<(u64, Vec<(PathBuf, u64)>)> = Vec::new();
    for (size, files) in sets {
        let mut by: HashMap<[u8; 32], Vec<(PathBuf, u64)>> = HashMap::new();
        for (p, m) in files {
            if !th.tick(task) {
                return Vec::new();
            }
            if let Some(h) = hash_file(&p, Some(PARTIAL), task) {
                by.entry(h).or_default().push((p, m));
            }
        }
        partial_sets.extend(by.into_values().filter(|v| v.len() > 1).map(|v| (size, v)));
    }
    task.set_phase("Comparing whole files");
    for (size, files) in partial_sets {
        let mut by: HashMap<[u8; 32], Vec<(PathBuf, u64)>> = HashMap::new();
        for (p, m) in files {
            if !th.tick(task) {
                return Vec::new();
            }
            match hash_file(&p, None, task) {
                Some(h) => by.entry(h).or_default().push((p, m)),
                None if task.cancelled() => return Vec::new(),
                None => {}
            }
        }
        for (_, v) in by.into_iter().filter(|(_, v)| v.len() > 1) {
            groups.push(Group {
                id: 0,
                size,
                wasted: size * (v.len() as u64 - 1),
                files: v
                    .into_iter()
                    .map(|(p, m)| DupFile {
                        path: p.to_string_lossy().into_owned(),
                        mtime: m,
                    })
                    .collect(),
            });
        }
    }
    groups.sort_by(|a, b| {
        b.wasted
            .cmp(&a.wasted)
            .then(a.files[0].path.cmp(&b.files[0].path))
    });
    for (i, g) in groups.iter_mut().enumerate() {
        g.id = i as u32;
        g.files.sort_by(|a, b| a.path.cmp(&b.path));
    }
    groups
}

/// Which file of a group stays.
pub fn pick_keeper(g: &Group, keep: Keep) -> usize {
    let n = g.files.len();
    match keep {
        Keep::Index(i) if i < n => i,
        Keep::Oldest => (0..n).min_by_key(|&i| (g.files[i].mtime, i)).unwrap_or(0),
        Keep::ShortestPath => (0..n)
            .min_by_key(|&i| (g.files[i].path.len(), i))
            .unwrap_or(0),
        _ => (0..n)
            .max_by_key(|&i| (g.files[i].mtime, std::cmp::Reverse(i)))
            .unwrap_or(0),
    }
}

/// The batch that moves every copy except the keepers to the Recycle Bin. A
/// keeper is never put in the removal list, and a group with fewer than two
/// existing files is skipped.
pub fn build_batch(env: &Env, groups: &[&Group], choice: &dyn Fn(&Group) -> Keep) -> Option<Batch> {
    let mut entries = Vec::new();
    let mut keepers = Vec::new();
    for g in groups {
        let k = pick_keeper(g, choice(g));
        let keeper = PathBuf::from(&g.files[k].path);
        keepers.push(keeper.clone());
        for (i, f) in g.files.iter().enumerate() {
            let p = PathBuf::from(&f.path);
            if i == k || p == keeper || is_protected(env, &p) {
                continue;
            }
            entries.push(Entry {
                path: p,
                size: g.size,
                stamp: f.mtime,
            });
        }
    }
    (!entries.is_empty()).then(|| Batch {
        label: "Duplicate files".to_owned(),
        mode: Mode::Recycle,
        entries,
        keepers,
        bin_items: 0,
        bin_bytes: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_in(root: &Path) -> Env {
        let profile = root.join("Users").join("me");
        std::fs::create_dir_all(&profile).unwrap();
        Env {
            local: profile.join("AppData").join("Local"),
            roaming: profile.join("AppData").join("Roaming"),
            temp: profile.join("AppData").join("Local").join("Temp"),
            system_dirs: vec![root.join("Windows"), root.join("Program Files")],
            profile,
        }
    }

    fn run(env: &Env, root: &Path, min: u64) -> Vec<Group> {
        let task = Task::default();
        find(env, root, min, &task, &PauseState::new(), false)
    }

    #[test]
    fn identical_content_with_different_names_is_found() {
        let d = tempfile::tempdir().unwrap();
        let env = env_in(d.path());
        let root = env.profile.join("Pictures");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let data = vec![7u8; 200_000];
        std::fs::write(root.join("a.bin"), &data).unwrap();
        std::fs::write(root.join("sub").join("renamed copy.dat"), &data).unwrap();
        std::fs::write(root.join("sub").join("third.bin"), &data).unwrap();
        let g = run(&env, &root, 1000);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].files.len(), 3);
        assert_eq!(g[0].wasted, 400_000);
    }

    #[test]
    fn same_size_different_content_is_not_a_duplicate() {
        let d = tempfile::tempdir().unwrap();
        let env = env_in(d.path());
        let root = env.profile.join("Data");
        std::fs::create_dir_all(&root).unwrap();
        let a = vec![1u8; 300_000];
        let mut b = a.clone();
        b[10] = 2; // differs inside the first 64 KB
        let mut c = a.clone();
        c[299_999] = 9; // same first 64 KB as `a`: only the full hash tells them apart
        let mut d = a.clone();
        d[299_999] = 8;
        for (n, data) in [("a", &a), ("b", &b), ("c", &c), ("d", &d)] {
            std::fs::write(root.join(n), data).unwrap();
        }
        assert!(run(&env, &root, 1000).is_empty());
    }

    #[test]
    fn hard_links_to_one_file_are_not_duplicates() {
        let d = tempfile::tempdir().unwrap();
        let env = env_in(d.path());
        let root = env.profile.join("Links");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("one.bin"), vec![5u8; 100_000]).unwrap();
        std::fs::hard_link(root.join("one.bin"), root.join("two.bin")).unwrap();
        assert!(run(&env, &root, 1000).is_empty());
        // A real copy next to the links is found, and counts as one extra copy only.
        std::fs::write(root.join("copy.bin"), vec![5u8; 100_000]).unwrap();
        let g = run(&env, &root, 1000);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].files.len(), 2);
    }

    #[test]
    fn small_files_and_program_folders_are_skipped() {
        let d = tempfile::tempdir().unwrap();
        let env = env_in(d.path());
        let root = d.path().to_path_buf();
        let pf = root.join("Program Files").join("App");
        std::fs::create_dir_all(&pf).unwrap();
        let data = vec![3u8; 50_000];
        std::fs::write(pf.join("x.dll"), &data).unwrap();
        std::fs::write(pf.join("y.dll"), &data).unwrap();
        let docs = env.profile.join("Docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("s1"), vec![1u8; 10]).unwrap();
        std::fs::write(docs.join("s2"), vec![1u8; 10]).unwrap();
        assert!(run(&env, &root, 1000).is_empty());
    }

    #[test]
    fn the_keeper_is_never_in_the_removal_list() {
        let d = tempfile::tempdir().unwrap();
        let env = env_in(d.path());
        let g = Group {
            id: 0,
            size: 10,
            wasted: 20,
            files: ["a", "bb", "ccc"]
                .iter()
                .enumerate()
                .map(|(i, n)| DupFile {
                    path: env.profile.join(n).to_string_lossy().into_owned(),
                    mtime: 100 + i as u64,
                })
                .collect(),
        };
        for keep in [
            Keep::Newest,
            Keep::Oldest,
            Keep::ShortestPath,
            Keep::Index(1),
            Keep::Index(99),
        ] {
            let b = build_batch(&env, &[&g], &|_| keep).unwrap();
            let k = &b.keepers[0];
            assert_eq!(b.entries.len(), 2, "{keep:?}");
            assert!(b.entries.iter().all(|e| &e.path != k), "{keep:?}");
        }
        assert_eq!(pick_keeper(&g, Keep::Newest), 2);
        assert_eq!(pick_keeper(&g, Keep::Oldest), 0);
        assert_eq!(pick_keeper(&g, Keep::ShortestPath), 0);
        assert_eq!(pick_keeper(&g, Keep::Index(1)), 1);
    }
}
