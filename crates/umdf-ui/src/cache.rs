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

/// Canonical key for the cache tables.
///
/// Windows paths are case-insensitive and can arrive with or without the
/// extended-length prefix (`\\?\`): the folder browser canonicalises paths,
/// while `config.toml` stores the display form. Normalising both to the same
/// key avoids spurious cache misses when the user reopens the app.
fn normalize_key(text: &str, windows: bool) -> String {
    if !windows {
        return text.to_string();
    }
    let mut key = text.replace('/', "\\");
    if let Some(rest) = key.strip_prefix(r"\\?\UNC\") {
        key = format!(r"\\{rest}");
    } else if let Some(rest) = key.strip_prefix(r"\\?\") {
        key = rest.to_string();
    }
    key.to_lowercase()
}

fn cache_key(path: &Path) -> String {
    normalize_key(&path.to_string_lossy(), cfg!(windows))
}

/// Merge cache entries created with different Windows path spellings
/// (`\\?\B:\...`, `B:/...`, different case) into a single key per file.
///
/// Runs once per database (`PRAGMA user_version`), before the cache is used.
/// The blobs stay in SQLite; only row ids and paths are read into memory.
fn migrate_keys_for(conn: &Connection, windows: bool) -> Result<()> {
    if !windows {
        return Ok(());
    }
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version >= 1 {
        return Ok(());
    }

    fn plan(
        rows: Vec<(i64, String, String)>,
    ) -> (Vec<i64>, Vec<(i64, String)>) {
        use std::collections::HashMap;
        let mut kept: HashMap<String, (i64, String)> = HashMap::new();
        let mut delete = Vec::new();
        for (rowid, path, created) in rows {
            let key = normalize_key(&path, true);
            match kept.get(&key) {
                Some((_, existing)) if existing >= &created => delete.push(rowid),
                Some((old_rowid, _)) => {
                    delete.push(*old_rowid);
                    kept.insert(key, (rowid, created));
                }
                None => {
                    kept.insert(key, (rowid, created));
                }
            }
        }
        let updates = kept
            .into_iter()
            .map(|(key, (rowid, _))| (rowid, key))
            .collect();
        (delete, updates)
    }

    let fingerprint_rows: Vec<(i64, String, String)> = {
        let mut stmt =
            conn.prepare("SELECT rowid, path, created_at FROM fingerprint_cache")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let embedding_rows: Vec<(i64, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT rowid, path, created_at FROM embedding_cache",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    let (fingerprint_delete, fingerprint_update) = plan(fingerprint_rows);
    let (embedding_delete, embedding_update) = plan(embedding_rows);

    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare("DELETE FROM fingerprint_cache WHERE rowid = ?1")?;
        for rowid in fingerprint_delete {
            stmt.execute([rowid])?;
        }
    }
    {
        let mut stmt = tx.prepare("UPDATE fingerprint_cache SET path = ?1 WHERE rowid = ?2")?;
        for (rowid, key) in fingerprint_update {
            stmt.execute(params![key, rowid])?;
        }
    }
    {
        let mut stmt = tx.prepare("DELETE FROM embedding_cache WHERE rowid = ?1")?;
        for rowid in embedding_delete {
            stmt.execute([rowid])?;
        }
    }
    {
        let mut stmt = tx.prepare("UPDATE embedding_cache SET path = ?1 WHERE rowid = ?2")?;
        for (rowid, key) in embedding_update {
            stmt.execute(params![key, rowid])?;
        }
    }
    tx.pragma_update(None, "user_version", 1)?;
    tx.commit()?;
    Ok(())
}

impl CacheDb {
    /// `<config dir>/umdf/cache.db`, except on Windows where it lives in
    /// `%LOCALAPPDATA%` so SQLite is not affected by roaming-profile sync.
    pub fn default_path() -> PathBuf {
        #[cfg(windows)]
        let base = dirs::data_local_dir().or_else(dirs::config_dir);
        #[cfg(not(windows))]
        let base = dirs::config_dir();
        base.unwrap_or_else(|| PathBuf::from("."))
            .join("umdf")
            .join("cache.db")
    }

    pub fn open() -> Result<Self> {
        let path = Self::default_path();
        #[cfg(windows)]
        Self::migrate_legacy_cache(&path);
        Self::open_at(path)
    }

    /// Move a cache created by an older build from `%APPDATA%` (Roaming) to
    /// `%LOCALAPPDATA%`. The cache is disposable, so a failed move is harmless.
    #[cfg(windows)]
    fn migrate_legacy_cache(new_path: &Path) {
        if new_path.exists() {
            return;
        }
        let Some(old_base) = dirs::config_dir() else {
            return;
        };
        let old_path = old_base.join("umdf").join("cache.db");
        if !old_path.is_file() {
            return;
        }
        if let Some(parent) = new_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        for suffix in ["", "-wal", "-shm"] {
            let from = PathBuf::from(format!("{}{suffix}", old_path.display()));
            if !from.is_file() {
                continue;
            }
            let to = PathBuf::from(format!("{}{suffix}", new_path.display()));
            if std::fs::rename(&from, &to).is_err() {
                let _ = std::fs::copy(&from, &to);
                let _ = std::fs::remove_file(&from);
            }
        }
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
        migrate_keys_for(&conn, cfg!(windows)).context("migrating cache keys")?;
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
                params![cache_key(path)],
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
                cache_key(path),
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
                params![cache_key(path), model],
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
                cache_key(path),
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
    fn windows_cache_keys_are_normalized() {
        assert_eq!(
            normalize_key(r"\\?\B:\OwnCloud\Album\A.JPG", true),
            r"b:\owncloud\album\a.jpg"
        );
        assert_eq!(
            normalize_key(r"B:/ownCloud/A.JPG", true),
            r"b:\owncloud\a.jpg"
        );
        assert_eq!(
            normalize_key(r"\\?\UNC\Server\Share\A.JPG", true),
            r"\\server\share\a.jpg"
        );
        assert_eq!(
            normalize_key("/Photos/A.JPG", false),
            "/Photos/A.JPG"
        );
    }

    #[test]
    fn migrates_extended_and_drive_letter_keys() {
        let (path, db) = temp_db("migrate");
        {
            let conn = db.conn.lock().unwrap();
            let data = encode_i16(&[1, 2, 3, 4]);
            conn.execute(
                "INSERT INTO fingerprint_cache(path, size, modified, grid, mean, data, created_at)
                 VALUES(?1, 10, 100, 2, 0, ?2, '2026-01-01 00:00:00')",
                params![r"\\?\B:\Photos\A.JPG", data],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO fingerprint_cache(path, size, modified, grid, mean, data, created_at)
                 VALUES(?1, 10, 100, 2, 0, ?2, '2026-01-02 00:00:00')",
                params![r"B:/Photos/a.jpg", data],
            )
            .unwrap();
            migrate_keys_for(&conn, true).unwrap();
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM fingerprint_cache", [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 1);
            let stored: String = conn
                .query_row("SELECT path FROM fingerprint_cache", [], |row| row.get(0))
                .unwrap();
            assert_eq!(stored, r"b:\photos\a.jpg");
            let version: i64 = conn
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap();
            assert_eq!(version, 1);
        }
        // The stored key is already normalised; on Windows `cache_key` also
        // normalises any spelling of the same file before querying.
        assert!(db
            .get_fingerprint(Path::new(r"b:\photos\a.jpg"), 10, Some(100), 2)
            .is_some());
        let _ = std::fs::remove_file(path);
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
