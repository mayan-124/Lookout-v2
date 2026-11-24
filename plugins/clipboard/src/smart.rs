//! Guesses what a clip is (link, email, color, ...) so it can get a label and
//! one fitting extra action.

use serde::de::IgnoredAny;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Url,
    Email,
    Phone,
    Color,
    Json,
    Code,
    Text,
}

impl Kind {
    pub fn label(self) -> Option<&'static str> {
        match self {
            Kind::Url => Some("Link"),
            Kind::Email => Some("Email"),
            Kind::Phone => Some("Phone"),
            Kind::Color => Some("Color"),
            Kind::Json => Some("JSON"),
            Kind::Code => Some("Code"),
            Kind::Text => None,
        }
    }
}

pub fn is_url(t: &str) -> bool {
    let lower = t.to_ascii_lowercase();
    !t.contains(char::is_whitespace)
        && (lower.starts_with("http://") || lower.starts_with("https://"))
        && t.len() > 8
}

pub fn is_email(t: &str) -> bool {
    if t.contains(char::is_whitespace) || t.matches('@').count() != 1 {
        return false;
    }
    let Some((local, domain)) = t.split_once('@') else {
        return false;
    };
    let ok = |c: char| c.is_alphanumeric() || "._%+-".contains(c);
    !local.is_empty()
        && local.chars().all(ok)
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && domain
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-')
}

fn is_phone(t: &str) -> bool {
    let digits = t.chars().filter(char::is_ascii_digit).count();
    (7..=15).contains(&digits)
        && t.chars()
            .all(|c| c.is_ascii_digit() || " +-().".contains(c))
        && (t.starts_with('+') || t.contains([' ', '-', '(', '.']))
}

/// `#rgb`, `#rrggbb`, `#rrggbbaa` or `rgb(r, g, b)`.
pub fn parse_color(t: &str) -> Option<(u8, u8, u8)> {
    let t = t.trim();
    if let Some(hex) = t.strip_prefix('#') {
        if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let v = |s: &str| u8::from_str_radix(s, 16).ok();
        return match hex.len() {
            3 => {
                let d = |i: usize| v(&hex[i..=i].repeat(2));
                Some((d(0)?, d(1)?, d(2)?))
            }
            6 | 8 => Some((v(&hex[0..2])?, v(&hex[2..4])?, v(&hex[4..6])?)),
            _ => None,
        };
    }
    let inner = t
        .to_ascii_lowercase()
        .strip_prefix("rgb(")?
        .strip_suffix(')')?
        .to_owned();
    let parts: Vec<u8> = inner
        .split(',')
        .map(|p| p.trim().parse().ok())
        .collect::<Option<_>>()?;
    match parts[..] {
        [r, g, b] => Some((r, g, b)),
        _ => None,
    }
}

fn is_json(t: &str) -> bool {
    (t.starts_with('{') || t.starts_with('[')) && serde_json::from_str::<IgnoredAny>(t).is_ok()
}

fn is_code(t: &str) -> bool {
    const MARKERS: &[&str] = &[
        "fn ",
        "def ",
        "function ",
        "import ",
        "#include",
        "=>",
        "class ",
        "return ",
        "();",
        "){",
        ") {",
        "};",
        "let ",
        "const ",
        "var ",
        " == ",
        " != ",
        "println",
        "console.",
    ];
    MARKERS.iter().filter(|m| t.contains(*m)).count() >= 2
}

pub fn detect(text: &str) -> Kind {
    let t = text.trim();
    if is_url(t) {
        Kind::Url
    } else if is_email(t) {
        Kind::Email
    } else if parse_color(t).is_some() {
        Kind::Color
    } else if is_phone(t) {
        Kind::Phone
    } else if is_json(t) {
        Kind::Json
    } else if is_code(t) {
        Kind::Code
    } else {
        Kind::Text
    }
}

/// "#ff8800 = rgb(255, 136, 0) = hsl(32, 100%, 50%)".
pub fn describe_color(text: &str) -> Option<String> {
    let (r, g, b) = parse_color(text)?;
    let (rf, gf, bf) = (
        f64::from(r) / 255.0,
        f64::from(g) / 255.0,
        f64::from(b) / 255.0,
    );
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let l = (max + min) / 2.0;
    let d = max - min;
    let (h, s) = if d == 0.0 {
        (0.0, 0.0)
    } else {
        let s = d / (1.0 - (2.0 * l - 1.0).abs());
        let h = if max == rf {
            ((gf - bf) / d).rem_euclid(6.0)
        } else if max == gf {
            (bf - rf) / d + 2.0
        } else {
            (rf - gf) / d + 4.0
        };
        (h * 60.0, s)
    };
    Some(format!(
        "#{r:02x}{g:02x}{b:02x} = rgb({r}, {g}, {b}) = hsl({:.0}, {:.0}%, {:.0}%)",
        h,
        s * 100.0,
        l * 100.0
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_type_is_recognised() {
        assert_eq!(detect("https://example.com/a?b=1"), Kind::Url);
        assert_eq!(detect("  me@example.co.uk "), Kind::Email);
        assert_eq!(detect("+91 98765 43210"), Kind::Phone);
        assert_eq!(detect("(555) 123-4567"), Kind::Phone);
        assert_eq!(detect("#ff8800"), Kind::Color);
        assert_eq!(detect("rgb(1, 2, 3)"), Kind::Color);
        assert_eq!(detect(r#"{"a": [1, 2]}"#), Kind::Json);
        assert_eq!(detect("fn main() {\n    return 1;\n}"), Kind::Code);
    }

    #[test]
    fn ordinary_text_is_not_mistaken_for_anything() {
        for t in [
            "hello world",
            "see https://a.com now",
            "a@b",
            "1234567",
            "{not json",
            "#12",
            "meeting at 5",
        ] {
            assert_eq!(detect(t), Kind::Text, "{t}");
        }
    }

    #[test]
    fn colors_are_described() {
        assert_eq!(parse_color("#f80"), Some((255, 136, 0)));
        assert_eq!(parse_color("#ff8800ff"), Some((255, 136, 0)));
        assert_eq!(
            describe_color("#ff8800").as_deref(),
            Some("#ff8800 = rgb(255, 136, 0) = hsl(32, 100%, 50%)")
        );
        assert!(describe_color("#gg0000").is_none());
    }
}
