//! Quick actions, the pure part: parsing typed web addresses and network
//! paths, and matching typed words to the fixed list of system entries.
//! Nothing here touches the network, the registry or the disk.

/// Longest address accepted.
const MAX_URL: usize = 2048;

/// Top-level domains accepted when no scheme is typed. Deliberately short:
/// file names such as `main.rs`, `notes.md` or `a.zip` must not look like sites.
const TLDS: &[&str] = &[
    "com", "org", "net", "io", "dev", "app", "ai", "co", "edu", "gov", "info", "biz", "me", "tv",
    "us", "uk", "de", "fr", "jp", "cn", "in", "ru", "br", "au", "ca", "nl", "se", "es", "it", "ch",
    "eu", "xyz", "online", "site", "tech", "cloud", "page",
];

/// Removes control characters (they can hide or split what is shown and run).
pub fn strip_controls(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// A web address that may be opened: always `http` or `https`, host in
/// lower case, no whitespace, no user-info.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebUrl {
    pub url: String,
}

impl WebUrl {
    /// The address without path, query or fragment: safe to write to a log.
    pub fn redacted(&self) -> String {
        redact(&self.url)
    }
}

/// `scheme://host[:port]` only.
pub fn redact(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            format!("{scheme}://{}", &rest[..end])
        }
        None => "(address)".to_owned(),
    }
}

fn valid_label(l: &str) -> bool {
    !l.is_empty()
        && l.len() <= 63
        && !l.starts_with('-')
        && !l.ends_with('-')
        && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn is_ipv4(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 3
                && p.bytes().all(|b| b.is_ascii_digit())
                && p.parse::<u16>().is_ok_and(|n| n <= 255)
        })
}

/// Reads typed text as a web address. `https://` is added when no scheme is
/// typed (`http://` for localhost and IP addresses, which rarely have TLS).
/// Anything with another scheme (`javascript:`, `file:`, `data:`, `ftp:`...)
/// is refused.
pub fn parse_web_url(text: &str) -> Option<WebUrl> {
    let t = strip_controls(text);
    let t = t.trim();
    if t.is_empty() || t.len() > MAX_URL || t.chars().any(char::is_whitespace) {
        return None;
    }
    let (explicit, rest) = match t.split_once("://") {
        Some((scheme, rest)) => {
            let scheme = scheme.to_ascii_lowercase();
            if scheme != "http" && scheme != "https" {
                return None;
            }
            (Some(scheme), rest)
        }
        None => (None, t),
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    if authority.is_empty() || authority.contains(['@', '[', ']', '\\']) {
        return None;
    }
    if tail.contains(['"', '<', '>', '\\', '`']) {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => {
            if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let n: u32 = p.parse().ok()?;
            if n == 0 || n > 65535 {
                return None;
            }
            (h, Some(p))
        }
        None => (authority, None),
    };
    let host = host.to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').collect();
    if !labels.iter().all(|l| valid_label(l)) {
        return None;
    }
    let last = labels.last().copied()?;
    let local = host == "localhost" || host.ends_with(".localhost");
    let ip = is_ipv4(&host);
    if !local && !ip {
        // `3.14` or `1.2.3` are numbers, not names.
        if last.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        // Without a scheme only a known top-level domain counts.
        if explicit.is_none() && (labels.len() < 2 || !TLDS.contains(&last)) {
            return None;
        }
    }
    let scheme = explicit.unwrap_or_else(|| if local || ip { "http" } else { "https" }.to_owned());
    let port = port.map(|p| format!(":{p}")).unwrap_or_default();
    Some(WebUrl {
        url: format!("{scheme}://{host}{port}{tail}"),
    })
}

/// Reads typed text as a network path (`\\server\share\folder`). Device
/// paths (`\\?\`, `\\.\`) and anything with wildcard or stream characters
/// are refused. Returns the path with backslashes. Never touches the network.
pub fn parse_unc(text: &str) -> Option<String> {
    let t = strip_controls(text);
    let t = t.trim().trim_matches('"').replace('/', "\\");
    let rest = t.strip_prefix("\\\\")?;
    if t.len() > 1024 || rest.is_empty() || rest.starts_with(['?', '.', '\\']) {
        return None;
    }
    if rest.contains(['<', '>', '"', '|', '?', '*', ':']) {
        return None;
    }
    let parts: Vec<&str> = rest.trim_end_matches('\\').split('\\').collect();
    if parts
        .iter()
        .any(|p| p.is_empty() || *p == ".." || *p == ".")
    {
        return None;
    }
    Some(format!("\\\\{}", parts.join("\\")))
}

/// What a system entry does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    /// Switch between dark and light mode (registry, undoable).
    Theme,
    /// Turn the Wi-Fi or Bluetooth radio on or off.
    Radio(RadioKind),
    /// Open this fixed Settings page.
    Page(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadioKind {
    Wifi,
    Bluetooth,
}

impl RadioKind {
    pub fn id(self) -> &'static str {
        match self {
            RadioKind::Wifi => "wifi",
            RadioKind::Bluetooth => "bluetooth",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            RadioKind::Wifi => "Wi-Fi",
            RadioKind::Bluetooth => "Bluetooth",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "wifi" => Some(RadioKind::Wifi),
            "bluetooth" => Some(RadioKind::Bluetooth),
            _ => None,
        }
    }
}

pub struct Entry {
    pub id: &'static str,
    pub title: &'static str,
    pub subtitle: &'static str,
    pub keywords: &'static [&'static str],
    pub what: What,
}

/// The whole list. Settings pages are fixed here and nowhere else: no other
/// `ms-settings:` address can be opened.
pub const ENTRIES: &[Entry] = &[
    Entry {
        id: "dark_mode",
        title: "Switch dark / light mode",
        subtitle: "Changes Windows and app colors",
        keywords: &[
            "dark mode",
            "light mode",
            "dark theme",
            "light theme",
            "dark",
            "theme",
        ],
        what: What::Theme,
    },
    Entry {
        id: "wifi",
        title: "Turn Wi-Fi on or off",
        subtitle: "Asks first, can be undone",
        keywords: &["wifi", "wi-fi", "wi fi", "wireless", "wlan"],
        what: What::Radio(RadioKind::Wifi),
    },
    Entry {
        id: "bluetooth",
        title: "Turn Bluetooth on or off",
        subtitle: "Asks first, can be undone",
        keywords: &["bluetooth"],
        what: What::Radio(RadioKind::Bluetooth),
    },
    Entry {
        id: "wifi_page",
        title: "Open Wi-Fi settings",
        subtitle: "Network & internet",
        keywords: &["wifi", "wi-fi", "wi fi", "wireless", "wlan"],
        what: What::Page("ms-settings:network-wifi"),
    },
    Entry {
        id: "bluetooth_page",
        title: "Open Bluetooth settings",
        subtitle: "Bluetooth & devices",
        keywords: &["bluetooth"],
        what: What::Page("ms-settings:bluetooth"),
    },
    Entry {
        id: "night_light",
        title: "Open Night light settings",
        subtitle: "System > Display",
        keywords: &["night light", "nightlight", "blue light"],
        what: What::Page("ms-settings:nightlight"),
    },
    Entry {
        id: "focus",
        title: "Open Do not disturb settings",
        subtitle: "System > Notifications",
        keywords: &[
            "do not disturb",
            "dnd",
            "focus",
            "focus assist",
            "quiet hours",
        ],
        what: What::Page("ms-settings:notifications"),
    },
    Entry {
        id: "airplane",
        title: "Open Airplane mode settings",
        subtitle: "Network & internet",
        keywords: &["airplane mode", "airplane", "aeroplane mode", "flight mode"],
        what: What::Page("ms-settings:network-airplanemode"),
    },
    Entry {
        id: "sound",
        title: "Open Sound settings",
        subtitle: "System",
        keywords: &["sound", "audio", "volume"],
        what: What::Page("ms-settings:sound"),
    },
    Entry {
        id: "display",
        title: "Open Display settings",
        subtitle: "System",
        keywords: &["display", "brightness", "resolution", "screen resolution"],
        what: What::Page("ms-settings:display"),
    },
    Entry {
        id: "battery",
        title: "Open Battery settings",
        subtitle: "System > Power & battery",
        keywords: &["battery", "battery saver", "power"],
        what: What::Page("ms-settings:batterysaver"),
    },
    Entry {
        id: "storage",
        title: "Open Storage settings",
        subtitle: "System",
        keywords: &["storage", "storage sense", "disk space"],
        what: What::Page("ms-settings:storagesense"),
    },
    Entry {
        id: "apps",
        title: "Open Installed apps settings",
        subtitle: "Apps",
        keywords: &["apps", "installed apps", "apps and features", "uninstall"],
        what: What::Page("ms-settings:appsfeatures"),
    },
    Entry {
        id: "update",
        title: "Open Windows Update",
        subtitle: "Windows Update settings",
        keywords: &["windows update", "update", "updates"],
        what: What::Page("ms-settings:windowsupdate"),
    },
    Entry {
        id: "settings",
        title: "Open Windows Settings",
        subtitle: "Settings home",
        keywords: &["settings", "windows settings", "system settings"],
        what: What::Page("ms-settings:"),
    },
];

pub fn entry(id: &str) -> Option<&'static Entry> {
    ENTRIES.iter().find(|e| e.id == id)
}

/// Scores for a match: a whole keyword, or the start of one.
pub const EXACT: f32 = 1.2;
pub const PREFIX: f32 = 0.6;
/// Most system results one query returns, so they never crowd out files and apps.
pub const MAX_SYSTEM: usize = 3;

const LEADING: &[&str] = &[
    "toggle ",
    "turn on ",
    "turn off ",
    "turn ",
    "switch to ",
    "switch ",
    "enable ",
    "disable ",
    "open ",
];

/// Entries that a typed word or phrase refers to, best first, at most [`MAX_SYSTEM`].
pub fn match_entries(typed: &str) -> Vec<(&'static Entry, f32)> {
    let lower = strip_controls(typed).to_lowercase();
    let mut q = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    for p in LEADING {
        if let Some(r) = q.strip_prefix(p) {
            if !r.is_empty() {
                q = r.to_owned();
                break;
            }
        }
    }
    for s in [" settings", " setting"] {
        if let Some(r) = q.strip_suffix(s) {
            if !r.is_empty() {
                q = r.to_owned();
                break;
            }
        }
    }
    if q.len() < 3 {
        return Vec::new();
    }
    let mut out: Vec<(&'static Entry, f32)> = ENTRIES
        .iter()
        .filter_map(|e| {
            let best = e
                .keywords
                .iter()
                .map(|k| {
                    if *k == q {
                        EXACT
                    } else if k.starts_with(q.as_str()) {
                        PREFIX
                    } else {
                        0.0
                    }
                })
                .fold(0.0, f32::max);
            (best > 0.0).then_some((e, best))
        })
        .collect();
    // Stable: ties keep the order of the list (toggles before pages).
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out.truncate(MAX_SYSTEM);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Option<String> {
        parse_web_url(s).map(|u| u.url)
    }

    #[test]
    fn adds_https_and_accepts_plain_addresses() {
        assert_eq!(url("apple.com").as_deref(), Some("https://apple.com"));
        assert_eq!(
            url("Apple.COM/Path?q=1").as_deref(),
            Some("https://apple.com/Path?q=1")
        );
        assert_eq!(url("https://x.com/y").as_deref(), Some("https://x.com/y"));
        assert_eq!(url("HTTP://x.com").as_deref(), Some("http://x.com"));
        assert_eq!(
            url("localhost:3000").as_deref(),
            Some("http://localhost:3000")
        );
        assert_eq!(
            url("192.168.1.1:8080/a").as_deref(),
            Some("http://192.168.1.1:8080/a")
        );
        assert_eq!(
            url("http://intranet/wiki").as_deref(),
            Some("http://intranet/wiki")
        );
    }

    #[test]
    fn refuses_every_other_scheme() {
        for bad in [
            "javascript:alert(1)",
            "javascript://%0aalert(1)",
            "file:///C:/Windows/System32/calc.exe",
            "file://server/share/x.exe",
            "data:text/html,<script>1</script>",
            "ftp://example.com",
            "mailto:a@b.com",
            "ms-settings:display",
            "vbscript:msgbox(1)",
            "search-ms:query=x",
            "calc.exe",
        ] {
            assert_eq!(url(bad), None, "{bad}");
        }
    }

    #[test]
    fn refuses_things_that_are_not_addresses() {
        for bad in [
            "report.pdf",
            "main.rs",
            "notes.txt",
            "hello world.com",
            "two words",
            "3.14",
            "http://",
            "https://user:pw@evil.com",
            "https://google.com@evil.com",
            "a.com:99999",
            "a.com:0",
            "https://exa mple.com",
            "https://a.com/\"onmouseover",
            "-bad-.com",
            "word",
            "",
        ] {
            assert_eq!(url(bad), None, "{bad}");
        }
    }

    #[test]
    fn control_characters_are_removed_before_judging() {
        assert_eq!(url("apple\u{0}.com").as_deref(), Some("https://apple.com"));
        assert_eq!(url("java\nscript:alert(1)"), None);
        assert_eq!(
            url("https://a.com/x\r\n").as_deref(),
            Some("https://a.com/x")
        );
    }

    #[test]
    fn redaction_keeps_no_path_or_query() {
        let u = parse_web_url("https://x.com/secret/path?token=abc#frag").unwrap();
        assert_eq!(u.redacted(), "https://x.com");
        assert_eq!(redact("https://x.com:81?a=b"), "https://x.com:81");
    }

    #[test]
    fn unc_paths_are_accepted_and_device_paths_refused() {
        assert_eq!(
            parse_unc(r"\\server\share\folder").as_deref(),
            Some(r"\\server\share\folder")
        );
        assert_eq!(
            parse_unc("//server/share/").as_deref(),
            Some(r"\\server\share")
        );
        assert_eq!(parse_unc(r"\\server").as_deref(), Some(r"\\server"));
        for bad in [
            r"\\?\C:\Windows",
            r"\\.\pipe\x",
            r"\\",
            r"\\\share",
            r"\\server\sh*re",
            r"\\server\share\..\x",
            r"\\server\share:stream",
            r"C:\Windows",
            "plain",
        ] {
            assert_eq!(parse_unc(bad), None, "{bad}");
        }
        assert_eq!(
            parse_unc("\\\\ser\u{7}ver\\share").as_deref(),
            Some(r"\\server\share")
        );
    }

    fn ids(q: &str) -> Vec<&'static str> {
        match_entries(q).into_iter().map(|(e, _)| e.id).collect()
    }

    #[test]
    fn typed_words_find_system_entries() {
        assert_eq!(ids("dark mode"), ["dark_mode"]);
        assert_eq!(ids("Dark  Mode"), ["dark_mode"]);
        assert_eq!(ids("wifi"), ["wifi", "wifi_page"]);
        assert_eq!(ids("bluetooth"), ["bluetooth", "bluetooth_page"]);
        assert_eq!(ids("turn off wifi"), ["wifi", "wifi_page"]);
        assert_eq!(ids("night light"), ["night_light"]);
        assert_eq!(ids("do not disturb"), ["focus"]);
        assert_eq!(ids("airplane mode"), ["airplane"]);
        assert_eq!(ids("display settings"), ["display"]);
        assert_eq!(ids("windows update"), ["update"]);
        assert_eq!(ids("settings"), ["settings"]);
        assert_eq!(ids("blue"), ["bluetooth", "bluetooth_page", "night_light"]);
    }

    #[test]
    fn short_or_unrelated_text_matches_nothing() {
        assert!(ids("wi").is_empty());
        assert!(ids("report").is_empty());
        assert!(ids("").is_empty());
        assert!(ids("darkroom photos").is_empty());
        assert!(match_entries("a").is_empty());
    }

    #[test]
    fn a_whole_keyword_outranks_a_prefix_and_the_count_is_capped() {
        let m = match_entries("dark");
        assert_eq!(m[0].1, EXACT);
        assert!(match_entries("wifi").len() <= MAX_SYSTEM);
        assert!(match_entries("blue")[0].1 < EXACT);
    }

    #[test]
    fn every_settings_page_is_a_fixed_ms_settings_address() {
        for e in ENTRIES {
            if let What::Page(p) = e.what {
                assert!(p.starts_with("ms-settings:"), "{p}");
                assert!(!p.contains(['/', '\\', ' ']), "{p}");
            }
        }
    }
}
