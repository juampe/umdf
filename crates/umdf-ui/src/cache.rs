//! SQLite cache for derived values (fingerprints and semantic embeddings).
//!
//! This database is *only* a cache: deleting it never changes the user's
//! configuration and immediately frees disk space. Configuration lives in
//! `config.toml`; this file is `cache.db` next to it.
//!
//! Cached values are keyed by absolute path and validated against file size,
//! modification time and the grid/model that produced them, so editing or
//! replacing a file invalidates its entry automatically.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use umdf_core::scan::ScanCache;
use umdf_core::Fingerprint;

pub struct CacheDb {
    conn: Mutex<Connection>,
    path: PathBuf,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub fingerprints: u64,
    pub embeddings: u64,
    pub bytes: u64,
}

impl CacheDb {
    /// `<config dir>/umdf/cache.db`.
    pub fn default_path() -> PathBuf {
        let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        base.join("umdf").join("cache.db")
    }

    pub fn open() -> Result<Self> {
        Self::open_at(Self::default_path())
    }

    pub fn open_at(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = Connection::open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS fingerprint_cache (
                 path TEXT PRIMARY KEY,
                 size INTEGER NOT NULL,
                 modified INTEGER,
                 grid INTEGER NOT NULL,
                 mean INTEGER NOT NULL,
                 data BLOB NOT NULL,
                 created_at TEXT NOT NULL DEFAULT (datetime('now'))
             );
             CREATE INDEX IF NOT EXISTS fingerprint_cache_created
                 ON fingerprint_cache(created_at);
             CREATE TABLE IF NOT EXISTS embedding_cache (
                 path TEXT NOT NULL,
                 model TEXT NOT NULL,
                 size INTEGER NOT NULL,
                 modified INTEGER,
                 dims INTEGER NOT NULL,
                 data BLOB NOT NULL,
                 created_at TEXT NOT NULL DEFAULT (datetime('now')),
                 PRIMARY KEY (path, model)
             );
             CREATE INDEX IF NOT EXISTS embedding_cache_created
                 ON embedding_cache(created_at);",
        )
        .context("creating cache schema")?;
        Ok(Self {
            conn: Mutex::new(conn),
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn stats(&self) -> CacheStats {
        let Ok(conn) = self.conn.lock() else {
            return CacheStats::default();
        };
        let fingerprints = conn
            .query_row("SELECT COUNT(*) FROM fingerprint_cache", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap_or(0);
        let embeddings = conn
            .query_row("SELECT COUNT(*) FROM embedding_cache", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap_or(0);
        let fingerprint_bytes = conn
            .query_row(
                "SELECT COALESCE(SUM(LENGTH(data)), 0) FROM fingerprint_cache",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0);
        let embedding_bytes = conn
            .query_row(
                "SELECT COALESCE(SUM(LENGTH(data)), 0) FROM embedding_cache",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0);
        CacheStats {
            fingerprints: fingerprints.max(0) as u64,
            embeddings: embeddings.max(0) as u64,
            bytes: (fingerprint_bytes + embedding_bytes).max(0) as u64,
        }
    }

    /// Drop every cached value. The configuration is untouched.
    pub fn clear(&self) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow::anyhow!("cache lock poisoned"))?;
        conn.execute_batch(
            "DELETE FROM fingerprint_cache;
             DELETE FROM embedding_cache;",
        )?;
        Ok(())
    }

    fn get_fingerprint(
        &self,
        path: &Path,
        size: u64,
        modified: Option<i64>,
        grid: u32,
    ) -> Option<Fingerprint> {
        let conn = self.conn.lock().ok()?;
        let (cached_size, cached_modified, cached_grid, mean, bytes) = conn
            .query_row(
                "SELECT size, modified, grid, mean, data
                 FROM fingerprint_cache WHERE path = ?1",
                params![path.to_string_lossy()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Vec<u8>>(4)?,
                    ))
                },
            )
            .ok()?;
        if cached_size as u64 != size
            || cached_modified != modified
            || cached_grid as u32 != grid
            || cached_grid <= 0
        {
            return None;
        }
        let values = decode_i16(&bytes);
        if values.len() != (cached_grid as usize).pow(2) {
            return None;
        }
        Some(Fingerprint {
            size: cached_grid as u32,
            mean: mean.clamp(i16::MIN as i64, i16::MAX as i64) as i16,
            values,
        })
    }

    fn put_fingerprint(
        &self,
        path: &Path,
        size: u64,
        modified: Option<i64>,
        grid: u32,
        fingerprint: &Fingerprint,
    ) {
        let Ok(conn) = self.conn.lock() else { return };
        let bytes = encode_i16(&fingerprint.values);
        let _ = conn.execute(
            "INSERT INTO fingerprint_cache(path, size, modified, grid, mean, data)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(path) DO UPDATE SET
                 size = excluded.size,
                 modified = excluded.modified,
                 grid = excluded.grid,
                 mean = excluded.mean,
                 data = excluded.data,
                 created_at = datetime('now')",
            params![
                path.to_string_lossy(),
                size as i64,
                modified,
                grid as i64,
                fingerprint.mean as i64,
                bytes
            ],
        );
    }

    fn get_embedding(
        &self,
        path: &Path,
        model: &str,
        size: u64,
        modified: Option<i64>,
    ) -> Option<Vec<f32>> {
        let conn = self.conn.lock().ok()?;
        let (cached_size, cached_modified, dims, bytes) = conn
            .query_row(
                "SELECT size, modified, dims, data
                 FROM embedding_cache WHERE path = ?1 AND model = ?2",
                params![path.to_string_lossy(), model],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                    ))
                },
            )
            .ok()?;
        if cached_size as u64 != size || cached_modified != modified || dims <= 0 {
            return None;
        }
        let values = decode_f32(&bytes);
        if values.len() != dims as usize {
            return None;
        }
        Some(values)
    }

    fn put_embedding(
        &self,
        path: &Path,
        model: &str,
        size: u64,
        modified: Option<i64>,
        values: &[f32],
    ) {
        let Ok(conn) = self.conn.lock() else { return };
        let bytes = encode_f32(values);
        let _ = conn.execute(
            "INSERT INTO embedding_cache(path, model, size, modified, dims, data)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(path, model) DO UPDATE SET
                 size = excluded.size,
                 modified = excluded.modified,
                 dims = excluded.dims,
                 data = excluded.data,
                 created_at = datetime('now')",
            params![
                path.to_string_lossy(),
                model,
                size as i64,
                modified,
                values.len() as i64,
                bytes
            ],
        );
    }
}

impl ScanCache for CacheDb {
    fn fingerprint(
        &self,
        path: &Path,
        size: u64,
        modified_ms: Option<i64>,
        grid: u32,
    ) -> Option<Fingerprint> {
        self.get_fingerprint(path, size, modified_ms, grid)
    }

    fn store_fingerprint(
        &self,
        path: &Path,
        size: u64,
        modified_ms: Option<i64>,
        grid: u32,
        fingerprint: &Fingerprint,
    ) {
        self.put_fingerprint(path, size, modified_ms, grid, fingerprint)
    }

    fn embedding(
        &self,
        path: &Path,
        model: &str,
        size: u64,
        modified_ms: Option<i64>,
    ) -> Option<Vec<f32>> {
        self.get_embedding(path, model, size, modified_ms)
    }

    fn store_embedding(
        &self,
        path: &Path,
        model: &str,
        size: u64,
        modified_ms: Option<i64>,
        values: &[f32],
    ) {
        self.put_embedding(path, model, size, modified_ms, values)
    }
}

fn encode_i16(values: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 2);
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

fn decode_i16(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect()
}

fn encode_f32(values: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

fn decode_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(name: &str) -> (PathBuf, CacheDb) {
        let path = std::env::temp_dir().join(format!(
            "umdf-cache-{}-{}-{name}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = CacheDb::open_at(path.clone()).unwrap();
        (path, db)
    }

    #[test]
    fn fingerprint_round_trip_and_invalidation() {
        let (path, db) = temp_db("fp");
        let image = Path::new("/photos/a.jpg");
        let fingerprint = Fingerprint {
            size: 2,
            mean: -3,
            values: vec![1, -2, 3, -4],
        };
        assert!(db.get_fingerprint(image, 10, Some(100), 2).is_none());
        db.put_fingerprint(image, 10, Some(100), 2, &fingerprint);
        assert_eq!(
            db.get_fingerprint(image, 10, Some(100), 2),
            Some(fingerprint.clone())
        );
        // Different size, mtime or grid must miss.
        assert!(db.get_fingerprint(image, 11, Some(100), 2).is_none());
        assert!(db.get_fingerprint(image, 10, Some(101), 2).is_none());
        assert!(db.get_fingerprint(image, 10, Some(100), 75).is_none());
        let stats = db.stats();
        assert_eq!(stats.fingerprints, 1);
        assert_eq!(stats.bytes, 8);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn embedding_round_trip() {
        let (path, db) = temp_db("emb");
        let image = Path::new("/photos/b.jpg");
        let vector = vec![0.1f32, -0.25, 0.5, 0.75];
        db.put_embedding(image, "/models/clip.onnx", 20, None, &vector);
        assert_eq!(
            db.get_embedding(image, "/models/clip.onnx", 20, None),
            Some(vector.clone())
        );
        assert!(db.get_embedding(image, "/models/other.onnx", 20, None).is_none());
        assert!(db.get_embedding(image, "/models/clip.onnx", 21, None).is_none());
        let stats = db.stats();
        assert_eq!(stats.embeddings, 1);
        assert_eq!(stats.bytes, 16);
        assert_eq!(db.clear().unwrap(), ());
        assert_eq!(db.stats(), CacheStats::default());
        let _ = std::fs::remove_file(path);
    }
}
