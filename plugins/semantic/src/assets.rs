//! The files the user places in `<plugin data>/assets/`, and the pins that
//! decide whether they may be used. Nothing is downloaded by Lookout.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub const MODEL_FILE: &str = "model.onnx";
pub const TOKENIZER_FILE: &str = "tokenizer.json";
pub const RUNTIME_FILE: &str = "onnxruntime.dll";

/// Lower-case hex SHA-256 of each file.
#[derive(Debug, Clone)]
pub struct Pins {
    pub model: String,
    pub tokenizer: String,
    pub runtime: String,
}

/// The int8 ONNX export of bge-small-en-v1.5 (34,014,426 bytes), its
/// `tokenizer.json` (711,396 bytes), and `onnxruntime.dll` 1.32.0 for Windows
/// x64, taken from the `onnxruntime-1.32.0-cp312-cp312-win_amd64` wheel.
pub fn pinned() -> Pins {
    Pins {
        model: "6c9c6101a956d62dfb5e7190c538226c0c5bb9cb27b651234b6df063ee7dbfe4".into(),
        tokenizer: "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66".into(),
        runtime: "443c0065fcdba17618ddef05050308f4b471893c5c7da0337d6957c9073ad8a7".into(),
    }
}

/// Files that passed the check. The model and tokenizer are the very bytes that
/// were hashed (so they cannot change between the check and the use); the
/// runtime is loaded from `runtime` right after the check.
#[derive(Debug)]
pub struct Verified {
    pub model: Vec<u8>,
    pub tokenizer: Vec<u8>,
    pub runtime: PathBuf,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn read_checked(dir: &Path, name: &str, pin: &str) -> Result<Vec<u8>, String> {
    let path = dir.join(name);
    let bytes = std::fs::read(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("{name} is missing in {}", dir.display())
        } else {
            format!("cannot read {}: {e}", path.display())
        }
    })?;
    let found = sha256_hex(&bytes);
    if !found.eq_ignore_ascii_case(pin) {
        return Err(format!(
            "{name} does not match its pinned SHA-256 (expected {pin}, found {found}); refusing to load it"
        ));
    }
    Ok(bytes)
}

/// Checks all three files. The runtime must be an absolute path: it is loaded
/// by that path only, never by name or through `PATH`.
pub fn verify(dir: &Path, pins: &Pins) -> Result<Verified, String> {
    if !dir.is_absolute() {
        return Err("the assets folder is not an absolute path".into());
    }
    let tokenizer = read_checked(dir, TOKENIZER_FILE, &pins.tokenizer)?;
    let model = read_checked(dir, MODEL_FILE, &pins.model)?;
    // The runtime is only hashed here; it is loaded from the same absolute path next.
    read_checked(dir, RUNTIME_FILE, &pins.runtime)?;
    Ok(Verified {
        model,
        tokenizer,
        runtime: dir.join(RUNTIME_FILE),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pins_of(a: &[u8], b: &[u8], c: &[u8]) -> Pins {
        Pins {
            model: sha256_hex(a),
            tokenizer: sha256_hex(b),
            runtime: sha256_hex(c),
        }
    }

    fn put(dir: &Path, m: &[u8], t: &[u8], r: &[u8]) {
        std::fs::write(dir.join(MODEL_FILE), m).unwrap();
        std::fs::write(dir.join(TOKENIZER_FILE), t).unwrap();
        std::fs::write(dir.join(RUNTIME_FILE), r).unwrap();
    }

    #[test]
    fn matching_files_pass_and_each_kind_of_mismatch_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let pins = pins_of(b"m", b"t", b"r");
        assert!(verify(d.path(), &pins).unwrap_err().contains("missing"));
        put(d.path(), b"m", b"t", b"r");
        let ok = verify(d.path(), &pins).unwrap();
        assert_eq!(ok.model, b"m");
        for (m, t, r, which) in [
            (&b"x"[..], &b"t"[..], &b"r"[..], MODEL_FILE),
            (b"m", b"x", b"r", TOKENIZER_FILE),
            (b"m", b"t", b"x", RUNTIME_FILE),
        ] {
            put(d.path(), m, t, r);
            let e = verify(d.path(), &pins).unwrap_err();
            assert!(e.contains(which) && e.contains("pinned SHA-256"), "{e}");
        }
    }

    #[test]
    fn a_relative_folder_is_refused() {
        let e = verify(Path::new("assets"), &pins_of(b"", b"", b"")).unwrap_err();
        assert!(e.contains("absolute"));
    }
}
