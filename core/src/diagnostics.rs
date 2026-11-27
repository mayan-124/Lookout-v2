//! The text behind "Copy diagnostics": versions, display facts, counts, which
//! plugins are on and the latest error lines. It takes no file names,
//! searches, notes, clipboard text or paths.

/// What the shell knows; the rest comes from [`Lookout`](crate::lookout::Lookout).
#[derive(Debug, Default, Clone)]
pub struct Facts {
    pub app_version: String,
    /// Scale factor of each monitor.
    pub scales: Vec<f64>,
    pub mode: String,
    pub theme: String,
    pub safe_mode: bool,
}

/// What the running search knows.
#[derive(Debug, Default, Clone)]
pub struct Counts {
    pub files: usize,
    pub folders: usize,
    pub apps: usize,
    pub content_docs: u64,
    /// Unix seconds of the last finished crawl in this run.
    pub last_crawl: Option<u64>,
    /// Ids of the plugins that are on.
    pub plugins: Vec<String>,
}

pub const ERROR_LINES: usize = 20;

/// A version, theme word or plugin id; anything else could be free text.
fn word(s: &str) -> &str {
    let plain = !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._- ".contains(c));
    if plain {
        s
    } else {
        "other"
    }
}

fn when(secs: u64) -> String {
    i64::try_from(secs)
        .ok()
        .and_then(|s| chrono::DateTime::from_timestamp(s, 0))
        .map_or_else(
            || "unknown".into(),
            |t| {
                t.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            },
        )
}

pub fn render(facts: &Facts, counts: Option<&Counts>, errors: &[String]) -> String {
    let windows = sysinfo::System::long_os_version().unwrap_or_else(|| "unknown".into());
    let build = sysinfo::System::kernel_version().unwrap_or_default();
    let scales: Vec<String> = facts
        .scales
        .iter()
        .map(|s| format!("{:.0}%", s * 100.0))
        .collect();
    let mut out = vec![
        format!("Lookout {}", word(&facts.app_version)),
        format!("Windows: {windows} (build {build})"),
        format!("Monitors: {}", facts.scales.len()),
        format!("Scaling: {}", scales.join(", ")),
        format!("Mode: {}", word(&facts.mode)),
        format!("Theme: {}", word(&facts.theme)),
        format!("Safe mode: {}", if facts.safe_mode { "yes" } else { "no" }),
    ];
    match counts {
        Some(c) => {
            let on: Vec<&str> = c.plugins.iter().map(|p| word(p)).collect();
            out.push(format!(
                "Index: {} files, {} folders, {} apps, {} files with contents",
                c.files, c.folders, c.apps, c.content_docs
            ));
            out.push(format!(
                "Last crawl: {}",
                c.last_crawl
                    .map_or_else(|| "none since this start".into(), when)
            ));
            out.push(format!(
                "Add-ons on: {}",
                if on.is_empty() {
                    "none".into()
                } else {
                    on.join(", ")
                }
            ));
        }
        None => out.push("Search: not running".into()),
    }
    out.push(format!("Last errors: {}", errors.len()));
    out.extend(errors.iter().cloned());
    out.join("\r\n")
}
