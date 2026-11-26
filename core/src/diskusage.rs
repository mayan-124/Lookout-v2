//! Disk usage analyzer: a size tree of one folder or drive.
//!
//! Strictly on demand. Nothing here runs at start-up, no watcher exists and no
//! scan repeats by itself: a scan starts when the user picks a place and presses
//! Scan, closing the view drops the whole tree, and nothing is written to disk
//! unless the user presses Save (then it is sealed with the index key and is
//! removed by "Delete my index").
//!
//! The scan only lists folders (names, sizes, attributes, dates). It never
//! opens a file, so cloud placeholders are not downloaded. Folders that are
//! junctions, symlinks or cloud placeholders are listed but not entered.
//!
//! Memory: every folder gets a 48-byte node, plus an 88-byte [`Split`] (bytes
//! per file type) if it holds any data; each folder keeps only its
//! [`FILES_PER_FOLDER`] largest files as nodes plus one "other files" row, so a
//! drive with millions of files costs about as much as its folder count. Past
//! [`NODE_CAP`] nodes deeper folders get no node of their own: their sizes still
//! count in the nearest folder that has one and the view says detail is limited.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::crypto::{Key, OpenReader, SealWriter};
use crate::index_service::Throttle;
use crate::pause::PauseState;

mod fast;
pub use fast::{fast_info, FastInfo};

pub const NODE_CAP: usize = 1_200_000;
pub const FILES_PER_FOLDER: usize = 40;
/// Most segments one chart answer holds (plus its "Other" ones).
pub const CHART_CAP: usize = 1500;
/// [`ChartNode::cat`] of anything with no known file type.
const CAT_OTHER: u8 = 7;
const CAT_SYSTEM: u8 = 8;
const CAT_IMAGES: u8 = 9;
const CAT_DATA: u8 = 10;
/// Number of file types.
const CATS: usize = 11;
/// Types held by a version 3 saved scan.
const CATS_V3: usize = 8;
/// Bytes on disk per file type, in the order of [`file_type`].
pub type Split = [u64; CATS];

/// File type for the chart's "by type" colours, from the extension of a UTF-8
/// or UTF-16 name alone. Same order as `TYPES` in ui/src/chartcolors.ts:
/// documents, images, video, audio, archives, code, apps, other, system and
/// libraries, disk images and VMs, data and models. Only the last extension
/// counts, and one that means different things to different programs is "other".
fn file_type<C: Copy + PartialEq + From<u8> + TryInto<u8>>(name: &[C]) -> u8 {
    let Some(dot) = name.iter().rposition(|&c| c == C::from(b'.')) else {
        return CAT_OTHER;
    };
    let ext = &name[dot + 1..];
    let mut low = [0u8; 12];
    if ext.is_empty() || ext.len() > low.len() {
        return CAT_OTHER;
    }
    for (to, &from) in low.iter_mut().zip(ext) {
        let Ok(b) = from.try_into() else {
            return CAT_OTHER;
        };
        *to = b.to_ascii_lowercase();
    }
    match &low[..ext.len()] {
        b"txt" | b"md" | b"pdf" | b"doc" | b"docx" | b"docm" | b"odt" | b"rtf" | b"xls"
        | b"xlsx" | b"xlsm" | b"ods" | b"csv" | b"ppt" | b"pptx" | b"odp" | b"epub" | b"tex"
        | b"one" | b"msg" | b"eml" => 0,
        b"jpg" | b"jpeg" | b"png" | b"gif" | b"bmp" | b"tif" | b"tiff" | b"webp" | b"heic"
        | b"heif" | b"svg" | b"ico" | b"raw" | b"cr2" | b"nef" | b"arw" | b"dng" | b"psd"
        | b"avif" => 1,
        b"mp4" | b"mkv" | b"avi" | b"mov" | b"wmv" | b"flv" | b"webm" | b"m4v" | b"mpg"
        | b"mpeg" | b"m2ts" | b"vob" | b"3gp" => 2,
        b"mp3" | b"wav" | b"flac" | b"aac" | b"ogg" | b"m4a" | b"wma" | b"opus" | b"aiff"
        | b"mid" => 3,
        b"zip" | b"7z" | b"rar" | b"tar" | b"gz" | b"tgz" | b"bz2" | b"xz" | b"zst" | b"cab"
        | b"nupkg" | b"crate" | b"whl" => 4,
        b"rs" | b"py" | b"js" | b"mjs" | b"cjs" | b"ts" | b"tsx" | b"jsx" | b"java" | b"c"
        | b"h" | b"cpp" | b"hpp" | b"cc" | b"cs" | b"go" | b"rb" | b"php" | b"swift" | b"kt"
        | b"html" | b"htm" | b"css" | b"scss" | b"svelte" | b"vue" | b"json" | b"toml"
        | b"yaml" | b"yml" | b"xml" | b"sql" | b"sh" | b"ps1" | b"bat" | b"cmd" | b"lua"
        | b"ipynb" => 5,
        b"exe" | b"msi" | b"msix" | b"appx" | b"apk" | b"jar" | b"scr" => 6,
        b"dll" | b"sys" | b"drv" | b"ocx" | b"mui" | b"mun" | b"cat" | b"etl" | b"evtx"
        | b"dmp" | b"pdb" | b"lib" | b"a" | b"o" | b"rlib" | b"rmeta" | b"so" | b"pyd"
        | b"wasm" => CAT_SYSTEM,
        b"iso" | b"img" | b"vhd" | b"vhdx" | b"vmdk" | b"vdi" | b"qcow2" | b"ova" | b"wim"
        | b"esd" => CAT_IMAGES,
        b"pak" | b"bin" | b"dat" | b"db" | b"sqlite" | b"sqlite3" | b"mdf" | b"ldb" | b"sst"
        | b"parquet" | b"jsonl" | b"gguf" | b"onnx" | b"tflite" | b"safetensors" | b"pt"
        | b"pth" | b"ckpt" | b"heapsnapshot" => CAT_DATA,
        _ => CAT_OTHER,
    }
}

/// [`file_type`] as it was when version 3 scans were saved, so that such a
/// scan looks as it did until the folder is scanned again.
fn file_type_v3(name: &[u8]) -> u8 {
    let Some(dot) = name.iter().rposition(|&c| c == b'.') else {
        return CAT_OTHER;
    };
    match &name[dot + 1..].to_ascii_lowercase()[..] {
        b"dll" | b"sys" | b"drv" | b"ocx" => 6,
        b"iso" | b"img" | b"vhd" | b"vhdx" | b"vmdk" | b"wim" => 4,
        b"nupkg" | b"crate" | b"whl" => CAT_OTHER,
        _ => file_type(name).min(CAT_OTHER),
    }
}

fn add_to(to: &mut Split, from: &Split) {
    for (t, f) in to.iter_mut().zip(from) {
        *t = t.saturating_add(*f);
    }
}

/// The type holding the most bytes (the first of equals); "other" for none.
fn dominant(s: &Split) -> u8 {
    let mut best = (CAT_OTHER, 0);
    for (cat, &bytes) in (0u8..).zip(s) {
        if bytes > best.1 {
            best = (cat, bytes);
        }
    }
    best.0
}

/// Files at least this big (size on disk) are remembered for the "largest
/// files" and "old large files" tabs, up to [`BIG_CAP`] of the largest.
pub const BIG_MIN: u64 = 1 << 20;
pub const BIG_CAP: usize = 20_000;

const ATTR_DIRECTORY: u32 = 0x10;
const ATTR_SPARSE: u32 = 0x200;
const ATTR_REPARSE: u32 = 0x400;
const ATTR_COMPRESSED: u32 = 0x800;
const ATTR_OFFLINE: u32 = 0x1000;
const ATTR_RECALL_ON_OPEN: u32 = 0x4_0000;
const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
/// Attributes that mean "not stored locally": never entered, never counted on disk.
const ATTR_CLOUD: u32 = ATTR_OFFLINE | ATTR_RECALL_ON_OPEN | ATTR_RECALL_ON_DATA_ACCESS;

/// Node flags.
pub const F_UNREADABLE: u8 = 1;
/// A junction, symlink or cloud placeholder folder: listed, not entered.
pub const F_LINK: u8 = 2;
/// Sizes below this folder include folders that have no node of their own.
pub const F_LIMITED: u8 = 4;
/// Something below this folder could not be read, so its size is a lower bound.
pub const F_PARTIAL: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Folder = 0,
    File = 1,
    /// "Other files": every file of a folder that is not one of its largest.
    Others = 2,
}

#[derive(Debug, Clone, Copy)]
struct Node {
    name_off: u32,
    name_len: u16,
    kind: u8,
    flags: u8,
    parent: u32,
    first_child: u32,
    next: u32,
    size_disk: u64,
    logical: u64,
    items: u32,
    mtime: u32,
    /// Index into [`Tree::splits`] for a folder or "other files" row that holds
    /// data; [`NONE`] otherwise (a file's type comes from its name).
    split: u32,
}

const NONE: u32 = u32::MAX;

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct TreeStats {
    pub files: u64,
    pub folders: u64,
    pub unreadable: u64,
    /// Folders left out of the tree to stay under the memory cap.
    pub folded_folders: u64,
    /// Junctions and symlinks to folders (listed, not followed).
    pub links_skipped: u64,
    /// Cloud-only folders (not entered, so nothing is downloaded).
    pub cloud_folders: u64,
    /// Cloud-only files: listed, but they use no space on this disk.
    pub cloud_files: u64,
    /// Extra paths to a file already counted (hard links), and the bytes not counted twice.
    pub hard_links: u64,
    pub hard_link_bytes: u64,
    /// Space Windows says is used on the drive, when the whole drive was scanned
    /// (0 otherwise). The page shows the part the tree does not cover.
    pub volume_used: u64,
}

/// Used space of a drive as Windows reports it (total - free), for a scan of a
/// whole drive; 0 for any other folder.
pub(crate) fn volume_used(root: &Path) -> u64 {
    use std::path::Component;
    let mut c = root.components();
    let (Some(Component::Prefix(_)), Some(Component::RootDir), None) =
        (c.next(), c.next(), c.next())
    else {
        return 0;
    };
    let mut w: Vec<u16> = root.as_os_str().to_string_lossy().encode_utf16().collect();
    w.push(0);
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: NUL-terminated drive root and valid out-pointers.
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            w.as_ptr(),
            &mut avail,
            &mut total,
            &mut free,
        )
    };
    if ok == 0 {
        0
    } else {
        total.saturating_sub(free)
    }
}

/// A large file remembered during the scan.
#[derive(Debug, Clone)]
pub struct BigFile {
    pub path: PathBuf,
    pub size_disk: u64,
    pub logical: u64,
    pub mtime: u32,
}

pub struct Tree {
    nodes: Vec<Node>,
    names: Vec<u8>,
    /// Bytes per file type, for the nodes whose `split` points here.
    splits: Vec<Split>,
    root: PathBuf,
    pub stats: TreeStats,
    /// Unix seconds when the scan finished.
    pub scanned_at: u64,
    cluster: u32,
    /// Largest files first.
    big: Vec<BigFile>,
    /// Some files of at least `BIG_MIN` were dropped because the list was full.
    big_overflow: bool,
    /// While overflowed, files below this size may be missing.
    big_floor: u64,
    /// Loaded from a version 3 saved scan: types 8 and up do not exist in it.
    old_types: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Name,
    Size,
    Items,
    Modified,
}

impl SortBy {
    pub fn parse(s: &str) -> SortBy {
        match s {
            "name" => SortBy::Name,
            "items" => SortBy::Items,
            "modified" => SortBy::Modified,
            _ => SortBy::Size,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub id: u32,
    pub name: String,
    pub kind: NodeKind,
    pub size_disk: u64,
    pub logical: u64,
    pub items: u32,
    pub modified: u32,
    /// Share of the parent folder's size on disk, 0..=100.
    pub percent: f32,
    pub unreadable: bool,
    pub link: bool,
    pub limited: bool,
    pub partial: bool,
    pub has_children: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChartNode {
    /// `None` for the grouped "Other" and "not listed" segments.
    pub id: Option<u32>,
    pub name: String,
    /// "folder", "file", "files" (the other-files row), "other" or "unlisted".
    pub kind: &'static str,
    pub size: u64,
    /// Items inside a folder, 1 for a file, how many were grouped.
    pub items: u32,
    /// Unix seconds (a folder: the newest inside); 0 when unknown.
    pub modified: u32,
    /// File type (see [`file_type`]) holding the most bytes.
    pub cat: u8,
    /// Bytes per file type; adds up to `size`.
    pub split: Split,
    pub children: Vec<ChartNode>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BigRow {
    /// Position in the scan's large-file list; actions name a file by this.
    pub idx: u32,
    pub name: String,
    pub folder: String,
    pub size_disk: u64,
    pub logical: u64,
    pub modified: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Crumb {
    pub id: u32,
    pub name: String,
}

impl Tree {
    fn name(&self, n: &Node) -> String {
        let s = n.name_off as usize;
        String::from_utf8_lossy(&self.names[s..s + n.name_len as usize]).into_owned()
    }

    pub fn root_path(&self) -> &Path {
        &self.root
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Bytes this tree holds in memory.
    pub fn heap_bytes(&self) -> usize {
        self.nodes.capacity() * std::mem::size_of::<Node>()
            + self.names.capacity()
            + self.splits.capacity() * std::mem::size_of::<Split>()
    }

    /// Bytes per file type of a node.
    fn split_of(&self, id: u32) -> Split {
        let n = &self.nodes[id as usize];
        if n.kind != 1 {
            return self
                .splits
                .get(n.split as usize)
                .copied()
                .unwrap_or_default();
        }
        let s = n.name_off as usize;
        let mut split = Split::default();
        let name = &self.names[s..s + n.name_len as usize];
        let cat = if self.old_types {
            file_type_v3(name)
        } else {
            file_type(name)
        };
        split[usize::from(cat)] = n.size_disk;
        split
    }

    /// Adds to the bytes per type of a folder or "other files" row.
    fn add_split(&mut self, id: u32, add: &Split) {
        if add.iter().all(|&b| b == 0) {
            return;
        }
        let n = &mut self.nodes[id as usize];
        if n.split == NONE {
            n.split = self.splits.len() as u32;
            self.splits.push(Split::default());
        }
        add_to(&mut self.splits[n.split as usize], add);
    }

    pub fn total(&self) -> (u64, u64, u32) {
        let r = &self.nodes[0];
        (r.size_disk, r.logical, r.items)
    }

    pub fn kind_of(&self, id: u32) -> Option<NodeKind> {
        self.nodes.get(id as usize).map(|n| match n.kind {
            0 => NodeKind::Folder,
            1 => NodeKind::File,
            _ => NodeKind::Others,
        })
    }

    /// (items, size on disk) under `id`, for previews.
    pub fn totals_of(&self, id: u32) -> Option<(u32, u64)> {
        let n = self.nodes.get(id as usize)?;
        let items = if n.kind == 0 {
            n.items.saturating_add(1)
        } else {
            n.items
        };
        Some((items, n.size_disk))
    }

    /// Full path of a node. `None` for the "other files" row, which is not a path.
    pub fn path(&self, id: u32) -> Option<PathBuf> {
        let mut parts = Vec::new();
        let mut cur = id;
        if self.nodes.get(cur as usize)?.kind == 2 {
            return None;
        }
        while cur != 0 {
            let n = self.nodes.get(cur as usize)?;
            parts.push(self.name(n));
            cur = n.parent;
        }
        let mut p = self.root.clone();
        for part in parts.iter().rev() {
            p.push(part);
        }
        Some(p)
    }

    pub fn breadcrumb(&self, id: u32) -> Vec<Crumb> {
        let mut out = Vec::new();
        let mut cur = id;
        while let Some(n) = self.nodes.get(cur as usize) {
            let name = if cur == 0 {
                self.root.to_string_lossy().into_owned()
            } else {
                self.name(n)
            };
            out.push(Crumb { id: cur, name });
            if cur == 0 {
                break;
            }
            cur = n.parent;
        }
        out.reverse();
        out
    }

    pub fn children(&self, id: u32, sort: SortBy, descending: bool) -> Vec<Row> {
        let Some(parent) = self.nodes.get(id as usize) else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        let mut cur = parent.first_child;
        while cur != NONE {
            let n = &self.nodes[cur as usize];
            rows.push(self.row(cur, n, parent.size_disk));
            cur = n.next;
        }
        rows.sort_by(|a, b| {
            let o = match sort {
                SortBy::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortBy::Size => a.size_disk.cmp(&b.size_disk),
                SortBy::Items => a.items.cmp(&b.items),
                SortBy::Modified => a.modified.cmp(&b.modified),
            };
            let o = if descending { o.reverse() } else { o };
            // The "other files" row always sits last, whatever the sort.
            (a.kind == NodeKind::Others)
                .cmp(&(b.kind == NodeKind::Others))
                .then(o)
                .then(a.id.cmp(&b.id))
        });
        rows
    }

    fn row(&self, id: u32, n: &Node, parent_size: u64) -> Row {
        let percent = if parent_size == 0 {
            0.0
        } else {
            (n.size_disk as f64 / parent_size as f64 * 100.0) as f32
        };
        Row {
            id,
            name: if n.kind == 2 {
                format!("{} other files", n.items)
            } else {
                self.name(n)
            },
            kind: match n.kind {
                0 => NodeKind::Folder,
                1 => NodeKind::File,
                _ => NodeKind::Others,
            },
            size_disk: n.size_disk,
            logical: n.logical,
            items: n.items,
            modified: n.mtime,
            percent: percent.min(100.0),
            unreadable: n.flags & F_UNREADABLE != 0,
            link: n.flags & F_LINK != 0,
            limited: n.flags & F_LIMITED != 0,
            partial: n.flags & F_PARTIAL != 0,
            has_children: n.first_child != NONE,
        }
    }

    /// A nested size breakdown below `id` for the ring and treemap charts.
    /// Items under 0.5% of their folder are grouped into "Other (N items)";
    /// space the tree has no rows for (limited detail) is shown as "not
    /// listed". Built level by level, so [`CHART_CAP`] drops the deepest
    /// levels first. Reads names already in the tree, never the disk.
    pub fn chart(&self, id: u32, levels: u32) -> Option<ChartNode> {
        self.nodes.get(id as usize)?;
        // Slot 0 is the folder asked for; a parent's slot comes before its children's.
        let mut slots = vec![self.chart_entry(id)];
        let mut at = vec![(Some(id), 0usize, 0u32)]; // node, parent slot, level
        let mut next = 0;
        while next < slots.len() {
            let (node, _, level) = at[next];
            let parent = next;
            next += 1;
            let Some(node) = node else { continue };
            let n = &self.nodes[node as usize];
            if level >= levels || n.kind != 0 {
                continue;
            }
            let mut kids: Vec<(u32, u64)> = Vec::new();
            let mut cur = n.first_child;
            while cur != NONE {
                let c = &self.nodes[cur as usize];
                if c.size_disk > 0 {
                    kids.push((cur, c.size_disk));
                }
                cur = c.next;
            }
            kids.sort_by_key(|k| Reverse(k.1));
            let listed: u64 = kids.iter().map(|k| k.1).sum();
            let threshold = n.size_disk / 200; // 0.5% of the parent
            let mut keep = kids.partition_point(|k| k.1 >= threshold);
            // One small item is not worth hiding behind "Other".
            if kids.len() - keep == 1 {
                keep += 1;
            }
            if parent > 0 && slots.len() + keep + 2 > CHART_CAP {
                continue;
            }
            for &(cid, _) in &kids[..keep] {
                slots.push(self.chart_entry(cid));
                at.push((Some(cid), parent, level + 1));
            }
            let grouped = &kids[keep..];
            if !grouped.is_empty() {
                let (mut count, mut size, mut modified) = (0u32, 0u64, 0u32);
                let mut split = Split::default();
                for &(cid, s) in grouped {
                    add_to(&mut split, &self.split_of(cid));
                    let c = &self.nodes[cid as usize];
                    count = count.saturating_add(if c.kind == 2 { c.items } else { 1 });
                    size += s;
                    modified = modified.max(c.mtime);
                }
                slots.push(ChartNode {
                    id: None,
                    name: format!("Other ({count} items)"),
                    kind: "other",
                    size,
                    items: count,
                    modified,
                    cat: dominant(&split),
                    split,
                    children: Vec::new(),
                });
                at.push((None, parent, level + 1));
            }
            let unlisted = n.size_disk.saturating_sub(listed);
            if unlisted > threshold {
                // The folder's types less those of everything it has rows for.
                let mut rows = Split::default();
                for &(cid, _) in &kids {
                    add_to(&mut rows, &self.split_of(cid));
                }
                let mut split = self.split_of(node);
                for (s, r) in split.iter_mut().zip(rows) {
                    *s = s.saturating_sub(r);
                }
                slots.push(ChartNode {
                    id: None,
                    name: "not listed (limited detail)".to_owned(),
                    kind: "unlisted",
                    size: unlisted,
                    items: 0,
                    modified: 0,
                    cat: dominant(&split),
                    split,
                    children: Vec::new(),
                });
                at.push((None, parent, level + 1));
            }
        }
        // Attach from the end: every child is complete before its parent.
        while slots.len() > 1 {
            let mut child = slots.pop()?;
            child.children.reverse();
            let (_, parent, _) = at[slots.len()];
            slots[parent].children.push(child);
        }
        let mut root = slots.pop()?;
        root.children.reverse();
        Some(root)
    }

    fn chart_entry(&self, id: u32) -> ChartNode {
        let n = &self.nodes[id as usize];
        let split = self.split_of(id);
        ChartNode {
            id: Some(id),
            name: if id == 0 {
                self.root.to_string_lossy().into_owned()
            } else if n.kind == 2 {
                format!("{} other files", n.items)
            } else {
                self.name(n)
            },
            kind: match n.kind {
                0 => "folder",
                1 => "file",
                _ => "files",
            },
            size: n.size_disk,
            items: n.items,
            modified: n.mtime,
            cat: dominant(&split),
            split,
            children: Vec::new(),
        }
    }

    /// Remembered large files (largest first) that are at least `min_size` on
    /// disk and, if given, last modified before `modified_before`.
    pub fn big_files(
        &self,
        min_size: u64,
        modified_before: Option<u32>,
        limit: usize,
    ) -> Vec<BigRow> {
        self.big
            .iter()
            .enumerate()
            .filter(|(_, f)| f.size_disk >= min_size && modified_before.is_none_or(|t| f.mtime < t))
            .take(limit)
            .map(|(i, f)| BigRow {
                idx: i as u32,
                name: f
                    .path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                folder: f
                    .path
                    .parent()
                    .map_or_else(String::new, |p| p.to_string_lossy().into_owned()),
                size_disk: f.size_disk,
                logical: f.logical,
                modified: f.mtime,
            })
            .collect()
    }

    pub fn big_file(&self, idx: u32) -> Option<&BigFile> {
        self.big.get(idx as usize)
    }

    /// Whether the large-file list is complete for files of `min_size` and up.
    pub fn big_complete_for(&self, min_size: u64) -> bool {
        min_size >= BIG_MIN && (!self.big_overflow || min_size >= self.big_floor)
    }

    /// How many large files are remembered.
    pub fn big_len(&self) -> usize {
        self.big.len()
    }

    fn add(&mut self, name: &str, kind: u8, flags: u8, parent: u32) -> u32 {
        let id = self.nodes.len() as u32;
        let name: String = name.chars().take(1000).collect();
        let off = self.names.len() as u32;
        self.names.extend_from_slice(name.as_bytes());
        let first = if parent == NONE {
            NONE
        } else {
            self.nodes[parent as usize].first_child
        };
        self.nodes.push(Node {
            name_off: off,
            name_len: name.len().min(usize::from(u16::MAX)) as u16,
            kind,
            flags,
            parent: if parent == NONE { 0 } else { parent },
            first_child: NONE,
            next: first,
            size_disk: 0,
            logical: 0,
            items: 0,
            mtime: 0,
            split: NONE,
        });
        if parent != NONE {
            self.nodes[parent as usize].first_child = id;
        }
        id
    }
}

// ---------------------------------------------------------------------------
// Scanning

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanState {
    Idle,
    Scanning,
    Done,
    Cancelled,
    Failed,
}

fn state_of(v: u8) -> ScanState {
    match v {
        1 => ScanState::Scanning,
        2 => ScanState::Done,
        3 => ScanState::Cancelled,
        4 => ScanState::Failed,
        _ => ScanState::Idle,
    }
}

/// Live numbers of a running scan.
#[derive(Default)]
pub struct Progress {
    state: AtomicU8,
    files: AtomicU64,
    folders: AtomicU64,
    bytes: AtomicU64,
    unreadable: AtomicU64,
    waiting_for_resume: AtomicBool,
    cancel: AtomicBool,
    current: Mutex<String>,
    /// Why the scan failed, in words for the user.
    message: Mutex<String>,
    /// Peak memory of the fast-scan helper process, reported when it finishes.
    helper_peak: AtomicU64,
    // Where the time went, summed over all scan threads (nanoseconds).
    list_ns: AtomicU64,
    size_ns: AtomicU64,
    tree_ns: AtomicU64,
    lock_wait_ns: AtomicU64,
    link_ns: AtomicU64,
    cloud_files: AtomicU64,
    hard_links: AtomicU64,
    hard_link_bytes: AtomicU64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgressView {
    pub state: ScanState,
    pub files: u64,
    pub folders: u64,
    pub bytes: u64,
    pub unreadable: u64,
    pub paused: bool,
    pub current: String,
    /// Why a failed scan failed (empty otherwise).
    pub message: String,
    pub root: String,
    /// Set when this tree came from a saved scan, not from a fresh one.
    pub saved_at: Option<u64>,
}

#[derive(Clone)]
pub struct ScanOptions {
    pub node_cap: usize,
    pub files_per_folder: usize,
    /// Busy fraction of each scan thread, `0.01..=1.0` (1.0 = full speed).
    pub duty: f32,
    /// Scan threads, each taking whole folders from a shared queue.
    pub threads: usize,
}

/// Half the logical cores, at least 2 and at most 8.
pub fn default_threads() -> usize {
    let n = std::thread::available_parallelism().map_or(2, std::num::NonZeroUsize::get);
    (n / 2).clamp(2, 8)
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            node_cap: NODE_CAP,
            files_per_folder: FILES_PER_FOLDER,
            duty: 1.0,
            threads: default_threads(),
        }
    }
}

fn unix_secs_from_filetime(ft: u64) -> u32 {
    u32::try_from((ft / 10_000_000).saturating_sub(11_644_473_600)).unwrap_or(u32::MAX)
}

fn cluster_size(path: &Path) -> u32 {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceW;
    let Some(root) = path.ancestors().last() else {
        return 4096;
    };
    let mut wide: Vec<u16> = root.as_os_str().to_string_lossy().encode_utf16().collect();
    if !wide.ends_with(&[u16::from(b'\\')]) {
        wide.push(u16::from(b'\\'));
    }
    wide.push(0);
    let (mut spc, mut bps, mut a, mut b) = (0u32, 0u32, 0u32, 0u32);
    // SAFETY: `wide` is NUL-terminated and the four outputs are valid for writes.
    let ok = unsafe { GetDiskFreeSpaceW(wide.as_ptr(), &mut spc, &mut bps, &mut a, &mut b) };
    let c = spc.saturating_mul(bps);
    if ok == 0 || c == 0 {
        4096
    } else {
        c
    }
}

fn size_on_disk(attrs: u32, logical: u64, cluster: u32, path: &Path) -> u64 {
    if attrs & ATTR_CLOUD != 0 {
        return 0; // not stored on this disk
    }
    let rounded = || logical.div_ceil(u64::from(cluster)) * u64::from(cluster);
    if attrs & (ATTR_COMPRESSED | ATTR_SPARSE) != 0 {
        use windows_sys::Win32::Storage::FileSystem::GetCompressedFileSizeW;
        let mut wide: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().collect();
        wide.push(0);
        let mut high = 0u32;
        // SAFETY: NUL-terminated path; reads file attributes only, not its data.
        let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
        if low == u32::MAX {
            return rounded();
        }
        return (u64::from(high) << 32) | u64::from(low);
    }
    rounded()
}

struct Job {
    /// The folder's own node, or `None` when it was folded into `anchor`.
    node: Option<u32>,
    /// Nearest ancestor that has a node (itself when it has one).
    anchor: u32,
    path: PathBuf,
}

/// A subfolder found in a listing (the tree decides whether it gets a node).
struct SubDir {
    name: OsString,
    mtime: u32,
    /// Junction, symlink or cloud placeholder: listed, never entered.
    link: bool,
    cloud: bool,
}

type FileEntry = Reverse<(u64, u64, u32, String)>;

/// Everything one folder listing found, gathered without touching the tree.
#[derive(Default)]
struct Listing {
    subdirs: Vec<SubDir>,
    files: u32,
    disk: u64,
    logical: u64,
    newest: u32,
    top: Vec<(u64, u64, u32, String)>,
    rest_n: u32,
    rest_disk: u64,
    rest_logical: u64,
    rest_new: u32,
    big: Vec<(u64, u64, u32, PathBuf)>,
    /// Bytes per file type of all the files.
    types: Split,
    cloud_files: u64,
    hard_links: u64,
    hard_link_bytes: u64,
}

type BigEntry = Reverse<(u64, u64, u32, PathBuf)>;

/// What the scan threads share besides the tree: the large-file list and the
/// hard-link identities already counted.
struct Collect {
    heap: Mutex<BinaryHeap<BigEntry>>,
    floor: AtomicU64,
    overflow: AtomicBool,
    /// File IDs seen so far, in shards so scan threads rarely wait for each other.
    links: Vec<Mutex<HashSet<u128>>>,
}

impl Collect {
    fn new() -> Self {
        Self {
            heap: Mutex::new(BinaryHeap::new()),
            floor: AtomicU64::new(BIG_MIN),
            overflow: AtomicBool::new(false),
            links: (0..16).map(|_| Mutex::new(HashSet::new())).collect(),
        }
    }

    fn add(&self, items: Vec<(u64, u64, u32, PathBuf)>) {
        let mut h = self.heap.lock().unwrap_or_else(PoisonError::into_inner);
        for it in items {
            if h.len() >= BIG_CAP {
                self.overflow.store(true, Ordering::Relaxed);
                if h.peek().is_some_and(|m| it.0 <= m.0 .0) {
                    continue;
                }
                h.pop();
            }
            h.push(Reverse(it));
        }
        if h.len() >= BIG_CAP {
            let f = h.peek().map_or(BIG_MIN, |m| m.0 .0).max(BIG_MIN);
            self.floor.store(f, Ordering::Relaxed);
        }
    }

    /// True the first time a file identity is seen.
    fn first_sight(&self, key: u128) -> bool {
        self.links[(key as usize).wrapping_mul(0x9E37_79B9) >> 7 & 15]
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key)
    }
}

// Returned once per folder and unpacked at once: boxing would only add an allocation.
#[allow(clippy::large_enum_variant)]
enum Listed {
    Done(Listing),
    Unreadable,
    Cancelled,
}

fn nanos(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// The path as UTF-16, `\\?\`-prefixed when it is a plain drive path so long paths work.
fn long_wide(path: &Path) -> Vec<u16> {
    long_prefixed(path.as_os_str().encode_wide().collect())
}

/// Adds `\\?\` to a drive path and `\\?\UNC\` to a `\\server\share` path;
/// anything already extended (`\\?\`, `\\.\`) or relative is left alone.
fn long_prefixed(w: Vec<u16>) -> Vec<u16> {
    let bs = u16::from(b'\\');
    let plain_drive = w.len() >= 3 && w[1] == u16::from(b':') && w[2] == bs;
    let unc = w.len() > 2
        && w[0] == bs
        && w[1] == bs
        && w[2] != u16::from(b'?')
        && w[2] != u16::from(b'.');
    let mut out: Vec<u16> = Vec::with_capacity(w.len() + 8);
    if plain_drive {
        out.extend(r"\\?\".encode_utf16());
        out.extend(w);
    } else if unc {
        out.extend(r"\\?\UNC\".encode_utf16());
        out.extend(&w[2..]);
    } else {
        out = w;
    }
    out
}

/// Search pattern for a folder: its long path, then `\*`.
/// Lists one folder through its directory handle (no short names, a large
/// buffer, and each entry's file ID). Size, attributes and times come from the listing itself: a normal
/// file costs no further system call, and only files that are not among a
/// folder's largest get no name decoded at all.
fn list_folder(
    job: &Job,
    keep_top: bool,
    opts: &ScanOptions,
    progress: &Progress,
    collect: &Collect,
    cluster: u32,
) -> Listed {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FileIdExtdDirectoryInfo, FileIdExtdDirectoryRestartInfo,
        GetFileInformationByHandleEx, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    let mut dir_w = long_wide(&job.path);
    dir_w.push(0);
    // SAFETY: NUL-terminated path; the handle is closed below.
    let handle = unsafe {
        CreateFileW(
            dir_w.as_ptr(),
            FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Listed::Unreadable;
    }
    let mut l = Listing::default();
    let mut top: BinaryHeap<FileEntry> = BinaryHeap::new();
    let (mut seen, mut size_ns) = (0u32, 0u64);
    let mut cancelled = false;
    // 128 KB of directory entries per call (8-byte aligned).
    let mut buf = vec![0u64; 16 * 1024];
    let mut first_call = true;
    // Offsets inside FILE_ID_EXTD_DIR_INFO.
    const O_WRITE: usize = 24;
    const O_EOF: usize = 40;
    const O_ATTRS: usize = 56;
    const O_NAMELEN: usize = 60;
    const O_ID: usize = 72;
    const O_NAME: usize = 88;
    fn rd<T: Copy>(b: &[u8], o: usize) -> T {
        // SAFETY: callers keep `o + size_of::<T>()` inside the filled buffer.
        unsafe { std::ptr::read_unaligned(b.as_ptr().add(o).cast::<T>()) }
    }
    'listing: loop {
        // SAFETY: `handle` is a live directory handle; `buf` is writable for its length.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                handle,
                if first_call {
                    FileIdExtdDirectoryRestartInfo
                } else {
                    FileIdExtdDirectoryInfo
                },
                buf.as_mut_ptr().cast(),
                (buf.len() * 8) as u32,
            )
        };
        first_call = false;
        if ok == 0 {
            break; // no more entries (or the folder could not be read further)
        }
        // SAFETY: the u64 buffer viewed as bytes, same length.
        let bytes: &[u8] =
            unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), buf.len() * 8) };
        let mut off = 0usize;
        loop {
            let name_len = rd::<u32>(bytes, off + O_NAMELEN) as usize / 2;
            let name: Vec<u16> = (0..name_len)
                .map(|i| rd::<u16>(bytes, off + O_NAME + i * 2))
                .collect();
            let name = &name[..];
            let attrs = rd::<u32>(bytes, off + O_ATTRS);
            let write_ft = rd::<i64>(bytes, off + O_WRITE) as u64;
            let file_size = rd::<i64>(bytes, off + O_EOF) as u64;
            let file_id = rd::<u128>(bytes, off + O_ID);
            let dots = name == [u16::from(b'.')] || name == [u16::from(b'.'), u16::from(b'.')];
            if !dots {
                let mtime = unix_secs_from_filetime(write_ft);
                if attrs & ATTR_DIRECTORY != 0 {
                    l.subdirs.push(SubDir {
                        name: OsString::from_wide(name),
                        mtime,
                        link: attrs & (ATTR_REPARSE | ATTR_CLOUD) != 0,
                        cloud: attrs & ATTR_CLOUD != 0,
                    });
                } else {
                    let mut size = file_size;
                    let mut on_disk = if attrs & (ATTR_CLOUD | ATTR_COMPRESSED | ATTR_SPARSE) == 0 {
                        size.div_ceil(u64::from(cluster)) * u64::from(cluster)
                    } else {
                        let t = Instant::now();
                        let full = job.path.join(OsString::from_wide(name));
                        let v = size_on_disk(attrs, size, cluster, &full);
                        size_ns += nanos(t);
                        v
                    };
                    if attrs & ATTR_CLOUD != 0 {
                        l.cloud_files += 1;
                    }
                    // Hard links: a file with more than one path is counted once, at the
                    // first path found, whatever its size (the Fast scan follows the
                    // same rule). The file ID comes with the directory listing, so no
                    // file is opened for this.
                    let mut duplicate = false;
                    if attrs & (ATTR_CLOUD | ATTR_REPARSE) == 0 && !collect.first_sight(file_id) {
                        duplicate = true;
                        l.hard_links += 1;
                        l.hard_link_bytes += on_disk;
                        (size, on_disk) = (0, 0);
                    }
                    if !duplicate && on_disk >= collect.floor.load(Ordering::Relaxed) {
                        let f = job.path.join(OsString::from_wide(name));
                        l.big.push((on_disk, size, mtime, f));
                    }
                    if on_disk > 0 {
                        l.types[usize::from(file_type(name))] += on_disk;
                    }
                    l.files += 1;
                    l.disk += on_disk;
                    l.logical += size;
                    l.newest = l.newest.max(mtime);
                    let kept = keep_top
                        && opts.files_per_folder > 0
                        && (top.len() < opts.files_per_folder
                            || top.peek().is_some_and(|s| on_disk > s.0 .0));
                    if kept {
                        top.push(Reverse((
                            on_disk,
                            size,
                            mtime,
                            String::from_utf16_lossy(name),
                        )));
                        if top.len() > opts.files_per_folder {
                            if let Some(Reverse((d, s, m, _))) = top.pop() {
                                l.rest_n += 1;
                                l.rest_disk += d;
                                l.rest_logical += s;
                                l.rest_new = l.rest_new.max(m);
                            }
                        }
                    } else {
                        l.rest_n += 1;
                        l.rest_disk += on_disk;
                        l.rest_logical += size;
                        l.rest_new = l.rest_new.max(mtime);
                    }
                }
            }
            seen += 1;
            if seen.is_multiple_of(4096) && progress.cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break 'listing;
            }
            let next = rd::<u32>(bytes, off) as usize;
            if next == 0 {
                break;
            }
            off += next;
        }
    }
    // SAFETY: closing the handle opened above, once.
    unsafe { CloseHandle(handle) };
    progress.size_ns.fetch_add(size_ns, Ordering::Relaxed);
    if cancelled {
        return Listed::Cancelled;
    }
    l.top = top.into_vec().into_iter().map(|Reverse(t)| t).collect();
    Listed::Done(l)
}

/// Work queue shared by the scan threads.
struct Queue {
    jobs: Vec<Job>,
    in_flight: usize,
}

/// Scans `root` with `opts.threads` workers. `None` if cancelled or the root
/// cannot be read.
pub fn scan(
    root: &Path,
    opts: &ScanOptions,
    progress: &Progress,
    pause: &PauseState,
) -> Option<Tree> {
    let cluster = cluster_size(root);
    let mut tree = Tree {
        nodes: Vec::new(),
        names: Vec::new(),
        splits: Vec::new(),
        root: root.to_path_buf(),
        stats: TreeStats::default(),
        scanned_at: 0,
        cluster,
        big: Vec::new(),
        big_overflow: false,
        big_floor: BIG_MIN,
        old_types: false,
    };
    let root_md = std::fs::metadata(root).ok()?;
    if !root_md.is_dir() {
        return None;
    }
    let rid = tree.add(&root.to_string_lossy(), 0, 0, NONE);
    tree.nodes[rid as usize].mtime = unix_secs_from_filetime(root_md.last_write_time());
    let tree = Mutex::new(tree);
    let collect = Collect::new();
    let queue = Mutex::new(Queue {
        jobs: vec![Job {
            node: Some(rid),
            anchor: rid,
            path: root.to_path_buf(),
        }],
        in_flight: 0,
    });
    let wake = Condvar::new();
    let job_count = AtomicU64::new(0);

    std::thread::scope(|scope| {
        for _ in 0..opts.threads.max(1) {
            scope.spawn(|| {
                let mut throttle = Throttle::new(opts.duty);
                loop {
                    // Take a folder, or stop when none are left anywhere.
                    let job = {
                        let mut q = queue.lock().unwrap_or_else(PoisonError::into_inner);
                        loop {
                            if progress.cancel.load(Ordering::Relaxed) {
                                wake.notify_all();
                                return;
                            }
                            if let Some(j) = q.jobs.pop() {
                                q.in_flight += 1;
                                break j;
                            }
                            if q.in_flight == 0 {
                                wake.notify_all();
                                return;
                            }
                            q = wake
                                .wait_timeout(q, Duration::from_millis(50))
                                .unwrap_or_else(PoisonError::into_inner)
                                .0;
                        }
                    };
                    // Pause and cancel are looked at between folders.
                    while pause.is_paused() && !progress.cancel.load(Ordering::Relaxed) {
                        progress.waiting_for_resume.store(true, Ordering::Relaxed);
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    progress.waiting_for_resume.store(false, Ordering::Relaxed);
                    let started = Instant::now();
                    if job_count.fetch_add(1, Ordering::Relaxed).is_multiple_of(64) {
                        *progress
                            .current
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner) =
                            job.path.to_string_lossy().into_owned();
                    }
                    let listed =
                        list_folder(&job, job.node.is_some(), opts, progress, &collect, cluster);
                    progress
                        .list_ns
                        .fetch_add(nanos(started), Ordering::Relaxed);
                    let mut new_jobs = Vec::new();
                    match listed {
                        Listed::Cancelled => {
                            let mut q = queue.lock().unwrap_or_else(PoisonError::into_inner);
                            q.in_flight -= 1;
                            wake.notify_all();
                            return;
                        }
                        Listed::Unreadable => {
                            // Not readable: flagged, and never added up as if it were empty.
                            progress.unreadable.fetch_add(1, Ordering::Relaxed);
                            let mut t = tree.lock().unwrap_or_else(PoisonError::into_inner);
                            match job.node {
                                Some(id) => t.nodes[id as usize].flags |= F_UNREADABLE,
                                None => t.nodes[job.anchor as usize].flags |= F_PARTIAL,
                            }
                        }
                        Listed::Done(mut l) => {
                            progress.folders.fetch_add(1, Ordering::Relaxed);
                            progress
                                .files
                                .fetch_add(u64::from(l.files), Ordering::Relaxed);
                            progress.bytes.fetch_add(l.disk, Ordering::Relaxed);
                            progress
                                .cloud_files
                                .fetch_add(l.cloud_files, Ordering::Relaxed);
                            progress
                                .hard_links
                                .fetch_add(l.hard_links, Ordering::Relaxed);
                            progress
                                .hard_link_bytes
                                .fetch_add(l.hard_link_bytes, Ordering::Relaxed);
                            if !l.big.is_empty() {
                                collect.add(std::mem::take(&mut l.big));
                            }
                            let waited = Instant::now();
                            let mut t = tree.lock().unwrap_or_else(PoisonError::into_inner);
                            progress
                                .lock_wait_ns
                                .fetch_add(nanos(waited), Ordering::Relaxed);
                            let built = Instant::now();
                            apply(&mut t, &job, l, opts, &mut new_jobs);
                            drop(t);
                            progress.tree_ns.fetch_add(nanos(built), Ordering::Relaxed);
                        }
                    }
                    {
                        let mut q = queue.lock().unwrap_or_else(PoisonError::into_inner);
                        q.jobs.append(&mut new_jobs);
                        q.in_flight -= 1;
                    }
                    wake.notify_all();
                    throttle.after_work_unless(started.elapsed(), &progress.cancel);
                }
            });
        }
    });
    if progress.cancel.load(Ordering::Relaxed) {
        return None;
    }

    let mut tree = tree.into_inner().unwrap_or_else(PoisonError::into_inner);
    roll_up(&mut tree);
    tree.stats.files = progress.files.load(Ordering::Relaxed);
    tree.stats.folders = progress.folders.load(Ordering::Relaxed);
    tree.stats.unreadable = progress.unreadable.load(Ordering::Relaxed);
    tree.stats.cloud_files = progress.cloud_files.load(Ordering::Relaxed);
    tree.stats.hard_links = progress.hard_links.load(Ordering::Relaxed);
    tree.stats.hard_link_bytes = progress.hard_link_bytes.load(Ordering::Relaxed);
    tree.stats.volume_used = volume_used(&tree.root);
    tree.big_overflow = collect.overflow.load(Ordering::Relaxed);
    tree.big_floor = collect.floor.load(Ordering::Relaxed);
    let mut big: Vec<BigFile> = collect
        .heap
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner)
        .into_iter()
        .map(|Reverse((d, l, m, p))| BigFile {
            path: p,
            size_disk: d,
            logical: l,
            mtime: m,
        })
        .collect();
    big.sort_by(|a, b| b.size_disk.cmp(&a.size_disk).then(a.path.cmp(&b.path)));
    tree.big = big;
    tree.scanned_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    tree.nodes.shrink_to_fit();
    tree.names.shrink_to_fit();
    tree.splits.shrink_to_fit();
    Some(tree)
}

/// Adds every folder's totals into its parent. Children are always created
/// after their parent, so one backwards pass has every folder's own children
/// done before it is added to its parent.
fn roll_up(tree: &mut Tree) {
    for i in (1..tree.nodes.len()).rev() {
        let n = tree.nodes[i];
        if n.kind != 0 {
            continue;
        }
        if let Some(&split) = tree.splits.get(n.split as usize) {
            tree.add_split(n.parent, &split);
        }
        let p = &mut tree.nodes[n.parent as usize];
        p.size_disk = p.size_disk.saturating_add(n.size_disk);
        p.logical = p.logical.saturating_add(n.logical);
        p.items = p.items.saturating_add(n.items).saturating_add(1);
        p.mtime = p.mtime.max(n.mtime);
        if n.flags & (F_UNREADABLE | F_PARTIAL) != 0 {
            p.flags |= F_PARTIAL;
        }
        if n.flags & F_LIMITED != 0 {
            p.flags |= F_LIMITED;
        }
    }
}

/// Adds one folder's listing to the tree (called with the tree locked).
fn apply(tree: &mut Tree, job: &Job, l: Listing, opts: &ScanOptions, new_jobs: &mut Vec<Job>) {
    let anchor = job.anchor;
    for sub in l.subdirs {
        let lossy = sub.name.to_string_lossy();
        if sub.link {
            if sub.cloud {
                tree.stats.cloud_folders += 1;
            } else {
                tree.stats.links_skipped += 1;
            }
            if tree.nodes.len() < opts.node_cap {
                let id = tree.add(&lossy, 0, F_LINK, anchor);
                tree.nodes[id as usize].mtime = sub.mtime;
            }
            continue;
        }
        let path = job.path.join(&sub.name);
        if job.node.is_some() && tree.nodes.len() < opts.node_cap {
            let id = tree.add(&lossy, 0, 0, anchor);
            tree.nodes[id as usize].mtime = sub.mtime;
            new_jobs.push(Job {
                node: Some(id),
                anchor: id,
                path,
            });
        } else {
            // Over the cap: counted into the anchor, no node of its own.
            tree.stats.folded_folders += 1;
            let a = &mut tree.nodes[anchor as usize];
            a.flags |= F_LIMITED;
            a.items = a.items.saturating_add(1);
            new_jobs.push(Job {
                node: None,
                anchor,
                path,
            });
        }
    }
    {
        let a = &mut tree.nodes[anchor as usize];
        a.size_disk = a.size_disk.saturating_add(l.disk);
        a.logical = a.logical.saturating_add(l.logical);
        a.items = a.items.saturating_add(l.files);
        a.mtime = a.mtime.max(l.newest);
    }
    tree.add_split(anchor, &l.types);
    if job.node.is_some() {
        // What the listed files leave belongs to the "other files" row.
        let mut rest = l.types;
        for (d, s, m, name) in l.top {
            let cat = usize::from(file_type(name.as_bytes()));
            rest[cat] = rest[cat].saturating_sub(d);
            let id = tree.add(&name, 1, 0, anchor);
            let n = &mut tree.nodes[id as usize];
            (n.size_disk, n.logical, n.mtime, n.items) = (d, s, m, 1);
        }
        if l.rest_n > 0 {
            let id = tree.add("", 2, 0, anchor);
            let n = &mut tree.nodes[id as usize];
            (n.size_disk, n.logical, n.mtime, n.items) =
                (l.rest_disk, l.rest_logical, l.rest_new, l.rest_n);
            tree.add_split(id, &rest);
        }
    }
}

// ---------------------------------------------------------------------------
// The on-demand service

/// What the Recycle Bin preview needs to know: items and size on disk per path,
/// as of the scan the user is looking at.
pub type Hints = Arc<Mutex<std::collections::HashMap<PathBuf, (u32, u64)>>>;

struct Session {
    progress: Arc<Progress>,
    result: Arc<Mutex<Option<Arc<Tree>>>>,
    root: PathBuf,
    saved_at: Option<u64>,
    handle: Option<JoinHandle<()>>,
}

pub struct DiskService {
    pause: PauseState,
    duty: f32,
    options: ScanOptions,
    pub hints: Hints,
    session: Mutex<Option<Session>>,
    saved_file: Option<PathBuf>,
    key: Option<Key>,
}

#[derive(Debug, thiserror::Error)]
pub enum DiskError {
    #[error("Lookout is paused; resume it to scan")]
    Paused,
    #[error("that is not a folder Lookout can read")]
    NotAFolder,
    #[error("there is no finished scan to use")]
    NoScan,
    #[error("Saved scan is from an older version, please rescan")]
    OldFormat,
    #[error("saving needs Lookout's encrypted storage, which is not available")]
    NoStorage,
    #[error("{0}")]
    Io(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct SavedInfo {
    pub root: String,
    pub saved_at: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Location {
    pub label: String,
    pub path: String,
}

impl DiskService {
    pub fn new(
        pause: PauseState,
        duty: f32,
        saved_file: Option<PathBuf>,
        key: Option<Key>,
        hints: Hints,
    ) -> Self {
        Self {
            pause,
            duty,
            options: ScanOptions::default(),
            hints,
            session: Mutex::new(None),
            saved_file,
            key,
        }
    }

    /// For tests: scan with other limits.
    pub fn with_options(mut self, options: ScanOptions) -> Self {
        self.options = options;
        self
    }

    pub fn duty(&self) -> f32 {
        self.duty
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Session>> {
        self.session.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts a scan on a background thread, replacing whatever was shown.
    /// With `low_impact` the scan uses one thread held to the Balanced duty;
    /// otherwise it runs at full speed on a small thread pool.
    pub fn start(&self, root: &Path, low_impact: bool) -> Result<(), DiskError> {
        if self.pause.is_paused() {
            return Err(DiskError::Paused);
        }
        if !root.is_dir() {
            return Err(DiskError::NotAFolder);
        }
        let opts = ScanOptions {
            duty: if low_impact { self.duty } else { 1.0 },
            threads: if low_impact { 1 } else { self.options.threads },
            ..self.options.clone()
        };
        let pause = self.pause.clone();
        let path = root.to_path_buf();
        self.launch(root, move |p| scan(&path, &opts, p, &pause))
    }

    /// Fast scan: the whole tree comes from the drive's NTFS file table, read
    /// by the separate read-only helper `helper` (Windows asks for permission to
    /// run it). The app itself stays unelevated. A scan of a folder shows that
    /// folder's part of the drive.
    pub fn start_fast(&self, root: &Path, helper: &Path) -> Result<(), DiskError> {
        if self.pause.is_paused() {
            return Err(DiskError::Paused);
        }
        if !root.is_dir() {
            return Err(DiskError::NotAFolder);
        }
        let info = fast_info(root);
        if !info.available {
            return Err(DiskError::Io(info.reason));
        }
        let opts = self.options.clone();
        let (path, helper) = (root.to_path_buf(), helper.to_path_buf());
        self.launch(root, move |p| fast::scan_fast(&path, &helper, &opts, p))
    }

    /// Replaces whatever was shown with a scan running `work` on its own thread.
    fn launch(
        &self,
        root: &Path,
        work: impl FnOnce(&Progress) -> Option<Tree> + Send + 'static,
    ) -> Result<(), DiskError> {
        self.close();
        let progress = Arc::new(Progress::default());
        progress.state.store(1, Ordering::Release);
        let result: Arc<Mutex<Option<Arc<Tree>>>> = Arc::default();
        let (p, r) = (progress.clone(), result.clone());
        let handle = std::thread::Builder::new()
            .name("disk-scan".into())
            .spawn(move || {
                let tree = crate::guard::contain("the disk scan failed", || work(&p)).flatten();
                let state = match tree {
                    Some(t) => {
                        *r.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(t));
                        2
                    }
                    None if p.cancel.load(Ordering::Relaxed) => 3,
                    None => 4,
                };
                p.state.store(state, Ordering::Release);
            })
            .map_err(|e| DiskError::Io(e.to_string()))?;
        *self.lock() = Some(Session {
            progress,
            result,
            root: root.to_path_buf(),
            saved_at: None,
            handle: Some(handle),
        });
        Ok(())
    }

    /// Where the time of the current or last scan went.
    pub fn timings(&self) -> String {
        let guard = self.lock();
        let Some(s) = guard.as_ref() else {
            return String::new();
        };
        let p = &s.progress;
        let sec = |a: &AtomicU64| a.load(Ordering::Relaxed) as f64 / 1e9;
        format!(
            "thread-seconds: listing {:.1} (of which size calls {:.2}, hard-link checks {:.2}), tree building {:.1} (waiting for the tree lock {:.2})",
            sec(&p.list_ns),
            sec(&p.size_ns),
            sec(&p.link_ns),
            sec(&p.tree_ns),
            sec(&p.lock_wait_ns)
        )
    }

    /// Peak working set of the fast-scan helper in the last fast scan (0 if none).
    pub fn helper_peak_bytes(&self) -> u64 {
        self.lock()
            .as_ref()
            .map_or(0, |s| s.progress.helper_peak.load(Ordering::Relaxed))
    }

    pub fn cancel(&self) {
        if let Some(s) = self.lock().as_ref() {
            s.progress.cancel.store(true, Ordering::Release);
        }
    }

    /// Stops any scan, waits for it and drops every byte of scan data.
    pub fn close(&self) {
        let old = self.lock().take();
        if let Some(mut s) = old {
            s.progress.cancel.store(true, Ordering::Release);
            if let Some(h) = s.handle.take() {
                let _ = h.join();
            }
        }
        self.hints
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    pub fn progress(&self) -> ProgressView {
        let guard = self.lock();
        let Some(s) = guard.as_ref() else {
            return ProgressView {
                state: ScanState::Idle,
                files: 0,
                folders: 0,
                bytes: 0,
                unreadable: 0,
                paused: false,
                current: String::new(),
                message: String::new(),
                root: String::new(),
                saved_at: None,
            };
        };
        let p = &s.progress;
        let current = p
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let message = p
            .message
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        ProgressView {
            state: state_of(p.state.load(Ordering::Acquire)),
            files: p.files.load(Ordering::Relaxed),
            folders: p.folders.load(Ordering::Relaxed),
            bytes: p.bytes.load(Ordering::Relaxed),
            unreadable: p.unreadable.load(Ordering::Relaxed),
            paused: p.waiting_for_resume.load(Ordering::Relaxed),
            current,
            message,
            root: s.root.to_string_lossy().into_owned(),
            saved_at: s.saved_at,
        }
    }

    /// The finished tree, if there is one.
    pub fn tree(&self) -> Option<Arc<Tree>> {
        let guard = self.lock();
        let s = guard.as_ref()?;
        let t = s
            .result
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        t
    }

    /// Records what the Recycle Bin preview should say about `id`.
    pub fn hint_for(&self, id: u32) -> Option<PathBuf> {
        let tree = self.tree()?;
        let path = tree.path(id)?;
        let (items, size) = tree.totals_of(id)?;
        self.hints
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path.clone(), (items, size));
        Some(path)
    }

    /// Path of a remembered large file, registered for the Recycle Bin preview.
    pub fn hint_for_hit(&self, idx: u32) -> Option<PathBuf> {
        let tree = self.tree()?;
        let f = tree.big_file(idx)?;
        self.hints
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(f.path.clone(), (1, f.size_disk));
        Some(f.path.clone())
    }

    // ---- Save / load (the only thing that ever touches disk)

    pub fn save(&self) -> Result<(), DiskError> {
        let (Some(file), Some(key)) = (&self.saved_file, &self.key) else {
            return Err(DiskError::NoStorage);
        };
        let tree = self.tree().ok_or(DiskError::NoScan)?;
        write_tree(&tree, file, key).map_err(|e| DiskError::Io(e.to_string()))
    }

    pub fn saved_info(&self) -> Option<SavedInfo> {
        let (file, key) = (self.saved_file.as_ref()?, self.key.as_ref()?);
        read_header(file, key)
    }

    pub fn load_saved(&self) -> Result<(), DiskError> {
        let (Some(file), Some(key)) = (&self.saved_file, &self.key) else {
            return Err(DiskError::NoStorage);
        };
        if read_magic(file, key).is_some_and(|m| m != *MAGIC && m != *MAGIC_V3) {
            return Err(DiskError::OldFormat);
        }
        let tree = read_tree(file, key).ok_or(DiskError::NoScan)?;
        self.close();
        let progress = Arc::new(Progress::default());
        progress.state.store(2, Ordering::Release);
        let root = tree.root.clone();
        let saved_at = tree.scanned_at;
        *self.lock() = Some(Session {
            progress,
            result: Arc::new(Mutex::new(Some(Arc::new(tree)))),
            root,
            saved_at: Some(saved_at),
            handle: None,
        });
        Ok(())
    }

    pub fn delete_saved(&self) {
        if let Some(f) = &self.saved_file {
            let _ = std::fs::remove_file(f);
        }
    }
}

const MAGIC: &[u8; 6] = b"LKDU\x04\x00";
/// Still read: the same layout with [`CATS_V3`] file types.
const MAGIC_V3: &[u8; 6] = b"LKDU\x03\x00";

fn write_tree(tree: &Tree, path: &Path, key: &Key) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let file = std::fs::File::create(&tmp)?;
        let mut w = SealWriter::new(std::io::BufWriter::new(file), key)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        // A loaded version 3 scan is saved as one: its types stay what they were.
        let (magic, cats) = if tree.old_types {
            (MAGIC_V3, CATS_V3)
        } else {
            (MAGIC, CATS)
        };
        w.write_all(magic)?;
        let root = tree.root.to_string_lossy();
        w.write_all(&u32::try_from(root.len()).unwrap_or(0).to_le_bytes())?;
        w.write_all(root.as_bytes())?;
        w.write_all(&tree.scanned_at.to_le_bytes())?;
        w.write_all(&tree.cluster.to_le_bytes())?;
        for v in [
            tree.stats.files,
            tree.stats.folders,
            tree.stats.unreadable,
            tree.stats.folded_folders,
            tree.stats.links_skipped,
            tree.stats.cloud_folders,
            tree.stats.cloud_files,
            tree.stats.hard_links,
            tree.stats.hard_link_bytes,
        ] {
            w.write_all(&v.to_le_bytes())?;
        }
        w.write_all(&(tree.nodes.len() as u64).to_le_bytes())?;
        w.write_all(&(tree.names.len() as u64).to_le_bytes())?;
        w.write_all(&(tree.splits.len() as u64).to_le_bytes())?;
        for n in &tree.nodes {
            w.write_all(&n.name_off.to_le_bytes())?;
            w.write_all(&n.name_len.to_le_bytes())?;
            w.write_all(&[n.kind, n.flags])?;
            for v in [n.parent, n.first_child, n.next] {
                w.write_all(&v.to_le_bytes())?;
            }
            w.write_all(&n.size_disk.to_le_bytes())?;
            w.write_all(&n.logical.to_le_bytes())?;
            w.write_all(&n.items.to_le_bytes())?;
            w.write_all(&n.mtime.to_le_bytes())?;
            w.write_all(&n.split.to_le_bytes())?;
        }
        w.write_all(&tree.names)?;
        for v in tree.splits.iter().flat_map(|s| &s[..cats]) {
            w.write_all(&v.to_le_bytes())?;
        }
        w.write_all(&[u8::from(tree.big_overflow)])?;
        w.write_all(&tree.big_floor.to_le_bytes())?;
        w.write_all(&(tree.big.len() as u32).to_le_bytes())?;
        for f in &tree.big {
            let p = f.path.to_string_lossy();
            w.write_all(&(p.len() as u32).to_le_bytes())?;
            w.write_all(p.as_bytes())?;
            w.write_all(&f.size_disk.to_le_bytes())?;
            w.write_all(&f.logical.to_le_bytes())?;
            w.write_all(&f.mtime.to_le_bytes())?;
        }
        let file = w
            .finish()?
            .into_inner()
            .map_err(std::io::IntoInnerError::into_error)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

fn rd<const N: usize>(r: &mut impl Read) -> Option<[u8; N]> {
    let mut b = [0u8; N];
    r.read_exact(&mut b).ok()?;
    Some(b)
}

/// The format marker of a saved scan, or `None` if it is not a saved scan at all.
fn read_magic(path: &Path, key: &Key) -> Option<[u8; 6]> {
    let file = std::fs::File::open(path).ok()?;
    let mut r = OpenReader::new(std::io::BufReader::new(file), key).ok()?;
    let m = rd::<6>(&mut r)?;
    (m[..4] == MAGIC[..4]).then_some(m)
}

fn read_header(path: &Path, key: &Key) -> Option<SavedInfo> {
    let file = std::fs::File::open(path).ok()?;
    let mut r = OpenReader::new(std::io::BufReader::new(file), key).ok()?;
    // Any version: the first fields are the same, so an older scan is still listed.
    if rd::<6>(&mut r)?[..4] != MAGIC[..4] {
        return None;
    }
    let len = u32::from_le_bytes(rd(&mut r)?) as usize;
    if len > 1 << 16 {
        return None;
    }
    let mut root = vec![0u8; len];
    r.read_exact(&mut root).ok()?;
    let saved_at = u64::from_le_bytes(rd(&mut r)?);
    Some(SavedInfo {
        root: String::from_utf8_lossy(&root).into_owned(),
        saved_at,
    })
}

fn read_tree(path: &Path, key: &Key) -> Option<Tree> {
    let file = std::fs::File::open(path).ok()?;
    let mut r = OpenReader::new(std::io::BufReader::new(file), key).ok()?;
    let magic = rd::<6>(&mut r)?;
    if magic != *MAGIC && magic != *MAGIC_V3 {
        return None;
    }
    let old_types = magic == *MAGIC_V3;
    let cats = if old_types { CATS_V3 } else { CATS };
    let len = u32::from_le_bytes(rd(&mut r)?) as usize;
    if len > 1 << 16 {
        return None;
    }
    let mut root = vec![0u8; len];
    r.read_exact(&mut root).ok()?;
    let scanned_at = u64::from_le_bytes(rd(&mut r)?);
    let cluster = u32::from_le_bytes(rd(&mut r)?);
    let mut s = [0u64; 9];
    for v in &mut s {
        *v = u64::from_le_bytes(rd(&mut r)?);
    }
    let count = usize::try_from(u64::from_le_bytes(rd(&mut r)?)).ok()?;
    let names_len = usize::try_from(u64::from_le_bytes(rd(&mut r)?)).ok()?;
    let splits_len = usize::try_from(u64::from_le_bytes(rd(&mut r)?)).ok()?;
    if count == 0 || count > NODE_CAP * 2 + 1_000_000 || names_len > 1 << 31 || splits_len > count {
        return None;
    }
    let mut nodes = Vec::with_capacity(count);
    for _ in 0..count {
        let name_off = u32::from_le_bytes(rd(&mut r)?);
        let name_len = u16::from_le_bytes(rd(&mut r)?);
        let [kind, flags] = rd::<2>(&mut r)?;
        let parent = u32::from_le_bytes(rd(&mut r)?);
        let first_child = u32::from_le_bytes(rd(&mut r)?);
        let next = u32::from_le_bytes(rd(&mut r)?);
        nodes.push(Node {
            name_off,
            name_len,
            kind,
            flags,
            parent,
            first_child,
            next,
            size_disk: u64::from_le_bytes(rd(&mut r)?),
            logical: u64::from_le_bytes(rd(&mut r)?),
            items: u32::from_le_bytes(rd(&mut r)?),
            mtime: u32::from_le_bytes(rd(&mut r)?),
            split: u32::from_le_bytes(rd(&mut r)?),
        });
    }
    let mut names = vec![0u8; names_len];
    r.read_exact(&mut names).ok()?;
    let mut splits = vec![Split::default(); splits_len];
    for v in splits.iter_mut().flat_map(|s| &mut s[..cats]) {
        *v = u64::from_le_bytes(rd(&mut r)?);
    }
    let big_overflow = rd::<1>(&mut r)?[0] != 0;
    let big_floor = u64::from_le_bytes(rd(&mut r)?);
    let big_count = u32::from_le_bytes(rd(&mut r)?) as usize;
    if big_count > BIG_CAP {
        return None;
    }
    let mut big = Vec::with_capacity(big_count);
    for _ in 0..big_count {
        let plen = u32::from_le_bytes(rd(&mut r)?) as usize;
        if plen > 1 << 16 {
            return None;
        }
        let mut pb = vec![0u8; plen];
        r.read_exact(&mut pb).ok()?;
        big.push(BigFile {
            path: PathBuf::from(String::from_utf8_lossy(&pb).into_owned()),
            size_disk: u64::from_le_bytes(rd(&mut r)?),
            logical: u64::from_le_bytes(rd(&mut r)?),
            mtime: u32::from_le_bytes(rd(&mut r)?),
        });
    }
    // Reject a damaged file instead of indexing out of range later.
    let ok = nodes.iter().all(|n| {
        (n.name_off as usize + n.name_len as usize) <= names.len()
            && (n.parent as usize) < count
            && (n.first_child == NONE || (n.first_child as usize) < count)
            && (n.next == NONE || (n.next as usize) < count)
            && (n.split == NONE || (n.split as usize) < splits.len())
    });
    if !ok {
        return None;
    }
    Some(Tree {
        nodes,
        names,
        splits,
        root: PathBuf::from(String::from_utf8_lossy(&root).into_owned()),
        stats: TreeStats {
            files: s[0],
            folders: s[1],
            unreadable: s[2],
            folded_folders: s[3],
            links_skipped: s[4],
            cloud_folders: s[5],
            cloud_files: s[6],
            hard_links: s[7],
            hard_link_bytes: s[8],
            volume_used: 0,
        },
        scanned_at,
        cluster,
        big,
        big_overflow,
        big_floor,
        old_types,
    })
}

// ---------------------------------------------------------------------------
// Places to scan

/// Each local drive plus the user's own folder.
pub fn locations() -> Vec<Location> {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    const FIXED: u32 = 3;
    const REMOVABLE: u32 = 2;
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os("USERPROFILE") {
        out.push(Location {
            label: "Your user folder".to_owned(),
            path: home.to_string_lossy().into_owned(),
        });
    }
    // SAFETY: no arguments; returns a bit mask of drive letters.
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = char::from(b'A' + i as u8);
        let root = format!("{letter}:\\");
        let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: NUL-terminated drive root.
        let kind = unsafe { GetDriveTypeW(wide.as_ptr()) };
        if kind == FIXED || kind == REMOVABLE {
            out.push(Location {
                label: format!("Drive {letter}:"),
                path: root,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    #[test]
    fn long_path_prefix_covers_drive_and_unc_paths() {
        let enc = |t: &str| t.encode_utf16().collect::<Vec<u16>>();
        assert_eq!(long_prefixed(enc(r"C:\a\b")), enc(r"\\?\C:\a\b"));
        assert_eq!(
            long_prefixed(enc(r"\\srv\share\a")),
            enc(r"\\?\UNC\srv\share\a")
        );
        assert_eq!(long_prefixed(enc(r"\\?\C:\a")), enc(r"\\?\C:\a"));
        assert_eq!(long_prefixed(enc(r"\\.\pipe\x")), enc(r"\\.\pipe\x"));
        assert_eq!(long_prefixed(enc(r"rel\x")), enc(r"rel\x"));
    }

    fn write(p: &Path, n: usize) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![b'x'; n]).unwrap();
    }

    fn run(root: &Path, opts: ScanOptions) -> Option<Tree> {
        scan(root, &opts, &Progress::default(), &PauseState::new())
    }

    fn fast() -> ScanOptions {
        ScanOptions {
            threads: 4,
            ..ScanOptions::default()
        }
    }

    fn find(t: &Tree, name: &str) -> u32 {
        (0..t.len() as u32)
            .find(|&i| t.nodes[i as usize].kind != 2 && t.name(&t.nodes[i as usize]) == name)
            .unwrap_or_else(|| panic!("no node {name}"))
    }

    /// Waits for a background scan. The limit only catches a hang: how long a
    /// scan takes depends on the machine, and a throttled one sleeps many times
    /// longer than it works.
    fn wait_until(what: &str, done: impl Fn() -> bool) {
        let limit = Instant::now() + Duration::from_secs(120);
        while !done() {
            assert!(Instant::now() < limit, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Denies listing a folder to everyone; allows it again on drop so the
    /// temporary directory can be deleted.
    struct Locked(PathBuf);
    impl Locked {
        fn new(p: &Path) -> Option<Self> {
            let ok = Command::new("icacls")
                .arg(p)
                .args(["/deny", "*S-1-1-0:(RD)"])
                .output()
                .ok()?
                .status
                .success();
            ok.then(|| Self(p.to_path_buf()))
        }
    }
    impl Drop for Locked {
        fn drop(&mut self) {
            let _ = Command::new("icacls")
                .arg(&self.0)
                .args(["/remove:d", "*S-1-1-0"])
                .output();
        }
    }

    #[test]
    fn totals_counts_and_sizes_add_up_with_junction_symlink_and_unreadable_folder() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(&r.join("a.txt"), 10_000);
        write(&r.join("sub/b.bin"), 5_000);
        write(&r.join("sub/deep/c.bin"), 100);
        write(&r.join("sub/deep/more/d.bin"), 1);
        write(&r.join("empty/.keep"), 0);
        // A junction to a big folder outside the scan: must not be followed.
        let outside = tempfile::tempdir().unwrap();
        write(&outside.path().join("huge.bin"), 1_000_000);
        let junction = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(r.join("junc"))
            .arg(outside.path())
            .output()
            .unwrap();
        assert!(junction.status.success(), "mklink /J failed");
        // A directory symlink needs a privilege; skip that part without it.
        let symlinked = std::os::windows::fs::symlink_dir(outside.path(), r.join("sym")).is_ok();
        // A folder that cannot be listed.
        write(&r.join("locked/secret.bin"), 777);
        let lock = Locked::new(&r.join("locked"));
        eprintln!("unreadable-folder check active: {}", lock.is_some());

        let t = run(r, fast()).expect("scan finishes");
        let cluster = u64::from(t.cluster);
        let on_disk = |n: u64| n.div_ceil(cluster) * cluster;
        let expected = on_disk(10_000) + on_disk(5_000) + on_disk(100) + on_disk(1);
        let (disk, logical, items) = t.total();
        assert_eq!(logical, 10_000 + 5_000 + 100 + 1, "logical size");
        if lock.is_some() {
            assert_eq!(disk, expected, "size on disk, locked folder not counted");
            let locked = find(&t, "locked");
            let row = t
                .children(0, SortBy::Name, false)
                .into_iter()
                .find(|r| r.id == locked)
                .unwrap();
            assert!(row.unreadable, "unreadable folder is flagged, not zero");
            assert_eq!(t.stats.unreadable, 1);
            assert!(
                t.nodes[0].flags & F_PARTIAL != 0,
                "parent knows it is incomplete"
            );
        }
        // files a, b, c, d, .keep (+ secret.bin if the folder could be listed);
        // folders sub, deep, more, empty, locked, junc (+ sym).
        let files = if lock.is_some() { 5 } else { 6 };
        let folders = 6 + u32::from(symlinked);
        assert_eq!(items, files + folders, "item count");
        // The junction (and symlink) is listed as a link and contributes nothing.
        let j = find(&t, "junc");
        assert!(t.nodes[j as usize].flags & F_LINK != 0);
        assert_eq!(t.nodes[j as usize].size_disk, 0);
        assert!(t.nodes[j as usize].first_child == NONE);
        if symlinked {
            assert!(t.nodes[find(&t, "sym") as usize].flags & F_LINK != 0);
        }
        assert!(
            t.nodes[0].logical < 1_000_000,
            "nothing outside was counted"
        );
        // Percent of parent and the path of a nested node.
        let sub = find(&t, "sub");
        assert_eq!(
            t.path(find(&t, "c.bin")).unwrap(),
            r.join("sub").join("deep").join("c.bin")
        );
        let rows = t.children(sub, SortBy::Size, true);
        let sum: f32 = rows.iter().map(|x| x.percent).sum();
        assert!((sum - 100.0).abs() < 0.1, "children add to 100%: {sum}");
        drop(lock);
    }

    #[test]
    fn hard_links_count_the_data_once_and_big_files_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(&r.join("a/data.bin"), 2_000_000);
        fs::create_dir_all(r.join("b")).unwrap();
        fs::hard_link(r.join("a/data.bin"), r.join("b/link1.bin")).unwrap();
        fs::hard_link(r.join("a/data.bin"), r.join("link2.bin")).unwrap();
        write(&r.join("c/other.bin"), 3_000_000);
        write(&r.join("c/small.txt"), 10);
        let t = run(r, fast()).unwrap();
        let cluster = u64::from(t.cluster);
        let on = |n: u64| n.div_ceil(cluster) * cluster;
        assert_eq!(t.total().1, 5_000_010, "logical: the linked data once");
        assert_eq!(t.total().0, on(2_000_000) + on(3_000_000) + on(10));
        assert_eq!(t.stats.hard_links, 2, "two extra paths detected");
        assert_eq!(t.stats.hard_link_bytes, 2 * on(2_000_000));
        // Large files: the linked file once, largest first, none below 1 MB.
        let big = t.big_files(0, None, 100);
        assert_eq!(big.len(), 2);
        assert_eq!(big[0].name, "other.bin");
        assert!(t.big_files(2_500_000, None, 100).len() == 1);
        // A cutoff in the past matches nothing; one in the future matches both.
        assert!(t.big_files(0, Some(1), 100).is_empty());
        assert_eq!(t.big_files(0, Some(u32::MAX), 100).len(), 2);
        assert!(t.big_complete_for(BIG_MIN));
    }

    #[test]
    fn the_chart_adds_up_and_groups_small_items() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("big/f.bin"), 900_000);
        for i in 0..30 {
            write(&dir.path().join(format!("tiny{i}/x.bin")), 100);
        }
        let t = run(dir.path(), fast()).unwrap();
        let c = t.chart(0, 3).unwrap();
        let sum: u64 = c.children.iter().map(|k| k.size).sum();
        assert_eq!(sum, c.size, "children add up to the folder");
        assert!(
            c.children.iter().any(|k| k.kind == "other"),
            "tiny folders grouped"
        );
        assert!(c.children.len() < 10);
        assert_eq!(c.children[0].name, "big");
        let other = c.children.iter().find(|k| k.kind == "other").unwrap();
        assert_eq!(other.name, "Other (30 items)");
        assert_eq!(other.items, 30);
    }

    #[test]
    fn the_chart_carries_dates_counts_and_file_types() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("mix/deep/er/film.MKV"), 900_000);
        write(&dir.path().join("mix/note.txt"), 100_000);
        let t = run(dir.path(), fast()).unwrap();
        // One level: the type still comes from files far below the chart.
        let c = t.chart(0, 1).unwrap();
        let mix = &c.children[0];
        assert_eq!((mix.name.as_str(), mix.cat), ("mix", 2));
        assert_eq!(mix.items, 4, "two folders and two files");
        assert!(mix.modified > 0);
        assert_eq!(c.cat, 2);
        let full = t.chart(mix.id.unwrap(), 4).unwrap();
        let note = full.children.iter().find(|k| k.name == "note.txt").unwrap();
        assert_eq!((note.cat, note.items), (0, 1));
        assert_eq!(file_type(b"setup.exe"), 6);
        assert_eq!(file_type(b".gitignore"), CAT_OTHER);
        assert_eq!(file_type(b"no extension"), CAT_OTHER);
        assert_eq!(file_type(b"x.averylongextension"), CAT_OTHER);
        let wide = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        assert_eq!(file_type(&wide("Film.MKV")), 2);
        // U+016D must not be cut down to the "m" of ".md".
        assert_eq!(file_type(&wide("x.\u{16d}d")), CAT_OTHER);
    }

    /// Every chart segment's types add up to its size.
    fn splits_add_up(c: &ChartNode) {
        assert_eq!(c.split.iter().sum::<u64>(), c.size, "{}", c.name);
        assert_eq!(c.cat, dominant(&c.split), "{}", c.name);
        c.children.iter().for_each(splits_add_up);
    }

    #[test]
    fn folder_types_count_every_file_and_roll_up_to_parents() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        // 60 images (more than the 40 kept as rows) outweigh one larger video.
        for i in 0..60 {
            write(&r.join(format!("top/pics/p{i}.jpg")), 20_000);
        }
        write(&r.join("top/pics/clip.mp4"), 100_000);
        write(&r.join("top/code/main.rs"), 3_000);
        write(&r.join("top/code/blob.zzz"), 2_000);
        write(&r.join("top/doc.pdf"), 5_000);
        let t = run(r, fast()).unwrap();
        let cluster = u64::from(t.cluster);
        let on = |n: u64| n.div_ceil(cluster) * cluster;
        let want: Split = [
            on(5_000),
            60 * on(20_000),
            on(100_000),
            0,
            0,
            on(3_000),
            0,
            on(2_000),
            0,
            0,
            0,
        ];
        assert_eq!(t.split_of(0), want, "rolled up to the root");
        assert_eq!(t.split_of(find(&t, "top")), want);
        let c = t.chart(0, 4).unwrap();
        splits_add_up(&c);
        assert_eq!((c.cat, c.split), (1, want));
        let pics = t.chart(find(&t, "pics"), 1).unwrap();
        splits_add_up(&pics);
        let rest = pics
            .children
            .iter()
            .find(|k| k.kind == "files" || k.kind == "other")
            .expect("the images without a row");
        assert_eq!(rest.cat, 1, "images, not Other");
        // The same types when folders are folded for lack of rows.
        for node_cap in [2, 4] {
            let opts = ScanOptions { node_cap, ..fast() };
            let capped = run(r, opts).unwrap();
            assert_eq!(capped.split_of(0), want, "cap {node_cap}");
            splits_add_up(&capped.chart(0, 4).unwrap());
        }
        // Saved and loaded again.
        let file = tempfile::tempdir().unwrap();
        let file = file.path().join("disk.scan");
        let key = Key::generate().unwrap();
        write_tree(&t, &file, &key).unwrap();
        let back = read_tree(&file, &key).unwrap();
        assert_eq!(back.chart(0, 4).unwrap().split, want);
        let pics = find(&t, "pics");
        assert_eq!(back.split_of(pics), t.split_of(pics));
    }

    #[test]
    fn the_chart_of_empty_single_tiny_and_very_deep_folders() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        fs::create_dir_all(r.join("empty")).unwrap();
        write(&r.join("single/only.mp3"), 5_000);
        fs::create_dir_all(r.join("zero")).unwrap();
        for i in 0..300 {
            write(&r.join(format!("tiny/t{i}.txt")), 10);
            fs::write(r.join(format!("zero/z{i}")), b"").unwrap();
        }
        let mut deep = r.join("deep");
        for i in 0..60 {
            deep.push(format!("level-{i}"));
        }
        write(&deep.join("bottom.png"), 5_000);
        assert!(deep.as_os_str().len() > 500, "well past MAX_PATH");
        let t = run(r, fast()).unwrap();
        for name in ["empty", "zero"] {
            let c = t.chart(find(&t, name), 4).unwrap();
            assert_eq!((c.size, c.cat), (0, CAT_OTHER), "{name}");
            assert!(c.children.is_empty(), "{name}");
        }
        let single = t.chart(find(&t, "single"), 4).unwrap();
        assert_eq!((single.children.len(), single.cat), (1, 3));
        let tiny = t.chart(find(&t, "tiny"), 4).unwrap();
        let sum: u64 = tiny.children.iter().map(|k| k.size).sum();
        assert!(sum > 0);
        assert_eq!((sum, tiny.cat), (tiny.size, 0));
        let top = t.chart(find(&t, "deep"), 4).unwrap();
        assert_eq!(top.cat, 1, "the type comes from 60 levels down");
        let bottom = find(&t, "bottom.png");
        assert_eq!(t.path(bottom).unwrap(), deep.join("bottom.png"));
        assert_eq!(t.breadcrumb(bottom).len(), 63);
        splits_add_up(&t.chart(0, 5).unwrap());
        splits_add_up(&t.chart(bottom, 4).unwrap());
    }

    #[test]
    fn a_wide_and_deep_chart_stays_within_its_cap() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..40 {
            for j in 0..45 {
                write(&dir.path().join(format!("d{i}/e{j}/f.bin")), 5_000);
            }
        }
        let t = run(dir.path(), fast()).unwrap();
        let c = t.chart(0, 4).unwrap();
        let mut seen = 0;
        let mut open = vec![&c];
        while let Some(n) = open.pop() {
            seen += 1;
            if !n.children.is_empty() {
                let sum: u64 = n.children.iter().map(|k| k.size).sum();
                assert_eq!(sum, n.size, "{} adds up", n.name);
            }
            open.extend(&n.children);
        }
        assert!(seen > 40, "the first level is always there");
        assert!(seen <= CHART_CAP + 1, "{seen}");
        let sizes: Vec<u64> = c.children.iter().map(|k| k.size).collect();
        assert!(sizes.windows(2).all(|w| w[0] >= w[1]), "largest first");
    }

    #[test]
    fn extensions_map_to_their_type_whatever_the_case() {
        let cases = [
            (
                CAT_SYSTEM,
                "dll sys drv ocx mui mun cat etl evtx dmp pdb lib a o rlib rmeta so pyd wasm",
            ),
            (CAT_IMAGES, "iso img vhd vhdx vmdk vdi qcow2 ova wim esd"),
            (
                CAT_DATA,
                "pak bin dat db sqlite sqlite3 mdf ldb sst parquet",
            ),
            (
                CAT_DATA,
                "jsonl gguf onnx tflite safetensors pt pth ckpt heapsnapshot",
            ),
            (4, "cab nupkg crate whl"),
        ];
        for (cat, exts) in cases {
            for ext in exts.split_whitespace() {
                let name = format!("x.{ext}");
                assert_eq!(file_type(name.as_bytes()), cat, "{name}");
                let upper = name.to_uppercase();
                assert_eq!(file_type(upper.as_bytes()), cat, "{upper}");
                let wide: Vec<u16> = upper.encode_utf16().collect();
                assert_eq!(file_type(&wide), cat, "{upper} as UTF-16");
            }
        }
        // No extension, even when the whole name is one.
        for name in ["README", "dll", "iso", "ends.", ""] {
            assert_eq!(file_type(name.as_bytes()), CAT_OTHER, "{name}");
            assert_eq!(file_type_v3(name.as_bytes()), CAT_OTHER, "{name}");
        }
        // Only the last extension counts.
        assert_eq!(file_type(b"src.tar.gz"), 4);
        assert_eq!(file_type(b"setup.dll.txt"), 0);
        assert_eq!(file_type(b"data.db.zip"), 4);
        assert_eq!(file_type(b"disk.tar.iso"), CAT_IMAGES);
        // Ambiguous extensions stay in "other".
        for name in ["a.cache", "a.bundle", "a.obj", "a.pack", "a.map", "a.tmp"] {
            assert_eq!(file_type(name.as_bytes()), CAT_OTHER, "{name}");
        }
        // The mapping old saved scans keep.
        for (name, cat) in [
            ("A.DLL", 6),
            ("a.iso", 4),
            ("a.pak", CAT_OTHER),
            ("a.vdi", CAT_OTHER),
            ("a.whl", CAT_OTHER),
            ("a.tar.gz", 4),
            ("a.exe", 6),
        ] {
            assert_eq!(file_type_v3(name.as_bytes()), cat, "{name}");
        }
    }

    #[test]
    fn a_version_3_saved_scan_loads_and_shows_its_old_types() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a/lib.dll"), 5_000);
        write(&dir.path().join("a/notes.txt"), 100);
        let mut tree = run(dir.path(), fast()).unwrap();
        assert_eq!(tree.chart(0, 3).unwrap().cat, CAT_SYSTEM);
        // What version 3 held: eight types, with the DLL among the apps.
        let dll = tree.split_of(0)[usize::from(CAT_SYSTEM)];
        for s in &mut tree.splits {
            s[6] = std::mem::take(&mut s[usize::from(CAT_SYSTEM)]);
        }
        tree.old_types = true;
        let file = tempfile::tempdir().unwrap().keep().join("disk.scan");
        let key = Key::generate().unwrap();
        write_tree(&tree, &file, &key).unwrap();
        assert_eq!(read_magic(&file, &key), Some(*MAGIC_V3));
        let svc = DiskService::new(
            PauseState::new(),
            1.0,
            Some(file),
            Some(key),
            Hints::default(),
        );
        svc.load_saved().unwrap();
        let back = svc.tree().unwrap();
        let c = back.chart(0, 3).unwrap();
        assert_eq!((c.cat, c.split[6]), (6, dll));
        splits_add_up(&c);
        assert_eq!(c.split[CATS_V3..], [0; CATS - CATS_V3]);
        assert_eq!(back.split_of(find(&back, "lib.dll"))[6], dll);
    }

    #[test]
    fn a_saved_scan_from_an_older_format_gives_a_clear_error() {
        let key = Key::generate().unwrap();
        let file = tempfile::tempdir().unwrap().keep().join("disk.scan");
        {
            let f = std::fs::File::create(&file).unwrap();
            let mut w = SealWriter::new(std::io::BufWriter::new(f), &key).unwrap();
            w.write_all(b"LKDU\x01\x00").unwrap();
            w.write_all(&3u32.to_le_bytes()).unwrap();
            w.write_all(b"C:\\").unwrap();
            w.write_all(&123u64.to_le_bytes()).unwrap();
            w.finish().unwrap();
        }
        let svc = DiskService::new(
            PauseState::new(),
            1.0,
            Some(file),
            Some(key),
            Hints::default(),
        );
        assert_eq!(svc.saved_info().unwrap().saved_at, 123, "still listed");
        let err = svc.load_saved().unwrap_err();
        assert_eq!(
            err.to_string(),
            "Saved scan is from an older version, please rescan"
        );
        assert_eq!(svc.progress().state, ScanState::Idle, "nothing half-loaded");
    }

    #[test]
    fn a_big_flat_folder_stays_small_and_totals_stay_exact() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("many");
        fs::create_dir_all(&big).unwrap();
        let n = 12_000usize;
        for i in 0..n {
            fs::write(big.join(format!("f{i}.dat")), vec![0u8; (i % 7) * 100]).unwrap();
        }
        let t = run(dir.path(), fast()).unwrap();
        let (_, logical, items) = t.total();
        let want: u64 = (0..n).map(|i| ((i % 7) * 100) as u64).sum();
        assert_eq!(logical, want);
        assert_eq!(
            items as usize,
            n + 1,
            "every file and the folder are counted"
        );
        // 1 root + 1 folder + 40 kept files + 1 "other files" row.
        assert_eq!(t.len(), 1 + 1 + FILES_PER_FOLDER + 1);
        assert!(t.heap_bytes() < 64 * 1024);
        let rows = t.children(find(&t, "many"), SortBy::Size, true);
        assert_eq!(
            rows.last().unwrap().kind,
            NodeKind::Others,
            "other files row is last"
        );
        assert_eq!(rows.last().unwrap().items as usize, n - FILES_PER_FOLDER);
    }

    #[test]
    fn above_the_node_cap_detail_is_folded_but_totals_are_right() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..30 {
            write(&dir.path().join(format!("d{i}/x/y.bin")), 1000 + i);
        }
        let capped = run(
            dir.path(),
            ScanOptions {
                node_cap: 10,
                ..fast()
            },
        )
        .unwrap();
        let full = run(dir.path(), fast()).unwrap();
        assert_eq!(capped.total().1, full.total().1, "logical totals match");
        assert_eq!(capped.total().0, full.total().0, "size on disk matches");
        assert_eq!(capped.total().2, full.total().2, "item counts match");
        assert!(capped.stats.folded_folders > 0);
        assert!(capped.nodes[0].flags & F_LIMITED != 0);
        assert!(capped.len() < full.len());
    }

    #[test]
    fn cancel_stops_a_running_scan_and_leaves_no_tree() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..200 {
            write(&dir.path().join(format!("d{i}/f.bin")), 10);
        }
        let svc = DiskService::new(PauseState::new(), 0.05, None, None, Hints::default());
        svc.start(dir.path(), true).unwrap();
        svc.cancel();
        // The scan thread notices on its next folder.
        wait_until("the cancel", || svc.progress().state != ScanState::Scanning);
        assert_eq!(svc.progress().state, ScanState::Cancelled);
        assert!(svc.tree().is_none());
        svc.close();
    }

    #[test]
    fn a_junction_that_points_back_up_is_listed_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(&r.join("a/f.bin"), 1_000);
        let made = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(r.join("a").join("loop"))
            .arg(r)
            .output()
            .unwrap();
        assert!(made.status.success(), "mklink /J failed: {made:?}");
        let t = run(r, fast()).expect("scan finishes");
        assert_eq!(t.total().1, 1_000, "counted once");
        let l = find(&t, "loop");
        assert!(t.nodes[l as usize].flags & F_LINK != 0);
        assert!(t.nodes[l as usize].first_child == NONE);
    }

    #[test]
    fn cancel_in_the_middle_of_a_scan_leaves_no_tree_and_a_new_scan_works() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..400 {
            write(&dir.path().join(format!("d{i}/f.bin")), 10);
        }
        let svc = DiskService::new(PauseState::new(), 0.05, None, None, Hints::default());
        svc.start(dir.path(), true).unwrap();
        wait_until("the scan to get going", || {
            svc.progress().folders >= 5 || svc.progress().state != ScanState::Scanning
        });
        let seen = svc.progress().folders;
        svc.cancel();
        wait_until("the cancel", || svc.progress().state != ScanState::Scanning);
        assert!(seen >= 5, "never got going");
        assert_eq!(svc.progress().state, ScanState::Cancelled);
        assert!(svc.tree().is_none());
        svc.start(dir.path(), false).unwrap();
        wait_until("the new scan", || {
            svc.progress().state != ScanState::Scanning
        });
        assert_eq!(svc.progress().state, ScanState::Done);
        assert_eq!(svc.tree().unwrap().total().2, 800);
        svc.close();
    }

    #[test]
    fn closing_frees_the_scan_data_and_pause_blocks_a_new_scan() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a/b.bin"), 100);
        let pause = PauseState::new();
        let svc = DiskService::new(pause.clone(), 1.0, None, None, Hints::default());
        svc.start(dir.path(), false).unwrap();
        wait_until("the scan", || svc.progress().state != ScanState::Scanning);
        let tree = svc.tree().expect("finished");
        let weak = Arc::downgrade(&tree);
        drop(tree);
        assert!(weak.upgrade().is_some(), "held while the view is open");
        svc.close();
        assert!(weak.upgrade().is_none(), "freed when the view closes");
        assert_eq!(svc.progress().state, ScanState::Idle);
        pause.set(true);
        assert!(matches!(
            svc.start(dir.path(), false),
            Err(DiskError::Paused)
        ));
    }

    #[test]
    fn save_and_load_round_trip_encrypted_and_wrong_key_fails() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a/b.bin"), 5000);
        let tree = run(dir.path(), fast()).unwrap();
        let file = tempfile::tempdir().unwrap().keep().join("disk.scan");
        let key = Key::generate().unwrap();
        write_tree(&tree, &file, &key).unwrap();
        let raw = fs::read(&file).unwrap();
        assert!(
            !raw.windows(5).any(|w| w == b"b.bin"),
            "names are not stored in the clear"
        );
        let back = read_tree(&file, &key).unwrap();
        assert_eq!(back.total(), tree.total());
        assert_eq!(back.len(), tree.len());
        assert_eq!(
            read_header(&file, &key).unwrap().root,
            dir.path().to_string_lossy()
        );
        assert!(read_tree(&file, &Key::generate().unwrap()).is_none());
    }
}
