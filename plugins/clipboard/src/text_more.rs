//! The newer text actions: lines, case, encodings, extraction. Pure functions
//! like the ones in `text_actions`.

use crate::smart;

pub fn sort_lines(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    lines.sort_by_key(|l| (l.to_lowercase(), *l));
    lines.join("\n")
}

pub fn dedupe_lines(text: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    text.lines()
        .filter(|l| seen.insert(*l))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn count_chars(text: &str) -> String {
    let n = text.chars().count();
    format!("{n} {}", if n == 1 { "character" } else { "characters" })
}

/// Splits a line into words at non-alphanumerics and at lower-to-UPPER changes.
fn words(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let prev = i.checked_sub(1).map(|j| chars[j]);
        let next = chars.get(i + 1).copied();
        let boundary = c.is_uppercase()
            && prev.is_some_and(|p| {
                p.is_lowercase()
                    || p.is_ascii_digit()
                    || (p.is_uppercase() && next.is_some_and(char::is_lowercase))
            });
        if boundary && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn per_line(text: &str, f: impl Fn(&[String]) -> String) -> String {
    text.lines()
        .map(|l| f(&words(l)))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn snake_case(text: &str) -> String {
    per_line(text, |w| w.join("_").to_lowercase())
}

pub fn camel_case(text: &str) -> String {
    per_line(text, |w| {
        w.iter()
            .enumerate()
            .map(|(i, word)| {
                let lower = word.to_lowercase();
                let mut c = lower.chars();
                match (i, c.next()) {
                    (0, _) | (_, None) => lower,
                    (_, Some(f)) => f.to_uppercase().chain(c).collect(),
                }
            })
            .collect()
    })
}

pub fn url_encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn url_decode(text: &str) -> Result<String, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(v)) => {
                out.push(v);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| "The decoded text is not valid UTF-8".to_owned())
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, &b)| acc | (u32::from(b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(B64[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn base64_decode(text: &str) -> Result<String, String> {
    let bad = || "The clipboard text is not valid Base64".to_owned();
    let clean: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let body = clean
        .strip_suffix(b"==")
        .or_else(|| clean.strip_suffix(b"="))
        .unwrap_or(&clean);
    if body.is_empty() || body.len() % 4 == 1 {
        return Err(bad());
    }
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    for chunk in body.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            let v = B64.iter().position(|&x| x == c).ok_or_else(bad)?;
            n |= (v as u32) << (18 - 6 * i);
        }
        for i in 0..chunk.len() - 1 {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    String::from_utf8(out).map_err(|_| "The decoded data is not text".to_owned())
}

/// Every word that passes `keep` (after trimming punctuation), once each.
fn extract(text: &str, keep: fn(&str) -> bool, none: &str) -> Result<String, String> {
    let mut found: Vec<&str> = Vec::new();
    for w in text.split_whitespace() {
        let w = w
            .trim_matches(|c: char| "()<>[]{}\"'`,;:!?".contains(c))
            .trim_end_matches('.');
        if keep(w) && !found.contains(&w) {
            found.push(w);
        }
    }
    if found.is_empty() {
        Err(none.to_owned())
    } else {
        Ok(found.join("\n"))
    }
}

pub fn links(text: &str) -> Result<String, String> {
    extract(text, smart::is_url, "No links found")
}

pub fn emails(text: &str) -> Result<String, String> {
    extract(text, smart::is_email, "No email addresses found")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_and_dedupe_lines() {
        assert_eq!(sort_lines("b\nA\nc\na"), "A\na\nb\nc");
        assert_eq!(dedupe_lines("x\ny\nx\nz\ny"), "x\ny\nz");
    }

    #[test]
    fn case_conversions_split_words_sensibly() {
        assert_eq!(
            snake_case("helloWorld HTTPServer-v2 some_thing\nSecond Line"),
            "hello_world_http_server_v2_some_thing\nsecond_line"
        );
        assert_eq!(camel_case("hello world_foo-BAR"), "helloWorldFooBar");
        assert_eq!(camel_case("HTTPServer"), "httpServer");
    }

    #[test]
    fn url_and_base64_round_trip_including_unicode() {
        let t = "a b&c=d/é 😀";
        let enc = url_encode(t);
        assert_eq!(enc, "a%20b%26c%3Dd%2F%C3%A9%20%F0%9F%98%80");
        assert_eq!(url_decode(&enc).unwrap(), t);
        assert_eq!(url_decode("100%").unwrap(), "100%");
        assert!(url_decode("%FF").is_err());
        for t in ["a", "ab", "abc", "abcd", "héllo 😀"] {
            assert_eq!(base64_decode(&base64_encode(t.as_bytes())).unwrap(), t);
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert_eq!(base64_decode("TWE").unwrap(), "Ma");
        assert!(base64_decode("not base64!").is_err());
        assert!(base64_decode("").is_err());
    }

    #[test]
    fn base64_matches_the_rfc_4648_vectors() {
        for (plain, enc) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
            ("café 東京", "Y2Fmw6kg5p2x5Lqs"),
            ("\u{1F600}", "8J+YgA=="),
        ] {
            assert_eq!(base64_encode(plain.as_bytes()), enc, "encode {plain:?}");
            if !enc.is_empty() {
                assert_eq!(base64_decode(enc).unwrap(), plain, "decode {enc}");
            }
        }
        // Unpadded, wrapped and mixed-whitespace input is accepted.
        assert_eq!(base64_decode("Zm9v\r\nYg").unwrap(), "foob");
        assert_eq!(base64_decode(" Zm8 = ").unwrap(), "fo");
        for bad in ["Z", "Zg=a", "Z===", "Zm9v!", "Zg==Zg==", "/w=="] {
            assert!(base64_decode(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn url_encoding_matches_known_vectors() {
        assert_eq!(url_encode(""), "");
        assert_eq!(url_encode("AZaz09-_.~"), "AZaz09-_.~");
        assert_eq!(url_encode("café 東京"), "caf%C3%A9%20%E6%9D%B1%E4%BA%AC");
        assert_eq!(
            url_encode(":/?#[]@!$&'()*+,;= %"),
            "%3A%2F%3F%23%5B%5D%40%21%24%26%27%28%29%2A%2B%2C%3B%3D%20%25"
        );
        assert_eq!(url_decode("").unwrap(), "");
        assert_eq!(
            url_decode("caf%C3%A9%20%E6%9D%B1%E4%BA%AC").unwrap(),
            "café 東京"
        );
        assert_eq!(url_decode("%c3%a9").unwrap(), "é");
        assert_eq!(url_decode("a+b").unwrap(), "a+b");
        // Malformed escapes stay as typed; a sign is not a hex digit.
        for same in ["%", "%4", "%zz", "%+1", "%-1", "50%off", "%%41"] {
            let want = if same == "%%41" { "%A" } else { same };
            assert_eq!(url_decode(same).unwrap(), want, "{same:?}");
        }
        assert!(url_decode("%C3").is_err());
    }

    #[test]
    fn links_and_emails_are_extracted_once_each() {
        let t = "see (https://a.com/x?y=1), also https://a.com/x?y=1 and http://b.org. mail a@b.com; c@d.io.";
        assert_eq!(links(t).unwrap(), "https://a.com/x?y=1\nhttp://b.org");
        assert_eq!(emails(t).unwrap(), "a@b.com\nc@d.io");
        assert!(links("nothing here").is_err());
        assert!(emails("nothing here").is_err());
    }

    #[test]
    fn count_characters_counts_chars_not_bytes() {
        assert_eq!(count_chars("é😀 a"), "4 characters");
        assert_eq!(count_chars("x"), "1 character");
    }
}
