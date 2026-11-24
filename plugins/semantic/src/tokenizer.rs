//! A BERT WordPiece tokenizer for bge-small-en-v1.5 (uncased), read from the
//! model's own `tokenizer.json`. It follows the Hugging Face `BertNormalizer`,
//! `BertPreTokenizer`, `WordPiece` and the `[CLS] ... [SEP]` template:
//!
//! 1. clean: drop NUL, U+FFFD, control and format characters; any whitespace
//!    becomes a space;
//! 2. put spaces around CJK ideographs;
//! 3. strip accents (NFD, drop combining marks), then lowercase;
//! 4. split on whitespace, and every punctuation character is a word of its own;
//! 5. greedy longest-match WordPiece with the `##` continuation prefix; a word
//!    that cannot be matched, or has more than 100 characters, is `[UNK]`.
//!
//! Written instead of depending on the `tokenizers` crate (a large dependency
//! with a C regex library) because the algorithm is small; `tests/fixtures/
//! tokens.json` holds ids produced by `tokenizers` 0.21.4 for a mixed corpus
//! and the test `matches_the_reference_tokenizer` compares them.

use std::collections::HashMap;

use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

pub struct Tokenizer {
    vocab: HashMap<String, u32>,
    cls: u32,
    sep: u32,
    unk: u32,
    max_word_chars: usize,
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0x20000..=0x2A6DF | 0x2A700..=0x2B73F
        | 0x2B740..=0x2B81F | 0x2B820..=0x2CEAF | 0xF900..=0xFAFF | 0x2F800..=0x2FA1F)
}

/// ASCII punctuation and symbols (as BERT counts them) plus the common
/// Unicode punctuation blocks. Not the full Unicode `P*` table.
fn is_punct(c: char) -> bool {
    c.is_ascii_punctuation()
        || matches!(c as u32,
            0xA1 | 0xA7 | 0xAB | 0xB6 | 0xB7 | 0xBB | 0xBF
            | 0x2010..=0x2027 | 0x2030..=0x205E
            | 0x3001..=0x3003 | 0x3008..=0x3011 | 0x3014..=0x301F
            | 0xFF01..=0xFF0F | 0xFF1A..=0xFF20 | 0xFF3B..=0xFF40 | 0xFF5B..=0xFF65)
}

fn is_dropped(c: char) -> bool {
    c == '\0'
        || c == '\u{FFFD}'
        || (c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
        || matches!(c, '\u{200B}'..='\u{200F}' | '\u{2060}' | '\u{FEFF}')
}

impl Tokenizer {
    pub fn from_json(bytes: &[u8]) -> Result<Self, String> {
        let v: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|e| format!("tokenizer.json: {e}"))?;
        let model = v.get("model").ok_or("tokenizer.json has no model")?;
        if model.get("type").and_then(|t| t.as_str()) != Some("WordPiece") {
            return Err("tokenizer.json is not a WordPiece tokenizer".into());
        }
        let vocab: HashMap<String, u32> = model
            .get("vocab")
            .and_then(|m| m.as_object())
            .ok_or("tokenizer.json has no vocabulary")?
            .iter()
            .filter_map(|(k, id)| Some((k.clone(), u32::try_from(id.as_u64()?).ok()?)))
            .collect();
        let id = |t: &str| {
            vocab
                .get(t)
                .copied()
                .ok_or_else(|| format!("the vocabulary has no {t}"))
        };
        Ok(Self {
            cls: id("[CLS]")?,
            sep: id("[SEP]")?,
            unk: id("[UNK]")?,
            max_word_chars: model
                .get("max_input_chars_per_word")
                .and_then(|n| n.as_u64())
                .map_or(100, |n| n as usize),
            vocab,
        })
    }

    fn normalize(text: &str) -> String {
        let mut cleaned = String::with_capacity(text.len());
        for c in text.chars() {
            if is_dropped(c) {
                continue;
            }
            if c.is_whitespace() {
                cleaned.push(' ');
            } else if is_cjk(c) {
                cleaned.push(' ');
                cleaned.push(c);
                cleaned.push(' ');
            } else {
                cleaned.push(c);
            }
        }
        cleaned
            .nfd()
            .filter(|c| !is_combining_mark(*c))
            .collect::<String>()
            .to_lowercase()
    }

    fn words(normalized: &str) -> Vec<String> {
        let mut out = Vec::new();
        for piece in normalized.split_whitespace() {
            let mut cur = String::new();
            for c in piece.chars() {
                if is_punct(c) {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                    out.push(c.to_string());
                } else {
                    cur.push(c);
                }
            }
            if !cur.is_empty() {
                out.push(cur);
            }
        }
        out
    }

    fn wordpiece(&self, word: &str, out: &mut Vec<u32>) {
        let chars: Vec<char> = word.chars().collect();
        if chars.len() > self.max_word_chars {
            out.push(self.unk);
            return;
        }
        let mut pieces = Vec::new();
        let mut start = 0;
        while start < chars.len() {
            let mut end = chars.len();
            let mut found = None;
            while start < end {
                let mut s: String = chars[start..end].iter().collect();
                if start > 0 {
                    s.insert_str(0, "##");
                }
                if let Some(&id) = self.vocab.get(&s) {
                    found = Some(id);
                    break;
                }
                end -= 1;
            }
            match found {
                Some(id) => {
                    pieces.push(id);
                    start = end;
                }
                None => {
                    out.push(self.unk);
                    return;
                }
            }
        }
        out.extend(pieces);
    }

    /// `[CLS]`, the tokens, `[SEP]`; at most `max_len` ids in all (the tokens
    /// are cut, the two special ids are kept).
    pub fn encode(&self, text: &str, max_len: usize) -> Vec<u32> {
        let room = max_len.saturating_sub(2);
        let mut ids = Vec::with_capacity(room.min(512) + 2);
        ids.push(self.cls);
        let mut body = Vec::new();
        for word in Self::words(&Self::normalize(text)) {
            self.wordpiece(&word, &mut body);
            if body.len() >= room {
                break;
            }
        }
        body.truncate(room);
        ids.extend(body);
        ids.push(self.sep);
        ids
    }
}
