//! Date answers, worked out from the local date: `days until 25 dec`,
//! `days since 1 jan 2025`, `25 dec`, `today + 30 days`, `weekday of 15 aug 2027`.
//! Anything the text does not say (the year, day or month first) is stated in
//! the answer.

use chrono::{Datelike, Days, Months, NaiveDate};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// `answer` is what Enter copies; `detail` says how the text was read.
    Answer { answer: String, detail: String },
    /// A date question that has no answer; the text says why.
    Problem(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ask {
    Until,
    Since,
    Weekday,
}

const PREFIXES: [(&str, Ask); 5] = [
    ("days until ", Ask::Until),
    ("days till ", Ask::Until),
    ("days to ", Ask::Until),
    ("days since ", Ask::Since),
    ("weekday of ", Ask::Weekday),
];

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// Longer text is not a date question.
const MAX_LEN: usize = 60;
/// The most that can be added or taken away.
const MAX_STEPS: u32 = 100_000;
const FORMS: &str = "Try 25 dec, 25 december 2026, 2026-12-25 or 25/12/2026";

fn month_of(word: &str) -> Option<u32> {
    let word = word.strip_suffix('.').unwrap_or(word);
    if word == "sept" {
        return Some(9);
    }
    MONTHS
        .iter()
        .position(|m| word == *m || (word.len() == 3 && m.starts_with(word)))
        .map(|i| i as u32 + 1)
}

fn month_name(month: u32) -> &'static str {
    const NAMES: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    NAMES[(month.clamp(1, 12) - 1) as usize]
}

fn number(word: &str, max_len: usize) -> Option<u32> {
    let ok = !word.is_empty() && word.len() <= max_len && word.bytes().all(|b| b.is_ascii_digit());
    ok.then(|| word.parse().ok()).flatten()
}

/// "25", "25th", "1st".
fn day_of(word: &str) -> Option<u32> {
    let digits = ["st", "nd", "rd", "th"]
        .iter()
        .find_map(|s| word.strip_suffix(s))
        .unwrap_or(word);
    number(digits, 2).filter(|d| (1..=31).contains(d))
}

fn year_of(word: &str) -> Option<i32> {
    (word.len() == 4)
        .then(|| number(word, 4))
        .flatten()
        .filter(|y| (1600..=9999).contains(y))
        .map(|y| y as i32)
}

/// A date as typed: the year may be missing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Written {
    day: u32,
    month: u32,
    year: Option<i32>,
    /// How an unclear form was read.
    reading: Option<String>,
}

/// `2026-12-25`, `25/12/2026`; with `loose` also `25/12/26` and `25/12`.
fn numeric(word: &str, loose: bool) -> Option<Written> {
    let sep = word.chars().find(|c| matches!(c, '-' | '/' | '.'))?;
    let parts: Vec<&str> = word.split(sep).collect();
    if let [y, m, d] = parts[..] {
        if let Some(year) = year_of(y) {
            let month = number(m, 2).filter(|m| (1..=12).contains(m))?;
            return Some(Written {
                day: day_of(d)?,
                month,
                year: Some(year),
                reading: None,
            });
        }
    }
    let (a, b, y) = match parts[..] {
        [a, b, y] => (a, b, Some(y)),
        // A dot alone is a decimal number.
        [a, b] if loose && sep != '.' => (a, b, None),
        _ => return None,
    };
    let (a, b) = (number(a, 2)?, number(b, 2)?);
    let mut readings = Vec::new();
    let year = match y {
        None => None,
        Some(y) => match year_of(y) {
            Some(year) => Some(year),
            None if loose && y.len() == 2 => {
                let year = 2000 + number(y, 2)? as i32;
                readings.push(format!("year {y} read as {year}"));
                Some(year)
            }
            None => return None,
        },
    };
    let fits = |day: u32, month: u32| (1..=31).contains(&day) && (1..=12).contains(&month);
    let (day, month) = if fits(a, b) {
        if fits(b, a) && a != b {
            readings.insert(0, format!("read {word} as day/month"));
        }
        (a, b)
    } else if fits(b, a) {
        readings.insert(0, format!("read {word} as month/day"));
        (b, a)
    } else {
        return None;
    };
    Some(Written {
        day,
        month,
        year,
        reading: (!readings.is_empty()).then(|| readings.join(", ")),
    })
}

/// The whole of `words` as one date.
fn written(words: &[&str], loose: bool) -> Option<Written> {
    let named = |day: &str, month: &str, year: Option<&str>| {
        Some(Written {
            day: day_of(day)?,
            month: month_of(month)?,
            year: match year {
                Some(y) => Some(year_of(y)?),
                None => None,
            },
            reading: None,
        })
    };
    match *words {
        [one] => numeric(one, loose),
        [a, b] => named(a, b, None).or_else(|| named(b, a, None)),
        [a, "of", b] => named(a, b, None),
        [a, b, y] => named(a, b, Some(y)).or_else(|| named(b, a, Some(y))),
        [a, "of", b, y] => named(a, b, Some(y)),
        _ => None,
    }
}

fn long(date: NaiveDate) -> String {
    format!(
        "{} {} {} {}",
        weekday(date),
        date.day(),
        month_name(date.month()),
        date.year()
    )
}

fn weekday(date: NaiveDate) -> &'static str {
    const NAMES: [&str; 7] = [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ];
    NAMES[date.weekday().num_days_from_monday() as usize]
}

fn days(n: i64) -> String {
    format!("{n} day{}", if n == 1 { "" } else { "s" })
}

/// The date meant, and what had to be assumed. A missing year is the next such
/// day for "until", the last one for "since", else this year.
fn resolve(
    w: &Written,
    ask: Option<Ask>,
    today: NaiveDate,
) -> Result<(NaiveDate, Vec<String>), String> {
    let mut notes: Vec<String> = w.reading.iter().cloned().collect();
    let on = |year: i32| NaiveDate::from_ymd_opt(year, w.month, w.day);
    let name = format!("{} {}", w.day, month_name(w.month));
    // A leap year proves the day exists at all.
    if NaiveDate::from_ymd_opt(2028, w.month, w.day).is_none() {
        return Err(format!("{name} does not exist"));
    }
    if let Some(year) = w.year {
        let date = on(year).ok_or_else(|| format!("{name} does not exist in {year}"))?;
        return Ok((date, notes));
    }
    let this = today.year();
    let date = match ask {
        // Eight years always hold a 29 February.
        Some(Ask::Until) => (this..=this + 8).filter_map(on).find(|d| *d >= today),
        Some(Ask::Since) => (this - 8..=this).rev().filter_map(on).find(|d| *d <= today),
        _ => on(this),
    };
    let date = date.ok_or_else(|| format!("{name} does not exist in {this}; add a year"))?;
    notes.push(format!("no year given, used {}", date.year()));
    Ok((date, notes))
}

/// "today", "tomorrow", "yesterday" or a date.
fn base(
    words: &[&str],
    ask: Option<Ask>,
    today: NaiveDate,
) -> Option<Result<(NaiveDate, Vec<String>), String>> {
    let near = match words {
        ["today"] => Some(today),
        ["tomorrow"] => today.succ_opt(),
        ["yesterday"] => today.pred_opt(),
        _ => None,
    };
    if let Some(d) = near {
        return Some(Ok((d, Vec::new())));
    }
    written(words, true).map(|w| resolve(&w, ask, today))
}

#[derive(Clone, Copy)]
enum Step {
    Days(u32),
    Months(u32),
}

/// "30 days", "2 weeks", "1 month".
fn step(text: &str) -> Option<(Step, String)> {
    let text = text.trim();
    let at = text.find(|c: char| !c.is_ascii_digit())?;
    let (n, unit) = (number(&text[..at], 6)?, text[at..].trim());
    if n > MAX_STEPS {
        return None;
    }
    let (step, unit) = match unit {
        "d" | "day" | "days" => (Step::Days(n), "day"),
        "w" | "week" | "weeks" => (Step::Days(n.checked_mul(7)?), "week"),
        "month" | "months" => (Step::Months(n), "month"),
        "y" | "year" | "years" => (Step::Months(n.checked_mul(12)?), "year"),
        _ => return None,
    };
    Some((step, format!("{n} {unit}{}", if n == 1 { "" } else { "s" })))
}

/// `<date> + <n> <unit>`.
fn sum(text: &str, today: NaiveDate) -> Option<Reply> {
    // The last sign that is followed by an amount: ISO dates hold dashes too.
    let (at, (amount, said)) = text
        .rmatch_indices(['+', '-'])
        .find_map(|(at, _)| step(&text[at + 1..]).map(|s| (at, s)))?;
    let back = text.as_bytes()[at] == b'-';
    let words: Vec<&str> = text[..at].split_whitespace().collect();
    let (from, mut notes) = match base(&words, None, today)? {
        Ok(found) => found,
        Err(why) => return Some(Reply::Problem(why)),
    };
    let to = match amount {
        Step::Days(n) if back => from.checked_sub_days(Days::new(n.into())),
        Step::Days(n) => from.checked_add_days(Days::new(n.into())),
        Step::Months(n) if back => from.checked_sub_months(Months::new(n)),
        Step::Months(n) => from.checked_add_months(Months::new(n)),
    };
    let Some(to) = to.filter(|d| (1..=9999).contains(&d.year())) else {
        return Some(Reply::Problem("That is too far away to work out".into()));
    };
    if matches!(amount, Step::Months(_)) && to.day() != from.day() {
        notes.push(format!(
            "{} has no day {}, used its last day",
            month_name(to.month()),
            from.day()
        ));
    }
    let side = if back { "before" } else { "after" };
    notes.insert(0, format!("{said} {side} {}", long(from)));
    Some(Reply::Answer {
        answer: long(to),
        detail: notes.join(" · "),
    })
}

fn asked(ask: Ask, rest: &str, today: NaiveDate) -> Reply {
    let words: Vec<&str> = rest.split_whitespace().collect();
    let (date, notes) = match base(&words, Some(ask), today) {
        Some(Ok(found)) => found,
        Some(Err(why)) => return Reply::Problem(why),
        None => return Reply::Problem(format!("That date was not understood. {FORMS}")),
    };
    let delta = date.signed_duration_since(today).num_days();
    let (answer, head) = match ask {
        Ask::Weekday => (
            weekday(date).to_owned(),
            format!(
                "{} {} {}",
                date.day(),
                month_name(date.month()),
                date.year()
            ),
        ),
        Ask::Until if delta < 0 => (
            format!("{} ago", days(-delta)),
            format!("{} has passed", long(date)),
        ),
        Ask::Until => (days(delta), format!("Until {}", long(date))),
        Ask::Since if delta > 0 => (
            format!("In {}", days(delta)),
            format!("{} is still ahead", long(date)),
        ),
        Ask::Since => (days(-delta), format!("Since {}", long(date))),
    };
    let detail = std::iter::once(head)
        .chain(notes)
        .collect::<Vec<_>>()
        .join(" · ");
    Reply::Answer { answer, detail }
}

fn bare(text: &str, today: NaiveDate) -> Option<Reply> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let w = written(&words, false)?;
    Some(match resolve(&w, None, today) {
        Err(why) => Reply::Problem(why),
        Ok((date, notes)) => {
            let delta = date.signed_duration_since(today).num_days();
            let away = match delta {
                0 => "Today".to_owned(),
                d if d > 0 => format!("In {}", days(d)),
                d => format!("{} ago", days(-d)),
            };
            Reply::Answer {
                answer: long(date),
                detail: std::iter::once(away)
                    .chain(notes)
                    .collect::<Vec<_>>()
                    .join(" · "),
            }
        }
    })
}

/// The answer to `input`, or `None` when it is not a date question.
pub fn answer(input: &str, today: NaiveDate) -> Option<Reply> {
    // Cheap first: a date question is short and has a digit or a known start.
    if input.len() > MAX_LEN {
        return None;
    }
    let lower = input.trim().trim_end_matches('?').to_lowercase();
    if !lower.bytes().any(|b| b.is_ascii_digit())
        && !PREFIXES.iter().any(|p| lower.starts_with(p.0))
    {
        return None;
    }
    let text = lower.replace(',', " ");
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some((rest, ask)) = PREFIXES
        .iter()
        .find_map(|(p, ask)| text.strip_prefix(p).map(|rest| (rest, *ask)))
    {
        return Some(asked(ask, rest, today));
    }
    sum(&text, today).or_else(|| bare(&text, today))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap_or_default()
    }

    /// Thursday 26 November 2026.
    fn ask(text: &str) -> Option<Reply> {
        answer(text, date(2026, 11, 26))
    }

    fn got(text: &str) -> (String, String) {
        match ask(text) {
            Some(Reply::Answer { answer, detail }) => (answer, detail),
            other => panic!("{text}: {other:?}"),
        }
    }

    fn problem(text: &str) -> String {
        match ask(text) {
            Some(Reply::Problem(why)) => why,
            other => panic!("{text}: {other:?}"),
        }
    }

    #[test]
    fn the_asked_forms_are_answered() {
        let (a, d) = got("days until 25 dec");
        assert_eq!(a, "29 days");
        assert_eq!(
            d,
            "Until Friday 25 December 2026 · no year given, used 2026"
        );
        assert_eq!(got("Days since 1 Jan 2025").0, "694 days");
        assert_eq!(got("25 dec").0, "Friday 25 December 2026");
        assert_eq!(got("today + 30 days").0, "Saturday 26 December 2026");
        assert_eq!(got("today+2 weeks").0, "Thursday 10 December 2026");
        assert_eq!(got("tomorrow - 1 day").0, "Thursday 26 November 2026");
        assert_eq!(got("weekday of 15 aug 2027").0, "Sunday");
        assert_eq!(got("days until 26 nov").0, "0 days");
    }

    #[test]
    fn common_formats_read_the_same_day() {
        for text in [
            "25 dec",
            "25 december 2026",
            "25th Dec, 2026",
            "dec 25",
            "December 25 2026",
            "25th of december",
            "2026-12-25",
            "25/12/2026",
            "25.12.2026",
            "25-12-2026",
        ] {
            assert_eq!(got(text).0, "Friday 25 December 2026", "{text}");
        }
    }

    #[test]
    fn an_unclear_form_says_how_it_was_read() {
        let (a, d) = got("05/04/2026");
        assert_eq!(a, "Sunday 5 April 2026");
        assert!(d.contains("read 05/04/2026 as day/month"), "{d}");
        let (a, d) = got("12/25/2026");
        assert_eq!(a, "Friday 25 December 2026");
        assert!(d.contains("as month/day"), "{d}");
        assert!(!got("25/12/2026").1.contains("read"));
        let (_, d) = got("days until 25/12/27");
        assert!(d.contains("year 27 read as 2027"), "{d}");
        // A missing year goes forwards for "until" and back for "since".
        assert!(got("days until 1 jan").1.contains("used 2027"));
        assert!(got("days since 25 dec").1.contains("used 2025"));
        assert!(got("weekday of 15 aug").1.contains("used 2026"));
    }

    #[test]
    fn leap_years_and_month_ends() {
        assert_eq!(got("29 feb 2028").0, "Tuesday 29 February 2028");
        assert_eq!(problem("29 feb 2027"), "29 February does not exist in 2027");
        // 2100 is not a leap year, 2000 was.
        assert!(matches!(ask("29/02/2100"), Some(Reply::Problem(_))));
        assert_eq!(got("29/02/2000").0, "Tuesday 29 February 2000");
        assert!(got("days until 29 feb").1.contains("29 February 2028"));
        assert!(got("days since 29 feb").1.contains("29 February 2024"));
        assert!(problem("29 feb").contains("add a year"));
        assert_eq!(problem("31 apr"), "31 April does not exist");
        let (a, d) = got("31 jan 2027 + 1 month");
        assert_eq!(a, "Sunday 28 February 2027");
        assert!(d.contains("February has no day 31"), "{d}");
        assert_eq!(got("31 jan 2028 + 1 month").0, "Tuesday 29 February 2028");
        assert_eq!(got("2028-02-29 + 1 year").0, "Wednesday 28 February 2029");
        assert_eq!(got("2026-12-31 + 1 day").0, "Friday 1 January 2027");
        assert_eq!(got("1 mar 2028 - 1 day").0, "Tuesday 29 February 2028");
    }

    #[test]
    fn other_searches_are_left_alone() {
        for text in [
            "",
            "report",
            "dec",
            "dec 2026",
            "march",
            "25",
            "25/12",
            "25.12",
            "2 + 2",
            "2026-12",
            "100-25-3",
            "invoice 25 dec",
            "25 dec photos",
            "today",
            "last 30 days",
            "days until",
            "timer 5 min",
            "10 km to miles",
            "1/2/26",
        ] {
            assert_eq!(ask(text), None, "{text}");
        }
        assert!(problem("days until christmas").contains("not understood"));
        assert!(problem("today + 100000 years").contains("too far"));
    }
}
