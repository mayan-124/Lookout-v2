use std::path::PathBuf;

/// `--measure-latency <cycles> --out <csv path>`: run show/hide cycles, write
/// the samples and exit. Used by `bench`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasureArgs {
    pub cycles: usize,
    pub out: PathBuf,
}

/// Returns `None` when `--measure-latency` is absent or malformed.
pub fn parse_measure_args<I: IntoIterator<Item = String>>(args: I) -> Option<MeasureArgs> {
    let mut cycles = None;
    let mut out = None;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--measure-latency" => cycles = it.next()?.parse().ok(),
            "--out" => out = it.next().map(PathBuf::from),
            _ => {}
        }
    }
    Some(MeasureArgs {
        cycles: cycles?,
        out: out?,
    })
}

/// Developer flags: `--synthetic-index <files>` fills the index with generated
/// names instead of crawling (benchmarks only); `--index-root <path>` (repeatable)
/// replaces the configured roots; `--data-dir <path>` keeps settings, frecency
/// and the undo log in that folder instead of the user's real one (tests).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DevFlags {
    pub synthetic_index: Option<usize>,
    pub index_roots: Vec<PathBuf>,
    pub data_dir: Option<PathBuf>,
    /// `--synthetic-with-folders`: with `--synthetic-index`, also index the contents of the configured folders.
    pub synthetic_with_folders: bool,
}

pub fn parse_dev_flags<I: IntoIterator<Item = String>>(args: I) -> DevFlags {
    let mut flags = DevFlags::default();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--synthetic-index" => flags.synthetic_index = it.next().and_then(|v| v.parse().ok()),
            "--index-root" => flags.index_roots.extend(it.next().map(PathBuf::from)),
            "--data-dir" => flags.data_dir = it.next().map(PathBuf::from),
            "--synthetic-with-folders" => flags.synthetic_with_folders = true,
            _ => {}
        }
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_flags() {
        let a = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(parse_dev_flags(a(&["x.exe"])), DevFlags::default());
        let f = parse_dev_flags(a(&[
            "x.exe",
            "--synthetic-index",
            "1000000",
            "--index-root",
            "C:\\a",
            "--index-root",
            "D:\\b",
            "--data-dir",
            "E:\\t",
        ]));
        assert_eq!(f.data_dir, Some(PathBuf::from("E:\\t")));
        assert_eq!(f.synthetic_index, Some(1_000_000));
        assert_eq!(
            f.index_roots,
            [PathBuf::from("C:\\a"), PathBuf::from("D:\\b")]
        );
        assert_eq!(
            parse_dev_flags(a(&["--synthetic-index", "lots"])).synthetic_index,
            None
        );
    }

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn parses_both_flags() {
        let a = parse_measure_args(args(&[
            "lookout.exe",
            "--measure-latency",
            "5",
            "--out",
            "x.csv",
        ]));
        assert_eq!(
            a,
            Some(MeasureArgs {
                cycles: 5,
                out: "x.csv".into()
            })
        );
    }

    #[test]
    fn normal_launch_has_no_measure_mode() {
        assert_eq!(parse_measure_args(args(&["lookout.exe"])), None);
        assert_eq!(
            parse_measure_args(args(&["lookout.exe", "--measure-latency", "x"])),
            None
        );
        assert_eq!(
            parse_measure_args(args(&["lookout.exe", "--measure-latency", "3"])),
            None
        );
    }
}
