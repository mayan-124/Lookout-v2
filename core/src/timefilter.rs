//! Time phrases in queries (brief section 7.5): "yesterday", "last week",
//! "march", "last 3 days". The phrase is removed from the search words and
//! becomes a window `[from, to)` on the file's modified time.
//!
//! Rules (all in the user's local time zone, weeks start on Monday):
//! - `today`, `yesterday`
//! - `this week|month|year` runs from the start of that period until the end of today
//! - `last week|month|year` is the previous *calendar* period
//! - `past week|month|year` and `last N days|weeks|months` (N up to 365 days
//!   in total) are rolling windows ending at the end of today (a month is 30 days)
//! - a month name (`march`, `mar`), optionally with a year (`march 2025`): the most
//!   recent such month that has started (so `march` in November 2026 is March 2026)
//! - a plain year from 2019 up to the current one (`2024`)
//!
//! A month name is only a time phrase when it follows `in`, `from`, `during` or
//! `of`, or is followed by a four-digit year; otherwise `may` and `march` stay
//! ordinary words. A plain year counts only after one of those prepositions or
//! next to a kind word (`pdf 2024`), so `annual report 2024` stays a search for
//! a file name. "this week" and
//! "this month" are not cut off at the current moment, only at the end of today.

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone};

use crate::query::TimeRange;

const MONTHS: &[(&str, u32)] = &[
    ("january", 1),
    ("jan", 1),
    ("february", 2),
    ("feb", 2),
    ("march", 3),
    ("mar", 3),
    ("april", 4),
    ("apr", 4),
    ("may", 5),
    ("june", 6),
    ("jun", 6),
    ("july", 7),
    ("jul", 7),
    ("august", 8),
    ("aug", 8),
    ("september", 9),
    ("sept", 9),
    ("sep", 9),
    ("october", 10),
    ("oct", 10),
    ("november", 11),
    ("nov", 11),
    ("december", 12),
    ("dec", 12),
];

fn month_of(word: &str) -> Option<u32> {
    MONTHS.iter().find(|(n, _)| *n == word).map(|(_, m)| *m)
}

const PREPOSITIONS: &[&str] = &["in", "from", "during", "of"];

/// The longest rolling window, in days.
const MAX_DAYS: i64 = 365;

#[derive(Clone, Copy)]
enum Unit {
    Day,
    Week,
    Month,
    Year,
}

fn unit_of(word: &str) -> Option<Unit> {
    match word {
        "day" | "days" => Some(Unit::Day),
        "week" | "weeks" => Some(Unit::Week),
        "month" | "months" => Some(Unit::Month),
        "year" | "years" => Some(Unit::Year),
        _ => None,
    }
}

/// Start of `date` in `tz`, as unix seconds.
fn start_of<Tz: TimeZone>(tz: &Tz, date: NaiveDate) -> Option<i64> {
    let naive = date.and_hms_opt(0, 0, 0)?;
    let t = tz
        .from_local_datetime(&naive)
        .earliest()
        // A midnight skipped by daylight saving: use the first valid moment after it.
        .or_else(|| {
            tz.from_local_datetime(&(naive + Duration::hours(1)))
                .earliest()
        })?;
    Some(t.timestamp())
}

fn range<Tz: TimeZone>(tz: &Tz, from: NaiveDate, to: NaiveDate) -> Option<TimeRange> {
    let (a, b) = (start_of(tz, from)?, start_of(tz, to)?);
    Some(TimeRange {
        from: u64::try_from(a).ok()?,
        to: u64::try_from(b).ok()?,
    })
}

fn first_of_month(y: i32, m: u32) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(y, m, 1)
}

fn add_months(y: i32, m: u32, delta: i32) -> (i32, u32) {
    let idx = y * 12 + (m as i32 - 1) + delta;
    (idx.div_euclid(12), (idx.rem_euclid(12) + 1) as u32)
}

fn w(i: usize, words: &[String]) -> Option<&str> {
    words.get(i).map(String::as_str)
}

/// Removes `words[a..b]` and returns them as one phrase (the filter label).
fn take(words: &mut Vec<String>, a: usize, b: usize) -> String {
    let mut phrase = String::new();
    for w in words.drain(a..b) {
        if !phrase.is_empty() {
            phrase.push(' ');
        }
        phrase.push_str(&w);
    }
    phrase
}

/// Finds the first time phrase in `words` (lowercase), removes it and returns
/// its window with the words it was made of. `now` fixes both the current time
/// and the time zone.
pub fn extract<Tz: TimeZone>(
    words: &mut Vec<String>,
    now: &DateTime<Tz>,
    has_kind_word: bool,
) -> Option<(TimeRange, String)> {
    let tz = now.timezone();
    let today = now.date_naive();
    let tomorrow = today + Duration::days(1);

    for i in 0..words.len() {
        let one = words[i].as_str();
        // today / yesterday
        let simple = match one {
            "today" => range(&tz, today, tomorrow),
            "yesterday" => range(&tz, today - Duration::days(1), today),
            _ => None,
        };
        if let Some(r) = simple {
            return Some((r, take(words, i, i + 1)));
        }
        // this|last|past <unit>, last|past N <units>
        if matches!(one, "this" | "last" | "past") {
            if let Some(unit) = w(i + 1, words).and_then(unit_of) {
                let r = match (one, unit) {
                    ("this", Unit::Week) | ("last", Unit::Week) => {
                        let monday = today
                            - Duration::days(i64::from(today.weekday().num_days_from_monday()));
                        if one == "this" {
                            range(&tz, monday, tomorrow)
                        } else {
                            range(&tz, monday - Duration::days(7), monday)
                        }
                    }
                    ("this", Unit::Month) => {
                        range(&tz, first_of_month(today.year(), today.month())?, tomorrow)
                    }
                    ("last", Unit::Month) => {
                        let (y, m) = add_months(today.year(), today.month(), -1);
                        range(
                            &tz,
                            first_of_month(y, m)?,
                            first_of_month(today.year(), today.month())?,
                        )
                    }
                    ("this", Unit::Year) => range(&tz, first_of_month(today.year(), 1)?, tomorrow),
                    ("last", Unit::Year) => range(
                        &tz,
                        first_of_month(today.year() - 1, 1)?,
                        first_of_month(today.year(), 1)?,
                    ),
                    ("past", u) => {
                        let days = match u {
                            Unit::Day => 1,
                            Unit::Week => 7,
                            Unit::Month => 30,
                            Unit::Year => MAX_DAYS,
                        };
                        range(&tz, today - Duration::days(days - 1), tomorrow)
                    }
                    _ => None,
                };
                if let Some(r) = r {
                    return Some((r, take(words, i, i + 2)));
                }
            }
            if one != "this" {
                if let (Some(n), Some(unit)) = (
                    w(i + 1, words).and_then(|s| s.parse::<i64>().ok()),
                    w(i + 2, words).and_then(unit_of),
                ) {
                    let per = match unit {
                        Unit::Day => 1,
                        Unit::Week => 7,
                        Unit::Month => 30,
                        Unit::Year => 0, // "last 2 years" is not a window
                    };
                    let days = n.checked_mul(per).filter(|d| (1..=MAX_DAYS).contains(d));
                    if let Some(days) = days {
                        let r = range(&tz, today - Duration::days(days - 1), tomorrow)?;
                        return Some((r, take(words, i, i + 3)));
                    }
                }
            }
        }
        // month [year]
        if let Some(m) = month_of(one) {
            let year = w(i + 1, words)
                .filter(|y| y.len() == 4)
                .and_then(|y| y.parse::<i32>().ok())
                .filter(|y| (1970..=2200).contains(y));
            let has_prep = i > 0 && w(i - 1, words).is_some_and(|p| PREPOSITIONS.contains(&p));
            let consumed = 1 + usize::from(year.is_some()) + usize::from(has_prep);
            if !has_prep && year.is_none() {
                continue; // "may" and "march" on their own are ordinary words
            }
            let y = year.unwrap_or(if m <= today.month() {
                today.year()
            } else {
                today.year() - 1
            });
            let (ny, nm) = add_months(y, m, 1);
            let r = range(&tz, first_of_month(y, m)?, first_of_month(ny, nm)?)?;
            let start = i - usize::from(has_prep);
            return Some((r, take(words, start, start + consumed)));
        }
        // a plain year, next to other words
        if one.len() == 4 && words.len() > 1 {
            if let Some(y) = one
                .parse::<i32>()
                .ok()
                .filter(|y| (2019..=today.year()).contains(y))
            {
                let has_prep = i > 0 && w(i - 1, words).is_some_and(|p| PREPOSITIONS.contains(&p));
                if !has_prep && !has_kind_word {
                    continue;
                }
                let r = range(&tz, first_of_month(y, 1)?, first_of_month(y + 1, 1)?)?;
                let start = i - usize::from(has_prep);
                return Some((r, take(words, start, i + 1)));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, Utc};

    // Wednesday 18 November 2026, 10:30 in UTC+5:30.
    fn now() -> DateTime<FixedOffset> {
        FixedOffset::east_opt(5 * 3600 + 1800)
            .unwrap()
            .with_ymd_and_hms(2026, 11, 18, 10, 30, 0)
            .unwrap()
    }

    fn day(y: i32, m: u32, d: u32) -> u64 {
        FixedOffset::east_opt(5 * 3600 + 1800)
            .unwrap()
            .with_ymd_and_hms(y, m, d, 0, 0, 0)
            .unwrap()
            .timestamp() as u64
    }

    fn run(input: &str) -> (Vec<String>, Option<(u64, u64)>) {
        run_with(input, false)
    }

    fn run_with(input: &str, kind_words: bool) -> (Vec<String>, Option<(u64, u64)>) {
        let mut words: Vec<String> = input.split_whitespace().map(str::to_lowercase).collect();
        let r = extract(&mut words, &now(), kind_words).map(|(t, _)| (t.from, t.to));
        (words, r)
    }

    #[test]
    fn today_and_yesterday() {
        let (w, r) = run("invoice yesterday");
        assert_eq!(w, ["invoice"]);
        assert_eq!(r, Some((day(2026, 11, 17), day(2026, 11, 18))));
        assert_eq!(run("today").1, Some((day(2026, 11, 18), day(2026, 11, 19))));
    }

    #[test]
    fn weeks_start_on_monday() {
        // 18 Nov 2026 is a Wednesday; this week began Monday the 16th.
        assert_eq!(
            run("notes last week").1,
            Some((day(2026, 11, 9), day(2026, 11, 16)))
        );
        assert_eq!(
            run("this week budget").1,
            Some((day(2026, 11, 16), day(2026, 11, 19)))
        );
    }

    #[test]
    fn months_and_years() {
        assert_eq!(
            run("last month x").1,
            Some((day(2026, 10, 1), day(2026, 11, 1)))
        );
        assert_eq!(
            run("this month x").1,
            Some((day(2026, 11, 1), day(2026, 11, 19)))
        );
        assert_eq!(
            run("last year x").1,
            Some((day(2025, 1, 1), day(2026, 1, 1)))
        );
        assert_eq!(
            run("this year x").1,
            Some((day(2026, 1, 1), day(2026, 11, 19)))
        );
    }

    #[test]
    fn last_month_wraps_over_new_year() {
        let jan = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2026, 1, 10, 9, 0, 0)
            .unwrap();
        let mut w = vec!["last".to_owned(), "month".to_owned(), "x".to_owned()];
        let (r, _) = extract(&mut w, &jan, false).unwrap();
        let dec1 = Utc
            .with_ymd_and_hms(2025, 12, 1, 0, 0, 0)
            .unwrap()
            .timestamp() as u64;
        let jan1 = Utc
            .with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
            .unwrap()
            .timestamp() as u64;
        assert_eq!((r.from, r.to), (dec1, jan1));
    }

    #[test]
    fn rolling_windows() {
        assert_eq!(
            run("x last 3 days").1,
            Some((day(2026, 11, 16), day(2026, 11, 19)))
        );
        assert_eq!(
            run("x past week").1,
            Some((day(2026, 11, 12), day(2026, 11, 19)))
        );
        assert_eq!(
            run("x last 2 weeks").1,
            Some((day(2026, 11, 5), day(2026, 11, 19)))
        );
        assert_eq!(run("x last 0 days").1, None);
    }

    #[test]
    fn month_names_pick_the_most_recent_started_month() {
        let (w, r) = run("taxes in march");
        assert_eq!(w, ["taxes"]);
        assert_eq!(r, Some((day(2026, 3, 1), day(2026, 4, 1))));
        // December has not happened yet this year.
        assert_eq!(run("x in dec").1, Some((day(2025, 12, 1), day(2026, 1, 1))));
        // The current month counts.
        assert_eq!(
            run("x of november").1,
            Some((day(2026, 11, 1), day(2026, 12, 1)))
        );
        assert_eq!(
            run("x march 2024").1,
            Some((day(2024, 3, 1), day(2024, 4, 1)))
        );
        let (w, r) = run("photos from june 2025");
        assert_eq!(w, ["photos"]);
        assert_eq!(r, Some((day(2025, 6, 1), day(2025, 7, 1))));
    }

    #[test]
    fn a_lone_month_word_is_a_normal_word_unless_a_preposition_asks() {
        let (w, r) = run("march");
        assert_eq!((w, r), (vec!["march".to_owned()], None));
        let (w, r) = run("in march");
        assert!(w.is_empty() && r.is_some());
        // Without a preposition or a year the word stays text, however many words surround it.
        assert_eq!(run("march madness").1, None);
        assert_eq!(run("taxes march").1, None);
        assert_eq!(run("may notes").1, None);
        assert!(run("notes during may").1.is_some());
    }

    #[test]
    fn ordinary_text_is_left_alone() {
        for q in [
            "annual report",
            "last",
            "this is the week",
            "past tense",
            "2024",
            "2018 plan",
            "2099 plan",
            "may the force",
            "opened last",
        ] {
            let (w, r) = run(q);
            assert_eq!(r, None, "{q}");
            assert_eq!(w.join(" "), q);
        }
    }

    #[test]
    fn plain_years_and_invalid_counts() {
        let (w, r) = run("invoices in 2024");
        assert_eq!(w, ["invoices"]);
        assert_eq!(r, Some((day(2024, 1, 1), day(2025, 1, 1))));
        assert_eq!(
            run("x from 2019").1,
            Some((day(2019, 1, 1), day(2020, 1, 1)))
        );
        assert_eq!(
            run_with("x 2026", true).1,
            Some((day(2026, 1, 1), day(2027, 1, 1)))
        );
        assert_eq!(run("x last 366 days").1, None);
        assert_eq!(run("x last 13 months").1, None);
        assert_eq!(run("x last 2 years").1, None);
        assert_eq!(run("x last -3 days").1, None);
        assert_eq!(
            run("x last 365 days").1.map(|r| r.1),
            Some(day(2026, 11, 19))
        );
        assert_eq!(
            run("x last 12 months").1.map(|r| r.1),
            Some(day(2026, 11, 19))
        );
    }

    #[test]
    fn leap_day_and_year_boundaries() {
        let utc = FixedOffset::east_opt(0).unwrap();
        let t = |y, m, d| Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap().timestamp() as u64;
        let ask = |now: DateTime<FixedOffset>, phrase: &str| {
            let mut w: Vec<String> = phrase.split(' ').map(str::to_owned).collect();
            let (r, _) = extract(&mut w, &now, false).unwrap();
            (r.from, r.to)
        };
        let leap = utc.with_ymd_and_hms(2028, 3, 1, 12, 0, 0).unwrap();
        assert_eq!(ask(leap, "yesterday"), (t(2028, 2, 29), t(2028, 3, 1)));
        assert_eq!(ask(leap, "in february"), (t(2028, 2, 1), t(2028, 3, 1)));
        // Early January (a Saturday): last year is the whole year before, last week
        // crosses the new year, and "december" is last December.
        let jan = utc.with_ymd_and_hms(2027, 1, 2, 12, 0, 0).unwrap();
        assert_eq!(ask(jan, "last year"), (t(2026, 1, 1), t(2027, 1, 1)));
        assert_eq!(ask(jan, "last week"), (t(2026, 12, 21), t(2026, 12, 28)));
        assert_eq!(ask(jan, "in december"), (t(2026, 12, 1), t(2027, 1, 1)));
    }

    #[test]
    fn the_phrase_is_returned_for_the_label() {
        let mut w: Vec<String> = "x from march 2025".split(' ').map(str::to_owned).collect();
        let (_, phrase) = extract(&mut w, &now(), false).unwrap();
        assert_eq!(phrase, "from march 2025");
        assert_eq!(w, ["x"]);
    }

    #[test]
    fn only_the_first_phrase_is_used() {
        let (w, r) = run("yesterday in march");
        assert_eq!(r, Some((day(2026, 11, 17), day(2026, 11, 18))));
        assert_eq!(w, ["in", "march"]);
    }
}
