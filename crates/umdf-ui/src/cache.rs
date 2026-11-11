//! SQLite cache for derived values (fingerprints and semantic embeddings).
//!
//! This database is *only* a cache: deleting it never changes the user's
//! configuration and immediately frees disk space. Configuration lives in
//! `config.toml`; this file is `cache.db` next to it.
//!
//! Cached values are keyed by absolute path and validated against file size,
//! modification time and the grid/model that produced them, so editing or
//! replacing a file invalidates its entry automatically.

use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use umdf_core::scan::ScanCache;
use umdf_core::{CachedComparison, CachedMetadata, ComparisonEntry, Fingerprint, MatchKind};

/// Bump when the fingerprint algorithm or its storage encoding changes.
/// Old rows are ignored and rebuilt once, without deleting the whole cache.
const FP_VERSION: i64 = 2;
/// Rows accumulated by the single writer before it commits a transaction.
const WRITE_BATCH: usize = 256;
/// Maximum time a write waits for a batch before being committed.
const WRITE_INTERVAL: Duration = Duration::from_millis(200);
/// SQLite memory map per connection. With 32x32 fingerprints the whole cache
/// of a 113k-image library fits here, so reads hit the OS page cache.
const MMAP_BYTES: i64 = 1024 * 1024 * 1024;
/// Comparison results are far larger than one fingerprint, so keep the newest
/// few signatures and refuse anything above this size.
const MAX_COMPARISON_BYTES: usize = 64 * 1024 * 1024;
const MAX_COMPARISON_ENTRIES: usize = 1_000_000;
const COMPARISON_KEEP: usize = 4;

#[derive(Clone)]
struct CachedFingerprint {
    size: u64,
    modified: Option<i64>,
    grid: u32,
    fingerprint: Fingerprint,
}

#[derive(Clone)]
struct CachedMetadataEntry {
    size: u64,
    modified: Option<i64>,
    metadata: CachedMetadata,
}

#[derive(Clone)]
struct CachedEmbedding {
    size: u64,
    modified: Option<i64>,
    values: Vec<f32>,
}

#[derive(Clone)]
struct CachedText {
    size: u64,
    modified: Option<i64>,
    value: String,
}

/// In-memory view of the cache for the files of the current scan.
///
/// When `active` is true a missing entry means "not cached", so the worker
/// threads never fall back to SQLite. The single writer updates these maps as
/// it commits, which keeps repeated scans warm without a second disk read.
#[derive(Default)]
struct MemoryCache {
    active: bool,
    fingerprints: HashMap<String, CachedFingerprint>,
    metadata: HashMap<String, CachedMetadataEntry>,
    embeddings: HashMap<(String, String), CachedEmbedding>,
    content_hashes: HashMap<String, CachedText>,
    media_signatures: HashMap<(String, String), CachedText>,
    video_frames: HashMap<(String, i64, u32), CachedFingerprint>,
    failures: HashMap<String, CachedText>,
}

/// One value queued for the single writer thread.
enum WriteOp {
    Fingerprint {
        key: String,
        size: u64,
        modified: Option<i64>,
        grid: u32,
        mean: i16,
        values: Vec<i16>,
    },
    Metadata {
        key: String,
        size: u64,
        modified: Option<i64>,
        metadata: CachedMetadata,
    },
    Failure {
        key: String,
        size: u64,
        modified: Option<i64>,
        reason: String,
    },
    ContentHash {
        key: String,
        size: u64,
        modified: Option<i64>,
        hash: String,
    },
    MediaSignature {
        key: String,
        params: String,
        size: u64,
        modified: Option<i64>,
        signature: String,
    },
    VideoFrame {
        key: String,
        at_ms: i64,
        grid: u32,
        size: u64,
        modified: Option<i64>,
        mean: i16,
        values: Vec<i16>,
    },
    Embedding {
        key: String,
        model: String,
        size: u64,
        modified: Option<i64>,
        values: Vec<f32>,
    },
    Flush(Sender<()>),
    Shutdown,
}

struct WriterHandle {
    tx: std::sync::mpsc::SyncSender<WriteOp>,
    join: Option<JoinHandle<()>>,
}

pub struct CacheDb {
    pool: Mutex<Vec<Connection>>,
    path: PathBuf,
    memory: Arc<RwLock<MemoryCache>>,
    writer: Mutex<Option<WriterHandle>>,
}

/// A connection borrowed from the pool. Returning it on drop keeps SQLite
/// reads concurrent across Rayon workers instead of serialising everything on
/// one mutex.
struct PooledConn<'a> {
    db: &'a CacheDb,
    conn: Option<Connection>,
}

impl Deref for PooledConn<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("pooled connection")
    }
}

impl Drop for PooledConn<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            if let Ok(mut pool) = self.db.pool.lock() {
                if pool.len() < 32 {
                    pool.push(conn);
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub fingerprints: u64,
    pub embeddings: u64,
    pub metadata: u64,
    /// Stored whole-library comparison results.
    pub comparisons: u64,
    pub bytes: u64,
    pub comparison_bytes: u64,
    /// Manual star/out decisions stored by the user.
    pub manual_choices: u64,
}

fn put_text(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&(text.len() as u32).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
}

fn take_u32(data: &[u8], pos: &mut usize) -> Option<u32> {
    let bytes = data.get(*pos..*pos + 4)?;
    *pos += 4;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn take_text(data: &[u8], pos: &mut usize) -> Option<String> {
    let len = take_u32(data, pos)? as usize;
    let bytes = data.get(*pos..*pos + len)?;
    *pos += len;
    String::from_utf8(bytes.to_vec()).ok()
}

fn encode_comparison(entries: &[ComparisonEntry]) -> Vec<u8> {
    let mut out = Vec::with_capacity(entries.len() * 96);
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in entries {
        put_text(&mut out, &entry.a.to_string_lossy());
        put_text(&mut out, &entry.b.to_string_lossy());
        out.extend_from_slice(&entry.similarity.to_le_bytes());
        out.push(match entry.kind {
            MatchKind::Exact => 0,
            MatchKind::Similar => 1,
            MatchKind::MediaContent => 2,
        });
    }
    out
}

fn decode_comparison(data: &[u8]) -> Option<Vec<ComparisonEntry>> {
    let mut pos = 0usize;
    let count = take_u32(data, &mut pos)? as usize;
    if count > MAX_COMPARISON_ENTRIES {
        return None;
    }
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let a = take_text(data, &mut pos)?;
        let b = take_text(data, &mut pos)?;
        let similarity = f32::from_le_bytes(data.get(pos..pos + 4)?.try_into().ok()?);
        pos += 4;
        let kind = match *data.get(pos)? {
            0 => MatchKind::Exact,
            1 => MatchKind::Similar,
            2 => MatchKind::MediaContent,
            _ => return None,
        };
        pos += 1;
        entries.push(ComparisonEntry {
            a: PathBuf::from(a),
            b: PathBuf::from(b),
            similarity,
            kind,
        });
    }
    Some(entries)
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
        let mut stmt = conn.prepare("SELECT rowid, path, created_at FROM fingerprint_cache")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, String::new(), row.get(2)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let embedding_rows: Vec<(i64, String, String, String)> = {
        let mut stmt =
            conn.prepare("SELECT rowid, path, model, created_at FROM embedding_cache")?;
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

fn execute_op(tx: &Connection, op: &WriteOp) -> rusqlite::Result<()> {
    match op {
        WriteOp::Fingerprint {
            key,
            size,
            modified,
            grid,
            mean,
            values,
        } => {
            let bytes = encode_i16(values);
            tx.prepare_cached(
                "INSERT INTO fingerprint_cache(
                     path, size, modified, grid, fp_version, mean, data)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(path) DO UPDATE SET
                     size = excluded.size,
                     modified = excluded.modified,
                     grid = excluded.grid,
                     fp_version = excluded.fp_version,
                     mean = excluded.mean,
                     data = excluded.data,
                     created_at = datetime('now')",
            )?
            .execute(params![
                key,
                *size as i64,
                modified,
                *grid as i64,
                FP_VERSION,
                *mean as i64,
                bytes
            ])?;
        }
        WriteOp::Metadata {
            key,
            size,
            modified,
            metadata,
        } => {
            tx.prepare_cached(
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
            )?
            .execute(params![
                key,
                *size as i64,
                modified,
                metadata.width.map(|v| v as i64),
                metadata.height.map(|v| v as i64),
                metadata.created_ms,
                metadata.exif_taken_ms,
                metadata.exif_fields as i64,
                metadata.exif_camera as i64,
                metadata.duration_ms.map(|v| v as i64),
                metadata.has_audio as i64,
            ])?;
        }
        WriteOp::Failure {
            key,
            size,
            modified,
            reason,
        } => {
            tx.prepare_cached(
                "INSERT INTO failure_cache(path, size, modified, reason)
                 VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(path) DO UPDATE SET
                     size = excluded.size,
                     modified = excluded.modified,
                     reason = excluded.reason,
                     created_at = datetime('now')",
            )?
            .execute(params![key, *size as i64, modified, reason])?;
        }
        WriteOp::ContentHash {
            key,
            size,
            modified,
            hash,
        } => {
            tx.prepare_cached(
                "INSERT INTO content_hash_cache(path, size, modified, hash)
                 VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(path) DO UPDATE SET
                     size = excluded.size,
                     modified = excluded.modified,
                     hash = excluded.hash,
                     created_at = datetime('now')",
            )?
            .execute(params![key, *size as i64, modified, hash])?;
        }
        WriteOp::MediaSignature {
            key,
            params: params_key,
            size,
            modified,
            signature,
        } => {
            tx.prepare_cached(
                "INSERT INTO media_signature_cache(
                     path, params, size, modified, signature)
                 VALUES(?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(path, params) DO UPDATE SET
                     size = excluded.size,
                     modified = excluded.modified,
                     signature = excluded.signature,
                     created_at = datetime('now')",
            )?
            .execute(params![key, params_key, *size as i64, modified, signature])?;
        }
        WriteOp::VideoFrame {
            key,
            at_ms,
            grid,
            size,
            modified,
            mean,
            values,
        } => {
            let bytes = encode_i16(values);
            tx.prepare_cached(
                "INSERT INTO video_frame_cache(
                     path, at_ms, grid, fp_version, size, modified, mean, data)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(path, at_ms, grid) DO UPDATE SET
                     fp_version = excluded.fp_version,
                     size = excluded.size,
                     modified = excluded.modified,
                     mean = excluded.mean,
                     data = excluded.data,
                     created_at = datetime('now')",
            )?
            .execute(params![
                key,
                *at_ms,
                *grid as i64,
                FP_VERSION,
                *size as i64,
                modified,
                *mean as i64,
                bytes
            ])?;
        }
        WriteOp::Embedding {
            key,
            model,
            size,
            modified,
            values,
        } => {
            let bytes = encode_f32(values);
            tx.prepare_cached(
                "INSERT INTO embedding_cache(
                     path, model, size, modified, dims, data)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(path, model) DO UPDATE SET
                     size = excluded.size,
                     modified = excluded.modified,
                     dims = excluded.dims,
                     data = excluded.data,
                     created_at = datetime('now')",
            )?
            .execute(params![
                key,
                model,
                *size as i64,
                modified,
                values.len() as i64,
                bytes
            ])?;
        }
        WriteOp::Flush(_) | WriteOp::Shutdown => {}
    }
    Ok(())
}

fn store_in_memory(memory: &Arc<RwLock<MemoryCache>>, op: &WriteOp) {
    let Ok(mut mem) = memory.write() else { return };
    match op {
        WriteOp::Fingerprint {
            key,
            size,
            modified,
            grid,
            mean,
            values,
        } => {
            mem.fingerprints.insert(
                key.clone(),
                CachedFingerprint {
                    size: *size,
                    modified: *modified,
                    grid: *grid,
                    fingerprint: Fingerprint {
                        size: *grid,
                        mean: *mean,
                        values: values.clone(),
                    },
                },
            );
        }
        WriteOp::Metadata {
            key,
            size,
            modified,
            metadata,
        } => {
            mem.metadata.insert(
                key.clone(),
                CachedMetadataEntry {
                    size: *size,
                    modified: *modified,
                    metadata: metadata.clone(),
                },
            );
        }
        WriteOp::Failure {
            key,
            size,
            modified,
            reason,
        } => {
            mem.failures.insert(
                key.clone(),
                CachedText {
                    size: *size,
                    modified: *modified,
                    value: reason.clone(),
                },
            );
        }
        WriteOp::ContentHash {
            key,
            size,
            modified,
            hash,
        } => {
            mem.content_hashes.insert(
                key.clone(),
                CachedText {
                    size: *size,
                    modified: *modified,
                    value: hash.clone(),
                },
            );
        }
        WriteOp::MediaSignature {
            key,
            params,
            size,
            modified,
            signature,
        } => {
            mem.media_signatures.insert(
                (key.clone(), params.clone()),
                CachedText {
                    size: *size,
                    modified: *modified,
                    value: signature.clone(),
                },
            );
        }
        WriteOp::VideoFrame {
            key,
            at_ms,
            grid,
            size,
            modified,
            mean,
            values,
        } => {
            mem.video_frames.insert(
                (key.clone(), *at_ms, *grid),
                CachedFingerprint {
                    size: *size,
                    modified: *modified,
                    grid: *grid,
                    fingerprint: Fingerprint {
                        size: *grid,
                        mean: *mean,
                        values: values.clone(),
                    },
                },
            );
        }
        WriteOp::Embedding {
            key,
            model,
            size,
            modified,
            values,
        } => {
            mem.embeddings.insert(
                (key.clone(), model.clone()),
                CachedEmbedding {
                    size: *size,
                    modified: *modified,
                    values: values.clone(),
                },
            );
        }
        WriteOp::Flush(_) | WriteOp::Shutdown => {}
    }
}

fn commit_ops(conn: &mut Connection, ops: &[WriteOp], memory: &Arc<RwLock<MemoryCache>>) {
    if ops.is_empty() {
        return;
    }
    let result = (|| -> rusqlite::Result<()> {
        // The writer is the only thread that writes, so the unchecked
        // transaction needs no runtime borrow guard and lets the prepared
        // statement cache live on the connection.
        let tx = conn.unchecked_transaction()?;
        for op in ops {
            execute_op(&tx, op)?;
        }
        tx.commit()
    })();
    if let Err(error) = result {
        log_writer_error(&error);
        return;
    }
    // Keep the warm in-memory view in sync with the committed batch.
    for op in ops {
        store_in_memory(memory, op);
    }
}

fn log_writer_error(error: &rusqlite::Error) {
    umdf_core::log_error(format_args!("UMDF: cache writer error: {error}"));
}

/// Single writer thread: every insert/update goes through this channel and is
/// committed in batches, so the extraction workers never wait for SQLite.
fn writer_loop(path: PathBuf, rx: Receiver<WriteOp>, memory: Arc<RwLock<MemoryCache>>) {
    let mut conn = match CacheDb::open_connection(&path) {
        Ok(conn) => conn,
        Err(error) => {
            umdf_core::log_error(format_args!(
                "UMDF: cache writer could not open {}: {error}",
                path.display()
            ));
            return;
        }
    };
    let _ = conn.busy_timeout(Duration::from_secs(30));
    let mut pending: Vec<WriteOp> = Vec::with_capacity(WRITE_BATCH);
    loop {
        match rx.recv_timeout(WRITE_INTERVAL) {
            Ok(WriteOp::Shutdown) => {
                commit_ops(&mut conn, &pending, &memory);
                break;
            }
            Ok(WriteOp::Flush(ack)) => {
                commit_ops(&mut conn, &pending, &memory);
                pending.clear();
                let _ = ack.send(());
            }
            Ok(op) => {
                pending.push(op);
                if pending.len() >= WRITE_BATCH {
                    commit_ops(&mut conn, &pending, &memory);
                    pending.clear();
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if !pending.is_empty() {
                    commit_ops(&mut conn, &pending, &memory);
                    pending.clear();
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                commit_ops(&mut conn, &pending, &memory);
                break;
            }
        }
    }
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

    fn open_connection(path: &Path) -> Result<Connection> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        // WAL is a database property: set it once in `open_at` instead of
        // acquiring a lock every time a pooled connection is created.
        conn.execute_batch(
            "PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;
             PRAGMA busy_timeout = 5000;",
        )?;
        // Best effort: a filesystem that cannot memory-map the database must
        // not prevent the cache from opening.
        let _ = conn.execute_batch(&format!(
            "PRAGMA mmap_size = {MMAP_BYTES};
             PRAGMA cache_size = -16384;"
        ));
        Ok(conn)
    }

    fn checkout(&self) -> Option<PooledConn<'_>> {
        let conn = self.pool.lock().ok()?.pop();
        let conn = match conn {
            Some(conn) => conn,
            None => Self::open_connection(&self.path).ok()?,
        };
        Some(PooledConn {
            db: self,
            conn: Some(conn),
        })
    }

    pub fn open_at(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = Self::open_connection(&path)?;
        conn.execute_batch("PRAGMA journal_mode = WAL;")?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;
             CREATE TABLE IF NOT EXISTS fingerprint_cache (
                 path TEXT PRIMARY KEY,
                 size INTEGER NOT NULL,
                 modified INTEGER,
                 grid INTEGER NOT NULL,
                 fp_version INTEGER NOT NULL DEFAULT 1,
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
             CREATE TABLE IF NOT EXISTS failure_cache (
                 path TEXT PRIMARY KEY,
                 size INTEGER NOT NULL,
                 modified INTEGER,
                 reason TEXT NOT NULL,
                 created_at TEXT NOT NULL DEFAULT (datetime('now'))
             );
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
                 fp_version INTEGER NOT NULL DEFAULT 1,
                 size INTEGER NOT NULL,
                 modified INTEGER,
                 mean INTEGER NOT NULL,
                 data BLOB NOT NULL,
                 created_at TEXT NOT NULL DEFAULT (datetime('now')),
                 PRIMARY KEY (path, at_ms, grid)
             );
             CREATE TABLE IF NOT EXISTS comparison_cache (
                 signature TEXT PRIMARY KEY,
                 pairs INTEGER NOT NULL,
                 matches BLOB NOT NULL,
                 created_at TEXT NOT NULL DEFAULT (datetime('now'))
             );
             CREATE INDEX IF NOT EXISTS comparison_cache_created
                 ON comparison_cache(created_at);
             CREATE TABLE IF NOT EXISTS manual_choice_cache (
                 path TEXT PRIMARY KEY,
                 choice TEXT NOT NULL,
                 updated_at TEXT NOT NULL DEFAULT (datetime('now'))
             );",
        )
        .context("creating cache schema")?;
        // Existing databases predate `fp_version`. Adding the column with a
        // default of 1 makes every old row miss FP_VERSION (2) once, so the
        // fingerprints are rebuilt with the new grid/algorithm.
        let _ = conn.execute_batch(
            "ALTER TABLE fingerprint_cache
                 ADD COLUMN fp_version INTEGER NOT NULL DEFAULT 1;",
        );
        let _ = conn.execute_batch(
            "ALTER TABLE video_frame_cache
                 ADD COLUMN fp_version INTEGER NOT NULL DEFAULT 1;",
        );
        migrate_keys_for(&conn, cfg!(windows)).context("migrating cache keys")?;

        // Bounded: if the disk cannot keep up, extraction waits instead of
        // queueing hundreds of megabytes of fingerprints in RAM.
        let (write_tx, write_rx) = mpsc::sync_channel(8192);
        let memory = Arc::new(RwLock::new(MemoryCache::default()));
        let writer_memory = memory.clone();
        let writer_path = path.clone();
        let join = std::thread::Builder::new()
            .name("umdf-cache-writer".to_string())
            .spawn(move || writer_loop(writer_path, write_rx, writer_memory))
            .context("starting cache writer")?;

        Ok(Self {
            pool: Mutex::new(vec![conn]),
            path,
            memory,
            writer: Mutex::new(Some(WriterHandle {
                tx: write_tx,
                join: Some(join),
            })),
        })
    }

    fn enqueue(&self, op: WriteOp) {
        if let Ok(guard) = self.writer.lock() {
            if let Some(writer) = guard.as_ref() {
                let _ = writer.tx.send(op);
            }
        }
    }

    /// Commit every queued write and wait for the writer to finish the batch.
    pub fn flush(&self) {
        let Ok(guard) = self.writer.lock() else {
            return;
        };
        let Some(writer) = guard.as_ref() else { return };
        let (ack_tx, ack_rx) = mpsc::channel();
        if writer.tx.send(WriteOp::Flush(ack_tx)).is_ok() {
            let _ = ack_rx.recv_timeout(Duration::from_secs(30));
        }
    }

    /// Load the cache index and blobs for these files into memory.
    ///
    /// A 32x32 fingerprint is 2 KB, so warming a 113k-image library costs
    /// roughly 226 MB of RAM and removes every fingerprint query from the
    /// extraction loop. Rows that changed (size, mtime, grid or version) are
    /// not loaded, so the workers recompute them once.
    pub fn preload(&self, paths: &[PathBuf], model: Option<&str>) {
        if paths.is_empty() {
            return;
        }
        let started = std::time::Instant::now();
        let wanted: HashSet<String> = paths.iter().map(|path| cache_key(path)).collect();
        // Re-scans in the same session already have the warm view. Keep it
        // unless the folder changed substantially; missing entries still miss
        // and are recomputed normally.
        if let Ok(current) = self.memory.read() {
            if current.active {
                let covered = wanted
                    .iter()
                    .filter(|key| {
                        current.fingerprints.contains_key(*key)
                            || current.metadata.contains_key(*key)
                            || current.failures.contains_key(*key)
                    })
                    .count();
                if covered * 10 >= wanted.len() * 9 {
                    drop(current);
                    umdf_core::log_error(format_args!(
                        "UMDF: cache preload: reusing warm view for {} files ({} cached), {} ms",
                        wanted.len(),
                        covered,
                        started.elapsed().as_millis()
                    ));
                    return;
                }
            }
        }
        // Invalidate the previous warm view before loading the new one, so a
        // failed preload falls back to SQLite instead of using stale entries.
        if let Ok(mut current) = self.memory.write() {
            *current = MemoryCache::default();
        }
        let Some(conn) = self.checkout() else { return };
        let mut memory = MemoryCache {
            active: true,
            ..MemoryCache::default()
        };
        // A fresh or cleared cache has nothing to warm. Skipping the chunked
        // queries saves a few thousand round trips on the first scan.
        let any_cached = [
            "fingerprint_cache",
            "metadata_cache",
            "embedding_cache",
            "content_hash_cache",
            "media_signature_cache",
            "video_frame_cache",
            "failure_cache",
        ]
        .iter()
        .any(|table| {
            conn.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {table})"),
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0)
                != 0
        });
        if !any_cached {
            if let Ok(mut current) = self.memory.write() {
                *current = memory;
            }
            umdf_core::log_error(format_args!(
                "UMDF: cache preload: {} candidates, empty cache, {} ms",
                paths.len(),
                started.elapsed().as_millis()
            ));
            return;
        }
        // Keep `IN (...)` lists well below SQLite's variable limit.
        const CHUNK: usize = 400;
        let keys: Vec<&String> = wanted.iter().collect();
        for chunk in keys.chunks(CHUNK) {
            let placeholders = vec!["?"; chunk.len()].join(",");

            let sql = format!(
                "SELECT path, size, modified, grid, mean, data
                 FROM fingerprint_cache
                 WHERE path IN ({placeholders}) AND fp_version = ?{}",
                chunk.len() + 1
            );
            if let Ok(mut stmt) = conn.prepare(&sql) {
                let mut binds: Vec<&dyn rusqlite::ToSql> = chunk
                    .iter()
                    .map(|key| *key as &dyn rusqlite::ToSql)
                    .collect();
                binds.push(&FP_VERSION);
                if let Ok(rows) = stmt.query_map(rusqlite::params_from_iter(binds), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Vec<u8>>(5)?,
                    ))
                }) {
                    for row in rows.flatten() {
                        let (key, size, modified, grid, mean, bytes) = row;
                        if grid <= 0 {
                            continue;
                        }
                        let values = decode_i16(&bytes);
                        if values.len() != (grid as usize).pow(2) {
                            continue;
                        }
                        memory.fingerprints.insert(
                            key,
                            CachedFingerprint {
                                size: size.max(0) as u64,
                                modified,
                                grid: grid as u32,
                                fingerprint: Fingerprint {
                                    size: grid as u32,
                                    mean: mean.clamp(i16::MIN as i64, i16::MAX as i64) as i16,
                                    values,
                                },
                            },
                        );
                    }
                }
            }

            let sql = format!(
                "SELECT path, size, modified, width, height, created, exif_taken,
                        exif_fields, exif_camera, duration, has_audio
                 FROM metadata_cache WHERE path IN ({placeholders})"
            );
            if let Ok(mut stmt) = conn.prepare(&sql) {
                if let Ok(rows) =
                    stmt.query_map(rusqlite::params_from_iter(chunk.iter().copied()), |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, Option<i64>>(2)?,
                            row.get::<_, Option<i64>>(3)?,
                            row.get::<_, Option<i64>>(4)?,
                            row.get::<_, Option<i64>>(5)?,
                            row.get::<_, Option<i64>>(6)?,
                            row.get::<_, i64>(7)?,
                            row.get::<_, i64>(8)?,
                            row.get::<_, Option<i64>>(9)?,
                            row.get::<_, i64>(10)?,
                        ))
                    })
                {
                    for row in rows.flatten() {
                        let (
                            key,
                            size,
                            modified,
                            width,
                            height,
                            created,
                            exif_taken,
                            exif_fields,
                            exif_camera,
                            duration,
                            has_audio,
                        ) = row;
                        memory.metadata.insert(
                            key,
                            CachedMetadataEntry {
                                size: size.max(0) as u64,
                                modified,
                                metadata: CachedMetadata {
                                    width: width.and_then(|v| u32::try_from(v).ok()),
                                    height: height.and_then(|v| u32::try_from(v).ok()),
                                    created_ms: created,
                                    exif_taken_ms: exif_taken,
                                    exif_fields: exif_fields.max(0) as u32,
                                    exif_camera: exif_camera != 0,
                                    duration_ms: duration.and_then(|v| u64::try_from(v).ok()),
                                    has_audio: has_audio != 0,
                                },
                            },
                        );
                    }
                }
            }

            // Only the active model is useful to this scan; loading every
            // historical embedding would multiply memory for no benefit.
            if let Some(active_model) = model {
                let sql = format!(
                    "SELECT path, model, size, modified, dims, data
                     FROM embedding_cache
                     WHERE path IN ({placeholders}) AND model = ?{}",
                    chunk.len() + 1
                );
                if let Ok(mut stmt) = conn.prepare(&sql) {
                    let mut binds: Vec<&dyn rusqlite::ToSql> = chunk
                        .iter()
                        .map(|key| *key as &dyn rusqlite::ToSql)
                        .collect();
                    binds.push(&active_model);
                    if let Ok(rows) = stmt.query_map(rusqlite::params_from_iter(binds), |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, Option<i64>>(3)?,
                            row.get::<_, i64>(4)?,
                            row.get::<_, Vec<u8>>(5)?,
                        ))
                    }) {
                        for row in rows.flatten() {
                            let (key, model, size, modified, dims, bytes) = row;
                            if dims <= 0 {
                                continue;
                            }
                            let values = decode_f32(&bytes);
                            if values.len() != dims as usize {
                                continue;
                            }
                            memory.embeddings.insert(
                                (key, model),
                                CachedEmbedding {
                                    size: size.max(0) as u64,
                                    modified,
                                    values,
                                },
                            );
                        }
                    }
                }
            }

            let sql = format!(
                "SELECT path, size, modified, hash
                 FROM content_hash_cache WHERE path IN ({placeholders})"
            );
            if let Ok(mut stmt) = conn.prepare(&sql) {
                if let Ok(rows) =
                    stmt.query_map(rusqlite::params_from_iter(chunk.iter().copied()), |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, Option<i64>>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    })
                {
                    for row in rows.flatten() {
                        let (key, size, modified, hash) = row;
                        memory.content_hashes.insert(
                            key,
                            CachedText {
                                size: size.max(0) as u64,
                                modified,
                                value: hash,
                            },
                        );
                    }
                }
            }

            let sql = format!(
                "SELECT path, size, modified, reason
                 FROM failure_cache WHERE path IN ({placeholders})"
            );
            if let Ok(mut stmt) = conn.prepare(&sql) {
                if let Ok(rows) =
                    stmt.query_map(rusqlite::params_from_iter(chunk.iter().copied()), |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, Option<i64>>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    })
                {
                    for row in rows.flatten() {
                        let (key, size, modified, reason) = row;
                        memory.failures.insert(
                            key,
                            CachedText {
                                size: size.max(0) as u64,
                                modified,
                                value: reason,
                            },
                        );
                    }
                }
            }

            let sql = format!(
                "SELECT path, at_ms, grid, size, modified, mean, data
                 FROM video_frame_cache
                 WHERE path IN ({placeholders}) AND fp_version = ?{}",
                chunk.len() + 1
            );
            if let Ok(mut stmt) = conn.prepare(&sql) {
                let mut binds: Vec<&dyn rusqlite::ToSql> = chunk
                    .iter()
                    .map(|key| *key as &dyn rusqlite::ToSql)
                    .collect();
                binds.push(&FP_VERSION);
                if let Ok(rows) = stmt.query_map(rusqlite::params_from_iter(binds), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, Vec<u8>>(6)?,
                    ))
                }) {
                    for row in rows.flatten() {
                        let (key, at_ms, grid, size, modified, mean, bytes) = row;
                        if grid <= 0 {
                            continue;
                        }
                        let values = decode_i16(&bytes);
                        if values.len() != (grid as usize).pow(2) {
                            continue;
                        }
                        memory.video_frames.insert(
                            (key, at_ms, grid as u32),
                            CachedFingerprint {
                                size: size.max(0) as u64,
                                modified,
                                grid: grid as u32,
                                fingerprint: Fingerprint {
                                    size: grid as u32,
                                    mean: mean.clamp(i16::MIN as i64, i16::MAX as i64) as i16,
                                    values,
                                },
                            },
                        );
                    }
                }
            }

            let sql = format!(
                "SELECT path, params, size, modified, signature
                 FROM media_signature_cache WHERE path IN ({placeholders})"
            );
            if let Ok(mut stmt) = conn.prepare(&sql) {
                if let Ok(rows) =
                    stmt.query_map(rusqlite::params_from_iter(chunk.iter().copied()), |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, Option<i64>>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    })
                {
                    for row in rows.flatten() {
                        let (key, params, size, modified, signature) = row;
                        memory.media_signatures.insert(
                            (key, params),
                            CachedText {
                                size: size.max(0) as u64,
                                modified,
                                value: signature,
                            },
                        );
                    }
                }
            }
        }
        umdf_core::log_error(format_args!(
            "UMDF: cache preload: {} candidates, {} fingerprints, {} metadata, {} embeddings, {} ms",
            paths.len(),
            memory.fingerprints.len(),
            memory.metadata.len(),
            memory.embeddings.len(),
            started.elapsed().as_millis()
        ));
        if let Ok(mut current) = self.memory.write() {
            *current = memory;
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn stats(&self) -> CacheStats {
        let Some(conn) = self.checkout() else {
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
        let comparisons = conn
            .query_row("SELECT COUNT(*) FROM comparison_cache", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap_or(0);
        let comparison_bytes = conn
            .query_row(
                "SELECT COALESCE(SUM(LENGTH(matches)), 0) FROM comparison_cache",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0);
        let manual_choices = conn
            .query_row("SELECT COUNT(*) FROM manual_choice_cache", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap_or(0);
        CacheStats {
            fingerprints: fingerprints.max(0) as u64,
            embeddings: embeddings.max(0) as u64,
            metadata: metadata.max(0) as u64,
            comparisons: comparisons.max(0) as u64,
            bytes: (fingerprint_bytes + embedding_bytes + comparison_bytes).max(0) as u64,
            comparison_bytes: comparison_bytes.max(0) as u64,
            manual_choices: manual_choices.max(0) as u64,
        }
    }

    /// Manual star/out decisions stored by the user.
    ///
    /// Returns `(references, out)`. Both sets use the same normalised
    /// `umdf_core::config::path_key` as the scan, so Windows case and
    /// `\\?\` prefixes do not create duplicate decisions.
    pub fn manual_choices(&self) -> (HashSet<String>, HashSet<String>) {
        let mut references = HashSet::new();
        let mut out = HashSet::new();
        let Some(conn) = self.checkout() else {
            return (references, out);
        };
        let Ok(mut stmt) = conn.prepare("SELECT path, choice FROM manual_choice_cache") else {
            return (references, out);
        };
        let Ok(rows) = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) else {
            return (references, out);
        };
        for row in rows.flatten() {
            match row.1.as_str() {
                "reference" => {
                    references.insert(row.0);
                }
                "out" => {
                    out.insert(row.0);
                }
                _ => {}
            }
        }
        (references, out)
    }

    /// Store or clear a starred group reference.
    pub fn set_manual_reference(&self, path: &str, on: bool) -> Result<()> {
        self.set_manual_choice(path, "reference", on)
    }

    /// Store or clear a manual "remove from group" decision. The file is
    /// never deleted or moved.
    pub fn set_manual_out(&self, path: &str, on: bool) -> Result<()> {
        self.set_manual_choice(path, "out", on)
    }

    fn set_manual_choice(&self, path: &str, choice: &str, on: bool) -> Result<()> {
        self.flush();
        let conn = self
            .checkout()
            .ok_or_else(|| anyhow::anyhow!("cache connection unavailable"))?;
        if on {
            // A file is either the reference or out of its group, never both.
            conn.execute(
                "INSERT INTO manual_choice_cache (path, choice, updated_at)
                 VALUES (?1, ?2, datetime('now'))
                 ON CONFLICT(path) DO UPDATE SET
                     choice = excluded.choice,
                     updated_at = excluded.updated_at",
                params![path, choice],
            )?;
        } else {
            conn.execute(
                "DELETE FROM manual_choice_cache WHERE path = ?1 AND choice = ?2",
                params![path, choice],
            )?;
        }
        Ok(())
    }

    /// Drop only the cached whole-library comparison results. Fingerprints,
    /// embeddings and metadata are kept, so the next scan still extracts
    /// instantly but recomputes the pairs.
    pub fn clear_comparison(&self) -> Result<()> {
        self.flush();
        let conn = self
            .checkout()
            .ok_or_else(|| anyhow::anyhow!("cache connection unavailable"))?;
        conn.execute_batch("DELETE FROM comparison_cache;")?;
        Ok(())
    }

    /// Drop every cached value. The configuration is untouched.
    pub fn clear(&self) -> Result<()> {
        self.flush();
        let conn = self
            .checkout()
            .ok_or_else(|| anyhow::anyhow!("cache connection unavailable"))?;
        conn.execute_batch(
            "DELETE FROM fingerprint_cache;
             DELETE FROM embedding_cache;
             DELETE FROM metadata_cache;
             DELETE FROM failure_cache;
             DELETE FROM content_hash_cache;
             DELETE FROM media_signature_cache;
             DELETE FROM video_frame_cache;
             DELETE FROM comparison_cache;
             DELETE FROM manual_choice_cache;",
        )?;
        if let Ok(mut memory) = self.memory.write() {
            *memory = MemoryCache::default();
        }
        Ok(())
    }

    fn get_fingerprint(
        &self,
        path: &Path,
        size: u64,
        modified: Option<i64>,
        grid: u32,
    ) -> Option<Fingerprint> {
        let key = cache_key(path);
        if let Ok(mem) = self.memory.read() {
            if mem.active {
                return mem
                    .fingerprints
                    .get(&key)
                    .filter(|entry| {
                        entry.size == size && entry.modified == modified && entry.grid == grid
                    })
                    .map(|entry| entry.fingerprint.clone());
            }
        }
        let conn = self.checkout()?;
        let (cached_size, cached_modified, cached_grid, version, mean, bytes) = conn
            .query_row(
                "SELECT size, modified, grid, fp_version, mean, data
                 FROM fingerprint_cache WHERE path = ?1",
                params![key],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Vec<u8>>(5)?,
                    ))
                },
            )
            .ok()?;
        if cached_size as u64 != size
            || cached_modified != modified
            || cached_grid as u32 != grid
            || cached_grid <= 0
            || version != FP_VERSION
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
        self.enqueue(WriteOp::Fingerprint {
            key: cache_key(path),
            size,
            modified,
            grid,
            mean: fingerprint.mean,
            values: fingerprint.values.clone(),
        });
    }

    fn get_metadata(
        &self,
        path: &Path,
        size: u64,
        modified: Option<i64>,
    ) -> Option<CachedMetadata> {
        let key = cache_key(path);
        if let Ok(mem) = self.memory.read() {
            if mem.active {
                return mem
                    .metadata
                    .get(&key)
                    .filter(|entry| entry.size == size && entry.modified == modified)
                    .map(|entry| entry.metadata.clone());
            }
        }
        let conn = self.checkout()?;
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
        let (
            cached_size,
            cached_modified,
            width,
            height,
            created,
            exif_taken,
            exif_fields,
            exif_camera,
            duration,
            has_audio,
        ) = row;
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
        self.enqueue(WriteOp::Metadata {
            key: cache_key(path),
            size,
            modified,
            metadata: metadata.clone(),
        });
    }

    fn get_failure(&self, path: &Path, size: u64, modified: Option<i64>) -> Option<String> {
        let key = cache_key(path);
        if let Ok(mem) = self.memory.read() {
            if mem.active {
                return mem
                    .failures
                    .get(&key)
                    .filter(|entry| entry.size == size && entry.modified == modified)
                    .map(|entry| entry.value.clone());
            }
        }
        let conn = self.checkout()?;
        let (cached_size, cached_modified, reason) = conn
            .query_row(
                "SELECT size, modified, reason FROM failure_cache WHERE path = ?1",
                params![key],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .ok()?;
        if cached_size as u64 == size && cached_modified == modified {
            Some(reason)
        } else {
            None
        }
    }

    fn put_failure(&self, path: &Path, size: u64, modified: Option<i64>, reason: &str) {
        self.enqueue(WriteOp::Failure {
            key: cache_key(path),
            size,
            modified,
            reason: reason.to_string(),
        });
    }

    fn get_content_hash(&self, path: &Path, size: u64, modified: Option<i64>) -> Option<String> {
        let key = cache_key(path);
        if let Ok(mem) = self.memory.read() {
            if mem.active {
                return mem
                    .content_hashes
                    .get(&key)
                    .filter(|entry| entry.size == size && entry.modified == modified)
                    .map(|entry| entry.value.clone());
            }
        }
        let conn = self.checkout()?;
        let (cached_size, cached_modified, hash) = conn
            .query_row(
                "SELECT size, modified, hash FROM content_hash_cache WHERE path = ?1",
                params![key],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .ok()?;
        if cached_size as u64 == size && cached_modified == modified {
            Some(hash)
        } else {
            None
        }
    }

    fn put_content_hash(&self, path: &Path, size: u64, modified: Option<i64>, hash: &str) {
        self.enqueue(WriteOp::ContentHash {
            key: cache_key(path),
            size,
            modified,
            hash: hash.to_string(),
        });
    }

    fn get_media_signature(
        &self,
        path: &Path,
        size: u64,
        modified: Option<i64>,
        params_key: &str,
    ) -> Option<String> {
        let key = cache_key(path);
        if let Ok(mem) = self.memory.read() {
            if mem.active {
                return mem
                    .media_signatures
                    .get(&(key, params_key.to_string()))
                    .filter(|entry| entry.size == size && entry.modified == modified)
                    .map(|entry| entry.value.clone());
            }
        }
        let conn = self.checkout()?;
        let (cached_size, cached_modified, signature) = conn
            .query_row(
                "SELECT size, modified, signature FROM media_signature_cache
                 WHERE path = ?1 AND params = ?2",
                params![key, params_key],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
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
        self.enqueue(WriteOp::MediaSignature {
            key: cache_key(path),
            params: params_key.to_string(),
            size,
            modified,
            signature: signature.to_string(),
        });
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
        let key = cache_key(path);
        if let Ok(mem) = self.memory.read() {
            if mem.active {
                return mem
                    .video_frames
                    .get(&(key.clone(), at_ms, grid))
                    .filter(|entry| {
                        entry.size == size && entry.modified == modified && entry.grid == grid
                    })
                    .map(|entry| entry.fingerprint.clone());
            }
        }
        let conn = self.checkout()?;
        let (cached_size, cached_modified, version, mean, bytes) = conn
            .query_row(
                "SELECT size, modified, fp_version, mean, data FROM video_frame_cache
                 WHERE path = ?1 AND at_ms = ?2 AND grid = ?3",
                params![key, at_ms, grid as i64],
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
            || grid == 0
            || version != FP_VERSION
        {
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
        self.enqueue(WriteOp::VideoFrame {
            key: cache_key(path),
            at_ms: (at_secs * 1000.0).round() as i64,
            grid,
            size,
            modified,
            mean: fingerprint.mean,
            values: fingerprint.values.clone(),
        });
    }

    fn get_embedding(
        &self,
        path: &Path,
        model: &str,
        size: u64,
        modified: Option<i64>,
    ) -> Option<Vec<f32>> {
        let key = cache_key(path);
        if let Ok(mem) = self.memory.read() {
            if mem.active {
                return mem
                    .embeddings
                    .get(&(key, model.to_string()))
                    .filter(|entry| entry.size == size && entry.modified == modified)
                    .map(|entry| entry.values.clone());
            }
        }
        let conn = self.checkout()?;
        let (cached_size, cached_modified, dims, bytes) = conn
            .query_row(
                "SELECT size, modified, dims, data
                 FROM embedding_cache WHERE path = ?1 AND model = ?2",
                params![key, model],
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
        self.enqueue(WriteOp::Embedding {
            key: cache_key(path),
            model: model.to_string(),
            size,
            modified,
            values: values.to_vec(),
        });
    }
}

impl Drop for CacheDb {
    fn drop(&mut self) {
        let Ok(mut guard) = self.writer.lock() else {
            return;
        };
        let Some(mut writer) = guard.take() else {
            return;
        };
        let (ack_tx, ack_rx) = mpsc::channel();
        if writer.tx.send(WriteOp::Flush(ack_tx)).is_ok() {
            let _ = ack_rx.recv_timeout(Duration::from_secs(30));
        }
        let _ = writer.tx.send(WriteOp::Shutdown);
        if let Some(join) = writer.join.take() {
            let _ = join.join();
        }
    }
}

impl ScanCache for CacheDb {
    fn preload(&self, paths: &[PathBuf], model: Option<&str>) {
        CacheDb::preload(self, paths, model)
    }

    fn flush(&self) {
        CacheDb::flush(self)
    }

    fn fingerprint(
        &self,
        path: &Path,
        size: u64,
        modified_ms: Option<i64>,
        grid: u32,
    ) -> Option<Fingerprint> {
        self.get_fingerprint(path, size, modified_ms, grid)
    }

    fn comparison(&self, signature: &str) -> Option<CachedComparison> {
        let conn = self.checkout()?;
        let (pairs, blob): (i64, Vec<u8>) = conn
            .query_row(
                "SELECT pairs, matches FROM comparison_cache WHERE signature = ?1",
                params![signature],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok()?;
        Some(CachedComparison {
            pairs: pairs.max(0) as u64,
            matches: decode_comparison(&blob)?,
        })
    }

    fn store_comparison(&self, signature: &str, pairs: u64, matches: &[ComparisonEntry]) {
        if matches.len() > MAX_COMPARISON_ENTRIES {
            return;
        }
        let blob = encode_comparison(matches);
        if blob.len() > MAX_COMPARISON_BYTES {
            return;
        }
        let Some(conn) = self.checkout() else {
            return;
        };
        if conn
            .execute(
                "INSERT INTO comparison_cache(signature, pairs, matches)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(signature) DO UPDATE SET
                     pairs = excluded.pairs,
                     matches = excluded.matches,
                     created_at = datetime('now')",
                params![signature, pairs as i64, blob],
            )
            .is_err()
        {
            return;
        }
        let _ = conn.execute(
            &format!(
                "DELETE FROM comparison_cache WHERE rowid NOT IN (
                     SELECT rowid FROM comparison_cache ORDER BY rowid DESC LIMIT {COMPARISON_KEEP}
                 )"
            ),
            [],
        );
        let _ = conn.execute_batch("PRAGMA optimize;");
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

    fn metadata(&self, path: &Path, size: u64, modified_ms: Option<i64>) -> Option<CachedMetadata> {
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

    fn failure(&self, path: &Path, size: u64, modified_ms: Option<i64>) -> Option<String> {
        self.get_failure(path, size, modified_ms)
    }

    fn store_failure(&self, path: &Path, size: u64, modified_ms: Option<i64>, reason: &str) {
        self.put_failure(path, size, modified_ms, reason)
    }

    fn content_hash(&self, path: &Path, size: u64, modified_ms: Option<i64>) -> Option<String> {
        self.get_content_hash(path, size, modified_ms)
    }

    fn store_content_hash(&self, path: &Path, size: u64, modified_ms: Option<i64>, hash: &str) {
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
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[a, b]| i16::from_le_bytes([*a, *b]))
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
        .as_chunks::<4>()
        .0
        .iter()
        .map(|[a, b, c, d]| f32::from_le_bytes([*a, *b, *c, *d]))
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
        assert_eq!(normalize_key("/Photos/A.JPG", false), "/Photos/A.JPG");
    }

    #[test]
    fn migrates_extended_and_drive_letter_keys() {
        let (path, db) = temp_db("migrate");
        {
            let conn = db.checkout().unwrap();
            let data = encode_i16(&[1, 2, 3, 4]);
            conn.execute(
                "INSERT INTO fingerprint_cache(
                     path, size, modified, grid, fp_version, mean, data, created_at)
                 VALUES(?1, 10, 100, 2, ?3, 0, ?2, '2026-01-01 00:00:00')",
                params![r"\\?\B:\Photos\A.JPG", data, FP_VERSION],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO fingerprint_cache(
                     path, size, modified, grid, fp_version, mean, data, created_at)
                 VALUES(?1, 10, 100, 2, ?3, 0, ?2, '2026-01-02 00:00:00')",
                params![r"B:/Photos/a.jpg", data, FP_VERSION],
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
                .query_row("SELECT COUNT(*) FROM fingerprint_cache", [], |row| {
                    row.get(0)
                })
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
        db.flush();
        assert_eq!(
            db.get_content_hash(file, 10, Some(100)),
            Some("abc123".to_string())
        );
        assert!(db.get_content_hash(file, 10, Some(101)).is_none());

        db.put_media_signature(file, 10, Some(100), "audio|5|5", "sig");
        db.flush();
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
        db.flush();
        assert_eq!(
            db.get_video_frame(file, 10, Some(100), 1.5, 2),
            Some(fingerprint)
        );
        assert!(db.get_video_frame(file, 10, Some(101), 1.5, 2).is_none());
        assert!(db.get_video_frame(file, 10, Some(100), 1.4, 2).is_none());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn manual_choices_round_trip_and_clear_semantics() {
        let (path, db) = temp_db("manual");
        assert_eq!(db.manual_choices(), (HashSet::new(), HashSet::new()));

        db.set_manual_reference("/photos/a.jpg", true).unwrap();
        db.set_manual_out("/photos/b.jpg", true).unwrap();
        let (references, out) = db.manual_choices();
        assert!(references.contains("/photos/a.jpg"));
        assert!(out.contains("/photos/b.jpg"));
        assert_eq!(db.stats().manual_choices, 2);

        // A file cannot be both the reference and out of its group.
        db.set_manual_out("/photos/a.jpg", true).unwrap();
        let (references, out) = db.manual_choices();
        assert!(!references.contains("/photos/a.jpg"));
        assert!(out.contains("/photos/a.jpg"));

        db.set_manual_reference("/photos/b.jpg", true).unwrap();
        let (references, out) = db.manual_choices();
        assert!(references.contains("/photos/b.jpg"));
        assert!(!out.contains("/photos/b.jpg"));

        // Clearing comparison results keeps manual decisions.
        db.clear_comparison().unwrap();
        assert_eq!(db.stats().manual_choices, 2);

        // Clearing the whole cache removes them too.
        db.clear().unwrap();
        assert_eq!(db.manual_choices(), (HashSet::new(), HashSet::new()));
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
        db.flush();
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
        db.flush();
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
        db.flush();
        assert_eq!(
            db.get_embedding(image, "/models/clip.onnx", 20, None),
            Some(vector.clone())
        );
        assert!(db
            .get_embedding(image, "/models/other.onnx", 20, None)
            .is_none());
        assert!(db
            .get_embedding(image, "/models/clip.onnx", 21, None)
            .is_none());
        let stats = db.stats();
        assert_eq!(stats.embeddings, 1);
        assert_eq!(stats.bytes, 16);
        assert_eq!(db.clear().unwrap(), ());
        assert_eq!(db.stats(), CacheStats::default());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn old_schema_is_upgraded_without_losing_the_database() {
        let path = std::env::temp_dir().join(format!(
            "umdf-cache-old-schema-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE fingerprint_cache (
                     path TEXT PRIMARY KEY,
                     size INTEGER NOT NULL,
                     modified INTEGER,
                     grid INTEGER NOT NULL,
                     mean INTEGER NOT NULL,
                     data BLOB NOT NULL,
                     created_at TEXT NOT NULL DEFAULT (datetime('now'))
                 );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO fingerprint_cache(path, size, modified, grid, mean, data)
                 VALUES(?1, 10, 100, 75, 0, ?2)",
                params![
                    cache_key(Path::new("/photos/a.jpg")),
                    encode_i16(&[1, 2, 3])
                ],
            )
            .unwrap();
        }
        let db = CacheDb::open_at(path.clone()).unwrap();
        // The row survives, but its old version is a miss so the new grid is
        // rebuilt on the next scan.
        assert!(db
            .get_fingerprint(Path::new("/photos/a.jpg"), 10, Some(100), 75)
            .is_none());
        db.preload(&[PathBuf::from("/photos/a.jpg")], None);
        assert!(db
            .get_fingerprint(Path::new("/photos/a.jpg"), 10, Some(100), 75)
            .is_none());
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn failed_decodes_are_cached_and_invalidate_on_change() {
        let (path, db) = temp_db("failure");
        let image = Path::new("/photos/broken.jpg");
        assert!(db.get_failure(image, 0, Some(100)).is_none());
        db.put_failure(image, 0, Some(100), "empty image");
        db.flush();
        assert_eq!(
            db.get_failure(image, 0, Some(100)),
            Some("empty image".to_string())
        );
        assert!(db.get_failure(image, 1, Some(100)).is_none());
        assert!(db.get_failure(image, 0, Some(101)).is_none());
        db.preload(&[image.to_path_buf()], None);
        assert_eq!(
            db.get_failure(image, 0, Some(100)),
            Some("empty image".to_string())
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn preload_answers_from_memory_and_isolates_paths() {
        let (path, db) = temp_db("preload");
        let a = Path::new("/photos/a.jpg");
        let b = Path::new("/photos/b.jpg");
        let fp = Fingerprint {
            size: 2,
            mean: 1,
            values: vec![1, 2, 3, 4],
        };
        db.put_fingerprint(a, 10, Some(100), 2, &fp);
        db.put_fingerprint(b, 10, Some(100), 2, &fp);
        db.flush();

        db.preload(&[a.to_path_buf()], None);
        assert_eq!(db.get_fingerprint(a, 10, Some(100), 2), Some(fp));
        // `b` exists in SQLite but was not part of this scan: the warm view
        // reports a miss so the worker recomputes it.
        assert!(db.get_fingerprint(b, 10, Some(100), 2).is_none());
        // A changed file also misses without touching SQLite.
        assert!(db.get_fingerprint(a, 11, Some(100), 2).is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn old_fingerprint_version_is_ignored_by_preload() {
        let (path, db) = temp_db("fpversion");
        let image = Path::new("/photos/a.jpg");
        {
            let conn = db.checkout().unwrap();
            conn.execute(
                "INSERT INTO fingerprint_cache(
                     path, size, modified, grid, fp_version, mean, data)
                 VALUES(?1, 10, 100, 2, 1, 0, ?2)",
                params![cache_key(image), encode_i16(&[1, 2, 3, 4])],
            )
            .unwrap();
        }
        db.preload(&[image.to_path_buf()], None);
        assert!(db.get_fingerprint(image, 10, Some(100), 2).is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn second_scan_is_served_from_the_warm_fingerprint_cache() {
        let dir = std::env::temp_dir().join(format!(
            "umdf-warm-scan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let make = |seed: u32| {
            image::GrayImage::from_fn(64, 64, |x, y| {
                image::Luma([((x * 7 + y * 3 + seed * 29) % 256) as u8])
            })
        };
        for (name, seed) in [("a.png", 0), ("b.png", 0), ("c.png", 1), ("d.png", 2)] {
            make(seed).save(dir.join(name)).unwrap();
        }

        let (path, db) = temp_db("warm");
        let cfg = umdf_core::ScanConfig {
            folders: vec![dir.clone()],
            pixel_tolerance: 0,
            similarity_threshold: 100.0,
            ..Default::default()
        };

        let first = umdf_core::run_with_progress_cached(&cfg, None, Some(&db)).expect("first scan");
        assert_eq!(first.items.len(), 4);
        assert_eq!(first.stats.fingerprints_computed, 4);

        let second =
            umdf_core::run_with_progress_cached(&cfg, None, Some(&db)).expect("second scan");
        assert_eq!(second.items.len(), 4);
        assert_eq!(second.stats.fingerprints_from_cache, 4);
        assert_eq!(second.stats.fingerprints_computed, 0);

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(path);
    }

    /// Manual benchmark: `cargo test -p umdf-ui warm_scan_benchmark -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn warm_scan_benchmark() {
        let dir = std::env::temp_dir().join(format!("umdf-warm-bench-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let total = 5_000usize;
        for i in 0..total {
            let seed = (i % 97) as u32;
            let img = image::GrayImage::from_fn(96, 96, |x, y| {
                image::Luma([((x * 7 + y * 3 + seed * 29) % 256) as u8])
            });
            img.save(dir.join(format!("{i:05}.png"))).unwrap();
        }
        let (path, db) = temp_db("warm-bench");
        let cfg = umdf_core::ScanConfig {
            folders: vec![dir.clone()],
            pixel_tolerance: 0,
            similarity_threshold: 100.0,
            ..Default::default()
        };

        let first = umdf_core::run_with_progress_cached(&cfg, None, Some(&db)).unwrap();
        let second = umdf_core::run_with_progress_cached(&cfg, None, Some(&db)).unwrap();
        println!(
            "files={} first_extract={}ms computed={} compare={}ms pairs={} groups={} second_extract={}ms cached={}",
            first.items.len(),
            first.stats.extract_ms,
            first.stats.fingerprints_computed,
            first.stats.compare_ms,
            first.stats.pairs_compared,
            first.stats.groups,
            second.stats.extract_ms,
            second.stats.fingerprints_from_cache,
        );
        assert_eq!(second.stats.fingerprints_from_cache, total);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(path);
    }
}
