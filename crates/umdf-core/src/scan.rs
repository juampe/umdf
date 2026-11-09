//! Scan orchestrator: discovery → extraction → matching → grouping.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Instant;

use rayon::prelude::*;

use crate::action::{self, ActionReport};
use crate::config::{is_audio_ext, is_image_ext, is_video_ext, Action, ScanConfig};
use crate::fingerprint::{self, aspect_ratio, LEGACY_ASPECT_TOLERANCE};
use crate::group;
use crate::hash;
use crate::media;
use crate::metadata;
use crate::model::{CachedMetadata, Fingerprint, Group, Match, MatchKind, MediaItem, MediaKind};
use crate::walk;

#[derive(Debug, Clone)]
pub enum Progress {
    Discovered { total: usize },
    Loaded { index: usize, total: usize, path: PathBuf },
    Embedding { done: usize, total: usize },
    /// Which execution provider the semantic model actually uses.
    EmbeddingDevice { cuda: bool, forced_cpu: bool },
    Hashing { done: usize, total: usize },
    Comparing { done: usize, total: usize },
    Grouped { groups: usize, duplicates: usize },
    Finished,
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub files_scanned: usize,
    pub files_hashed: usize,
    pub files_fingerprinted: usize,
    pub files_media_signed: usize,
    pub pairs_compared: u64,
    pub duplicates_found: usize,
    pub groups: usize,
    pub total_bytes: u64,
    pub reclaimable_bytes: u64,
    pub elapsed_ms: u128,
    /// True when the scan returned early because Stop was pressed.
    pub cancelled: bool,
    /// Worker threads used by this scan.
    pub threads: usize,
    pub embeddings: usize,
    /// Files that could not be decoded during extraction.
    pub extract_errors: usize,
    /// Fingerprints restored from the cache without decoding the file.
    pub fingerprints_from_cache: usize,
    /// Fingerprints produced by decoding the file in this scan.
    pub fingerprints_computed: usize,
    /// Embeddings restored from the cache.
    pub embeddings_from_cache: usize,
    /// Embeddings produced by the model in this scan.
    pub embeddings_computed: usize,
    /// Files whose dimensions/EXIF/duration came from the metadata cache.
    pub metadata_from_cache: usize,
    /// Files whose metadata had to be read from disk.
    pub metadata_computed: usize,
    /// Whole-file SHA-256 values restored from the cache.
    pub hashes_from_cache: usize,
    /// Whole-file SHA-256 values computed in this scan.
    pub hashes_computed: usize,
    /// Audio/video signatures restored from the cache.
    pub media_signatures_from_cache: usize,
    /// Video frame fingerprints restored from the cache.
    pub video_frames_from_cache: usize,
    /// Images whose semantic embedding failed (the scan continued).
    pub embedding_errors: usize,
    pub extract_ms: u128,
    pub hash_ms: u128,
    pub compare_ms: u128,
    pub group_ms: u128,
}

#[derive(Debug)]
pub struct ScanReport {
    pub items: Vec<MediaItem>,
    pub matches: Vec<Match>,
    pub groups: Vec<Group>,
    pub stats: Stats,
    pub actions: Option<ActionReport>,
}

/// Seconds of decoded audio used for the content signature.
const AUDIO_SAMPLE_SECONDS: f64 = 5.0;

/// Used when the caller did not provide a cancellation flag.
static NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);

/// Optional cache for values derived from file contents.
///
/// Implementations must be safe to call from the worker threads. Cache misses
/// and write failures are ignored by the scanner: the cache only accelerates a
/// scan, it never changes its result.
pub trait ScanCache: Send + Sync {
    fn fingerprint(
        &self,
        path: &Path,
        size: u64,
        modified_ms: Option<i64>,
        grid: u32,
    ) -> Option<Fingerprint>;

    /// Cached dimensions, EXIF and duration for an unchanged file.
    fn metadata(
        &self,
        _path: &Path,
        _size: u64,
        _modified_ms: Option<i64>,
    ) -> Option<CachedMetadata> {
        None
    }

    fn store_metadata(
        &self,
        _path: &Path,
        _size: u64,
        _modified_ms: Option<i64>,
        _metadata: &CachedMetadata,
    ) {
    }

    /// Cached whole-file SHA-256 for an unchanged file.
    fn content_hash(
        &self,
        _path: &Path,
        _size: u64,
        _modified_ms: Option<i64>,
    ) -> Option<String> {
        None
    }

    fn store_content_hash(
        &self,
        _path: &Path,
        _size: u64,
        _modified_ms: Option<i64>,
        _hash: &str,
    ) {
    }

    /// Cached audio PCM signature. `params` identifies sample position/length.
    fn media_signature(
        &self,
        _path: &Path,
        _size: u64,
        _modified_ms: Option<i64>,
        _params: &str,
    ) -> Option<String> {
        None
    }

    fn store_media_signature(
        &self,
        _path: &Path,
        _size: u64,
        _modified_ms: Option<i64>,
        _params: &str,
        _signature: &str,
    ) {
    }

    /// Cached perceptual fingerprint of a video frame.
    fn video_frame(
        &self,
        _path: &Path,
        _size: u64,
        _modified_ms: Option<i64>,
        _at_secs: f64,
        _grid: u32,
    ) -> Option<Fingerprint> {
        None
    }

    fn store_video_frame(
        &self,
        _path: &Path,
        _size: u64,
        _modified_ms: Option<i64>,
        _at_secs: f64,
        _grid: u32,
        _fingerprint: &Fingerprint,
    ) {
    }

    fn store_fingerprint(
        &self,
        path: &Path,
        size: u64,
        modified_ms: Option<i64>,
        grid: u32,
        fingerprint: &Fingerprint,
    );

    fn embedding(
        &self,
        path: &Path,
        model: &str,
        size: u64,
        modified_ms: Option<i64>,
    ) -> Option<Vec<f32>>;

    fn store_embedding(
        &self,
        path: &Path,
        model: &str,
        size: u64,
        modified_ms: Option<i64>,
        values: &[f32],
    );
}

pub fn run(cfg: &ScanConfig) -> anyhow::Result<ScanReport> {
    run_with_progress(cfg, None)
}

/// Number of worker threads actually used for a scan (0 = all CPUs).
pub fn effective_threads(cfg: &ScanConfig) -> usize {
    if cfg.threads == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    } else {
        cfg.threads.clamp(1, 256)
    }
}

pub fn run_with_progress(
    cfg: &ScanConfig,
    tx: Option<&Sender<Progress>>,
) -> anyhow::Result<ScanReport> {
    run_with_progress_cached(cfg, tx, None)
}

/// Like [`run_with_progress`], but reuses previously computed fingerprints and
/// embeddings through `cache`. The cache is optional and purely an
/// optimisation; deleting it only makes the next scan slower.
pub fn run_with_progress_cached(
    cfg: &ScanConfig,
    tx: Option<&Sender<Progress>>,
    cache: Option<&dyn ScanCache>,
) -> anyhow::Result<ScanReport> {
    let threads = effective_threads(cfg);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("umdf-worker-{i}"))
        .build()?;
    pool.install(|| run_inner(cfg, tx, threads, cache))
}

fn run_inner(
    cfg: &ScanConfig,
    tx: Option<&Sender<Progress>>,
    threads: usize,
    cache: Option<&dyn ScanCache>,
) -> anyhow::Result<ScanReport> {
    let started = Instant::now();
    let mut stats = Stats::default();
    stats.threads = threads;

    let cancel_ref: &AtomicBool = cfg.cancel.as_deref().unwrap_or(&NEVER_CANCELLED);
    let pause_ref = cfg.pause.as_deref();
    let (candidates, walk_cancelled) = walk::collect_candidates_cancellable(cfg, cancel_ref);
    stats.files_scanned = candidates.len();
    emit(
        tx,
        Progress::Discovered {
            total: candidates.len(),
        },
    );
    if walk_cancelled {
        // Stop pressed while walking the tree: return immediately.
        stats.cancelled = true;
        stats.elapsed_ms = started.elapsed().as_millis();
    stats.cancelled = cfg.is_cancelled();
        emit(tx, Progress::Finished);
        return Ok(ScanReport {
            items: Vec::new(),
            matches: Vec::new(),
            groups: Vec::new(),
            stats,
            actions: None,
        });
    }

    // ---- Stage 1: extraction (parallel) ------------------------------------
    let extract_started = Instant::now();
    // Image decoding is the dominant cost, so candidates are processed in
    // parallel batches. The batch is capped to keep peak memory bounded with
    // very large photos (each decoded image can be hundreds of MB).
    let ffprobe_bin = ffprobe_path(cfg);
    let ffmpeg_bin = cfg.ffmpeg.clone();
    let batch = threads.clamp(2, 12);

    let mut items: Vec<MediaItem> = Vec::with_capacity(candidates.len());
    let mut scanned = 0usize;
    for chunk in candidates.chunks(batch) {
        wait_if_paused(pause_ref, cancel_ref);
        if cfg.is_cancelled() {
            break;
        }
        let base = scanned;
        let results: Vec<Option<MediaItem>> = chunk
            .par_iter()
            .enumerate()
            .map(|(k, cand)| {
                if cfg.is_cancelled() {
                    return None;
                }
                // A malformed file must never abort the whole scan: decoders
                // normally return an error, but a panic in a third-party codec
                // would unwind through Rayon and stop everything.
                let extracted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    extract_item(
                        base + k,
                        cand,
                        cfg,
                        cancel_ref,
                        ffprobe_bin.as_deref(),
                        ffmpeg_bin.as_deref(),
                        cache,
                    )
                }));
                match extracted {
                    Ok(item) => item,
                    Err(_) => {
                        crate::log_error(format_args!(
                            "UMDF: decoder panicked while reading {}; skipping it",
                            cand.path.display()
                        ));
                        let mut item = MediaItem::new(
                            base + k,
                            cand.path.clone(),
                            classify(&cand.extension),
                            cand.size,
                        );
                        item.extract_error = Some("decoder panic".to_string());
                        Some(item)
                    }
                }
            })
            .collect();

        for (k, maybe) in results.into_iter().enumerate() {
            let cand = &chunk[k];
            emit(
                tx,
                Progress::Loaded {
                    index: base + k + 1,
                    total: candidates.len(),
                    path: cand.path.clone(),
                },
            );
            if let Some(item) = maybe {
                if item.kind != MediaKind::Other {
                    if item.metadata_cached {
                        stats.metadata_from_cache += 1;
                    } else {
                        stats.metadata_computed += 1;
                    }
                }
                if item.fingerprint.is_some() {
                    stats.files_fingerprinted += 1;
                    if item.fingerprint_cached {
                        stats.fingerprints_from_cache += 1;
                    } else {
                        stats.fingerprints_computed += 1;
                    }
                }
                if item.media_signature.is_some() {
                    stats.files_media_signed += 1;
                    if item.media_signature_cached {
                        stats.media_signatures_from_cache += 1;
                    }
                }
                if item.video_frame_cached {
                    stats.video_frames_from_cache += 1;
                }
                items.push(item);
            }
        }
        scanned += chunk.len();
        stats.files_scanned = scanned;
    }
    // Re-number ids after filtering and record how many files were actually
    // examined (a cancelled scan stops early).
    stats.files_scanned = scanned;
    for (i, item) in items.iter_mut().enumerate() {
        item.id = i;
    }

    stats.extract_ms = extract_started.elapsed().as_millis();

    // ---- Stage 1b: semantic embeddings (optional) --------------------------
    #[cfg(feature = "embeddings")]
    if !cfg.only_identical {
        if let Some(ecfg) = &cfg.embeddings {
            // Only images that decoded during extraction are eligible. This
            // prevents a corrupt image from failing a second time in the
            // semantic stage and aborting the whole scan.
            let ids: Vec<usize> = items
                .iter()
                .filter(|item| {
                    item.kind == MediaKind::Image
                        && item.fingerprint.is_some()
                        && item.extract_error.is_none()
                })
                .map(|item| item.id)
                .collect();
            if !ids.is_empty() {
                // Include the model file metadata so replacing the .onnx at the
                // same path invalidates the previously cached vectors.
                let model_key = {
                    let path = ecfg.model_path.to_string_lossy().to_string();
                    match std::fs::metadata(&ecfg.model_path) {
                        Ok(meta) => {
                            let modified = meta
                                .modified()
                                .ok()
                                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                                .map(|delta| delta.as_millis())
                                .unwrap_or(0);
                            format!("{path}|{}|{modified}", meta.len())
                        }
                        Err(_) => path,
                    }
                };
                let meta: Vec<(PathBuf, u64, Option<i64>)> = ids
                    .iter()
                    .map(|id| {
                        let item = &items[*id];
                        (
                            item.path.clone(),
                            item.size,
                            item.modified.map(|date| date.timestamp_millis()),
                        )
                    })
                    .collect();

                // Reuse vectors from previous scans and only run the model on
                // the images whose file size or mtime changed.
                let mut vectors: Vec<Option<Vec<f32>>> = meta
                    .iter()
                    .map(|(path, size, modified)| {
                        cache.and_then(|cache| {
                            cache.embedding(path, &model_key, *size, *modified)
                        })
                    })
                    .collect();
                let missing: Vec<usize> = (0..ids.len())
                    .filter(|slot| vectors[*slot].is_none())
                    .collect();
                let cached_embeddings = ids.len() - missing.len();
                stats.embeddings_from_cache += cached_embeddings;
                emit(
                    tx,
                    Progress::Embedding {
                        done: cached_embeddings,
                        total: ids.len(),
                    },
                );

                if !missing.is_empty() {
                    let paths: Vec<PathBuf> =
                        missing.iter().map(|slot| meta[*slot].0.clone()).collect();
                    // Semantic matching is an optional enhancement: if the
                    // model cannot be loaded or inference fails, keep scanning
                    // with the perceptual fingerprints instead of aborting.
                    let engine = match crate::embeddings::EmbeddingEngine::new(ecfg) {
                        Ok(engine) => Some(engine),
                        Err(error) => {
                            crate::log_error(format_args!(
                                "UMDF: semantic model unavailable ({error}); continuing without embeddings"
                            ));
                            stats.embedding_errors += paths.len();
                            None
                        }
                    };
                    if let Some(mut engine) = engine {
                        if engine.forced_cpu() {
                            crate::log_error(format_args!(
                                "UMDF: semantic embeddings are running on the CPU"
                            ));
                        }
                        emit(
                            tx,
                            Progress::EmbeddingDevice {
                                cuda: engine.uses_cuda(),
                                forced_cpu: engine.forced_cpu(),
                            },
                        );
                        let computed_before = stats.embeddings_computed;
                        let result = engine.embed_paths_incremental(
                            &paths,
                            cancel_ref,
                            pause_ref,
                            |start, batch| {
                            let batch_len = batch.len();
                            for (offset, maybe_vector) in batch.into_iter().enumerate() {
                                let slot = missing[start + offset];
                                match maybe_vector {
                                    Some(vector) => {
                                        stats.embeddings_computed += 1;
                                        if let Some(cache) = cache {
                                            let (path, size, modified) = &meta[slot];
                                            cache.store_embedding(
                                                path,
                                                &model_key,
                                                *size,
                                                *modified,
                                                &vector,
                                            );
                                        }
                                        vectors[slot] = Some(vector);
                                    }
                                    None => stats.embedding_errors += 1,
                                }
                            }
                            let done = (cached_embeddings + start + batch_len).min(ids.len());
                            emit(
                                tx,
                                Progress::Embedding {
                                    done,
                                    total: ids.len(),
                                },
                            );
                        });
                        if let Err(error) = result {
                            crate::log_error(format_args!(
                                "UMDF: semantic embeddings failed ({error}); continuing with the remaining files"
                            ));
                            let computed = stats.embeddings_computed - computed_before;
                            stats.embedding_errors += paths.len().saturating_sub(computed);
                        }
                    }
                }
                for (slot, id) in ids.iter().enumerate() {
                    items[*id].embedding = vectors[slot].take();
                }
                stats.embeddings = ids.len();
                emit(
                    tx,
                    Progress::Embedding {
                        done: ids.len(),
                        total: ids.len(),
                    },
                );
            }
        }
    }

    // ---- Stage 2: exact content --------------------------------------------
    let hash_started = Instant::now();
    // Hashing is only useful when two files share a size.
    let mut size_counts = std::collections::HashMap::new();
    for it in &items {
        *size_counts.entry(it.size).or_insert(0usize) += 1;
    }
    let hash_targets: Vec<usize> = items
        .iter()
        .filter(|i| size_counts.get(&i.size).copied().unwrap_or(0) > 1)
        .map(|i| i.id)
        .collect();
    emit(
        tx,
        Progress::Hashing {
            done: 0,
            total: hash_targets.len(),
        },
    );
    let mut hashes: Vec<(usize, String, bool)> = Vec::with_capacity(hash_targets.len());
    let mut hashed_done = 0usize;
    for chunk in hash_targets.chunks(16) {
        wait_if_paused(pause_ref, cancel_ref);
        if cfg.is_cancelled() {
            break;
        }
        let part: Vec<(usize, String, bool)> = chunk
            .par_iter()
            .filter_map(|id| {
                if cfg.is_cancelled() {
                    return None;
                }
                let it = &items[*id];
                let modified_ms = it.modified.map(|date| date.timestamp_millis());
                if let Some(hash) = cache.and_then(|cache| {
                    cache.content_hash(&it.path, it.size, modified_ms)
                }) {
                    return Some((*id, hash, true));
                }
                hash::file_hash_cancellable(&it.path, cancel_ref)
                    .ok()
                    .flatten()
                    .map(|hash| {
                        if let Some(cache) = cache {
                            cache.store_content_hash(
                                &it.path,
                                it.size,
                                modified_ms,
                                &hash,
                            );
                        }
                        (*id, hash, false)
                    })
            })
            .collect();
        hashed_done += chunk.len();
        hashes.extend(part);
        emit(
            tx,
            Progress::Hashing {
                done: hashed_done.min(hash_targets.len()),
                total: hash_targets.len(),
            },
        );
    }
    for (id, hash, cached) in hashes {
        items[id].content_hash = Some(hash);
        stats.files_hashed += 1;
        if cached {
            stats.hashes_from_cache += 1;
        } else {
            stats.hashes_computed += 1;
        }
    }

    let mut matches: Vec<Match> = Vec::new();
    // Group by (size, hash) and emit one pair per additional member.
    let mut by_content: std::collections::HashMap<(u64, String), Vec<usize>> =
        std::collections::HashMap::new();
    for it in &items {
        if let Some(h) = &it.content_hash {
            by_content
                .entry((it.size, h.clone()))
                .or_default()
                .push(it.id);
        }
    }
    for (_, ids) in by_content {
        for w in ids.windows(2) {
            matches.push(Match {
                a: w[0],
                b: w[1],
                similarity: 100.0,
                kind: MatchKind::Exact,
            });
        }
    }

    // Same decoded media content (audio PCM / video stream signature) across
    // different containers or tags.
    let mut by_media: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    for it in &items {
        if let Some(sig) = &it.media_signature {
            by_media.entry(sig.clone()).or_default().push(it.id);
        }
    }
    for (_, ids) in by_media {
        for w in ids.windows(2) {
            matches.push(Match {
                a: w[0],
                b: w[1],
                similarity: 99.0,
                kind: MatchKind::MediaContent,
            });
        }
    }

    stats.hash_ms = hash_started.elapsed().as_millis();

    // ---- Stage 3: perceptual ------------------------------------------------
    let compare_started = Instant::now();
    if !cfg.only_identical {
        let mut index: Vec<usize> = items
            .iter()
            .filter(|i| i.fingerprint.is_some() && i.width.is_some() && i.height.is_some())
            .map(|i| i.id)
            .collect();
        // Sort by aspect ratio so the 1.5x gate becomes a sliding window.
        index.sort_by(|a, b| {
            let ra = aspect_ratio(items[*a].width.unwrap(), items[*a].height.unwrap());
            let rb = aspect_ratio(items[*b].width.unwrap(), items[*b].height.unwrap());
            ra.partial_cmp(&rb).unwrap_or(std::cmp::Ordering::Equal)
        });

        // Count compatible pairs first (cheap, cancellable) so the progress
        // bar has a total, then compare them streaming. This avoids building a
        // Vec with potentially billions of pairs and keeps Stop responsive.
        let mut total_pairs: u64 = 0;
        'count: for i in 0..index.len() {
            wait_if_paused(pause_ref, cancel_ref);
            if cfg.is_cancelled() {
                break;
            }
            let ai = index[i];
            let ra = aspect_ratio(items[ai].width.unwrap(), items[ai].height.unwrap());
            let mut inner = 0usize;
            for &bj in &index[i + 1..] {
                inner += 1;
                if inner % 4096 == 0 && cfg.is_cancelled() {
                    break 'count;
                }
                let rb = aspect_ratio(items[bj].width.unwrap(), items[bj].height.unwrap());
                if ra.max(rb) / ra.min(rb) > LEGACY_ASPECT_TOLERANCE {
                    break;
                }
                if items[ai].content_hash.is_some()
                    && items[ai].content_hash == items[bj].content_hash
                {
                    continue;
                }
                total_pairs += 1;
            }
        }

        emit(
            tx,
            Progress::Comparing {
                done: 0,
                total: total_pairs as usize,
            },
        );

        // Parallelise over the outer index (one Rayon region) instead of
        // spawning a parallel region per window. Nested regions scale badly
        // once the pool exceeds the number of chunks.
        let results: Vec<(u64, Vec<Match>)> = (0..index.len())
            .into_par_iter()
            .map(|i| {
                let mut local: Vec<Match> = Vec::new();
                let mut counted: u64 = 0;
                wait_if_paused(pause_ref, cancel_ref);
                if cfg.is_cancelled() {
                    return (0, local);
                }
                let ai = index[i];
                let ra = aspect_ratio(items[ai].width.unwrap(), items[ai].height.unwrap());
                let mut seen = 0usize;
                for &bj in &index[i + 1..] {
                    let rb = aspect_ratio(items[bj].width.unwrap(), items[bj].height.unwrap());
                    if ra.max(rb) / ra.min(rb) > LEGACY_ASPECT_TOLERANCE {
                        break;
                    }
                    if items[ai].content_hash.is_some()
                        && items[ai].content_hash == items[bj].content_hash
                    {
                        continue;
                    }
                    seen += 1;
                    if seen % 256 == 0 && cfg.is_cancelled() {
                        break;
                    }
                    counted += 1;
                    let uses_embeddings = items[ai].embedding.is_some()
                        && items[bj].embedding.is_some();
                    let sim = if let (Some(ea), Some(eb)) =
                        (items[ai].embedding.as_ref(), items[bj].embedding.as_ref())
                    {
                        fingerprint::cosine_percent(ea, eb)
                    } else {
                        let (Some(fa), Some(fb)) = (
                            items[ai].fingerprint.as_ref(),
                            items[bj].fingerprint.as_ref(),
                        ) else {
                            continue;
                        };
                        let Some(similarity) =
                            fingerprint::similarity(fa, fb, cfg.metric, cfg.pixel_tolerance)
                        else {
                            continue;
                        };
                        similarity
                    };
                    let threshold = if uses_embeddings {
                        cfg.embeddings
                            .as_ref()
                            .map(|e| e.threshold * 100.0)
                            .unwrap_or(cfg.similarity_threshold)
                    } else {
                        cfg.similarity_threshold
                    };
                    if sim >= threshold {
                        local.push(Match {
                            a: ai,
                            b: bj,
                            similarity: sim,
                            kind: MatchKind::Similar,
                        });
                    }
                }
                (counted, local)
            })
            .collect();

        let mut compared: u64 = 0;
        for (counted, found) in results {
            compared += counted;
            matches.extend(found);
        }
        emit(
            tx,
            Progress::Comparing {
                done: compared as usize,
                total: total_pairs as usize,
            },
        );
        stats.pairs_compared = compared;
    }

    stats.compare_ms = compare_started.elapsed().as_millis();

    // ---- Stage 4: groups + policy ------------------------------------------
    let group_started = Instant::now();
    let groups = group::build_groups(&items, &matches, cfg);
    stats.group_ms = group_started.elapsed().as_millis();
    stats.groups = groups.len();
    stats.duplicates_found = groups.iter().map(|g| g.remove.len()).sum();
    stats.total_bytes = items.iter().map(|i| i.size).sum();
    stats.reclaimable_bytes = groups.iter().map(|g| g.reclaimable_bytes).sum();
    stats.elapsed_ms = started.elapsed().as_millis();
    stats.cancelled = cfg.is_cancelled();

    emit(
        tx,
        Progress::Grouped {
            groups: groups.len(),
            duplicates: stats.duplicates_found,
        },
    );

    // ---- Stage 5: optional actions -----------------------------------------
    let actions = if cfg.apply && !matches!(cfg.action, Action::None) {
        Some(action::execute(cfg, &items, &groups)?)
    } else {
        None
    };

    stats.extract_errors = items
        .iter()
        .filter(|item| item.extract_error.is_some())
        .count();
    emit(tx, Progress::Finished);

    Ok(ScanReport {
        items,
        matches,
        groups,
        stats,
        actions,
    })
}

fn extract_item(
    index: usize,
    cand: &walk::Candidate,
    cfg: &ScanConfig,
    cancel: &AtomicBool,
    ffprobe: Option<&Path>,
    ffmpeg: Option<&Path>,
    cache: Option<&dyn ScanCache>,
) -> Option<MediaItem> {
    let kind = classify(&cand.extension);
    let mut item = MediaItem::new(index, cand.path.clone(), kind, cand.size);
    let (modified, created) = metadata::times(&cand.path);
    item.modified = modified;
    let modified_ms = modified.map(|date| date.timestamp_millis());

    let cached_metadata = cache.and_then(|cache| {
        cache.metadata(&cand.path, cand.size, modified_ms)
    });
    let mut metadata_ready = false;
    let mut has_audio = kind == MediaKind::Audio;
    if let Some(meta) = cached_metadata {
        item.width = meta.width;
        item.height = meta.height;
        item.created = meta.created_ms.and_then(ms_to_datetime);
        item.exif_taken = meta.exif_taken_ms.and_then(ms_to_datetime);
        item.exif_fields = meta.exif_fields;
        item.exif_camera = meta.exif_camera;
        item.duration_ms = meta.duration_ms;
        has_audio = meta.has_audio;
        item.metadata_cached = true;
        metadata_ready = true;
    } else {
        item.created = created;
    }

    match kind {
        MediaKind::Image => {
            if !metadata_ready {
                if let Some((w, h)) = metadata::image_dimensions(&cand.path) {
                    item.width = Some(w);
                    item.height = Some(h);
                }
                if let Some(meta) = metadata::exif_meta(&cand.path) {
                    item.exif_taken = meta.taken;
                    item.exif_fields = meta.field_count;
                    item.exif_camera = meta.camera;
                }
                if let Some(cache) = cache {
                    cache.store_metadata(
                        &cand.path,
                        cand.size,
                        modified_ms,
                        &cached_metadata_from_item(&item, false),
                    );
                }
            }
            if below_min_dimensions(cfg, &item) {
                return None;
            }
            match cache.and_then(|cache| {
                cache.fingerprint(&cand.path, cand.size, modified_ms, cfg.grid)
            }) {
                Some(fp) => {
                    item.fingerprint = Some(fp);
                    item.fingerprint_cached = true;
                }
                None => match fingerprint::load_fingerprint(&cand.path, cfg.grid) {
                    Ok(fp) => {
                        if let Some(cache) = cache {
                            cache.store_fingerprint(
                                &cand.path,
                                cand.size,
                                modified_ms,
                                cfg.grid,
                                &fp,
                            );
                        }
                        item.fingerprint = Some(fp);
                    }
                    Err(e) => item.extract_error = Some(e.to_string()),
                },
            }
        }
        MediaKind::Video | MediaKind::Audio => {
            if !metadata_ready {
                if let Some(probe_bin) = ffprobe {
                    if let Ok(p) = media::probe(probe_bin, &cand.path, cancel) {
                        item.width = p.width;
                        item.height = p.height;
                        item.duration_ms = p.duration_ms;
                        has_audio = p.has_audio;
                        if let Some(cache) = cache {
                            cache.store_metadata(
                                &cand.path,
                                cand.size,
                                modified_ms,
                                &cached_metadata_from_item(&item, has_audio),
                            );
                        }
                    }
                }
            }
            if below_min_dimensions(cfg, &item) {
                return None;
            }
            if cfg.compare_media_content {
                if let Some(ffmpeg) = ffmpeg {
                    if kind == MediaKind::Video {
                        if let Some(fp) = cache.and_then(|cache| {
                            cache.video_frame(
                                &cand.path,
                                cand.size,
                                modified_ms,
                                cfg.frame_position_secs,
                                cfg.grid,
                            )
                        }) {
                            item.fingerprint = Some(fp);
                            item.video_frame_cached = true;
                        } else if let Ok(fp) = video_frame_fingerprint(
                            ffmpeg,
                            &cand.path,
                            cfg.frame_position_secs,
                            cfg.grid,
                            cancel,
                        ) {
                            if let Some(cache) = cache {
                                cache.store_video_frame(
                                    &cand.path,
                                    cand.size,
                                    modified_ms,
                                    cfg.frame_position_secs,
                                    cfg.grid,
                                    &fp,
                                );
                            }
                            item.fingerprint = Some(fp);
                        }
                    }
                    if has_audio {
                        let params_key = format!(
                            "audio|{}|{}",
                            cfg.frame_position_secs, AUDIO_SAMPLE_SECONDS
                        );
                        if let Some(signature) = cache.and_then(|cache| {
                            cache.media_signature(
                                &cand.path,
                                cand.size,
                                modified_ms,
                                &params_key,
                            )
                        }) {
                            item.media_signature = Some(signature);
                            item.media_signature_cached = true;
                        } else if let Ok(signature) = media::audio_pcm_hash(
                            ffmpeg,
                            &cand.path,
                            cfg.frame_position_secs,
                            AUDIO_SAMPLE_SECONDS,
                            cancel,
                        ) {
                            if let Some(cache) = cache {
                                cache.store_media_signature(
                                    &cand.path,
                                    cand.size,
                                    modified_ms,
                                    &params_key,
                                    &signature,
                                );
                            }
                            item.media_signature = Some(signature);
                        }
                    }
                }
            }
        }
        MediaKind::Other => {}
    }

    Some(item)
}

fn ms_to_datetime(ms: i64) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
}

fn cached_metadata_from_item(item: &MediaItem, has_audio: bool) -> CachedMetadata {
    CachedMetadata {
        width: item.width,
        height: item.height,
        created_ms: item.created.map(|date| date.timestamp_millis()),
        exif_taken_ms: item.exif_taken.map(|date| date.timestamp_millis()),
        exif_fields: item.exif_fields,
        exif_camera: item.exif_camera,
        duration_ms: item.duration_ms,
        has_audio,
    }
}

fn classify(ext: &str) -> MediaKind {
    if is_image_ext(ext) {
        MediaKind::Image
    } else if is_video_ext(ext) {
        MediaKind::Video
    } else if is_audio_ext(ext) {
        MediaKind::Audio
    } else {
        MediaKind::Other
    }
}

fn below_min_dimensions(cfg: &ScanConfig, item: &MediaItem) -> bool {
    let w = item.width.unwrap_or(0);
    let h = item.height.unwrap_or(0);
    // Only apply when we actually know the dimensions.
    if item.width.is_none() || item.height.is_none() {
        return false;
    }
    w < cfg.min_width || h < cfg.min_height
}

fn ffprobe_path(cfg: &ScanConfig) -> Option<PathBuf> {
    cfg.ffmpeg
        .as_ref()
        .and_then(|ff| {
            let sibling = ff.with_file_name(if cfg!(windows) {
                "ffprobe.exe"
            } else {
                "ffprobe"
            });
            if sibling.is_file() {
                Some(sibling)
            } else {
                None
            }
        })
        .or_else(|| media::find_tool(&["ffprobe", "ffprobe.exe"]))
}

fn video_frame_fingerprint(
    ffmpeg: &std::path::Path,
    file: &std::path::Path,
    at_secs: f64,
    grid: u32,
    cancel: &AtomicBool,
) -> anyhow::Result<Fingerprint> {
    let tmp = std::env::temp_dir().join(format!(
        "umdf-frame-{}-{}.png",
        std::process::id(),
        hash::hex::encode(file.to_string_lossy().as_bytes())[..12].to_string()
    ));
    media::extract_frame(ffmpeg, file, at_secs, &tmp, cancel)?;
    let fp = fingerprint::load_fingerprint(&tmp, grid);
    let _ = std::fs::remove_file(&tmp);
    fp
}

fn wait_if_paused(pause: Option<&AtomicBool>, cancel: &AtomicBool) {
    if let Some(pause) = pause {
        while pause.load(Ordering::Relaxed) && !cancel.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

fn emit(tx: Option<&Sender<Progress>>, p: Progress) {
    if let Some(tx) = tx {
        let _ = tx.send(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Default)]
    struct MemoryCache {
        fingerprints: Mutex<HashMap<PathBuf, (u64, Option<i64>, u32, Fingerprint)>>,
        metadata: Mutex<HashMap<PathBuf, (u64, Option<i64>, CachedMetadata)>>,
    }

    impl ScanCache for MemoryCache {
        fn fingerprint(
            &self,
            path: &Path,
            size: u64,
            modified_ms: Option<i64>,
            grid: u32,
        ) -> Option<Fingerprint> {
            let cache = self.fingerprints.lock().ok()?;
            cache.get(path).and_then(|(cached_size, cached_modified, cached_grid, fp)| {
                if *cached_size == size && *cached_modified == modified_ms && *cached_grid == grid
                {
                    Some(fp.clone())
                } else {
                    None
                }
            })
        }

        fn store_fingerprint(
            &self,
            path: &Path,
            size: u64,
            modified_ms: Option<i64>,
            grid: u32,
            fingerprint: &Fingerprint,
        ) {
            if let Ok(mut cache) = self.fingerprints.lock() {
                cache.insert(
                    path.to_path_buf(),
                    (size, modified_ms, grid, fingerprint.clone()),
                );
            }
        }

        fn metadata(
            &self,
            path: &Path,
            size: u64,
            modified_ms: Option<i64>,
        ) -> Option<CachedMetadata> {
            let cache = self.metadata.lock().ok()?;
            cache
                .get(path)
                .and_then(|(cached_size, cached_modified, metadata)| {
                    if *cached_size == size && *cached_modified == modified_ms {
                        Some(metadata.clone())
                    } else {
                        None
                    }
                })
        }

        fn store_metadata(
            &self,
            path: &Path,
            size: u64,
            modified_ms: Option<i64>,
            metadata: &CachedMetadata,
        ) {
            if let Ok(mut cache) = self.metadata.lock() {
                cache.insert(path.to_path_buf(), (size, modified_ms, metadata.clone()));
            }
        }

        fn embedding(
            &self,
            _path: &Path,
            _model: &str,
            _size: u64,
            _modified_ms: Option<i64>,
        ) -> Option<Vec<f32>> {
            None
        }

        fn store_embedding(
            &self,
            _path: &Path,
            _model: &str,
            _size: u64,
            _modified_ms: Option<i64>,
            _values: &[f32],
        ) {
        }
    }

    fn valid_image(path: &Path) {
        let image = image::RgbImage::from_fn(32, 32, |x, y| {
            image::Rgb([(x * 8) as u8, (y * 8) as u8, 120])
        });
        image.save(path).unwrap();
    }

    #[test]
    fn scan_waits_while_paused() {
        let root =
            std::env::temp_dir().join(format!("umdf-scan-pause-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        valid_image(&root.join("a.png"));
        valid_image(&root.join("b.png"));

        let mut cfg = ScanConfig::default();
        cfg.folders = vec![root.clone()];
        cfg.extensions = vec!["png".into()];
        let pause = Arc::new(AtomicBool::new(true));
        cfg.pause = Some(pause.clone());

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(run(&cfg).is_ok());
        });
        std::thread::sleep(Duration::from_millis(200));
        assert!(rx.try_recv().is_err(), "scan should be paused");
        pause.store(false, Ordering::Relaxed);
        assert!(rx.recv_timeout(Duration::from_secs(5)).unwrap());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn second_scan_reuses_cached_fingerprints() {
        let root =
            std::env::temp_dir().join(format!("umdf-scan-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        valid_image(&root.join("a.png"));
        valid_image(&root.join("b.png"));

        let mut cfg = ScanConfig::default();
        cfg.folders = vec![root.clone()];
        cfg.extensions = vec!["png".into()];
        let cache = MemoryCache::default();

        let first = run_with_progress_cached(&cfg, None, Some(&cache)).unwrap();
        assert_eq!(first.stats.files_fingerprinted, 2);
        assert_eq!(first.stats.fingerprints_computed, 2);
        assert_eq!(first.stats.fingerprints_from_cache, 0);
        assert_eq!(first.stats.metadata_computed, 2);
        assert_eq!(first.stats.metadata_from_cache, 0);

        let second = run_with_progress_cached(&cfg, None, Some(&cache)).unwrap();
        assert_eq!(second.stats.files_fingerprinted, 2);
        assert_eq!(second.stats.fingerprints_computed, 0);
        assert_eq!(second.stats.fingerprints_from_cache, 2);
        assert_eq!(second.stats.metadata_computed, 0);
        assert_eq!(second.stats.metadata_from_cache, 2);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn corrupt_image_is_skipped_and_the_scan_continues() {
        let root =
            std::env::temp_dir().join(format!("umdf-scan-skip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        valid_image(&root.join("a.png"));
        valid_image(&root.join("b.png"));
        // High-entropy bytes with a .jpg extension and no JPEG markers: the
        // kind of file that used to abort the whole scan.
        let junk: Vec<u8> = (0..4096u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        std::fs::write(root.join("broken.jpg"), junk).unwrap();

        let mut cfg = ScanConfig::default();
        cfg.folders = vec![root.clone()];
        cfg.extensions = vec!["png".into(), "jpg".into()];
        let report = run(&cfg).expect("a corrupt image must not abort the scan");

        assert_eq!(report.stats.files_scanned, 3);
        assert_eq!(report.stats.extract_errors, 1);
        assert_eq!(report.stats.files_fingerprinted, 2);
        assert_eq!(report.stats.groups, 1);

        let _ = std::fs::remove_dir_all(&root);
    }
}
