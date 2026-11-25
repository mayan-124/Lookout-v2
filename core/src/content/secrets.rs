//! Secret masking (brief section 9): text is masked **before** it reaches the
//! index, so a key, token, password or card number found in a file or in
//! OCR'd text is never stored and can never be searched for or shown.
//!
//! Detection is by pattern, so it is best effort: it catches the common
//! formats listed below, not every secret. The same detector is meant to be
//! reused for the clipboard in Phase 4.
//!
//! - private key blocks (`-----BEGIN ... PRIVATE KEY-----`)
//! - AWS, GitHub, Slack, Stripe, Google and OpenAI-style keys, JSON web tokens
//! - `password = ...`, `api_key: ...`, `token=...` style assignments (value only)
//! - card numbers (13 to 19 digits that pass the Luhn check)
//! - long high-entropy strings (random-looking tokens of 32+ characters)

use std::borrow::Cow;
use std::sync::OnceLock;

use regex::Regex;

pub const MASK: &str = "[masked]";

struct Patterns {
    /// Replace the whole match.
    whole: Vec<Regex>,
    /// `name = value`: replace only the value (group 3).
    assignment: Option<Regex>,
    /// `Authorization: Bearer|Basic value`: replace only the value (group 2).
    auth: Option<Regex>,
    cardish: Option<Regex>,
    tokenish: Option<Regex>,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| {
        let whole = [
            r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|\z)",
            r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
            r"\bgh[pousr]_[A-Za-z0-9]{30,}\b",
            r"\bgithub_pat_[A-Za-z0-9_]{20,}\b",
            r"\bxox[abprs]-[A-Za-z0-9-]{10,}",
            r"\b[sprk]k_(?:live|test)_[A-Za-z0-9]{16,}\b",
            r"\bsk-[A-Za-z0-9_-]{20,}\b",
            r"\bAIza[0-9A-Za-z_-]{35}\b",
            r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
        ];
        // The literals are fixed; if one ever failed to compile, the unit test
        // `all_patterns_compile` fails (a failure here must not panic the app).
        Patterns {
            whole: whole.iter().filter_map(|s| Regex::new(s).ok()).collect(),
            assignment: Regex::new(
                r#"(?i)(password|passwd|passphrase|pwd|secret|api[_-]?key|access[_-]?key|auth[_-]?token|token|client[_-]?secret)(["']?\s*[:=]\s*["']?)([^\s"',;}]+)"#,
            )
            .ok(),
            auth: Regex::new(
                r#"(?i)(authorization["']?\s*[:=]\s*["']?(?:bearer|basic)\s+)([^\s"']+)"#,
            )
            .ok(),
            cardish: Regex::new(r"\b\d(?:[ -]?\d){12,18}\b").ok(),
            tokenish: Regex::new(r"[A-Za-z0-9+/_=-]{32,}").ok(),
        }
    })
}

/// True if the digits (separators ignored) pass the Luhn check.
pub fn luhn(digits: &str) -> bool {
    let nums: Vec<u32> = digits.chars().filter_map(|c| c.to_digit(10)).collect();
    if !(13..=19).contains(&nums.len()) {
        return false;
    }
    let sum: u32 = nums
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| {
            if i % 2 == 1 {
                let x = d * 2;
                if x > 9 {
                    x - 9
                } else {
                    x
                }
            } else {
                d
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

fn entropy(s: &str) -> f64 {
    let mut counts = [0u32; 128];
    let mut n = 0f64;
    for b in s.bytes().filter(u8::is_ascii) {
        counts[usize::from(b)] += 1;
        n += 1.0;
    }
    if n == 0.0 {
        return 0.0;
    }
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = f64::from(c) / n;
            -p * p.log2()
        })
        .sum()
}

/// A random-looking token: long, mixes letters and digits, high entropy.
/// Plain hex (git hashes, checksums) stays below the entropy bar.
fn looks_like_token(s: &str) -> bool {
    s.len() >= 32
        && s.bytes().any(|b| b.is_ascii_digit())
        && s.bytes().any(|b| b.is_ascii_alphabetic())
        && entropy(s) >= 4.2
}

type Replacer<'a> = &'a dyn Fn(&regex::Captures<'_>) -> Option<String>;

fn apply(out: &mut Cow<'_, str>, count: &mut usize, re: &Regex, f: Replacer<'_>) {
    if !re.is_match(out) {
        return;
    }
    let replaced = re.replace_all(out, |caps: &regex::Captures<'_>| match f(caps) {
        Some(r) => {
            *count += 1;
            r
        }
        None => caps[0].to_owned(),
    });
    *out = Cow::Owned(replaced.into_owned());
}

/// Returns `text` with secrets replaced by [`MASK`], and how many were masked.
pub fn mask(text: &str) -> (Cow<'_, str>, usize) {
    let p = patterns();
    let mut out = Cow::Borrowed(text);
    let mut count = 0usize;
    for re in &p.whole {
        apply(&mut out, &mut count, re, &|_| Some(MASK.to_owned()));
    }
    if let Some(re) = &p.auth {
        apply(&mut out, &mut count, re, &|c| {
            (&c[2] != MASK).then(|| format!("{}{MASK}", &c[1]))
        });
    }
    if let Some(re) = &p.assignment {
        apply(&mut out, &mut count, re, &|c| {
            // `[masked]` itself stops at `]`-free classes below; skip already-masked values.
            (!c[3].starts_with("[masked]")).then(|| format!("{}{}{MASK}", &c[1], &c[2]))
        });
    }
    if let Some(re) = &p.cardish {
        apply(&mut out, &mut count, re, &|c| {
            luhn(&c[0]).then(|| MASK.to_owned())
        });
    }
    if let Some(re) = &p.tokenish {
        apply(&mut out, &mut count, re, &|c| {
            looks_like_token(&c[0]).then(|| MASK.to_owned())
        });
    }
    (out, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fake secrets are assembled at run time so that no real-looking secret
    // sits in the source file.
    fn aws() -> String {
        format!("AKIA{}", "IOSFODNN7EXAMPLE")
    }
    fn github() -> String {
        format!("ghp_{}", "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8")
    }

    #[test]
    fn all_patterns_compile() {
        let p = patterns();
        assert_eq!(p.whole.len(), 9, "a built-in pattern failed to compile");
        assert!(
            p.assignment.is_some()
                && p.auth.is_some()
                && p.cardish.is_some()
                && p.tokenish.is_some()
        );
    }

    #[test]
    fn known_key_formats_are_masked() {
        let jwt = format!(
            "eyJ{}.eyJ{}.{}",
            "hbGciOiJIUzI1NiJ9", "zdWIiOiIxMjM0NTY3", "SflKxwRJSMeKKF2QT4fw"
        );
        let samples = [
            aws(),
            github(),
            format!("xoxb-{}", "123456789012-abcdefghijkl"),
            format!("sk_live_{}", "abcdefghijklmnop1234"),
            format!("AIza{}", "SyA1234567890abcdefghijklmnopqrstuv"),
            jwt,
            format!(
                "-----BEGIN RSA PRIVATE KEY-----\n{}\n-----END RSA PRIVATE KEY-----",
                "MIIBOgIBAAJBAKj34GkxFhD90vcNLYLInFEX6Ppy1tPf9Cnzj4p4WGeKLs1Pt8Qu"
            ),
        ];
        for s in samples {
            let text = format!("before {s} after");
            let (m, n) = mask(&text);
            assert_eq!(n, 1, "{s}");
            assert!(m.contains(MASK) && !m.contains(&s), "{s} -> {m}");
            assert!(m.starts_with("before ") && m.ends_with(" after"));
        }
    }

    #[test]
    fn assignments_mask_only_the_value() {
        let (m, n) = mask("db_password = Tr0ub4dor&3xyz\nuser = alice\napi_key: \"abcd1234efgh\"");
        assert_eq!(n, 2);
        assert!(m.contains("user = alice"));
        assert!(!m.contains("Tr0ub4dor") && !m.contains("abcd1234efgh"));
        assert!(m.contains("password = [masked]"));
    }

    #[test]
    fn card_numbers_need_a_valid_luhn_checksum() {
        let (m, n) = mask("card 4111 1111 1111 1111 expires soon");
        assert_eq!(n, 1);
        assert!(!m.contains("4111"));
        // Invoice and order numbers of the same length are kept.
        let (m, n) = mask("order 4111 1111 1111 1112 shipped");
        assert_eq!(n, 0);
        assert!(m.contains("1112"));
        assert!(luhn("4111111111111111") && !luhn("4111111111111112"));
    }

    #[test]
    fn high_entropy_tokens_are_masked_but_ordinary_text_and_hashes_are_not() {
        let token = "q8Zr3KxP9mW2vLbN7yTfH5cJdA4sGeU6oXi1";
        let text = format!("session {token} end");
        let (m, n) = mask(&text);
        assert_eq!(n, 1);
        assert!(!m.contains(token));
        let sha = "da39a3ee5e6b4b0d3255bfef95601890afd80709";
        let text = format!("commit {sha} fixes the invoice");
        let (m, n) = mask(&text);
        assert_eq!(n, 0);
        assert!(m.contains(sha));
        let (m, n) = mask("The quarterly invoice 4821 was paid by Acme Corp on March 3.");
        assert_eq!(n, 0);
        assert!(
            matches!(m, Cow::Borrowed(_)),
            "unchanged text is not copied"
        );
    }

    #[test]
    fn authorization_header_values_are_masked() {
        for (text, secret) in [
            ("Authorization: Bearer abc", "abc"),
            ("authorization: basic dXNlcjpw", "dXNlcjpw"),
            ("curl -H 'Authorization: Bearer x1' https://a.test", "x1"),
            (
                r#"curl -H "Authorization: Basic QWxhZGRpbg==" url"#,
                "QWxhZGRpbg",
            ),
            (r#"{"Authorization": "Bearer tok"}"#, "tok\""),
        ] {
            let (m, n) = mask(text);
            assert!(n >= 1 && !m.contains(secret), "{text} -> {m}");
            assert!(m.contains(MASK), "{text} -> {m}");
        }
        let (m, _) = mask("curl -H 'Authorization: Bearer x1' https://a.test");
        assert!(m.contains("Authorization: Bearer [masked]'") && m.contains("https://a.test"));
    }

    #[test]
    fn short_secret_values_are_masked() {
        for (text, secret) in [
            ("password=abc", "abc"),
            ("pwd: 1234", "1234"),
            (r#"{"token": "x9"}"#, "x9"),
            ("api_key=k", "=k"),
            ("passwd = qw", "qw"),
            ("secret: s3", "s3"),
        ] {
            let (m, n) = mask(text);
            assert!(
                n == 1 && !m.contains(secret) && m.contains(MASK),
                "{text} -> {m}"
            );
        }
    }

    #[test]
    fn harmless_text_is_not_masked() {
        for text in [
            "pass the salt, the password policy is long",
            "Authorization is required for this page",
            "the bearer of the news; Basic English",
            "tokens: 5 per minute, secret garden",
            "bypass: yes, compass = north",
            "Reset your password on the settings page.",
            "invoice 4821 paid on March 3",
        ] {
            let (m, n) = mask(text);
            assert!(n == 0 && m == text, "{text} -> {m}");
        }
    }

    #[test]
    fn quoted_keys_are_masked() {
        for (text, secret) in [
            (r#"{"password": "Hunter2xyz!"}"#, "Hunter2xyz"),
            (r#"{"api_key":"abcd1234efgh"}"#, "abcd1234efgh"),
            ("'client_secret' : 'Zx81qwerty'", "Zx81qwerty"),
            ("PASSWORD=Sup3rS3cret", "Sup3rS3cret"),
        ] {
            let (m, n) = mask(text);
            assert!(n >= 1 && !m.contains(secret), "{text} -> {m}");
        }
    }
}
