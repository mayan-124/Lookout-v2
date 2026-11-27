//! The action registry the window reads (`ui/src/registry.json`): every example
//! in it must be something Lookout really understands.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;

use lookout_core::query::parse;
use lookout_core::{actions::kill_port, answers, calc, notes, quick, timers, uimode, units};
use serde_json::Value;

fn actions() -> Vec<Value> {
    serde_json::from_str(include_str!("../../ui/src/registry.json")).unwrap()
}

fn triggers(manifest: &str) -> Vec<String> {
    let v: Value = serde_json::from_str(manifest).unwrap();
    serde_json::from_value(v["triggers"].clone()).unwrap()
}

#[test]
fn ids_are_unique_and_switches_and_modes_are_real() {
    let shown = serde_json::to_value(uimode::Show::default()).unwrap();
    let mut seen = HashSet::new();
    for a in actions() {
        let id = a["id"].as_str().unwrap();
        assert!(seen.insert(id.to_owned()), "{id} twice");
        if let Some(f) = a.get("feature").and_then(Value::as_str) {
            assert!(shown.get(f).is_some(), "{id}: {f}");
        }
        serde_json::from_value::<uimode::Mode>(a["mode"].clone()).unwrap();
    }
}

#[test]
fn the_copy_is_short() {
    for a in actions() {
        let text = a["description"].as_str().unwrap();
        assert!(text.len() <= 60 && !text.ends_with('.'), "{text}");
        let low = text.to_lowercase();
        assert!(!low.contains("please") && !low.contains("successfully"));
        assert!(a["synonyms"].as_array().unwrap().len() >= 5, "{}", a["id"]);
    }
}

#[test]
fn every_example_is_understood() {
    let clip = triggers(include_str!("../../plugins/clipboard/plugin.json"));
    let shot = triggers(include_str!("../../plugins/screenshot/plugin.json"));
    for a in actions() {
        let id = a["id"].as_str().unwrap();
        let example = a["example"].as_str().unwrap();
        let typed = a.get("try").and_then(Value::as_str).unwrap_or(example);
        let q = parse(typed);
        assert!(!q.is_empty(), "{id}: nothing to search for");
        let first = typed.split_whitespace().next().unwrap().to_owned();
        match id {
            "picturetext" => assert_eq!(q.kind, Some(lookout_core::item::Kind::Screenshot)),
            "calculator" => assert!(calc::try_evaluate(typed).is_some()),
            "units" => assert!(units::convert(typed).is_some()),
            "url" => assert!(quick::parse_web_url(typed).is_some()),
            "netpath" => assert!(quick::parse_unc(example).is_some()),
            "bydate" | "photos" => assert!(q.time.is_some() && !q.exts.is_empty(), "{typed}"),
            "op_ext" | "op_size" | "op_in" | "op_name" => {
                assert_eq!(q.understood.operators, [typed]);
                assert!(!q.ops.is_empty() && q.tokens.is_empty(), "{typed}");
            }
            "recent" => assert!(q.time.is_some()),
            "timer" => assert!(matches!(timers::parse(typed), timers::Parsed::Timer { .. })),
            "timers" => assert!(timers::is_list_request(typed)),
            "dates" | "timezone" => {
                assert!(
                    answers::is_question(typed, &chrono::Local::now()),
                    "{typed}"
                );
                assert!(q.time.is_none() && q.understood.operators.is_empty());
            }
            "note" => {
                assert!(matches!(
                    notes::command(typed),
                    Some(notes::Command::Save(_))
                ));
                assert!(q.ops.is_empty() && !q.understood.invalid_operator);
            }
            "notes" => assert_eq!(notes::command(typed), Some(notes::Command::List)),
            "darkmode" | "winsettings" | "lock" | "emptybin" => {
                assert!(!quick::match_entries(typed).is_empty());
            }
            "websearch" => assert!(lookout_core::websearch::command(typed).is_some()),
            "killport" => assert_eq!(kill_port::parse_kill_request(typed), Some(3000)),
            "clipboard" => assert!(clip.contains(&first)),
            "screenshot" | "scrolling" | "record" | "extracttext" => {
                assert!(shot.contains(&first), "{typed}");
                // What choosing it types must reach the add-on too.
                let command = a["commands"][0].as_str().unwrap();
                assert!(
                    shot.iter()
                        .any(|t| command.split(' ').next() == Some(t.as_str())),
                    "{command}"
                );
            }
            "commandline" => {
                let args: Vec<String> = example
                    .split_whitespace()
                    .skip(1)
                    .map(str::to_owned)
                    .collect();
                match lookout_cli::cmd::parse(&args) {
                    Ok(lookout_cli::cmd::Command::Search { query, .. }) => {
                        assert_eq!(query, typed);
                    }
                    other => panic!("{example}: {other:?}"),
                }
            }
            // The window itself answers these.
            "disk" => assert_eq!(typed, "disk usage"),
            "help" => assert_eq!(typed, "help"),
            "settings" | "appearance" | "privacy" | "setup" | "queryshortcut" => {}
            // Plain name searches.
            "find" | "quickkey" => assert!(q.time.is_none()),
            other => panic!("no check for {other}"),
        }
    }
}
