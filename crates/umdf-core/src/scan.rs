//! Scan orchestrator: discovery → extraction → matching → grouping.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
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
use crate::model::{Fingerprint, Group, Match, MatchKind, MediaItem, MediaKind};
use crate::walk;

#[derive(Debug, Clone)]
pub enum Progress {
    Discovered { total: usize },
    Loaded { index: usize, total: usize, path: PathBuf },
    Embedding { done: usize, total: usize },
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
                extract_item(
                    base + k,
                    cand,
                    cfg,
                    cancel_ref,
                    ffprobe_bin.as_deref(),
                    ffmpeg_bin.as_deref(),
                    cache,
                )
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
                if item.fingerprint.is_some() {
                    stats.files_fingerprinted += 1;
                }
                if item.media_signature.is_some() {
                    stats.files_media_signed += 1;
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
            let ids: Vec<usize> = items
                .iter()
                .filter(|item| item.kind == MediaKind::Image)
                .map(|item| item.id)
                .collect();
            if !ids.is_empty() {
                emit(
                    tx,
                    Progress::Embedding {
                        done: 0,
                        total: ids.len(),
                    },
                );
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
                if !missing.is_empty() {
                    let paths: Vec<PathBuf> =
                        missing.iter().map(|slot| meta[*slot].0.clone()).collect();
                    let mut engine = crate::embeddings::EmbeddingEngine::new(ecfg)?;
                    if engine.forced_cpu() {
                        eprintln!("UMDF: semantic embeddings are running on the CPU");
                    }
                    let computed = engine.embed_paths(&paths)?;
                    for (slot, vector) in missing.into_iter().zip(computed) {
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
    let mut hashes: Vec<(usize, String)> = Vec::with_capacity(hash_targets.len());
    let mut hashed_done = 0usize;
    for chunk in hash_targets.chunks(16) {
        if cfg.is_cancelled() {
            break;
        }
        let part: Vec<(usize, String)> = chunk
            .par_iter()
            .filter_map(|id| {
                if cfg.is_cancelled() {
                    return None;
                }
                let it = &items[*id];
                hash::file_hash_cancellable(&it.path, cancel_ref)
                    .ok()
                    .flatten()
                    .map(|h| (*id, h))
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
    for (id, h) in hashes {
        items[id].content_hash = Some(h);
        stats.files_hashed += 1;
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
    item.modified = metadata::modified(&cand.path);
    item.created = metadata::created(&cand.path);

    match kind {
        MediaKind::Image => {
            if let Some((w, h)) = metadata::image_dimensions(&cand.path) {
                item.width = Some(w);
                item.height = Some(h);
            }
            if let Some(meta) = metadata::exif_meta(&cand.path) {
                item.exif_taken = meta.taken;
                item.exif_fields = meta.field_count;
                item.exif_camera = meta.camera;
            }
            if below_min_dimensions(cfg, &item) {
                return None;
            }
            let modified_ms = item.modified.map(|date| date.timestamp_millis());
            match cache.and_then(|cache| {
                cache.fingerprint(&cand.path, cand.size, modified_ms, cfg.grid)
            }) {
                Some(fp) => item.fingerprint = Some(fp),
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
            let mut has_audio = kind == MediaKind::Audio;
            if let Some(probe_bin) = ffprobe {
                if let Ok(p) = media::probe(probe_bin, &cand.path, cancel) {
                    item.width = p.width;
                    item.height = p.height;
                    item.duration_ms = p.duration_ms;
                    has_audio = p.has_audio;
                    if below_min_dimensions(cfg, &item) {
                        return None;
                    }
                }
            }
            if cfg.compare_media_content {
                if let Some(ffmpeg) = ffmpeg {
                    if kind == MediaKind::Video {
                        if let Ok(fp) = video_frame_fingerprint(
                            ffmpeg,
                            &cand.path,
                            cfg.frame_position_secs,
                            cfg.grid,
                            cancel,
                        ) {
                            item.fingerprint = Some(fp);
                        }
                    }
                    if has_audio {
                        if let Ok(signature) = media::audio_pcm_hash(
                            ffmpeg,
                            &cand.path,
                            cfg.frame_position_secs,
                            AUDIO_SAMPLE_SECONDS,
                            cancel,
                        ) {
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

fn emit(tx: Option<&Sender<Progress>>, p: Progress) {
    if let Some(tx) = tx {
        let _ = tx.send(p);
    }
}
