//! Hotkey-to-visible latency samples, statistics and the CSV exchanged between
//! the app (`--measure-latency`) and the `bench` tool.

use std::fmt::Write as _;

/// Budget from the spec: hotkey to visible bar, warm.
pub const HOTKEY_BUDGET_MS: f64 = 80.0;

pub const CSV_HEADER: &str = "index,native_ms,painted_ms";

/// One show cycle. `t0` is taken when the hotkey handler starts running.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// `t0` until the native `show()` + `set_focus()` calls returned.
    pub native_ms: f64,
    /// `t0` until the frontend painted two frames after being shown.
    pub painted_ms: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum CsvError {
    #[error("missing or wrong CSV header")]
    BadHeader,
    #[error("line {0}: expected 3 numeric fields")]
    BadLine(usize),
}

pub fn to_csv(samples: &[Sample]) -> String {
    let mut out = String::from(CSV_HEADER);
    out.push('\n');
    for (i, s) in samples.iter().enumerate() {
        // Writing to a String cannot fail.
        let _ = writeln!(out, "{i},{:.3},{:.3}", s.native_ms, s.painted_ms);
    }
    out
}

pub fn from_csv(text: &str) -> Result<Vec<Sample>, CsvError> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some(CSV_HEADER) {
        return Err(CsvError::BadHeader);
    }
    let mut samples = Vec::new();
    for (n, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let bad = || CsvError::BadLine(n + 2);
        let mut f = line.split(',');
        let _index = f.next().ok_or_else(bad)?;
        let native_ms = f.next().and_then(|v| v.parse().ok()).ok_or_else(bad)?;
        let painted_ms = f.next().and_then(|v| v.parse().ok()).ok_or_else(bad)?;
        samples.push(Sample {
            native_ms,
            painted_ms,
        });
    }
    Ok(samples)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stats {
    pub count: usize,
    pub min: f64,
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
}

/// Nearest-rank percentiles. Returns `None` for an empty slice.
pub fn summarize(values: &[f64]) -> Option<Stats> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let rank = |p: f64| {
        let idx = ((p / 100.0) * v.len() as f64).ceil() as usize;
        v[idx.clamp(1, v.len()) - 1]
    };
    Some(Stats {
        count: v.len(),
        min: v[0],
        p50: rank(50.0),
        p95: rank(95.0),
        max: v[v.len() - 1],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_nearest_rank() {
        let values: Vec<f64> = (1..=100).map(f64::from).collect();
        let s = summarize(&values).unwrap();
        assert_eq!(s.count, 100);
        assert_eq!((s.min, s.p50, s.p95, s.max), (1.0, 50.0, 95.0, 100.0));
    }

    #[test]
    fn single_value_and_empty() {
        let s = summarize(&[7.0]).unwrap();
        assert_eq!((s.p50, s.p95), (7.0, 7.0));
        assert!(summarize(&[]).is_none());
    }

    #[test]
    fn csv_round_trips() {
        let samples = vec![
            Sample {
                native_ms: 1.5,
                painted_ms: 20.25,
            },
            Sample {
                native_ms: 2.0,
                painted_ms: 18.0,
            },
        ];
        assert_eq!(from_csv(&to_csv(&samples)).unwrap(), samples);
    }

    #[test]
    fn csv_rejects_garbage() {
        assert!(matches!(from_csv("nope\n"), Err(CsvError::BadHeader)));
        let bad = format!("{CSV_HEADER}\n0,1.0\n");
        assert!(matches!(from_csv(&bad), Err(CsvError::BadLine(2))));
    }
}
