//! Deterministic synthetic file names for benchmarks and tests. Nothing here
//! touches the file system: names exist only inside a [`NameIndex`].

use std::path::Path;

use crate::name_index::{IndexError, NameIndex};

const WORDS: &[&str] = &[
    "report",
    "invoice",
    "budget",
    "project",
    "meeting",
    "notes",
    "summary",
    "draft",
    "final",
    "photo",
    "holiday",
    "family",
    "design",
    "mockup",
    "logo",
    "banner",
    "icon",
    "screenshot",
    "backup",
    "archive",
    "data",
    "export",
    "import",
    "config",
    "settings",
    "readme",
    "license",
    "todo",
    "plan",
    "roadmap",
    "schedule",
    "calendar",
    "contract",
    "proposal",
    "resume",
    "letter",
    "receipt",
    "statement",
    "tax",
    "insurance",
    "travel",
    "ticket",
    "booking",
    "recipe",
    "workout",
    "music",
    "podcast",
    "video",
    "lecture",
    "assignment",
    "thesis",
    "chapter",
    "paper",
    "survey",
    "analysis",
    "dataset",
    "model",
    "training",
    "results",
    "metrics",
    "dashboard",
    "template",
    "slides",
    "keynote",
    "manual",
    "guide",
    "tutorial",
    "course",
    "exam",
    "homework",
    "grades",
    "client",
    "vendor",
    "order",
    "shipping",
    "payroll",
    "expense",
    "forecast",
    "quarterly",
    "annual",
    "monthly",
    "weekly",
    "daily",
    "review",
    "feedback",
    "release",
    "changelog",
    "build",
    "debug",
    "profile",
    "sample",
    "test",
    "demo",
    "prototype",
    "wireframe",
    "sketch",
    "render",
    "scan",
    "copy",
    "old",
    "new",
    "temp",
    "misc",
    "shared",
    "private",
    "public",
    "team",
    "alpha",
    "beta",
    "gamma",
    "delta",
    "north",
    "south",
    "east",
    "west",
    "spring",
    "summer",
    "autumn",
    "winter",
    "monday",
    "friday",
    "london",
    "paris",
    "tokyo",
    "berlin",
    "sydney",
    "mumbai",
];

const EXTENSIONS: &[(&str, u32)] = &[
    ("pdf", 14),
    ("docx", 10),
    ("xlsx", 8),
    ("txt", 10),
    ("jpg", 16),
    ("png", 10),
    ("mp4", 3),
    ("zip", 4),
    ("md", 4),
    ("rs", 3),
    ("js", 4),
    ("json", 4),
    ("csv", 4),
    ("pptx", 3),
    ("exe", 1),
    ("dll", 2),
    ("log", 2),
];

/// SplitMix64: small, fast, and good enough for spreading names around.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn word(&mut self) -> &'static str {
        WORDS[self.below(WORDS.len())]
    }

    fn extension(&mut self) -> &'static str {
        let total: u32 = EXTENSIONS.iter().map(|e| e.1).sum();
        let mut pick = (self.next() % u64::from(total)) as u32;
        for (ext, weight) in EXTENSIONS {
            if pick < *weight {
                return ext;
            }
            pick -= weight;
        }
        "txt"
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

fn file_name(rng: &mut Rng) -> String {
    let ext = rng.extension();
    let pretty = rng.below(100) < 40;
    let w = |rng: &mut Rng| {
        let w = rng.word();
        if pretty {
            capitalize(w)
        } else {
            w.to_owned()
        }
    };
    match rng.below(6) {
        0 => format!("{}_{}_{}.{ext}", w(rng), w(rng), rng.below(1000)),
        1 => format!("{} {} {}.{ext}", w(rng), w(rng), 2015 + rng.below(11)),
        2 => format!(
            "IMG_{:04}.{}",
            rng.below(10_000),
            if pretty { "JPG" } else { "jpg" }
        ),
        3 => format!("{}-{}.{ext}", w(rng), w(rng)),
        4 => format!("{} {} v{}.{ext}", w(rng), w(rng), 1 + rng.below(9)),
        _ => format!("{} (copy {}).{ext}", w(rng), 1 + rng.below(5)),
    }
}

/// Fills `index` with `files` synthetic files spread over a random directory
/// tree (about one directory per 40 files, at most 6 levels deep) under a
/// fictitious root. Output depends only on `files` and `seed`.
pub fn populate(
    index: &mut NameIndex,
    files: usize,
    seed: u64,
    now: u32,
) -> Result<(), IndexError> {
    let mut rng = Rng(seed);
    let root = index.add_root(Path::new(r"C:\synthetic"), now)?;
    let dir_count = (files / 40).max(1);
    // (id, depth) of every directory created so far.
    let mut dirs: Vec<(u32, u8)> = vec![(root, 0)];
    for i in 0..dir_count {
        let (parent, depth) = dirs[rng.below(dirs.len())];
        if depth >= 6 {
            continue;
        }
        let name = format!("{} {}", capitalize(rng.word()), i);
        let id = index.add_dir(parent, &name, now - rng.below(86_400 * 700) as u32)?;
        dirs.push((id, depth + 1));
    }
    let three_years = 3 * 365 * 86_400;
    for _ in 0..files {
        let (dir, _) = dirs[rng.below(dirs.len())];
        let name = file_name(&mut rng);
        index.add_file(
            dir,
            &name,
            now.saturating_sub(rng.below(three_years) as u32),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::name_index::SearchParams;
    use crate::ranking::RankingWeights;

    const NOW: u32 = 1_700_000_000;

    fn dump(index: &NameIndex) -> Vec<String> {
        let tokens = vec![b".".to_vec()];
        let p = SearchParams {
            tokens: &tokens,
            kind: Some(&crate::item::Kind::File),
            limit: 100_000,
            now: NOW,
            weights: RankingWeights::default(),
            time: None,
            exts: &[],
            filter: None,
        };
        let mut names: Vec<String> = index
            .search(&p, &|| false)
            .iter()
            .filter_map(|h| index.entry_info(h.entry))
            .map(|i| i.name)
            .collect();
        names.sort();
        names
    }

    #[test]
    fn same_seed_gives_same_names_and_different_seed_does_not() {
        let build = |seed| {
            let mut idx = NameIndex::new();
            populate(&mut idx, 2000, seed, NOW).unwrap();
            idx
        };
        assert_eq!(dump(&build(1)), dump(&build(1)));
        assert_ne!(dump(&build(1)), dump(&build(2)));
    }

    #[test]
    fn creates_the_requested_number_of_files_in_a_tree() {
        let mut idx = NameIndex::new();
        populate(&mut idx, 4000, 7, NOW).unwrap();
        let c = idx.counts();
        assert_eq!(c.files, 4000);
        assert!(c.folders > 20 && c.folders <= 101, "{}", c.folders);
    }

    #[test]
    fn names_look_realistic_and_common_words_recur() {
        let mut idx = NameIndex::new();
        populate(&mut idx, 20_000, 3, NOW).unwrap();
        let names = dump(&idx);
        assert!(names.iter().all(|n| n.contains('.')));
        let report = names
            .iter()
            .filter(|n| n.to_lowercase().contains("report"))
            .count();
        assert!(report > 100 && report < 2000, "{report}");
        assert!(names.iter().any(|n| n.starts_with("IMG_")));
        assert!(names
            .iter()
            .any(|n| n.chars().next().is_some_and(char::is_uppercase)));
    }
}
