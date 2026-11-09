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
use umdf_core::{CachedMetadata, Fingerprint};

pub struct CacheDb {
    conn: Mutex<Connection>,
    path: PathBuf,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub fingerprints: u64,
    pub embeddings: u64,
    pub metadata: u64,
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
        rows: Vec<(i64, String, String, String)>,
        include_model: bool,
    ) -> (Vec<i64>, Vec<(i64, String)>) {
        use std::collections::HashMap;
        let mut kept: HashMap<String, (i64, String, String)> = HashMap::new();
        let mut delete = Vec::new();
        for (rowid, path, model, created) in rows {
            let normalized = normalize_key(&path, true);
            // Embeddings are keyed by (path, model); keep each model's entry.
            let key = if include_model {
                format!("{normalized}\u{0}{model}")
            } else {
                normalized.clone()
            };
            match kept.get(&key) {
                Some((_, existing, _)) if existing >= &created => delete.push(rowid),
                Some((old_rowid, _, _)) => {
                    delete.push(*old_rowid);
                    kept.insert(key, (rowid, created, normalized));
                }
                None => {
                    kept.insert(key, (rowid, created, normalized));
                }
            }
        }
        let updates = kept
            .into_values()
            .map(|(rowid, _, normalized)| (rowid, normalized))
            .collect();
        (delete, updates)
    }

    let metadata_rows: Vec<(i64, String, String, String)> = {
        let mut stmt = conn.prepare("SELECT rowid, path, created_at FROM metadata_cache")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, String::new(), row.get(2)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let fingerprint_rows: Vec<(i64, String, String, String)> = {
        let mut stmt =
            conn.prepare("SELECT rowid, path, created_at FROM fingerprint_cache")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, String::new(), row.get(2)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let embedding_rows: Vec<(i64, String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT rowid, path, model, created_at FROM embedding_cache",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    let (fingerprint_delete, fingerprint_update) = plan(fingerprint_rows, false);
    let (embedding_delete, embedding_update) = plan(embedding_rows, true);
    let (metadata_delete, metadata_update) = plan(metadata_rows, false);

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
        let mut stmt = tx.prepare("DELETE FROM metadata_cache WHERE rowid = ?1")?;
        for rowid in metadata_delete {
            stmt.execute([rowid])?;
        }
    }
    {
        let mut stmt = tx.prepare("UPDATE metadata_cache SET path = ?1 WHERE rowid = ?2")?;
        for (rowid, key) in metadata_update {
            stmt.execute(params![key, rowid])?;
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
             PRAGMA temp_store = MEMORY;
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
                 ON embedding_cache(created_at);
             CREATE TABLE IF NOT EXISTS metadata_cache (
                 path TEXT PRIMARY KEY,
                 size INTEGER NOT NULL,
                 modified INTEGER,
                 width INTEGER,
                 height INTEGER,
                 created INTEGER,
                 exif_taken INTEGER,
                 exif_fields INTEGER NOT NULL DEFAULT 0,
                 exif_camera INTEGER NOT NULL DEFAULT 0,
                 duration INTEGER,
                 has_audio INTEGER NOT NULL DEFAULT 0,
                 created_at TEXT NOT NULL DEFAULT (datetime('now'))
             );
             CREATE INDEX IF NOT EXISTS metadata_cache_created
                 ON metadata_cache(created_at);
             CREATE TABLE IF NOT EXISTS content_hash_cache (
                 path TEXT PRIMARY KEY,
                 size INTEGER NOT NULL,
                 modified INTEGER,
                 hash TEXT NOT NULL,
                 created_at TEXT NOT NULL DEFAULT (datetime('now'))
             );
             CREATE TABLE IF NOT EXISTS media_signature_cache (
                 path TEXT NOT NULL,
                 params TEXT NOT NULL,
                 size INTEGER NOT NULL,
                 modified INTEGER,
                 signature TEXT NOT NULL,
                 created_at TEXT NOT NULL DEFAULT (datetime('now')),
                 PRIMARY KEY (path, params)
             );
             CREATE TABLE IF NOT EXISTS video_frame_cache (
                 path TEXT NOT NULL,
                 at_ms INTEGER NOT NULL,
                 grid INTEGER NOT NULL,
                 size INTEGER NOT NULL,
                 modified INTEGER,
                 mean INTEGER NOT NULL,
                 data BLOB NOT NULL,
                 created_at TEXT NOT NULL DEFAULT (datetime('now')),
                 PRIMARY KEY (path, at_ms, grid)
             );",
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
        let metadata = conn
            .query_row("SELECT COUNT(*) FROM metadata_cache", [], |row| {
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
            metadata: metadata.max(0) as u64,
            bytes: (fingerprint_bytes + embedding_bytes).max(0) as u64,
        }
    }

    /// Drop every cached value. The configuration is untouched.
    pub fn clear(&self) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow::anyhow!("cache lock poisoned"))?;
        conn.execute_batch(
            "DELETE FROM fingerprint_cache;
             DELETE FROM embedding_cache;
             DELETE FROM metadata_cache;
             DELETE FROM content_hash_cache;
             DELETE FROM media_signature_cache;
             DELETE FROM video_frame_cache;",
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

    fn get_metadata(
        &self,
        path: &Path,
        size: u64,
        modified: Option<i64>,
    ) -> Option<CachedMetadata> {
        let conn = self.conn.lock().ok()?;
        let row = conn
            .query_row(
                "SELECT size, modified, width, height, created, exif_taken,
                        exif_fields, exif_camera, duration, has_audio
                 FROM metadata_cache WHERE path = ?1",
                params![cache_key(path)],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, Option<i64>>(8)?,
                        row.get::<_, i64>(9)?,
                    ))
                },
            )
            .ok()?;
        let (cached_size, cached_modified, width, height, created, exif_taken,
            exif_fields, exif_camera, duration, has_audio) = row;
        if cached_size as u64 != size || cached_modified != modified {
            return None;
        }
        Some(CachedMetadata {
            width: width.and_then(|v| u32::try_from(v).ok()),
            height: height.and_then(|v| u32::try_from(v).ok()),
            created_ms: created,
            exif_taken_ms: exif_taken,
            exif_fields: exif_fields.max(0) as u32,
            exif_camera: exif_camera != 0,
            duration_ms: duration.and_then(|v| u64::try_from(v).ok()),
            has_audio: has_audio != 0,
        })
    }

    fn put_metadata(
        &self,
        path: &Path,
        size: u64,
        modified: Option<i64>,
        metadata: &CachedMetadata,
    ) {
        let Ok(conn) = self.conn.lock() else { return };
        let _ = conn.execute(
            "INSERT INTO metadata_cache(
                 path, size, modified, width, height, created, exif_taken,
                 exif_fields, exif_camera, duration, has_audio)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(path) DO UPDATE SET
                 size = excluded.size,
                 modified = excluded.modified,
                 width = excluded.width,
                 height = excluded.height,
                 created = excluded.created,
                 exif_taken = excluded.exif_taken,
                 exif_fields = excluded.exif_fields,
                 exif_camera = excluded.exif_camera,
                 duration = excluded.duration,
                 has_audio = excluded.has_audio,
                 created_at = datetime('now')",
            params![
                cache_key(path),
                size as i64,
                modified,
                metadata.width.map(|v| v as i64),
                metadata.height.map(|v| v as i64),
                metadata.created_ms,
                metadata.exif_taken_ms,
                metadata.exif_fields as i64,
                metadata.exif_camera as i64,
                metadata.duration_ms.map(|v| v as i64),
                metadata.has_audio as i64,
            ],
        );
    }

    fn get_content_hash(
        &self,
        path: &Path,
        size: u64,
        modified: Option<i64>,
    ) -> Option<String> {
        let conn = self.conn.lock().ok()?;
        let (cached_size, cached_modified, hash) = conn
            .query_row(
                "SELECT size, modified, hash FROM content_hash_cache WHERE path = ?1",
                params![cache_key(path)],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?, row.get::<_, String>(2)?)),
            )
            .ok()?;
        if cached_size as u64 == size && cached_modified == modified {
            Some(hash)
        } else {
            None
        }
    }

    fn put_content_hash(
        &self,
        path: &Path,
        size: u64,
        modified: Option<i64>,
        hash: &str,
    ) {
        let Ok(conn) = self.conn.lock() else { return };
        let _ = conn.execute(
            "INSERT INTO content_hash_cache(path, size, modified, hash)
             VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET
                 size = excluded.size, modified = excluded.modified,
                 hash = excluded.hash, created_at = datetime('now')",
            params![cache_key(path), size as i64, modified, hash],
        );
    }

    fn get_media_signature(
        &self,
        path: &Path,
        size: u64,
        modified: Option<i64>,
        params_key: &str,
    ) -> Option<String> {
        let conn = self.conn.lock().ok()?;
        let (cached_size, cached_modified, signature) = conn
            .query_row(
                "SELECT size, modified, signature FROM media_signature_cache
                 WHERE path = ?1 AND params = ?2",
                params![cache_key(path), params_key],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?, row.get::<_, String>(2)?)),
            )
            .ok()?;
        if cached_size as u64 == size && cached_modified == modified {
            Some(signature)
        } else {
            None
        }
    }

    fn put_media_signature(
        &self,
        path: &Path,
        size: u64,
        modified: Option<i64>,
        params_key: &str,
        signature: &str,
    ) {
        let Ok(conn) = self.conn.lock() else { return };
        let _ = conn.execute(
            "INSERT INTO media_signature_cache(path, params, size, modified, signature)
             VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path, params) DO UPDATE SET
                 size = excluded.size, modified = excluded.modified,
                 signature = excluded.signature, created_at = datetime('now')",
            params![cache_key(path), params_key, size as i64, modified, signature],
        );
    }

    fn get_video_frame(
        &self,
        path: &Path,
        size: u64,
        modified: Option<i64>,
        at_secs: f64,
        grid: u32,
    ) -> Option<Fingerprint> {
        let at_ms = (at_secs * 1000.0).round() as i64;
        let conn = self.conn.lock().ok()?;
        let (cached_size, cached_modified, mean, bytes) = conn
            .query_row(
                "SELECT size, modified, mean, data FROM video_frame_cache
                 WHERE path = ?1 AND at_ms = ?2 AND grid = ?3",
                params![cache_key(path), at_ms, grid as i64],
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
        if cached_size as u64 != size || cached_modified != modified || grid == 0 {
            return None;
        }
        let values = decode_i16(&bytes);
        if values.len() != (grid as usize).pow(2) {
            return None;
        }
        Some(Fingerprint {
            size: grid,
            mean: mean.clamp(i16::MIN as i64, i16::MAX as i64) as i16,
            values,
        })
    }

    fn put_video_frame(
        &self,
        path: &Path,
        size: u64,
        modified: Option<i64>,
        at_secs: f64,
        grid: u32,
        fingerprint: &Fingerprint,
    ) {
        let Ok(conn) = self.conn.lock() else { return };
        let at_ms = (at_secs * 1000.0).round() as i64;
        let bytes = encode_i16(&fingerprint.values);
        let _ = conn.execute(
            "INSERT INTO video_frame_cache(path, at_ms, grid, size, modified, mean, data)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(path, at_ms, grid) DO UPDATE SET
                 size = excluded.size, modified = excluded.modified,
                 mean = excluded.mean, data = excluded.data,
                 created_at = datetime('now')",
            params![
                cache_key(path),
                at_ms,
                grid as i64,
                size as i64,
                modified,
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

    fn metadata(
        &self,
        path: &Path,
        size: u64,
        modified_ms: Option<i64>,
    ) -> Option<CachedMetadata> {
        self.get_metadata(path, size, modified_ms)
    }

    fn store_metadata(
        &self,
        path: &Path,
        size: u64,
        modified_ms: Option<i64>,
        metadata: &CachedMetadata,
    ) {
        self.put_metadata(path, size, modified_ms, metadata)
    }

    fn content_hash(
        &self,
        path: &Path,
        size: u64,
        modified_ms: Option<i64>,
    ) -> Option<String> {
        self.get_content_hash(path, size, modified_ms)
    }

    fn store_content_hash(
        &self,
        path: &Path,
        size: u64,
        modified_ms: Option<i64>,
        hash: &str,
    ) {
        self.put_content_hash(path, size, modified_ms, hash)
    }

    fn media_signature(
        &self,
        path: &Path,
        size: u64,
        modified_ms: Option<i64>,
        params_key: &str,
    ) -> Option<String> {
        self.get_media_signature(path, size, modified_ms, params_key)
    }

    fn store_media_signature(
        &self,
        path: &Path,
        size: u64,
        modified_ms: Option<i64>,
        params_key: &str,
        signature: &str,
    ) {
        self.put_media_signature(path, size, modified_ms, params_key, signature)
    }

    fn video_frame(
        &self,
        path: &Path,
        size: u64,
        modified_ms: Option<i64>,
        at_secs: f64,
        grid: u32,
    ) -> Option<Fingerprint> {
        self.get_video_frame(path, size, modified_ms, at_secs, grid)
    }

    fn store_video_frame(
        &self,
        path: &Path,
        size: u64,
        modified_ms: Option<i64>,
        at_secs: f64,
        grid: u32,
        fingerprint: &Fingerprint,
    ) {
        self.put_video_frame(path, size, modified_ms, at_secs, grid, fingerprint)
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
            // Same file under two spellings for two different embedding
            // models: the path duplicates must merge, the models must not.
            let blob = encode_f32(&[0.1, 0.2]);
            for (path, model, created) in [
                (r"\\?\B:\Photos\A.JPG", "model-a", "2026-01-01 00:00:00"),
                (r"B:/Photos/a.jpg", "model-a", "2026-01-02 00:00:00"),
                (r"B:\Photos\a.jpg", "model-b", "2026-01-01 00:00:00"),
            ] {
                conn.execute(
                    "INSERT INTO embedding_cache(path, model, size, modified, dims, data, created_at)
                     VALUES(?1, ?2, 10, 100, 2, ?3, ?4)",
                    params![path, model, blob, created],
                )
                .unwrap();
            }
            // `open_at` already migrated the empty database on Windows and
            // set user_version = 1; reset it so the test exercises the merge.
            conn.pragma_update(None, "user_version", 0).unwrap();
            migrate_keys_for(&conn, true).unwrap();
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM fingerprint_cache", [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 1);
            let stored: String = conn
                .query_row("SELECT path FROM fingerprint_cache", [], |row| row.get(0))
                .unwrap();
            assert_eq!(stored, r"b:\photos\a.jpg");
            let embeddings: i64 = conn
                .query_row("SELECT COUNT(*) FROM embedding_cache", [], |row| row.get(0))
                .unwrap();
            assert_eq!(embeddings, 2);
            let models: Vec<String> = {
                let mut stmt = conn
                    .prepare("SELECT model FROM embedding_cache ORDER BY model")
                    .unwrap();
                let rows = stmt
                    .query_map([], |row| row.get(0))
                    .unwrap()
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .unwrap();
                rows
            };
            assert_eq!(models, vec!["model-a", "model-b"]);
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
    fn content_media_and_video_round_trip() {
        let (path, db) = temp_db("content");
        let file = Path::new("/media/a.mp4");

        db.put_content_hash(file, 10, Some(100), "abc123");
        assert_eq!(
            db.get_content_hash(file, 10, Some(100)),
            Some("abc123".to_string())
        );
        assert!(db.get_content_hash(file, 10, Some(101)).is_none());

        db.put_media_signature(file, 10, Some(100), "audio|5|5", "sig");
        assert_eq!(
            db.get_media_signature(file, 10, Some(100), "audio|5|5"),
            Some("sig".to_string())
        );
        assert!(db
            .get_media_signature(file, 10, Some(100), "audio|6|5")
            .is_none());

        let fingerprint = Fingerprint {
            size: 2,
            mean: -2,
            values: vec![1, 2, 3, 4],
        };
        db.put_video_frame(file, 10, Some(100), 1.5, 2, &fingerprint);
        assert_eq!(
            db.get_video_frame(file, 10, Some(100), 1.5, 2),
            Some(fingerprint)
        );
        assert!(db.get_video_frame(file, 10, Some(101), 1.5, 2).is_none());
        assert!(db.get_video_frame(file, 10, Some(100), 1.4, 2).is_none());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn metadata_round_trip_and_invalidation() {
        let (path, db) = temp_db("meta");
        let image = Path::new("/photos/a.jpg");
        let metadata = CachedMetadata {
            width: Some(1600),
            height: Some(1200),
            created_ms: Some(1000),
            exif_taken_ms: Some(2000),
            exif_fields: 12,
            exif_camera: true,
            duration_ms: None,
            has_audio: false,
        };
        assert!(db.get_metadata(image, 10, Some(100)).is_none());
        db.put_metadata(image, 10, Some(100), &metadata);
        let back = db.get_metadata(image, 10, Some(100)).unwrap();
        assert_eq!(back.width, Some(1600));
        assert_eq!(back.height, Some(1200));
        assert_eq!(back.exif_fields, 12);
        assert!(back.exif_camera);
        assert!(db.get_metadata(image, 11, Some(100)).is_none());
        assert!(db.get_metadata(image, 10, Some(101)).is_none());
        assert_eq!(db.stats().metadata, 1);
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
