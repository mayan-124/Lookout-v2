//! A tantivy [`Directory`] that stores every file encrypted.
//!
//! Each file is `nonce (12 bytes) || ChaCha20(content)`. ChaCha20 is a stream
//! cipher with a seekable keystream, so tantivy can still read any byte range
//! without decrypting the whole file, which keeps memory low and search fast.
//! Writes are sequential (tantivy never rewrites part of a file), so they are
//! encrypted as they stream through.
//!
//! This gives confidentiality of the index at rest. It does not authenticate
//! the bytes: someone who can write to the index folder could corrupt it
//! (tantivy's own checksums would notice most of that) but cannot read it.

use std::fmt;
use std::io::{self, BufWriter, Write};
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use tantivy::directory::error::{DeleteError, LockError, OpenReadError, OpenWriteError};
use tantivy::directory::{
    AntiCallToken, Directory, DirectoryLock, FileHandle, Lock, MmapDirectory, OwnedBytes,
    TerminatingWrite, WatchCallback, WatchHandle, WritePtr,
};
use tantivy::HasLen;

use crate::crypto::{ctr_apply, random_nonce, Key, CTR_NONCE_LEN};

#[derive(Clone)]
pub struct EncryptedDirectory {
    inner: MmapDirectory,
    key: Arc<Key>,
}

impl fmt::Debug for EncryptedDirectory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EncryptedDirectory({:?})", self.inner)
    }
}

impl EncryptedDirectory {
    /// Opens `dir` (which must exist).
    pub fn open(
        dir: &Path,
        key: Key,
    ) -> Result<Self, tantivy::directory::error::OpenDirectoryError> {
        Ok(Self {
            inner: MmapDirectory::open(dir)?,
            key: Arc::new(key),
        })
    }
}

fn split_nonce(raw: &[u8]) -> io::Result<([u8; CTR_NONCE_LEN], &[u8])> {
    let (head, body) = raw
        .split_at_checked(CTR_NONCE_LEN)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "encrypted file too short"))?;
    let mut nonce = [0u8; CTR_NONCE_LEN];
    nonce.copy_from_slice(head);
    Ok((nonce, body))
}

#[derive(Debug)]
struct DecryptingHandle {
    inner: Arc<dyn FileHandle>,
    key: Arc<Key>,
    nonce: [u8; CTR_NONCE_LEN],
}

impl HasLen for DecryptingHandle {
    fn len(&self) -> usize {
        self.inner.len().saturating_sub(CTR_NONCE_LEN)
    }
}

impl FileHandle for DecryptingHandle {
    fn read_bytes(&self, range: Range<usize>) -> io::Result<OwnedBytes> {
        let shifted = range.start + CTR_NONCE_LEN..range.end + CTR_NONCE_LEN;
        let raw = self.inner.read_bytes(shifted)?;
        let mut data = raw.as_slice().to_vec();
        ctr_apply(&self.key, &self.nonce, range.start as u64, &mut data);
        Ok(OwnedBytes::new(data))
    }
}

struct EncryptingWriter {
    inner: WritePtr,
    key: Arc<Key>,
    nonce: [u8; CTR_NONCE_LEN],
    pos: u64,
}

impl Write for EncryptingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut data = buf.to_vec();
        ctr_apply(&self.key, &self.nonce, self.pos, &mut data);
        self.inner.write_all(&data)?;
        self.pos += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl TerminatingWrite for EncryptingWriter {
    fn terminate_ref(&mut self, token: AntiCallToken) -> io::Result<()> {
        self.inner.flush()?;
        self.inner.get_mut().terminate_ref(token)
    }
}

impl Directory for EncryptedDirectory {
    fn get_file_handle(&self, path: &Path) -> Result<Arc<dyn FileHandle>, OpenReadError> {
        let inner = self.inner.get_file_handle(path)?;
        let mut nonce = [0u8; CTR_NONCE_LEN];
        if inner.len() >= CTR_NONCE_LEN {
            let head = inner
                .read_bytes(0..CTR_NONCE_LEN)
                .map_err(|e| OpenReadError::wrap_io_error(e, path.to_owned()))?;
            nonce.copy_from_slice(head.as_slice());
        }
        Ok(Arc::new(DecryptingHandle {
            inner,
            key: self.key.clone(),
            nonce,
        }))
    }

    fn delete(&self, path: &Path) -> Result<(), DeleteError> {
        self.inner.delete(path)
    }

    fn exists(&self, path: &Path) -> Result<bool, OpenReadError> {
        self.inner.exists(path)
    }

    fn open_write(&self, path: &Path) -> Result<WritePtr, OpenWriteError> {
        let mut inner = self.inner.open_write(path)?;
        let nonce = random_nonce().map_err(|e| {
            OpenWriteError::wrap_io_error(io::Error::other(e.to_string()), path.to_owned())
        })?;
        inner
            .write_all(&nonce)
            .map_err(|e| OpenWriteError::wrap_io_error(e, path.to_owned()))?;
        Ok(BufWriter::new(Box::new(EncryptingWriter {
            inner,
            key: self.key.clone(),
            nonce,
            pos: 0,
        })))
    }

    fn atomic_read(&self, path: &Path) -> Result<Vec<u8>, OpenReadError> {
        let raw = self.inner.atomic_read(path)?;
        let (nonce, body) =
            split_nonce(&raw).map_err(|e| OpenReadError::wrap_io_error(e, path.to_owned()))?;
        let mut data = body.to_vec();
        ctr_apply(&self.key, &nonce, 0, &mut data);
        Ok(data)
    }

    fn atomic_write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let nonce = random_nonce()?;
        let mut out = Vec::with_capacity(CTR_NONCE_LEN + data.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(data);
        ctr_apply(&self.key, &nonce, 0, &mut out[CTR_NONCE_LEN..]);
        self.inner.atomic_write(path, &out)
    }

    fn sync_directory(&self) -> io::Result<()> {
        self.inner.sync_directory()
    }

    fn watch(&self, callback: WatchCallback) -> tantivy::Result<WatchHandle> {
        self.inner.watch(callback)
    }

    /// The folder's own OS file lock, which the system drops when the process
    /// dies. The default lock is a plain file that a killed process leaves
    /// behind, and then every later start fails with "lock busy".
    fn acquire_lock(&self, lock: &Lock) -> Result<DirectoryLock, LockError> {
        self.inner.acquire_lock(lock)
    }
}
