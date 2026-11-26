//! What the bar shows for `help` or `?`. To add a row, add a line to [`HELP`];
//! the tests check that its example is something Lookout really understands.

/// A group of rows: the switch that shows it (none: always shown), and whether
/// Simple and Standard fold it away under "Advanced".
struct Group {
    name: &'static str,
    feature: Option<&'static str>,
    advanced: bool,
}

const fn group(name: &'static str, feature: Option<&'static str>, advanced: bool) -> Group {
    Group {
        name,
        feature,
        advanced,
    }
}

const SEARCH: Group = group("Search", None, false);
const DATES: Group = group("Time filters", Some("filters"), false);
const TIMERS: Group = group("Timers", Some("timers"), false);
const QUICK_KEYS: Group = group("Quick keys", Some("quick_keys"), true);
const SYSTEM: Group = group("System", None, true);
const DISK: Group = group("Disk", Some("disk"), true);
const CLIPBOARD: Group = group("Clipboard", Some("clipboard"), false);
const CAPTURE: Group = group("Capture", Some("capture"), false);
const COMMAND_LINE: Group = group("Command line", None, true);

/// One row of the help view, also used for the tips on Home.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Help {
    pub group: &'static str,
    /// One line, sentence case, verb first.
    pub what: &'static str,
    /// What the row shows.
    pub example: &'static str,
    /// What clicking the row puts in the search box.
    pub fill: &'static str,
    /// The Settings > Features switch that shows this row, if any.
    pub feature: Option<&'static str>,
    pub advanced: bool,
}

/// Groups in the order they are shown.
pub const GROUPS: [&str; 9] = [
    "Search",
    "Time filters",
    "Timers",
    "Quick keys",
    "System",
    "Disk",
    "Clipboard",
    "Capture",
    "Command line",
];

const fn row(g: Group, what: &'static str, example: &'static str) -> Help {
    filled(g, what, example, example)
}

const fn filled(g: Group, what: &'static str, example: &'static str, fill: &'static str) -> Help {
    Help {
        group: g.name,
        what,
        example,
        fill,
        feature: g.feature,
        advanced: g.advanced,
    }
}

pub const HELP: &[Help] = &[
    row(SEARCH, "Find files, folders and apps by name", "report"),
    row(SEARCH, "Find words in pictures", "screenshot with invoice"),
    row(SEARCH, "Calculate", "128 * 46"),
    row(SEARCH, "Convert units", "10 km to miles"),
    row(DATES, "Find a file type by date", "pdf last week"),
    row(DATES, "Find photos from a day", "photos yesterday"),
    row(DATES, "List recent changes", "past week"),
    row(TIMERS, "Start a timer", "timer 5 min"),
    row(TIMERS, "Name a timer", "timer 25 min tea"),
    row(TIMERS, "See running timers", "timers"),
    row(QUICK_KEYS, "Select a result, then Set quick key", "notepad"),
    row(SYSTEM, "Switch dark or light mode", "dark mode"),
    row(SYSTEM, "Open a Windows settings page", "wifi"),
    row(SYSTEM, "See or stop what uses a port", "kill port 3000"),
    row(DISK, "See what uses space, then press Enter", "disk usage"),
    // The trailing space opens the Clipboard section.
    filled(
        CLIPBOARD,
        "Find copied text (needs the add-on)",
        "clip",
        "clip ",
    ),
    row(CAPTURE, "Take a screenshot (needs the add-on)", "shot"),
    row(CAPTURE, "Record the screen", "record"),
    row(CAPTURE, "Copy text from the screen", "extract text"),
    filled(
        COMMAND_LINE,
        "Search from a terminal (turn on in Settings)",
        "lookout search pdf last week",
        "pdf last week",
    ),
];

/// The whole table.
pub fn entries() -> &'static [Help] {
    HELP
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parse;

    fn triggers(manifest: &str) -> Vec<String> {
        let v: serde_json::Value = serde_json::from_str(manifest).unwrap();
        serde_json::from_value(v["triggers"].clone()).unwrap()
    }

    #[test]
    fn every_group_has_rows_and_the_groups_stay_together() {
        let mut seen: Vec<&str> = HELP.iter().map(|h| h.group).collect();
        seen.dedup();
        assert_eq!(seen, GROUPS);
    }

    #[test]
    fn rows_name_a_real_switch() {
        let shown = serde_json::to_value(crate::uimode::Show::default()).unwrap();
        for h in HELP {
            if let Some(f) = h.feature {
                assert!(shown.get(f).is_some(), "{f}");
            }
        }
    }

    #[test]
    fn the_copy_is_short() {
        for h in HELP {
            assert!(h.what.len() <= 60 && !h.what.ends_with('.'), "{}", h.what);
            let low = h.what.to_lowercase();
            assert!(!low.contains("please") && !low.contains("successfully"));
        }
    }

    #[test]
    fn every_example_is_understood() {
        let clip = triggers(include_str!("../../plugins/clipboard/plugin.json"));
        let shot = triggers(include_str!("../../plugins/screenshot/plugin.json"));
        for h in HELP {
            let q = parse(h.fill);
            assert!(!q.is_empty(), "{}: nothing to search for", h.fill);
            let first = h.fill.split_whitespace().next().unwrap();
            match (h.group, h.fill) {
                ("Search", "screenshot with invoice") => {
                    assert_eq!(q.kind, Some(crate::item::Kind::Screenshot));
                }
                ("Search", "128 * 46") => {
                    assert!(crate::calc::try_evaluate(h.fill).is_some());
                }
                ("Search", "10 km to miles") => {
                    assert!(crate::units::convert(h.fill).is_some());
                }
                ("Time filters", "past week") => assert!(q.time.is_some()),
                ("Time filters", _) => {
                    assert!(q.time.is_some() && !q.exts.is_empty(), "{}", h.fill);
                }
                ("Timers", "timers") => assert!(crate::timers::is_list_request(h.fill)),
                ("Timers", _) => assert!(matches!(
                    crate::timers::parse(h.fill),
                    crate::timers::Parsed::Timer { .. }
                )),
                ("System", "kill port 3000") => assert_eq!(
                    crate::actions::kill_port::parse_kill_request(h.fill),
                    Some(3000)
                ),
                ("System", _) => {
                    assert!(
                        !crate::quick::match_entries(h.fill).is_empty(),
                        "{}",
                        h.fill
                    );
                }
                // The window opens the analyzer for exactly this text.
                ("Disk", _) => assert_eq!(h.fill, "disk usage"),
                ("Clipboard", _) => assert!(clip.iter().any(|t| t == first)),
                ("Capture", _) => assert!(shot.iter().any(|t| t == first)),
                ("Command line", _) => {
                    let args: Vec<String> = h
                        .example
                        .split_whitespace()
                        .skip(1)
                        .map(str::to_owned)
                        .collect();
                    match lookout_cli::cmd::parse(&args) {
                        Ok(lookout_cli::cmd::Command::Search { query, .. }) => {
                            assert_eq!(query, h.fill);
                        }
                        other => panic!("{}: {other:?}", h.example),
                    }
                }
                // Plain name searches.
                ("Search" | "Quick keys", _) => assert!(q.time.is_none()),
                (group, fill) => panic!("no check for {group}: {fill}"),
            }
        }
    }
}
