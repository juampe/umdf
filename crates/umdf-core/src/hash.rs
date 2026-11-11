//! File digests used for exact-duplicate detection.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

/// Number of leading bytes hashed by the cheap pre-filter. The original
/// program compared the first 256 bytes of each file before doing a full
/// content comparison, so we keep the same constant.
pub const PREFIX_LEN: usize = 256;

/// SHA-256 of the first `PREFIX_LEN` bytes.
pub fn prefix_hash(path: &Path) -> anyhow::Result<String> {
    let f = File::open(path)?;
    let mut reader = BufReader::new(f);
    let mut buf = vec![0u8; PREFIX_LEN];
    let n = read_full(&mut reader, &mut buf)?;
    let mut hasher = Sha256::new();
    hasher.update(&buf[..n]);
    Ok(hex::encode(hasher.finalize()))
}

/// Streaming SHA-256 of the whole file. Peak memory is 64 KiB.
pub fn file_hash(path: &Path) -> anyhow::Result<String> {
    let f = File::open(path)?;
    let mut reader = BufReader::with_capacity(64 * 1024, f);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Streaming SHA-256 that checks `cancel` before every 1 MiB block.
///
/// Returns `Ok(None)` when the caller asked to stop; the partially consumed
/// file is abandoned immediately instead of hashing gigabytes to the end.
pub fn file_hash_cancellable(path: &Path, cancel: &AtomicBool) -> anyhow::Result<Option<String>> {
    let f = File::open(path)?;
    let mut reader = BufReader::with_capacity(1 << 20, f);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(Some(hex::encode(hasher.finalize())))
}

fn read_full<R: Read>(r: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = r.read(&mut buf[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

/// Minimal hex encoder so `umdf-core` does not need another dependency.
pub mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let bytes = bytes.as_ref();
        let mut out = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0xf) as usize] as char);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        assert_eq!(hex::encode([0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    }

    #[test]
    fn cancellable_hash_stops_before_reading() {
        use std::sync::atomic::AtomicBool;

        let dir = std::env::temp_dir().join(format!("umdf-hash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.bin");
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(8 * 1024 * 1024).unwrap();
        drop(f);

        let cancelled = AtomicBool::new(true);
        let result = file_hash_cancellable(&path, &cancelled).unwrap();
        assert!(result.is_none(), "cancelled hash must not return a digest");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
