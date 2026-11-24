//! Ranking (brief section 7.6):
//! `score = w_match*match + w_frecency*frecency + w_recency*recency + w_semantic*semantic`.

use memchr::memmem;
use serde::{Deserialize, Serialize};

use crate::item::ScoreFeatures;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RankingWeights {
    pub w_match: f32,
    pub w_frecency: f32,
    pub w_recency: f32,
    pub w_semantic: f32,
}

impl Default for RankingWeights {
    fn default() -> Self {
        Self {
            w_match: 1.0,
            w_frecency: 0.6,
            w_recency: 0.2,
            w_semantic: 0.0,
        }
    }
}

impl RankingWeights {
    pub fn score(&self, f: &ScoreFeatures) -> f32 {
        self.w_match * f.match_quality
            + self.w_frecency * f.frecency
            + self.w_recency * f.recency
            + self.w_semantic * f.semantic.unwrap_or(0.0)
    }
}

/// Recency of a modification time: 1.0 now, 0.5 after 30 days, tending to 0.
pub fn recency(modified: u32, now: u32) -> f32 {
    if modified == 0 {
        return 0.0;
    }
    let age_days = now.saturating_sub(modified) as f32 / 86_400.0;
    1.0 / (1.0 + age_days / 30.0)
}

/// Frecency is stored in indexes as a `u8` level; this maps it to `0.0..=1.0`.
pub fn frecency_from_level(level: u8) -> f32 {
    f32::from(level) / 255.0
}

/// Maps an unbounded decayed open count to a `u8` level (`s / (s + 3)`).
pub fn frecency_level(score: f64) -> u8 {
    if score <= 0.0 {
        return 0;
    }
    (255.0 * score / (score + 3.0)).round().clamp(0.0, 255.0) as u8
}

fn is_boundary(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'.' | b'_' | b'-' | b'(' | b')' | b'[' | b']' | b'+' | b','
    )
}

/// Match quality of lowercase `name` against lowercase `tokens`, in `0.0..=1.0`,
/// or `None` when some token does not occur. Per token: exact name or stem
/// 1.0, prefix 0.85, word start 0.7, substring 0.4; averaged, then blended with
/// how much of the name the query covers so shorter names rank higher.
pub fn match_quality(name: &[u8], tokens: &[&[u8]]) -> Option<f32> {
    match_quality_camel(name, tokens, &|| 0)
}

/// Gap between match tiers. Larger than everything a bonus can add (frecency
/// 0.6 + recency 0.2 + app bonus 0.25) plus the spread of base qualities, so
/// bonuses only order results inside one tier.
pub const TIER_STEP: f32 = 2.5;

/// Whether the query equals the whole name or its stem (the exact tier).
pub fn is_exact(name: &[u8], tokens: &[&[u8]]) -> bool {
    let stem_len = match name.iter().rposition(|&b| b == b'.') {
        Some(i) if i > 0 => i,
        _ => name.len(),
    };
    let stem = &name[..stem_len];
    match tokens {
        [] => false,
        [t] => *t == name || *t == stem,
        many => {
            let joined = many.join(&b' ');
            joined == name || joined == stem
        }
    }
}

/// [`match_quality_camel`] raised by [`TIER_STEP`] for an exact name, used to
/// order search results.
pub fn match_quality_tiered(
    name: &[u8],
    tokens: &[&[u8]],
    camel: &dyn Fn() -> u128,
) -> Option<f32> {
    let base = match_quality_camel(name, tokens, camel)?;
    Some(
        base + if is_exact(name, tokens) {
            TIER_STEP
        } else {
            0.0
        },
    )
}

/// Lowercase letter without its accent ("é" gives `e`); `None` for a combining
/// mark, which is dropped ("İ" lowercases to `i` plus a combining dot).
fn fold_char(c: char) -> Option<char> {
    Some(match c {
        '\u{300}'..='\u{36f}' => return None,
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'č' => 'c',
        'ď' => 'd',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' | 'ě' => 'e',
        'ì' | 'í' | 'î' | 'ï' | 'ī' => 'i',
        'ł' => 'l',
        'ñ' | 'ń' | 'ň' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => 'o',
        'ř' => 'r',
        'ś' | 'š' | 'ş' => 's',
        'ť' => 't',
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' => 'u',
        'ý' | 'ÿ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        c => c,
    })
}

/// The form in which names and queries are compared: lowercase, accents
/// removed, so "cafe" finds "Café" and "muller" finds "Müller".
pub fn fold_lower(s: &str) -> String {
    s.to_lowercase().chars().filter_map(fold_char).collect()
}

/// Positions (byte offsets into the lowercase name, below 128) where an
/// uppercase letter starts a new word in `original`: `PowerToys` gives 5, `XMLParser` 3.
pub fn camel_starts(original: &str) -> u128 {
    let (mut bits, mut at) = (0u128, 0usize);
    let mut prev: Option<char> = None;
    let mut chars = original.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(p) = prev {
            if c.is_uppercase() && at < 128 {
                let next_lower = chars.peek().is_some_and(|n| n.is_lowercase());
                if p.is_lowercase() || p.is_ascii_digit() || (p.is_uppercase() && next_lower) {
                    bits |= 1 << at;
                }
            }
        }
        // Offsets are into the folded lowercase name (see `fold_lower`).
        at += if c.is_ascii() {
            1
        } else {
            c.to_lowercase()
                .filter_map(fold_char)
                .map(char::len_utf8)
                .sum()
        };
        prev = Some(c);
    }
    bits
}

/// [`match_quality`] where `camel` (asked for only when a hit is mid-word)
/// gives the [`camel_starts`] bits of the name, counted as word starts too.
pub fn match_quality_camel(name: &[u8], tokens: &[&[u8]], camel: &dyn Fn() -> u128) -> Option<f32> {
    let mut camel_bits: Option<u128> = None;
    if tokens.is_empty() || name.is_empty() {
        return None;
    }
    let stem_len = match name.iter().rposition(|&b| b == b'.') {
        Some(i) if i > 0 => i,
        _ => name.len(),
    };
    let stem = &name[..stem_len];

    let mut total = 0.0f32;
    let mut covered = 0usize;
    for token in tokens {
        if token.is_empty() {
            return None;
        }
        let mut best: Option<f32> = None;
        if *token == name || *token == stem {
            best = Some(1.0);
        } else {
            // Look at a few occurrences: a later word-start beats an earlier mid-word hit.
            for pos in memmem::find_iter(name, token).take(8) {
                let cat = if pos == 0 {
                    0.85
                } else if is_boundary(name[pos - 1])
                    || (pos < 128 && (*camel_bits.get_or_insert_with(camel) >> pos) & 1 == 1)
                {
                    0.7
                } else {
                    0.4
                };
                if best.is_none_or(|b| cat > b) {
                    best = Some(cat);
                }
                if cat >= 0.85 {
                    break;
                }
            }
        }
        total += best?;
        covered += token.len();
    }
    let avg = total / tokens.len() as f32;
    let coverage = (covered as f32 / name.len() as f32).min(1.0);
    Some(0.85 * avg + 0.15 * coverage)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(name: &str, tokens: &[&str]) -> Option<f32> {
        let t: Vec<&[u8]> = tokens.iter().map(|t| t.as_bytes()).collect();
        match_quality(name.as_bytes(), &t)
    }

    #[test]
    fn exact_beats_prefix_beats_word_start_beats_substring() {
        let exact = q("report", &["report"]).unwrap();
        let prefix = q("report final.docx", &["report"]).unwrap();
        let word = q("my report.docx", &["report"]).unwrap();
        let sub = q("myreportx.docx", &["report"]).unwrap();
        assert!(
            exact > prefix && prefix > word && word > sub,
            "{exact} {prefix} {word} {sub}"
        );
    }

    #[test]
    fn tiers_beat_bonuses() {
        let t: Vec<&[u8]> = vec![b"chrome"];
        let no = &|| 0u128;
        let exact = match_quality_tiered(b"chrome.html", &t, no).unwrap();
        let prefix = match_quality_tiered(b"chrome bookmarks.html", &t, no).unwrap();
        // The best bonuses on the lower tier still lose.
        assert!(exact > prefix + 1.05);
    }

    #[test]
    fn stem_counts_as_exact() {
        // The file stem equals the query, so it beats a mere prefix of a longer name.
        assert!(q("readme.md", &["readme"]).unwrap() > q("readme notes.md", &["readme"]).unwrap());
        assert!(q("readme.md", &["readme.md"]).unwrap() > 0.99);
    }

    #[test]
    fn all_tokens_must_match() {
        assert!(q("annual report.pdf", &["annual", "pdf"]).is_some());
        assert!(q("annual report.pdf", &["annual", "xlsx"]).is_none());
    }

    #[test]
    fn shorter_name_wins_on_equal_category() {
        let short = q("notes a.txt", &["notes"]).unwrap();
        let long = q("notes about the quarterly planning meeting.txt", &["notes"]).unwrap();
        assert!(short > long);
    }

    #[test]
    fn later_word_start_beats_earlier_substring() {
        let better = q("xreport report", &["report"]).unwrap();
        let worse = q("xreport", &["report"]).unwrap();
        assert!(better > worse);
    }

    #[test]
    fn folding_removes_accents_and_case() {
        assert_eq!(fold_lower("Café Müller İstanbul"), "cafe muller istanbul");
        assert_eq!(fold_lower("日本語"), "日本語");
        // Camel-case offsets follow the folded name: "É" is one byte there.
        assert_eq!(camel_starts("ÉcoleDesArts"), 1 << 5 | 1 << 8);
    }

    #[test]
    fn recency_decays() {
        let now = 100 * 86_400;
        assert!(recency(now, now) > 0.99);
        let month = recency(now - 30 * 86_400, now);
        assert!((month - 0.5).abs() < 0.01);
        assert_eq!(recency(0, now), 0.0);
    }

    #[test]
    fn frecency_level_is_monotonic_and_bounded() {
        assert_eq!(frecency_level(0.0), 0);
        let mut last = 0;
        for s in [0.5, 1.0, 3.0, 10.0, 1000.0] {
            let l = frecency_level(s);
            assert!(l >= last);
            last = l;
        }
        assert_eq!(frecency_level(1e9), 255);
        assert!((frecency_from_level(255) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn weights_combine_features() {
        let w = RankingWeights::default();
        let f = ScoreFeatures {
            match_quality: 1.0,
            frecency: 1.0,
            recency: 1.0,
            semantic: None,
        };
        assert!((w.score(&f) - 1.8).abs() < 1e-6);
    }

    #[test]
    fn camel_case_word_starts_count_as_word_starts() {
        let t: Vec<&[u8]> = vec![b"toys"];
        let with = camel_starts("PowerToysRun");
        assert_eq!(with, 1 << 5 | 1 << 9);
        let plain = match_quality(b"powertoysrun", &t).unwrap();
        let camel = match_quality_camel(b"powertoysrun", &t, &|| with).unwrap();
        assert!(camel > plain, "{camel} vs {plain}");
        // Same as a space-separated name's word start.
        let spaced = match_quality(b"power toys run", &t).unwrap();
        assert!(camel > 0.85 * 0.7 + 0.15 * (4.0 / 12.0) - 0.001);
        assert!(spaced > 0.0);
        assert_eq!(camel_starts("XMLParser"), 1 << 3);
        assert_eq!(camel_starts("lowercase"), 0);
        assert_eq!(camel_starts("ALLCAPS"), 0);
    }
}
