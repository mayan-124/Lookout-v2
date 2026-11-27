//! Batch rename: plan, apply (two phases, with rollback), reverse.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const MAX_FILES: usize = 500;

#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    /// (old path, new path) for the files that actually change.
    pub pairs: Vec<(PathBuf, PathBuf)>,
    /// Files left alone because the new name equals the old one.
    pub unchanged: usize,
}

const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

fn valid_name(name: &str) -> Result<(), String> {
    let stem = name.split('.').next().unwrap_or("").to_lowercase();
    if name.is_empty()
        || name
            .chars()
            .any(|c| c.is_control() || "\\/:*?\"<>|".contains(c))
        || name.ends_with('.')
        || name.ends_with(' ')
        || RESERVED.contains(&stem.as_str())
        || name.len() > 200
    {
        return Err(format!("'{name}' is not a valid Windows file name"));
    }
    Ok(())
}

/// The regular files directly inside `folder`, sorted by name.
fn files_in(folder: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(folder).map_err(|e| format!("cannot read the folder: {e}"))? {
        let Ok(entry) = entry else { continue };
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if meta.is_file() && name != "desktop.ini" && name != "thumbs.db" {
            out.push(entry.path());
        }
    }
    if out.len() > MAX_FILES {
        return Err(format!(
            "the folder has more than {MAX_FILES} files; not renaming that many at once"
        ));
    }
    out.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    Ok(out)
}

/// A `{` or `}` that is not part of `{n}` or `{name}` (for example `{1}`).
fn has_stray_braces(pattern: &str) -> bool {
    pattern
        .replace("{name}", "")
        .replace("{n}", "")
        .contains(['{', '}'])
}

/// The message for a pattern the user has to fix, with an example built from
/// the folder and what they typed.
fn usage(folder: &Path, pattern: &str) -> String {
    // What the user typed with every `{...}` (closed or not) taken out.
    let mut base = String::new();
    let mut depth = false;
    for c in pattern.replace("{name}", "").chars() {
        match c {
            '{' => depth = true,
            '}' => depth = false,
            _ if !depth => base.push(c),
            _ => {}
        }
    }
    let base = if base.trim().is_empty() {
        "name "
    } else {
        &base
    };
    let folder = folder
        .file_name()
        .map_or_else(|| "folder".into(), |n| n.to_string_lossy().into_owned());
    format!("Use {{n}} for the number, for example: rename {folder} to {base}{{n}}")
}

/// `{n}` is the position (1, 2, ... zero padded to the widest), `{name}` the
/// old name without its extension. The extension is always kept.
pub fn plan(folder: &Path, pattern: &str) -> Result<Plan, String> {
    if pattern.ends_with('.') || pattern.ends_with(' ') {
        return Err("a name cannot end with a dot or a space".into());
    }
    let files = files_in(folder)?;
    if files.is_empty() {
        return Err("there are no files directly in that folder".into());
    }
    let has_token = pattern.contains("{n}") || pattern.contains("{name}");
    if has_stray_braces(pattern) || (files.len() > 1 && !has_token) {
        return Err(usage(folder, pattern));
    }
    let width = files.len().to_string().len().max(2);
    let mut pairs = Vec::new();
    let mut unchanged = 0;
    let mut taken: HashSet<String> = HashSet::new();
    for (i, old) in files.iter().enumerate() {
        let stem = old
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ext = old
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let new_name = format!(
            "{}{ext}",
            pattern
                .replace("{n}", &format!("{:0width$}", i + 1))
                .replace("{name}", &stem)
        );
        valid_name(&new_name)?;
        if !taken.insert(new_name.to_lowercase()) {
            return Err(format!("two files would both be called '{new_name}'"));
        }
        let old_name = old
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if old_name == new_name {
            unchanged += 1;
        } else {
            pairs.push((old.clone(), folder.join(new_name)));
        }
    }
    // A new name may only match an existing file that is itself being renamed.
    let moving: HashSet<String> = pairs
        .iter()
        .filter_map(|(o, _)| o.file_name().map(|n| n.to_string_lossy().to_lowercase()))
        .collect();
    for (_, new) in &pairs {
        let name = new
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if new.exists() && !moving.contains(&name) {
            return Err(format!("'{}' already exists in that folder", name));
        }
    }
    if pairs.is_empty() {
        return Err("nothing to rename: the files already have these names".into());
    }
    Ok(Plan { pairs, unchanged })
}

/// Renames every pair, through temporary names so that swaps and chains work;
/// on any failure everything done so far is put back.
pub fn apply(pairs: &[(PathBuf, PathBuf)]) -> Result<(), String> {
    let sources: HashSet<String> = pairs.iter().map(|(o, _)| key(o)).collect();
    for (old, new) in pairs {
        if !old.is_file() {
            return Err(format!("'{}' is gone", old.display()));
        }
        if new.exists() && !sources.contains(&key(new)) {
            return Err(format!("'{}' already exists", new.display()));
        }
    }
    let tag = std::process::id();
    let temps: Vec<PathBuf> = pairs
        .iter()
        .enumerate()
        .map(|(i, (old, _))| old.with_file_name(format!(".lookout-rename-{tag}-{i}.tmp")))
        .collect();
    let mut staged = 0;
    let mut finished = 0;
    let result = (|| {
        for (i, (old, _)) in pairs.iter().enumerate() {
            std::fs::rename(old, &temps[i]).map_err(|e| format!("{}: {e}", old.display()))?;
            staged = i + 1;
        }
        for (i, (_, new)) in pairs.iter().enumerate() {
            std::fs::rename(&temps[i], new).map_err(|e| format!("{}: {e}", new.display()))?;
            finished = i + 1;
        }
        Ok(())
    })();
    if result.is_err() {
        for i in (0..finished).rev() {
            let _ = std::fs::rename(&pairs[i].1, &temps[i]);
        }
        for (i, tmp) in temps.iter().enumerate().take(staged) {
            let _ = std::fs::rename(tmp, &pairs[i].0);
        }
    }
    result
}

fn key(p: &Path) -> String {
    p.to_string_lossy().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn folder(names: &[&str]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for n in names {
            fs::write(d.path().join(n), n).unwrap();
        }
        d
    }

    fn names(d: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(d)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn numbers_keep_extensions_and_sort_by_name() {
        let d = folder(&["b.JPG", "a.jpg", "c.png"]);
        let p = plan(d.path(), "Trip {n}").unwrap();
        let shown: Vec<_> = p
            .pairs
            .iter()
            .map(|(o, n)| {
                (
                    o.file_name().unwrap().to_string_lossy().into_owned(),
                    n.file_name().unwrap().to_string_lossy().into_owned(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("a.jpg".into(), "Trip 01.jpg".into()),
                ("b.JPG".into(), "Trip 02.JPG".into()),
                ("c.png".into(), "Trip 03.png".into())
            ]
        );
        apply(&p.pairs).unwrap();
        assert_eq!(
            names(d.path()),
            ["Trip 01.jpg", "Trip 02.JPG", "Trip 03.png"]
        );
    }

    #[test]
    fn bad_patterns_are_refused_before_anything_changes() {
        let d = folder(&["a.txt", "b.txt"]);
        assert!(plan(d.path(), "same").is_err());
        assert!(plan(d.path(), "x/{n}").is_err());
        assert!(plan(folder(&["x.txt"]).path(), "CON").is_err());
        assert!(plan(d.path(), "{n}.").is_err());
        assert_eq!(names(d.path()), ["a.txt", "b.txt"]);
        // Collides with a file that is not being renamed.
        let d = folder(&["a.txt", "b.txt", "01.txt"]);
        assert!(plan(d.path(), "{n}").is_ok(), "01.txt is itself renamed");
        let d = folder(&["a.txt", "Trip 02.txt"]);
        let p = plan(d.path(), "Trip {n}").unwrap();
        assert_eq!((p.pairs.len(), p.unchanged), (1, 1));
    }

    #[test]
    fn a_pattern_without_a_usable_number_gets_a_plain_message_with_an_example() {
        let d = folder(&["a.txt", "b.txt"]);
        let name = d.path().file_name().unwrap().to_string_lossy().into_owned();
        let want = format!("Use {{n}} for the number, for example: rename {name} to holiday{{n}}");
        for pattern in ["holiday{1}", "holiday", "holiday{n"] {
            assert_eq!(plan(d.path(), pattern).unwrap_err(), want, "{pattern}");
        }
        assert!(plan(d.path(), "holiday{n}").is_ok());
        assert!(plan(d.path(), "{name} copy").is_ok());
        // One file needs no number.
        assert!(plan(folder(&["a.txt"]).path(), "holiday").is_ok());
    }

    #[test]
    fn chains_and_swaps_work_and_failure_rolls_back() {
        let d = folder(&["1.txt", "2.txt"]);
        let a = d.path().join("1.txt");
        let b = d.path().join("2.txt");
        // Swap.
        apply(&[(a.clone(), b.clone()), (b.clone(), a.clone())]).unwrap();
        assert_eq!(fs::read_to_string(&a).unwrap(), "2.txt");
        // A target that exists and is not part of the set: nothing moves.
        let other = d.path().join("keep.txt");
        fs::write(&other, "k").unwrap();
        assert!(apply(&[(a.clone(), other.clone())]).is_err());
        assert!(a.exists() && other.exists());
        // A source that vanished halfway: the rest is put back.
        let gone = d.path().join("nope.txt");
        assert!(apply(&[
            (a.clone(), d.path().join("z.txt")),
            (gone, d.path().join("y.txt"))
        ])
        .is_err());
        assert!(a.exists());
        assert_eq!(names(d.path()), ["1.txt", "2.txt", "keep.txt"]);
    }

    #[test]
    fn nothing_to_do_and_folders_are_left_alone() {
        let d = folder(&["a.txt"]);
        fs::create_dir(d.path().join("sub")).unwrap();
        assert!(plan(d.path(), "a").is_err(), "already named a.txt");
        let p = plan(d.path(), "b").unwrap();
        assert_eq!(p.pairs.len(), 1);
        assert!(plan(&d.path().join("sub"), "x").is_err(), "empty folder");
    }

    #[test]
    fn a_new_name_taken_by_something_that_is_not_renamed_is_refused() {
        // A sub-folder is never renamed, so its name is a real clash.
        let d = folder(&["a.jpg", "b.jpg"]);
        fs::create_dir(d.path().join("Trip 01.jpg")).unwrap();
        let err = plan(d.path(), "Trip {n}").unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        // A file created after the preview: apply refuses and moves nothing.
        let d = folder(&["a.txt"]);
        let p = plan(d.path(), "b").unwrap();
        fs::write(d.path().join("b.txt"), "someone else's").unwrap();
        assert!(apply(&p.pairs).is_err());
        assert_eq!(names(d.path()), ["a.txt", "b.txt"]);
        assert_eq!(
            fs::read_to_string(d.path().join("b.txt")).unwrap(),
            "someone else's"
        );
    }

    #[test]
    fn case_only_renames_work_and_undo_restores_every_name_exactly() {
        let original = [
            "IMG_0001.JPG",
            "My File.Png",
            "no_ext",
            "UPPER.TXT",
            "z last.md",
        ];
        let d = folder(&original);
        let p = plan(d.path(), "pic {n}").unwrap();
        apply(&p.pairs).unwrap();
        assert!(names(d.path()).iter().all(|n| n.starts_with("pic ")));
        // Undo: the reverse pairs, as the plugin stores them.
        let back: Vec<_> = p
            .pairs
            .iter()
            .map(|(o, n)| (n.clone(), o.clone()))
            .collect();
        apply(&back).unwrap();
        let mut want: Vec<String> = original.iter().map(|s| (*s).to_owned()).collect();
        want.sort();
        assert_eq!(names(d.path()), want, "names or their case changed");
        for n in original {
            assert_eq!(
                fs::read_to_string(d.path().join(n)).unwrap(),
                n,
                "contents moved"
            );
        }
        // A pure case change of one file.
        let d = folder(&["readme.txt"]);
        let p = plan(d.path(), "README").unwrap();
        apply(&p.pairs).unwrap();
        assert_eq!(names(d.path()), ["README.txt"]);
        let back: Vec<_> = p
            .pairs
            .iter()
            .map(|(o, n)| (n.clone(), o.clone()))
            .collect();
        apply(&back).unwrap();
        assert_eq!(names(d.path()), ["readme.txt"]);
    }
}
