//! What the bar shows for `help` or `?`. To add a row, add a line to [`HELP`];
//! the tests check that its example is something Lookout really understands.

/// One row of the help view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Help {
    pub group: &'static str,
    /// One line, sentence case, verb first.
    pub what: &'static str,
    /// What the row shows.
    pub example: &'static str,
    /// What clicking the row puts in the search box.
    pub fill: &'static str,
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

const fn row(group: &'static str, what: &'static str, example: &'static str) -> Help {
    Help {
        group,
        what,
        example,
        fill: example,
    }
}

pub const HELP: &[Help] = &[
    row("Search", "Find files, folders and apps by name", "report"),
    row(
        "Search",
        "Find words seen in pictures",
        "screenshot with invoice",
    ),
    row("Search", "Calculate", "128 * 46"),
    row("Search", "Convert units", "10 km to miles"),
    row(
        "Time filters",
        "Find a kind of file by date",
        "pdf last week",
    ),
    row("Time filters", "Find photos from a day", "photos yesterday"),
    row("Time filters", "List what changed lately", "past week"),
    row("Timers", "Start a timer", "timer 5 min"),
    row("Timers", "Name a timer", "timer 25 min tea"),
    row("Timers", "See running timers", "timers"),
    row(
        "Quick keys",
        "Select a result, then click Set quick key",
        "notepad",
    ),
    row("System", "Switch dark or light mode", "dark mode"),
    row("System", "Open a Windows settings page", "wifi"),
    row("System", "See and stop what uses a port", "kill port 3000"),
    row(
        "Disk",
        "See what takes up space, then press Enter",
        "disk usage",
    ),
    Help {
        group: "Clipboard",
        what: "Find copied text (needs the add-on in Settings)",
        example: "clip",
        // The trailing space opens the Clipboard section.
        fill: "clip ",
    },
    row(
        "Capture",
        "Take a screenshot (needs the add-on in Settings)",
        "shot",
    ),
    row("Capture", "Record the screen", "record"),
    row("Capture", "Copy text from the screen", "extract text"),
    Help {
        group: "Command line",
        what: "Search from a terminal (turn on in Settings > Privacy)",
        example: "lookout search pdf last week",
        fill: "pdf last week",
    },
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
