//! Encryption at rest (brief section 9).
//!
//! One random 256-bit key protects everything Lookout stores about the user's
//! files. The key itself is only ever written to disk wrapped by Windows DPAPI
//! (`CryptProtectData`, current-user scope). That protects against other Windows
//! users and against a stolen disk or copied files. It does NOT protect against
//! code running as the same Windows user: that code can call `CryptUnprotectData`
//! too.
//!
//! Two constructions share the key, each with its own random nonces:
//! - **sealed streams** (`SealWriter`/`OpenReader`): ChaCha20-Poly1305 in 64 KiB
//!   chunks, each authenticated, with a "last chunk" flag so truncation is
//!   detected. Used for `meta.db` and the name index snapshot.
//! - **CTR** (`ctr_apply`): plain ChaCha20 with a seekable keystream, so the
//!   full-text index can be read at random offsets. It gives confidentiality
//!   but no integrity check; the integrity of the full-text index is not
//!   protected (see docs/decisions.md).

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use chacha20::ChaCha20;
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, KeyInit};

pub const KEY_LEN: usize = 32;
const MAGIC: &[u8; 4] = b"LKO1";
const CHUNK: usize = 64 * 1024;
const LAST_FLAG: u32 = 1 << 31;
const TAG_LEN: usize = 16;
/// Entropy passed to DPAPI. It is a public constant (it is in this source
/// file), so it is NOT a secret and adds no protection against other programs
/// running as the same user; it only keeps blobs from other applications from
/// being unwrapped by mistake.
const DPAPI_ENTROPY: &[u8] = b"lookout.dev/key/v1";

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("the system random number generator failed")]
    Random,
    #[error("encryption failed")]
    Encrypt,
    #[error("data is corrupt, truncated or was encrypted with a different key")]
    Decrypt,
    #[error("not a Lookout encrypted file")]
    BadMagic,
    #[error("Windows DPAPI refused the key: {0}")]
    Dpapi(String),
    #[error("key file I/O failed: {0}")]
    Io(#[from] io::Error),
}

impl From<CryptoError> for io::Error {
    fn from(e: CryptoError) -> Self {
        match e {
            CryptoError::Io(e) => e,
            other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Key([u8; KEY_LEN]);

// Never print key material.
impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key(..)")
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        self.0 = [0; KEY_LEN];
    }
}

impl Key {
    pub fn generate() -> Result<Self, CryptoError> {
        let mut k = [0u8; KEY_LEN];
        getrandom::fill(&mut k).map_err(|_| CryptoError::Random)?;
        Ok(Self(k))
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        <[u8; KEY_LEN]>::try_from(bytes).ok().map(Self)
    }

    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// For handing a derived key to a plugin over its pipe.
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_hex(text: &str) -> Option<Self> {
        if text.len() != KEY_LEN * 2 || !text.is_ascii() {
            return None;
        }
        let mut k = [0u8; KEY_LEN];
        for (i, b) in k.iter_mut().enumerate() {
            *b = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(Self(k))
    }
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let pad = |x: u8| block.map(|b| b ^ x);
    let mut inner = Sha256::new();
    inner.update(pad(0x36));
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(pad(0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// A key for one plugin: HKDF-Expand (RFC 5869) with HMAC-SHA256, one block, from
/// the master key (already uniformly random, so no extract step) and the plugin
/// id. A plugin that is given this key cannot compute the master key or the key
/// of another plugin.
pub fn derive_plugin_key(master: &Key, plugin_id: &str) -> Key {
    let mut info = b"lookout plugin key v1:".to_vec();
    info.extend_from_slice(plugin_id.as_bytes());
    info.push(1);
    Key(hmac_sha256(master.as_bytes(), &info))
}

// ------------------------------------------------------------------ DPAPI

#[cfg(windows)]
mod dpapi {
    use super::{CryptoError, DPAPI_ENTROPY};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    fn blob(data: &[u8]) -> Result<CRYPT_INTEGER_BLOB, CryptoError> {
        Ok(CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(data.len())
                .map_err(|_| CryptoError::Dpapi("too large".into()))?,
            pbData: data.as_ptr().cast_mut(),
        })
    }

    fn last_error() -> String {
        format!("{}", std::io::Error::last_os_error())
    }

    /// Copies a DPAPI-allocated output blob and frees it.
    ///
    /// # Safety
    /// `out` must have been filled in by a successful DPAPI call.
    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        let v = if out.pbData.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec()
        };
        if !out.pbData.is_null() {
            LocalFree(out.pbData.cast());
        }
        v
    }

    pub fn protect(data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let input = blob(data)?;
        let entropy = blob(DPAPI_ENTROPY)?;
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: all pointers refer to live data for the duration of the call;
        // the output blob is owned by us afterwards and freed in `take`.
        unsafe {
            if CryptProtectData(
                &input,
                std::ptr::null(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            ) == 0
            {
                return Err(CryptoError::Dpapi(last_error()));
            }
            Ok(take(out))
        }
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let input = blob(data)?;
        let entropy = blob(DPAPI_ENTROPY)?;
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: as in `protect`.
        unsafe {
            if CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            ) == 0
            {
                return Err(CryptoError::Dpapi(last_error()));
            }
            Ok(take(out))
        }
    }
}

#[cfg(not(windows))]
mod dpapi {
    use super::CryptoError;
    pub fn protect(_: &[u8]) -> Result<Vec<u8>, CryptoError> {
        Err(CryptoError::Dpapi("DPAPI exists only on Windows".into()))
    }
    pub fn unprotect(_: &[u8]) -> Result<Vec<u8>, CryptoError> {
        Err(CryptoError::Dpapi("DPAPI exists only on Windows".into()))
    }
}

/// Wraps bytes with the current Windows user's DPAPI key.
pub fn dpapi_protect(data: &[u8]) -> Result<Vec<u8>, CryptoError> {
    dpapi::protect(data)
}

pub fn dpapi_unprotect(data: &[u8]) -> Result<Vec<u8>, CryptoError> {
    dpapi::unprotect(data)
}

/// The DPAPI-wrapped key file.
#[derive(Debug, Clone)]
pub struct KeyStore {
    path: PathBuf,
}

impl KeyStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Reads and unwraps the key, creating and storing a new one if there is none.
    /// An existing file that cannot be unwrapped (another Windows user, another
    /// machine, a damaged file) is an error: the data it protected is gone.
    pub fn load_or_create(&self) -> Result<Key, CryptoError> {
        match std::fs::read(&self.path) {
            Ok(wrapped) => {
                let raw = dpapi_unprotect(&wrapped)?;
                Key::from_bytes(&raw).ok_or_else(|| CryptoError::Dpapi("wrong key length".into()))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let key = Key::generate()?;
                if let Some(dir) = self.path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                write_atomic(&self.path, &dpapi_protect(key.as_bytes())?)?;
                Ok(key)
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn delete(&self) -> io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// Writes `data` to a temporary file next to `path` and renames it into place,
/// so a crash never leaves a half-written file.
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

// ---------------------------------------------------------- sealed streams

fn chunk_nonce(prefix: &[u8; 8], counter: u32) -> chacha20poly1305::Nonce {
    let mut n = [0u8; 12];
    n[..8].copy_from_slice(prefix);
    n[8..].copy_from_slice(&counter.to_be_bytes());
    chacha20poly1305::Nonce::from(n)
}

fn cipher(key: &Key) -> ChaCha20Poly1305 {
    ChaCha20Poly1305::new(&chacha20poly1305::Key::from(*key.as_bytes()))
}

/// Encrypts everything written to it. Call [`SealWriter::finish`] to write the
/// final chunk; dropping without it leaves a file that reads as truncated.
pub struct SealWriter<W: Write> {
    inner: W,
    cipher: ChaCha20Poly1305,
    prefix: [u8; 8],
    counter: u32,
    buf: Vec<u8>,
}

impl<W: Write> SealWriter<W> {
    pub fn new(mut inner: W, key: &Key) -> Result<Self, CryptoError> {
        let mut prefix = [0u8; 8];
        getrandom::fill(&mut prefix).map_err(|_| CryptoError::Random)?;
        inner.write_all(MAGIC)?;
        inner.write_all(&prefix)?;
        Ok(Self {
            inner,
            cipher: cipher(key),
            prefix,
            counter: 0,
            buf: Vec::with_capacity(CHUNK),
        })
    }

    fn emit(&mut self, last: bool) -> io::Result<()> {
        let nonce = chunk_nonce(&self.prefix, self.counter);
        self.counter = self.counter.checked_add(1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "too many chunks in one stream")
        })?;
        let sealed = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: &self.buf,
                    aad: &[u8::from(last)],
                },
            )
            .map_err(|_| io::Error::other(CryptoError::Encrypt))?;
        let len = u32::try_from(sealed.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "chunk too large"))?;
        let header = if last { len | LAST_FLAG } else { len };
        self.inner.write_all(&header.to_le_bytes())?;
        self.inner.write_all(&sealed)?;
        self.buf.clear();
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<W> {
        self.emit(true)?;
        self.inner.flush()?;
        Ok(self.inner)
    }
}

impl<W: Write> Write for SealWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        // A full chunk is emitted only once more data arrives, so the final
        // chunk is always written by `finish`.
        if self.buf.len() == CHUNK {
            self.emit(false)?;
        }
        let n = data.len().min(CHUNK - self.buf.len());
        self.buf.extend_from_slice(&data[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Decrypts a stream written by [`SealWriter`], verifying every chunk.
pub struct OpenReader<R: Read> {
    inner: R,
    cipher: ChaCha20Poly1305,
    prefix: [u8; 8],
    counter: u32,
    plain: Vec<u8>,
    pos: usize,
    done: bool,
}

impl<R: Read> OpenReader<R> {
    pub fn new(mut inner: R, key: &Key) -> Result<Self, CryptoError> {
        let mut head = [0u8; 12];
        inner.read_exact(&mut head).map_err(|e| {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                CryptoError::BadMagic
            } else {
                CryptoError::Io(e)
            }
        })?;
        if &head[..4] != MAGIC {
            return Err(CryptoError::BadMagic);
        }
        let mut prefix = [0u8; 8];
        prefix.copy_from_slice(&head[4..]);
        Ok(Self {
            inner,
            cipher: cipher(key),
            prefix,
            counter: 0,
            plain: Vec::new(),
            pos: 0,
            done: false,
        })
    }

    fn next_chunk(&mut self) -> io::Result<()> {
        let mut h = [0u8; 4];
        self.inner
            .read_exact(&mut h)
            .map_err(|_| io::Error::other(CryptoError::Decrypt))?;
        let raw = u32::from_le_bytes(h);
        let last = raw & LAST_FLAG != 0;
        let len = (raw & !LAST_FLAG) as usize;
        if !(TAG_LEN..=CHUNK + TAG_LEN).contains(&len) {
            return Err(io::Error::other(CryptoError::Decrypt));
        }
        let mut sealed = vec![0u8; len];
        self.inner
            .read_exact(&mut sealed)
            .map_err(|_| io::Error::other(CryptoError::Decrypt))?;
        let nonce = chunk_nonce(&self.prefix, self.counter);
        self.counter += 1;
        self.plain = self
            .cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &sealed,
                    aad: &[u8::from(last)],
                },
            )
            .map_err(|_| io::Error::other(CryptoError::Decrypt))?;
        self.pos = 0;
        self.done = last;
        Ok(())
    }
}

impl<R: Read> Read for OpenReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos == self.plain.len() {
            if self.done {
                return Ok(0);
            }
            self.next_chunk()?;
        }
        let n = out.len().min(self.plain.len() - self.pos);
        out[..n].copy_from_slice(&self.plain[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

pub fn seal(key: &Key, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let mut w = SealWriter::new(Vec::with_capacity(data.len() + 64), key)?;
    w.write_all(data)?;
    Ok(w.finish()?)
}

pub fn open(key: &Key, sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let mut r = OpenReader::new(sealed, key)?;
    let mut out = Vec::new();
    r.read_to_end(&mut out).map_err(|e| {
        if e.get_ref().is_some_and(|i| i.is::<CryptoError>()) {
            CryptoError::Decrypt
        } else {
            CryptoError::Io(e)
        }
    })?;
    Ok(out)
}

/// True if `data` starts like a sealed stream (as opposed to a plain file).
pub fn looks_sealed(data: &[u8]) -> bool {
    data.starts_with(MAGIC)
}

// -------------------------------------------------------------- random CTR

pub const CTR_NONCE_LEN: usize = 12;

pub fn random_nonce() -> Result<[u8; CTR_NONCE_LEN], CryptoError> {
    let mut n = [0u8; CTR_NONCE_LEN];
    getrandom::fill(&mut n).map_err(|_| CryptoError::Random)?;
    Ok(n)
}

/// XORs `buf` with the ChaCha20 keystream at byte `offset`. Encrypting and
/// decrypting are the same operation.
pub fn ctr_apply(key: &Key, nonce: &[u8; CTR_NONCE_LEN], offset: u64, buf: &mut [u8]) {
    let mut c = ChaCha20::new(
        &chacha20::Key::from(*key.as_bytes()),
        &chacha20::Nonce::from(*nonce),
    );
    c.seek(offset);
    c.apply_keystream(buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> Key {
        Key::generate().unwrap()
    }

    #[test]
    fn hmac_matches_the_rfc_4231_vector_and_plugin_keys_are_separate() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        let master = key();
        let a = derive_plugin_key(&master, "com.a");
        assert_eq!(a, derive_plugin_key(&master, "com.a"));
        assert_ne!(a, derive_plugin_key(&master, "com.b"));
        assert_ne!(a, master);
        assert_ne!(a, derive_plugin_key(&key(), "com.a"));
        assert_eq!(Key::from_hex(&a.to_hex()), Some(a));
        assert!(Key::from_hex("zz").is_none());
    }

    #[test]
    fn dpapi_round_trips_and_hides_the_plaintext() {
        let secret = b"0123456789abcdef0123456789abcdef";
        let wrapped = dpapi_protect(secret).unwrap();
        assert!(!wrapped.windows(secret.len()).any(|w| w == secret));
        assert_eq!(dpapi_unprotect(&wrapped).unwrap(), secret);
        // A damaged blob is refused rather than decoded to garbage.
        let mut bad = wrapped.clone();
        let mid = bad.len() / 2;
        bad[mid] ^= 0xff;
        assert!(dpapi_unprotect(&bad).is_err());
    }

    #[test]
    fn key_store_creates_once_and_reloads_the_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::new(dir.path().join("lookout.key"));
        assert!(!store.exists());
        let a = store.load_or_create().unwrap();
        assert!(store.exists());
        let on_disk = std::fs::read(store.path()).unwrap();
        assert!(
            !on_disk.windows(KEY_LEN).any(|w| w == a.as_bytes()),
            "the key must not be on disk in the clear"
        );
        assert_eq!(store.load_or_create().unwrap(), a);
        store.delete().unwrap();
        assert!(!store.exists());
        assert_ne!(store.load_or_create().unwrap(), a);
    }

    #[test]
    fn seal_round_trips_across_chunk_boundaries() {
        let k = key();
        for len in [0usize, 1, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK + 17] {
            let data: Vec<u8> = (0..len).map(|i| (i * 7 % 251) as u8).collect();
            let sealed = seal(&k, &data).unwrap();
            assert!(looks_sealed(&sealed));
            assert_eq!(open(&k, &sealed).unwrap(), data, "len {len}");
        }
    }

    #[test]
    fn sealed_data_is_not_readable_without_the_key() {
        let data = b"top secret invoice 4821 ".repeat(100);
        let sealed = seal(&key(), &data).unwrap();
        assert!(!sealed.windows(6).any(|w| w == b"secret"));
        assert!(matches!(open(&key(), &sealed), Err(CryptoError::Decrypt)));
    }

    #[test]
    fn tampering_and_truncation_are_detected() {
        let k = key();
        let data = vec![7u8; 3 * CHUNK];
        let sealed = seal(&k, &data).unwrap();
        let mut flipped = sealed.clone();
        flipped[100] ^= 1;
        assert!(open(&k, &flipped).is_err());
        // Cutting off whole trailing chunks must not read as a shorter valid file.
        let first_chunk_end = 12 + 4 + CHUNK + TAG_LEN;
        assert!(open(&k, &sealed[..first_chunk_end]).is_err());
        assert!(open(&k, &sealed[..sealed.len() - 1]).is_err());
        assert!(open(&k, b"SQLite format 3\0").is_err());
    }

    #[test]
    fn ctr_is_seekable_and_symmetric() {
        let k = key();
        let nonce = random_nonce().unwrap();
        let plain: Vec<u8> = (0..1000u32).map(|i| (i % 256) as u8).collect();
        let mut enc = plain.clone();
        ctr_apply(&k, &nonce, 0, &mut enc);
        assert_ne!(enc, plain);
        let mut part = enc[137..611].to_vec();
        ctr_apply(&k, &nonce, 137, &mut part);
        assert_eq!(part, plain[137..611]);
    }
}
