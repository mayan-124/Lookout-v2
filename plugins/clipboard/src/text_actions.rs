//! Text actions on the latest clipboard text. Each one is a pure function; the
//! handler reads the clipboard, applies it and writes the result back.

use serde::de::IgnoredAny;

use crate::text_more as more;

/// (action id, label, words that find it when typing after `clip`)
pub const ACTIONS: &[(&str, &str, &str)] = &[
    (
        "txt_clean_url",
        "Clean URL (remove tracking)",
        "clean url link tracking utm",
    ),
    ("txt_upper", "UPPERCASE", "upper uppercase caps"),
    ("txt_lower", "lowercase", "lower lowercase"),
    ("txt_title", "Title Case", "title case"),
    (
        "txt_one_line",
        "Remove line breaks",
        "remove line breaks newline join",
    ),
    ("txt_trim", "Trim spaces", "trim spaces whitespace"),
    ("txt_json", "Pretty-print JSON", "json pretty format"),
    ("txt_count", "Count words", "count words"),
    (
        "txt_chars",
        "Count characters",
        "count characters chars length",
    ),
    ("txt_sort", "Sort lines", "sort lines alphabetical"),
    (
        "txt_dedupe",
        "Remove duplicate lines",
        "remove duplicate dedupe unique lines",
    ),
    ("txt_snake", "snake_case", "snake case underscore"),
    ("txt_camel", "camelCase", "camel case"),
    ("txt_url_enc", "URL-encode", "url encode percent"),
    ("txt_url_dec", "URL-decode", "url decode percent"),
    ("txt_b64_enc", "Base64 encode", "base64 encode"),
    ("txt_b64_dec", "Base64 decode", "base64 decode"),
    ("txt_links", "Extract all links", "extract all links urls"),
    (
        "txt_emails",
        "Extract all emails",
        "extract all emails addresses",
    ),
];

pub fn label(id: &str) -> Option<&'static str> {
    ACTIONS.iter().find(|a| a.0 == id).map(|a| a.1)
}

/// Actions whose search words contain `rest` (all of them for "text").
pub fn matching(rest: &str) -> Vec<(&'static str, &'static str)> {
    if rest.is_empty() {
        return Vec::new();
    }
    ACTIONS
        .iter()
        .filter(|a| rest == "text" || rest == "actions" || a.2.contains(rest))
        .map(|a| (a.0, a.1))
        .collect()
}

pub fn apply(id: &str, text: &str) -> Result<String, String> {
    match id {
        "txt_clean_url" => Ok(clean_urls(text)),
        "txt_upper" => Ok(text.to_uppercase()),
        "txt_lower" => Ok(text.to_lowercase()),
        "txt_title" => Ok(title_case(text)),
        "txt_one_line" => Ok(one_line(text)),
        "txt_trim" => Ok(trim_spaces(text)),
        "txt_json" => pretty_json(text),
        "txt_count" => {
            let n = text.split_whitespace().count();
            Ok(format!("{n} {}", if n == 1 { "word" } else { "words" }))
        }
        "txt_chars" => Ok(more::count_chars(text)),
        "txt_sort" => Ok(more::sort_lines(text)),
        "txt_dedupe" => Ok(more::dedupe_lines(text)),
        "txt_snake" => Ok(more::snake_case(text)),
        "txt_camel" => Ok(more::camel_case(text)),
        "txt_url_enc" => Ok(more::url_encode(text)),
        "txt_url_dec" => more::url_decode(text),
        "txt_b64_enc" => Ok(more::base64_encode(text.as_bytes())),
        "txt_b64_dec" => more::base64_decode(text),
        "txt_links" => more::links(text),
        "txt_emails" => more::emails(text),
        _ => Err("unknown action".to_owned()),
    }
}

fn is_tracking(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("utm_")
        || matches!(
            n.as_str(),
            "fbclid"
                | "gclid"
                | "dclid"
                | "gbraid"
                | "wbraid"
                | "msclkid"
                | "yclid"
                | "twclid"
                | "igshid"
                | "mc_cid"
                | "mc_eid"
                | "_hsenc"
                | "_hsmi"
                | "ref_src"
                | "mkt_tok"
                | "vero_id"
                | "oly_enc_id"
                | "oly_anon_id"
        )
}

fn clean_one(url: &str) -> String {
    let (rest, fragment) = match url.split_once('#') {
        Some((a, b)) => (a, Some(b)),
        None => (url, None),
    };
    let (base, query) = match rest.split_once('?') {
        Some((a, b)) => (a, Some(b)),
        None => (rest, None),
    };
    let mut out = base.to_owned();
    if let Some(q) = query {
        let kept: Vec<&str> = q
            .split('&')
            .filter(|p| !p.is_empty() && !is_tracking(p.split('=').next().unwrap_or("")))
            .collect();
        if !kept.is_empty() {
            out.push('?');
            out.push_str(&kept.join("&"));
        }
    }
    if let Some(f) = fragment {
        out.push('#');
        out.push_str(f);
    }
    out
}

/// Cleans every `http(s)://` word; other text and the spacing stay as they were.
fn clean_urls(text: &str) -> String {
    text.split_inclusive(char::is_whitespace)
        .map(|seg| {
            let word = seg.trim_end();
            if word.starts_with("http://") || word.starts_with("https://") {
                format!("{}{}", clean_one(word), &seg[word.len()..])
            } else {
                seg.to_owned()
            }
        })
        .collect()
}

fn title_case(text: &str) -> String {
    let mut start = true;
    text.chars()
        .map(|c| {
            let out: String = if c.is_alphanumeric() && start {
                c.to_uppercase().collect()
            } else {
                c.to_lowercase().collect()
            };
            start = c.is_whitespace() || c == '-' || c == '(' || c == '"';
            out
        })
        .collect()
}

fn one_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn trim_spaces(text: &str) -> String {
    text.lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

/// Re-indents valid JSON with two spaces, keeping key order and number text.
fn pretty_json(text: &str) -> Result<String, String> {
    serde_json::from_str::<IgnoredAny>(text)
        .map_err(|_| "The clipboard text is not valid JSON".to_owned())?;
    let chars: Vec<char> = text.trim().chars().collect();
    let mut out = String::new();
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    let newline = |out: &mut String, depth: usize| {
        out.push('\n');
        out.push_str(&"  ".repeat(depth));
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if in_str {
            out.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '{' | '[' => {
                let close = if c == '{' { '}' } else { ']' };
                let next = chars[i..].iter().position(|x| !x.is_whitespace());
                match next {
                    Some(n) if chars[i + n] == close => {
                        out.push(c);
                        out.push(close);
                        i += n + 1;
                    }
                    _ => {
                        depth += 1;
                        out.push(c);
                        newline(&mut out, depth);
                    }
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                newline(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(c);
                newline(&mut out, depth);
            }
            ':' => out.push_str(": "),
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_url_drops_tracking_and_keeps_the_rest() {
        let got = apply(
            "txt_clean_url",
            "see https://a.com/p?id=7&utm_source=x&fbclid=abc&gclid=1#top ok",
        )
        .unwrap();
        assert_eq!(got, "see https://a.com/p?id=7#top ok");
        assert_eq!(
            apply("txt_clean_url", "https://a.com/?utm_medium=m&fbclid=z").unwrap(),
            "https://a.com/"
        );
    }

    #[test]
    fn upper_and_lower() {
        assert_eq!(apply("txt_upper", "Hello world").unwrap(), "HELLO WORLD");
        assert_eq!(apply("txt_lower", "Hello WORLD").unwrap(), "hello world");
    }

    #[test]
    fn title_case_capitalises_each_word() {
        assert_eq!(
            apply("txt_title", "the qUICK brown-fox\njumps").unwrap(),
            "The Quick Brown-Fox\nJumps"
        );
    }

    #[test]
    fn remove_line_breaks_joins_lines_with_one_space() {
        assert_eq!(
            apply("txt_one_line", "one\r\n  two \n\nthree").unwrap(),
            "one two three"
        );
    }

    #[test]
    fn trim_spaces_cleans_each_line_but_keeps_lines() {
        assert_eq!(
            apply("txt_trim", "  a   b \t c  \n\n   d  ").unwrap(),
            "a b c\n\nd"
        );
    }

    #[test]
    fn pretty_json_indents_keeps_order_and_rejects_bad_json() {
        let got = apply("txt_json", r#"{"b":[1,2.50,{}],"a":"x, {y}"}"#).unwrap();
        assert_eq!(
            got,
            "{\n  \"b\": [\n    1,\n    2.50,\n    {}\n  ],\n  \"a\": \"x, {y}\"\n}"
        );
        assert!(apply("txt_json", "{not json").is_err());
    }

    #[test]
    fn count_words_writes_the_count() {
        assert_eq!(apply("txt_count", " a b\nc ").unwrap(), "3 words");
        assert_eq!(apply("txt_count", "solo").unwrap(), "1 word");
    }

    #[test]
    fn search_words_find_actions() {
        assert!(matching("").is_empty());
        assert_eq!(matching("json")[0].0, "txt_json");
        assert!(matching("first").is_empty());
    }
}
