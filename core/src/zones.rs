//! Time zone answers: `time in tokyo`, `5pm ist in london`. Cities map to
//! Windows time zones, whose daylight-saving rules Windows keeps; an
//! abbreviation means exactly its offset. No network.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Utc};

use crate::dates::Reply;

/// Windows time zone id, then its cities (the first spelling is shown).
#[rustfmt::skip]
const CITIES: &[(&str, &[&str])] = &[
    ("Hawaiian Standard Time", &["Honolulu"]),
    ("Alaskan Standard Time", &["Anchorage"]),
    ("Pacific Standard Time", &["Los Angeles", "San Francisco", "Seattle", "Vancouver", "Las Vegas", "San Diego", "Portland"]),
    ("US Mountain Standard Time", &["Phoenix"]),
    ("Mountain Standard Time", &["Denver", "Calgary", "Edmonton", "Salt Lake City"]),
    ("Central Standard Time", &["Chicago", "Dallas", "Houston", "Austin", "Minneapolis", "Winnipeg"]),
    ("Central Standard Time (Mexico)", &["Mexico City"]),
    ("Central America Standard Time", &["Guatemala City"]),
    ("Eastern Standard Time", &["New York", "NYC", "Washington", "Boston", "Miami", "Atlanta", "Philadelphia", "Detroit", "Toronto", "Montreal", "Ottawa"]),
    ("SA Pacific Standard Time", &["Bogota", "Lima", "Quito"]),
    ("Cuba Standard Time", &["Havana"]),
    ("Venezuela Standard Time", &["Caracas"]),
    ("Atlantic Standard Time", &["Halifax"]),
    ("Newfoundland Standard Time", &["St Johns", "St. John's"]),
    ("SA Western Standard Time", &["La Paz"]),
    ("Pacific SA Standard Time", &["Santiago"]),
    ("E. South America Standard Time", &["Sao Paulo", "São Paulo", "Rio de Janeiro", "Rio", "Brasilia"]),
    ("Argentina Standard Time", &["Buenos Aires"]),
    ("Montevideo Standard Time", &["Montevideo"]),
    ("GMT Standard Time", &["London", "Dublin", "Lisbon", "Edinburgh", "Manchester"]),
    ("Greenwich Standard Time", &["Reykjavik", "Accra", "Dakar", "Abidjan"]),
    ("Morocco Standard Time", &["Casablanca"]),
    ("W. Central Africa Standard Time", &["Lagos", "Kinshasa"]),
    ("W. Europe Standard Time", &["Berlin", "Amsterdam", "Rome", "Stockholm", "Vienna", "Zurich", "Geneva", "Oslo", "Frankfurt", "Munich"]),
    ("Romance Standard Time", &["Paris", "Madrid", "Barcelona", "Brussels", "Copenhagen"]),
    ("Central Europe Standard Time", &["Prague", "Budapest", "Belgrade"]),
    ("Central European Standard Time", &["Warsaw", "Zagreb"]),
    ("GTB Standard Time", &["Athens", "Bucharest"]),
    ("FLE Standard Time", &["Helsinki", "Kyiv", "Kiev", "Sofia", "Riga", "Tallinn", "Vilnius"]),
    ("Egypt Standard Time", &["Cairo"]),
    ("South Africa Standard Time", &["Johannesburg", "Cape Town", "Pretoria", "Harare"]),
    ("Israel Standard Time", &["Jerusalem", "Tel Aviv"]),
    ("Middle East Standard Time", &["Beirut"]),
    ("Jordan Standard Time", &["Amman"]),
    ("Turkey Standard Time", &["Istanbul", "Ankara"]),
    ("Russian Standard Time", &["Moscow", "St Petersburg"]),
    ("Belarus Standard Time", &["Minsk"]),
    ("E. Africa Standard Time", &["Nairobi", "Addis Ababa", "Dar es Salaam"]),
    ("Arab Standard Time", &["Riyadh", "Kuwait City"]),
    ("Arabic Standard Time", &["Baghdad"]),
    ("Iran Standard Time", &["Tehran"]),
    ("Arabian Standard Time", &["Dubai", "Abu Dhabi", "Muscat"]),
    ("Azerbaijan Standard Time", &["Baku"]),
    ("Georgian Standard Time", &["Tbilisi"]),
    ("Caucasus Standard Time", &["Yerevan"]),
    ("Afghanistan Standard Time", &["Kabul"]),
    ("Pakistan Standard Time", &["Karachi", "Islamabad", "Lahore"]),
    ("West Asia Standard Time", &["Tashkent"]),
    ("India Standard Time", &["Mumbai", "Delhi", "New Delhi", "Bangalore", "Bengaluru", "Chennai", "Kolkata", "Hyderabad", "Pune", "Ahmedabad"]),
    ("Sri Lanka Standard Time", &["Colombo"]),
    ("Nepal Standard Time", &["Kathmandu"]),
    ("Bangladesh Standard Time", &["Dhaka"]),
    ("Myanmar Standard Time", &["Yangon"]),
    ("SE Asia Standard Time", &["Bangkok", "Hanoi", "Ho Chi Minh City", "Saigon", "Jakarta"]),
    ("China Standard Time", &["Beijing", "Shanghai", "Hong Kong", "Shenzhen", "Guangzhou"]),
    ("Singapore Standard Time", &["Singapore", "Kuala Lumpur", "Manila"]),
    ("Taipei Standard Time", &["Taipei"]),
    ("W. Australia Standard Time", &["Perth"]),
    ("Ulaanbaatar Standard Time", &["Ulaanbaatar"]),
    ("Tokyo Standard Time", &["Tokyo", "Osaka", "Kyoto"]),
    ("Korea Standard Time", &["Seoul"]),
    ("Cen. Australia Standard Time", &["Adelaide"]),
    ("AUS Central Standard Time", &["Darwin"]),
    ("E. Australia Standard Time", &["Brisbane"]),
    ("AUS Eastern Standard Time", &["Sydney", "Melbourne", "Canberra"]),
    ("Tasmania Standard Time", &["Hobart"]),
    ("West Pacific Standard Time", &["Port Moresby"]),
    ("Vladivostok Standard Time", &["Vladivostok"]),
    ("Central Pacific Standard Time", &["Noumea"]),
    ("New Zealand Standard Time", &["Auckland", "Wellington"]),
    ("Fiji Standard Time", &["Suva"]),
    ("Samoa Standard Time", &["Apia"]),
];

struct Abbr {
    name: &'static str,
    /// Minutes east of UTC.
    offset: i32,
    /// A city that uses it for part or all of the year.
    city: Option<&'static str>,
    /// Said when the letters stand for more than one zone.
    reading: Option<&'static str>,
}

const fn abbr(name: &'static str, offset: i32, city: Option<&'static str>) -> Abbr {
    Abbr {
        name,
        offset,
        city,
        reading: None,
    }
}

const fn unclear(
    name: &'static str,
    offset: i32,
    city: Option<&'static str>,
    reading: &'static str,
) -> Abbr {
    Abbr {
        name,
        offset,
        city,
        reading: Some(reading),
    }
}

const ABBREVIATIONS: &[Abbr] = &[
    abbr("UTC", 0, None),
    abbr("GMT", 0, Some("London")),
    unclear("BST", 60, Some("London"), "BST read as British Summer Time"),
    abbr("CET", 60, Some("Paris")),
    abbr("CEST", 120, Some("Paris")),
    abbr("EET", 120, Some("Athens")),
    abbr("EEST", 180, Some("Athens")),
    abbr("MSK", 180, Some("Moscow")),
    unclear("GST", 240, Some("Dubai"), "GST read as Gulf Standard Time"),
    abbr("PKT", 300, Some("Karachi")),
    unclear("IST", 330, Some("Delhi"), "IST read as India Standard Time"),
    abbr("HKT", 480, Some("Hong Kong")),
    abbr("SGT", 480, Some("Singapore")),
    abbr("AWST", 480, Some("Perth")),
    abbr("JST", 540, Some("Tokyo")),
    abbr("KST", 540, Some("Seoul")),
    abbr("ACST", 570, Some("Adelaide")),
    abbr("AEST", 600, Some("Sydney")),
    abbr("AEDT", 660, Some("Sydney")),
    abbr("NZST", 720, Some("Auckland")),
    abbr("NZDT", 780, Some("Auckland")),
    abbr("EST", -300, Some("New York")),
    abbr("EDT", -240, Some("New York")),
    unclear(
        "CST",
        -360,
        Some("Chicago"),
        "CST read as US Central Standard Time",
    ),
    abbr("CDT", -300, Some("Chicago")),
    abbr("MST", -420, Some("Denver")),
    abbr("MDT", -360, Some("Denver")),
    abbr("PST", -480, Some("Los Angeles")),
    abbr("PDT", -420, Some("Los Angeles")),
    abbr("AKST", -540, Some("Anchorage")),
    abbr("HST", -600, Some("Honolulu")),
];

#[derive(Clone, Copy)]
enum Place {
    City {
        name: &'static str,
        zone: &'static str,
    },
    Fixed(&'static Abbr),
}

impl Place {
    fn name(self) -> &'static str {
        match self {
            Place::City { name, .. } => name,
            Place::Fixed(a) => a.name,
        }
    }
}

fn city(text: &str) -> Option<Place> {
    CITIES.iter().find_map(|(zone, names)| {
        names
            .iter()
            .find(|n| n.to_lowercase() == text)
            .map(|name| Place::City { name, zone })
    })
}

fn place(text: &str) -> Option<Place> {
    ABBREVIATIONS
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(text))
        .map(Place::Fixed)
        .or_else(|| city(text))
}

/// "5pm", "5 pm", "5:30pm", "17:00", "noon", "midnight". Returns how many
/// words it took.
fn clock(words: &[&str]) -> Option<(NaiveTime, usize)> {
    let first = *words.first()?;
    match first {
        "noon" => return Some((NaiveTime::from_hms_opt(12, 0, 0)?, 1)),
        "midnight" => return Some((NaiveTime::from_hms_opt(0, 0, 0)?, 1)),
        _ => {}
    }
    let halves = |w: &str| match w {
        "am" | "a.m." => Some(false),
        "pm" | "p.m." => Some(true),
        _ => None,
    };
    let (digits, mut pm, mut used) = match ["am", "pm"].iter().find_map(|s| first.strip_suffix(s)) {
        Some(d) => (d, Some(first.ends_with("pm")), 1),
        None => (first, None, 1),
    };
    if pm.is_none() {
        if let Some(half) = words.get(1).and_then(|w| halves(w)) {
            (pm, used) = (Some(half), 2);
        }
    }
    let (h, m) = match digits.split_once(':') {
        Some((h, m)) if m.len() == 2 => (h, Some(m)),
        Some(_) => return None,
        None => (digits, None),
    };
    let number = |s: &str| {
        (!s.is_empty() && s.len() <= 2 && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u32>().ok())
            .flatten()
    };
    let mut hour = number(h)?;
    let minute = match m {
        Some(m) => number(m)?,
        None => 0,
    };
    match pm {
        Some(pm) => {
            if !(1..=12).contains(&hour) {
                return None;
            }
            hour = hour % 12 + if pm { 12 } else { 0 };
        }
        // A bare "5" is not a time.
        None if m.is_none() => return None,
        None => {}
    }
    Some((NaiveTime::from_hms_opt(hour, minute, 0)?, used))
}

/// Minutes east of UTC in `place` at the instant `utc`.
fn offset(place: Place, utc: NaiveDateTime) -> Option<i32> {
    match place {
        Place::Fixed(a) => Some(a.offset),
        Place::City { zone, .. } => windows_zone::offset(zone, utc),
    }
}

fn to_local(place: Place, utc: NaiveDateTime) -> Option<NaiveDateTime> {
    utc.checked_add_signed(Duration::minutes(offset(place, utc)?.into()))
}

/// The instant a wall-clock time names, and a note when the clocks there skip
/// it or show it twice that day.
fn to_utc(place: Place, local: NaiveDateTime) -> Option<(NaiveDateTime, Option<String>)> {
    let shift = |t: NaiveDateTime, minutes: i32| t - Duration::minutes(minutes.into());
    // The offsets in force a day either side cover both sides of a change.
    let mut offsets = Vec::new();
    for days in [-1, 1] {
        let o = offset(place, local + Duration::days(days))?;
        if !offsets.contains(&o) {
            offsets.push(o);
        }
    }
    let real: Vec<NaiveDateTime> = offsets
        .iter()
        .map(|o| shift(local, *o))
        .filter(|utc| to_local(place, *utc) == Some(local))
        .collect();
    let at = local.format("%H:%M");
    match real.as_slice() {
        [one] => Some((*one, None)),
        [] => {
            // Read with the offset from before the jump: it lands after it.
            let note = format!(
                "{at} is skipped in {} that day (clocks go forward)",
                place.name()
            );
            Some((shift(local, *offsets.first()?), Some(note)))
        }
        many => {
            let note = format!(
                "{at} happens twice in {} that day, used the first",
                place.name()
            );
            Some((*many.iter().min()?, Some(note)))
        }
    }
}

fn utc_label(minutes: i32) -> String {
    if minutes == 0 {
        return "UTC+0".into();
    }
    let sign = if minutes < 0 { '−' } else { '+' };
    let (h, m) = (minutes.abs() / 60, minutes.abs() % 60);
    if m == 0 {
        format!("UTC{sign}{h}")
    } else {
        format!("UTC{sign}{h}:{m:02}")
    }
}

fn day(date: NaiveDate) -> String {
    format!("{} {} {}", date.format("%a"), date.day(), date.format("%b"))
}

fn time(t: NaiveDateTime) -> String {
    format!("{:02}:{:02}", t.hour(), t.minute())
}

/// What an abbreviation leaves open: its other meanings, and a city that is on
/// another offset at that moment.
fn remarks(place: Place, utc: NaiveDateTime, out: &mut Vec<String>) {
    let Place::Fixed(a) = place else {
        return;
    };
    out.extend(a.reading.map(str::to_owned));
    let Some(there) = a.city.and_then(|c| city(&c.to_lowercase())) else {
        return;
    };
    if let Some(now) = offset(there, utc).filter(|o| *o != a.offset) {
        out.push(format!(
            "{} taken as {}; {} is on {} then",
            a.name,
            utc_label(a.offset),
            there.name(),
            utc_label(now)
        ));
    }
}

enum Question {
    Now(String),
    At(NaiveTime, String, String),
}

fn question(input: &str) -> Option<Question> {
    if input.len() > 80 {
        return None;
    }
    let lower = input.trim().trim_end_matches('?').to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    if let ["time", "in", rest @ ..] = words.as_slice() {
        return (!rest.is_empty()).then(|| Question::Now(rest.join(" ")));
    }
    let (at, used) = clock(&words)?;
    let split = words.iter().position(|w| *w == "in")?;
    if split <= used || split + 1 >= words.len() {
        return None;
    }
    let from = words[used..split].join(" ");
    let to = words[split + 1..].join(" ");
    // With neither side known, this is a search for words.
    (place(&from).is_some() || place(&to).is_some()).then_some(Question::At(at, from, to))
}

/// A time zone question whose places are all known.
pub fn is_known(input: &str) -> bool {
    match question(input) {
        Some(Question::Now(text)) => place(&text).is_some(),
        Some(Question::At(_, from, to)) => place(&from).is_some() && place(&to).is_some(),
        None => false,
    }
}

fn unknown(text: &str) -> Reply {
    Reply::Problem(format!("Unknown city or time zone: {text}"))
}

/// The answer to `input`, or `None` when it is not a time zone question.
/// `here` is this PC's offset from UTC in minutes at `now`.
pub fn answer(input: &str, now: DateTime<Utc>, here: i32) -> Option<Reply> {
    let now = now.naive_utc();
    let fail = || Reply::Problem("Windows has no data for that time zone".into());
    Some(match question(input)? {
        Question::Now(text) => {
            let Some(there) = place(&text) else {
                return Some(unknown(&text));
            };
            let (Some(o), Some(local)) = (offset(there, now), to_local(there, now)) else {
                return Some(fail());
            };
            let diff = o - here;
            let apart = match diff.abs() {
                0 => "same time as here".to_owned(),
                d => {
                    let (h, m) = (d / 60, d % 60);
                    let span = match (h, m) {
                        (0, m) => format!("{m} min"),
                        (h, 0) => format!("{h} h"),
                        (h, m) => format!("{h} h {m} min"),
                    };
                    let side = if diff > 0 { "ahead of" } else { "behind" };
                    format!("{span} {side} you")
                }
            };
            let mut detail = vec![day(local.date()), utc_label(o), apart];
            remarks(there, now, &mut detail);
            Reply::Answer {
                answer: format!("{} in {}", time(local), there.name()),
                detail: detail.join(" · "),
            }
        }
        Question::At(at, from_text, to_text) => {
            let Some(from) = place(&from_text) else {
                return Some(unknown(&from_text));
            };
            let Some(to) = place(&to_text) else {
                return Some(unknown(&to_text));
            };
            // Today as it is where the time was named.
            let Some(today) = to_local(from, now).map(|t| t.date()) else {
                return Some(fail());
            };
            let source = today.and_time(at);
            let Some((utc, odd)) = to_utc(from, source) else {
                return Some(fail());
            };
            let (Some(local), Some(o)) = (to_local(to, utc), offset(to, utc)) else {
                return Some(fail());
            };
            let mut detail = vec![
                format!(
                    "{} {} in {} is {} {}",
                    day(today),
                    time(source),
                    from.name(),
                    day(local.date()),
                    time(local)
                ),
                utc_label(o),
            ];
            detail.extend(odd);
            remarks(from, utc, &mut detail);
            remarks(to, utc, &mut detail);
            let shifted = match local.date().signed_duration_since(today).num_days() {
                0 => "",
                d if d > 0 => " (next day)",
                _ => " (previous day)",
            };
            Reply::Answer {
                answer: format!("{} in {}{shifted}", time(local), to.name()),
                detail: detail.join(" · "),
            }
        }
    })
}

#[cfg(windows)]
mod windows_zone {
    use std::sync::{Mutex, PoisonError};

    use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::Time::{
        EnumDynamicTimeZoneInformation, SystemTimeToTzSpecificLocalTimeEx,
        DYNAMIC_TIME_ZONE_INFORMATION,
    };

    /// Zones already looked up: Windows lists them one at a time.
    static FOUND: Mutex<Vec<(String, DYNAMIC_TIME_ZONE_INFORMATION)>> = Mutex::new(Vec::new());

    fn cached(id: &str) -> Option<DYNAMIC_TIME_ZONE_INFORMATION> {
        let mut found = FOUND.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, info)) = found.iter().find(|(name, _)| name == id) {
            return Some(*info);
        }
        let info = find(id)?;
        found.push((id.to_owned(), info));
        Some(info)
    }

    fn find(id: &str) -> Option<DYNAMIC_TIME_ZONE_INFORMATION> {
        let wanted: Vec<u16> = id.encode_utf16().collect();
        for index in 0.. {
            // SAFETY: the struct is plain data that the call fills.
            let mut info: DYNAMIC_TIME_ZONE_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: `info` is a valid, writable struct.
            if unsafe { EnumDynamicTimeZoneInformation(index, &mut info) } != 0 {
                return None;
            }
            let name = info.TimeZoneKeyName;
            let len = name.iter().position(|c| *c == 0).unwrap_or(name.len());
            if name[..len] == wanted[..] {
                return Some(info);
            }
        }
        None
    }

    /// Minutes east of UTC in the zone at `utc`, by Windows' rules for that year.
    pub fn offset(id: &str, utc: NaiveDateTime) -> Option<i32> {
        let info = cached(id)?;
        let from = SYSTEMTIME {
            wYear: u16::try_from(utc.year()).ok()?,
            wMonth: utc.month() as u16,
            wDayOfWeek: utc.weekday().num_days_from_sunday() as u16,
            wDay: utc.day() as u16,
            wHour: utc.hour() as u16,
            wMinute: utc.minute() as u16,
            wSecond: utc.second() as u16,
            wMilliseconds: 0,
        };
        // SAFETY: plain data that the call fills.
        let mut to: SYSTEMTIME = unsafe { std::mem::zeroed() };
        // SAFETY: all three pointers are valid for the call.
        if unsafe { SystemTimeToTzSpecificLocalTimeEx(&info, &from, &mut to) } == 0 {
            return None;
        }
        let local = NaiveDate::from_ymd_opt(to.wYear.into(), to.wMonth.into(), to.wDay.into())?
            .and_hms_opt(to.wHour.into(), to.wMinute.into(), to.wSecond.into())?;
        i32::try_from(
            local
                .signed_duration_since(utc.with_nanosecond(0)?)
                .num_minutes(),
        )
        .ok()
    }
}

#[cfg(not(windows))]
mod windows_zone {
    pub fn offset(_: &str, _: chrono::NaiveDateTime) -> Option<i32> {
        None
    }
}

#[cfg(all(test, windows))]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn at(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, 0, 0)
            .single()
            .unwrap_or_default()
    }

    /// 26 November 2026, 10:00 UTC, asked from India.
    fn ask(text: &str) -> Option<Reply> {
        answer(text, at(2026, 11, 26, 10), 330)
    }

    fn got(text: &str, now: DateTime<Utc>) -> (String, String) {
        match answer(text, now, 330) {
            Some(Reply::Answer { answer, detail }) => (answer, detail),
            other => panic!("{text}: {other:?}"),
        }
    }

    #[test]
    fn every_city_and_abbreviation_resolves() {
        let now = at(2026, 6, 1, 0).naive_utc();
        let mut names = 0;
        for (zone, cities) in CITIES {
            assert!(windows_zone::offset(zone, now).is_some(), "{zone}");
            for c in *cities {
                assert!(place(&c.to_lowercase()).is_some(), "{c}");
                names += 1;
            }
        }
        assert!(names >= 100, "{names}");
        for a in ABBREVIATIONS {
            let c = a.city.map(str::to_lowercase);
            assert!(c.is_none_or(|c| city(&c).is_some()), "{}", a.name);
        }
    }

    #[test]
    fn the_time_now_elsewhere() {
        let now = at(2026, 11, 26, 10);
        let (a, d) = got("time in Tokyo", now);
        assert_eq!(a, "19:00 in Tokyo");
        assert_eq!(d, "Thu 26 Nov · UTC+9 · 3 h 30 min ahead of you");
        assert_eq!(got("time in new york?", now).0, "05:00 in New York");
        assert_eq!(got("time in mumbai", now).0, "15:30 in Mumbai");
        assert!(got("time in mumbai", now).1.contains("same time as here"));
    }

    #[test]
    fn daylight_saving_follows_each_change() {
        // New York: forward on 8 March 2026 at 07:00 UTC, back on 1 November at 06:00.
        assert_eq!(
            got("time in new york", at(2026, 3, 8, 6)).0,
            "01:00 in New York"
        );
        assert_eq!(
            got("time in new york", at(2026, 3, 8, 7)).0,
            "03:00 in New York"
        );
        assert_eq!(
            got("time in new york", at(2026, 11, 1, 5)).0,
            "01:00 in New York"
        );
        assert_eq!(
            got("time in new york", at(2026, 11, 1, 6)).0,
            "01:00 in New York"
        );
        // London: 29 March and 25 October 2026, both at 01:00 UTC.
        assert_eq!(
            got("time in london", at(2026, 3, 29, 0)).0,
            "00:00 in London"
        );
        assert_eq!(
            got("time in london", at(2026, 3, 29, 1)).0,
            "02:00 in London"
        );
        assert_eq!(
            got("time in london", at(2026, 10, 25, 1)).0,
            "01:00 in London"
        );
        // The south is the other way round.
        assert!(got("time in sydney", at(2026, 1, 15, 0))
            .1
            .contains("UTC+11"));
        assert!(got("time in sydney", at(2026, 7, 15, 0))
            .1
            .contains("UTC+10"));
        // The same wall time converts differently either side of a change.
        let ist = |m, d| got("5pm ist in london", at(2026, m, d, 6)).0;
        assert_eq!(ist(3, 28), "11:30 in London");
        assert_eq!(ist(3, 30), "12:30 in London");
        assert_eq!(ist(10, 26), "11:30 in London");
    }

    #[test]
    fn a_skipped_or_doubled_time_is_said() {
        let (a, d) = got("2:30am new york in utc", at(2026, 3, 8, 12));
        assert_eq!(a, "07:30 in UTC");
        assert!(d.contains("is skipped in New York"), "{d}");
        let (a, d) = got("1:30am new york in utc", at(2026, 11, 1, 12));
        assert_eq!(a, "05:30 in UTC");
        assert!(d.contains("happens twice in New York"), "{d}");
    }

    #[test]
    fn conversions_say_what_was_assumed() {
        let (a, d) = got("5pm ist in london", at(2026, 11, 26, 10));
        assert_eq!(a, "11:30 in London");
        assert!(d.contains("IST read as India Standard Time"), "{d}");
        // A standard abbreviation in summer keeps its offset, and says so.
        let (a, d) = got("noon est in utc", at(2026, 7, 1, 12));
        assert_eq!(a, "17:00 in UTC");
        assert!(d.contains("New York is on UTC−4"), "{d}");
        let (a, _) = got("11:00 pm london in tokyo", at(2026, 11, 26, 10));
        assert_eq!(a, "08:00 in Tokyo (next day)");
        assert_eq!(
            got("17:00 jst in gmt", at(2026, 11, 26, 10)).0,
            "08:00 in GMT"
        );
    }

    #[test]
    fn unknown_places_are_not_guessed() {
        for text in [
            "time in gotham",
            "5pm ist in gotham",
            "5pm gotham in london",
        ] {
            assert!(
                matches!(ask(text), Some(Reply::Problem(why)) if why.contains("gotham")),
                "{text}"
            );
        }
        for text in [
            "time",
            "time in",
            "timer 5 min",
            "5 km in miles",
            "5pm meeting in notes",
            "report in london",
            "5 ist in london",
            "10 km in miles",
            "25pm ist in london",
        ] {
            assert_eq!(ask(text), None, "{text}");
            assert!(!is_known(text), "{text}");
        }
    }
}
