//! What the words after a verb mean.

/// `compress <name> under 200 kb`
#[derive(Debug, PartialEq, Eq)]
pub struct Compress {
    /// Words that name the file (searched by name).
    pub subject: String,
    pub target_bytes: u64,
}

fn unit(word: &str) -> Option<u64> {
    match word.to_lowercase().as_str() {
        "kb" | "k" | "kib" => Some(1024),
        "mb" | "m" | "mib" => Some(1024 * 1024),
        _ => None,
    }
}

/// A size in one token (`200kb`, `1.5MB`) and how many tokens it used.
fn size_at(tokens: &[&str], i: usize) -> Option<(u64, usize)> {
    let t = tokens.get(i)?;
    let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    if split > 0 {
        let (num, u) = t.split_at(split);
        if let (Ok(n), Some(mult)) = (num.parse::<f64>(), unit(u)) {
            return Some(((n * mult as f64) as u64, 1));
        }
        return None;
    }
    None
}

fn size_two_tokens(tokens: &[&str], i: usize) -> Option<(u64, usize)> {
    let n: f64 = tokens.get(i)?.parse().ok()?;
    let mult = unit(tokens.get(i + 1)?)?;
    Some(((n * mult as f64) as u64, 2))
}

pub fn compress(rest: &str) -> Option<Compress> {
    const FILLER: &[&str] = &[
        "under", "below", "to", "max", "at", "most", "less", "than", "<", "<=",
    ];
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    let (start, (bytes, used)) = (0..tokens.len()).find_map(|i| {
        size_at(&tokens, i)
            .or_else(|| size_two_tokens(&tokens, i))
            .map(|s| (i, s))
    })?;
    let mut from = start;
    while from > 0 && FILLER.contains(&tokens[from - 1].to_lowercase().as_str()) {
        from -= 1;
    }
    let subject: Vec<&str> = tokens[..from]
        .iter()
        .chain(tokens[start + used..].iter())
        .copied()
        .collect();
    let subject = subject.join(" ");
    (!subject.is_empty() && bytes >= 1024).then_some(Compress {
        subject,
        target_bytes: bytes,
    })
}

/// `strip metadata <name>` -> the name.
pub fn strip(rest: &str) -> Option<String> {
    const LEAD: &[&str] = &[
        "metadata", "exif", "info", "from", "of", "image", "photo", "data",
    ];
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    let skip = tokens
        .iter()
        .take_while(|t| LEAD.contains(&t.to_lowercase().as_str()))
        .count();
    let subject = tokens[skip..].join(" ");
    (!subject.is_empty()).then_some(subject)
}

/// `rename <folder name> to <pattern>` -> (folder words, pattern).
pub fn rename(rest: &str) -> Option<(String, String)> {
    let lower = rest.to_lowercase();
    let at = lower.find(" to ")?;
    let subject = rest[..at].trim();
    let pattern = rest[at + 4..].trim().trim_matches('"').trim();
    (!subject.is_empty() && !pattern.is_empty()).then(|| (subject.to_owned(), pattern.to_owned()))
}

pub fn human(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1024.0 * 1024.0 {
        format!("{:.1} MB", b / 1024.0 / 1024.0)
    } else if b >= 1024.0 {
        format!("{:.0} KB", b / 1024.0)
    } else {
        format!("{bytes} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_in_one_or_two_words() {
        let c = compress("holiday photo.png under 200 kb").unwrap();
        assert_eq!(c.subject, "holiday photo.png");
        assert_eq!(c.target_bytes, 200 * 1024);
        assert_eq!(compress("a.jpg 1.5MB").unwrap().target_bytes, 1_572_864);
        assert_eq!(compress("under 200kb a.jpg").unwrap().subject, "a.jpg");
        assert!(compress("a.jpg").is_none());
        assert!(compress("under 200 kb").is_none());
        assert!(compress("a.jpg under 10 bytes").is_none());
    }

    #[test]
    fn strip_and_rename() {
        assert_eq!(strip("metadata from photo.jpg").unwrap(), "photo.jpg");
        assert_eq!(strip("photo.jpg").unwrap(), "photo.jpg");
        assert!(strip("metadata").is_none());
        assert_eq!(
            rename("trip photos to Trip {n}").unwrap(),
            ("trip photos".into(), "Trip {n}".into())
        );
        assert_eq!(rename("a TO \"b {n}\"").unwrap().1, "b {n}");
        assert!(rename("trip photos").is_none());
        assert!(rename("trip to ").is_none());
    }
}
