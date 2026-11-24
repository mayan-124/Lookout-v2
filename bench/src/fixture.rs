//! A generated corpus for the content/OCR memory and CPU measurement: text
//! files with known words and screenshot-like images with known text. Written
//! to a temporary folder; nothing of the user's is read.

use std::fs;
use std::path::{Path, PathBuf};

use lookout_core::fixture_image;
use lookout_core::settings::Settings;

const WORDS: &[&str] = &[
    "budget",
    "invoice",
    "meeting",
    "project",
    "quarterly",
    "report",
    "schedule",
    "travel",
    "contract",
    "notes",
    "design",
    "review",
    "customer",
    "revenue",
    "plan",
    "draft",
    "summary",
    "agenda",
    "policy",
    "release",
    "backlog",
    "sprint",
    "ticket",
    "deploy",
    "archive",
];

pub struct Corpus {
    pub root: PathBuf,
    pub data_dir: PathBuf,
    base: PathBuf,
}

impl Corpus {
    pub fn remove(&self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// A name-only tree of `files` empty files with settings that index names
/// only (no contents, no OCR), for the startup measurement.
pub fn create_names_only(files: usize) -> Result<Corpus, String> {
    let base = std::env::temp_dir().join(format!("lookout-bench-startup-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let root = base.join("tree");
    let data_dir = base.join("data");
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    crate::crawl::make_tree(&root, files)?;
    let settings = Settings {
        index_roots: vec![root.clone()],
        first_run_done: true,
        index_contents: false,
        ocr: false,
        ..Settings::default()
    };
    settings
        .save(&data_dir.join("settings.json"))
        .map_err(|e| e.to_string())?;
    Ok(Corpus {
        root,
        data_dir,
        base,
    })
}

/// `text` files of about 4 KB each and `images` screenshots.
pub fn create(text: usize, images: usize) -> Result<Corpus, String> {
    let base = std::env::temp_dir().join(format!("lookout-bench-corpus-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let root = base.join("corpus");
    let data_dir = base.join("data");
    let io = |e: std::io::Error| e.to_string();
    for i in 0..text {
        let dir = root.join(format!("folder{:02}", i % 20));
        fs::create_dir_all(&dir).map_err(io)?;
        let mut seed = (i as u64 + 1) * 2_654_435_761;
        let mut body = String::new();
        while body.len() < 4096 {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            body.push_str(WORDS[(seed >> 33) as usize % WORDS.len()]);
            body.push(' ');
        }
        body.push_str(&format!("marker{i}"));
        fs::write(dir.join(format!("note{i:05}.txt")), body).map_err(io)?;
    }
    let shots = root.join("screenshots");
    fs::create_dir_all(&shots).map_err(io)?;
    for i in 0..images {
        let word = WORDS[i % WORDS.len()].to_uppercase();
        fs::write(
            shots.join(format!("shot{i:04}.bmp")),
            fixture_image::bmp(&format!("{word} {}", 1000 + i), 6),
        )
        .map_err(io)?;
    }
    fs::create_dir_all(&data_dir).map_err(io)?;
    // Indexing starts at once (no first-run question) and does not wait for the user to be idle.
    let settings = Settings {
        index_roots: vec![root.clone()],
        first_run_done: true,
        idle_secs: 0,
        ..Settings::default()
    };
    settings
        .save(&data_dir.join("settings.json"))
        .map_err(|e| e.to_string())?;
    Ok(Corpus {
        root,
        data_dir,
        base: Path::new(&base).to_path_buf(),
    })
}
