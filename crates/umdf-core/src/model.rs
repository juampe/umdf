//! Data model shared by the scanner, the policies, the exporters and the UI.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Broad category of a media file. Drives which "extractor" is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaKind {
    Image,
    Video,
    Audio,
    Other,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Image => "image",
            MediaKind::Video => "video",
            MediaKind::Audio => "audio",
            MediaKind::Other => "other",
        }
    }
}

/// File metadata that does not require decoding pixels. Cached by path, size
/// and modification time so an unchanged file is not read again.
#[derive(Debug, Clone, Default)]
pub struct CachedMetadata {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub created_ms: Option<i64>,
    pub exif_taken_ms: Option<i64>,
    pub exif_fields: u32,
    pub exif_camera: bool,
    pub duration_ms: Option<u64>,
    pub has_audio: bool,
}

/// One scanned file plus everything the comparison stage learned about it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaItem {
    pub id: usize,
    pub path: PathBuf,
    pub kind: MediaKind,
    pub size: u64,

    pub width: Option<u32>,
    pub height: Option<u32>,

    /// Filesystem modification time.
    pub modified: Option<DateTime<Utc>>,
    /// Filesystem creation/birth time when the platform exposes it.
    pub created: Option<DateTime<Utc>>,
    /// `DateTimeOriginal` from EXIF, when present.
    pub exif_taken: Option<DateTime<Utc>>,
    /// Number of EXIF fields found (metadata richness proxy).
    pub exif_fields: u32,
    /// True when Make/Model (camera identity) are present.
    pub exif_camera: bool,

    /// Duration for audio/video, in milliseconds.
    pub duration_ms: Option<u64>,

    /// Whole-file content digest (SHA-256, hex). Used for exact duplicates.
    pub content_hash: Option<String>,

    /// Decoded media signature (audio PCM digest, video frame hash, ...).
    /// Used for "same media content" matches across containers.
    pub media_signature: Option<String>,

    /// Perceptual fingerprint. `None` for files we cannot decode.
    pub fingerprint: Option<Fingerprint>,
    /// True when the fingerprint was restored from the cache instead of being
    /// decoded again. Transient: it is not part of exports.
    #[serde(skip)]
    pub fingerprint_cached: bool,
    /// True when width/height/EXIF/duration came from `metadata_cache`.
    #[serde(skip)]
    pub metadata_cached: bool,
    /// True when the audio signature came from the cache.
    #[serde(skip)]
    pub media_signature_cached: bool,
    /// True when the video frame fingerprint came from the cache.
    #[serde(skip)]
    pub video_frame_cached: bool,

    /// Optional semantic embedding (L2-normalised). Requires the `embeddings`
    /// feature; `None` otherwise.
    pub embedding: Option<Vec<f32>>,
    /// True when the embedding came from the cache instead of inference.
    #[serde(skip)]
    pub embedding_cached: bool,

    /// Human readable reason why the file was skipped during extraction.
    pub extract_error: Option<String>,
}

impl MediaItem {
    pub fn new(id: usize, path: PathBuf, kind: MediaKind, size: u64) -> Self {
        Self {
            id,
            path,
            kind,
            size,
            width: None,
            height: None,
            modified: None,
            created: None,
            exif_taken: None,
            exif_fields: 0,
            exif_camera: false,
            duration_ms: None,
            content_hash: None,
            media_signature: None,
            fingerprint: None,
            fingerprint_cached: false,
            metadata_cached: false,
            media_signature_cached: false,
            video_frame_cached: false,
            embedding: None,
            embedding_cached: false,
            extract_error: None,
        }
    }

    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.to_string_lossy().into_owned())
    }

    pub fn dimensions_label(&self) -> String {
        match (self.width, self.height) {
            (Some(w), Some(h)) => format!("{w} x {h}"),
            _ => "-".to_string(),
        }
    }

    /// "Best" timestamp for age based policies: EXIF first, then mtime.
    pub fn effective_date(&self) -> Option<DateTime<Utc>> {
        self.exif_taken.or(self.modified).or(self.created)
    }

    pub fn megapixels(&self) -> f64 {
        match (self.width, self.height) {
            (Some(w), Some(h)) => (w as f64 * h as f64) / 1_000_000.0,
            _ => 0.0,
        }
    }
}

/// Perceptual fingerprint of an image (or of a decoded video frame).
///
/// `values` is a row-major `size * size` grid of `pixel - mean(pixel)`, in the
/// range `[-255, 255]`. Storing the mean-normalised grid (instead of a binary
/// hash) lets us reproduce a legacy sign metric and also offer
/// correlation based metrics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Fingerprint {
    pub size: u32,
    /// Mean brightness that was subtracted from every cell.
    pub mean: i16,
    /// Row-major `size * size` grid of `pixel - mean`.
    pub values: Vec<i16>,
}

impl Fingerprint {
    pub fn pixel(&self, row: u32, col: u32) -> i16 {
        let n = self.size as usize;
        self.values[row as usize * n + col as usize]
    }

    /// Original (un-centred) grey value of a cell.
    pub fn raw(&self, row: u32, col: u32) -> i16 {
        self.pixel(row, col) + self.mean
    }

    /// Points where the normalised brightness is positive, raster order, capped.
    pub fn positive_points(&self, cap: usize) -> Vec<(u32, u32)> {
        let n = self.size;
        let mut out = Vec::with_capacity(cap);
        'outer: for r in 0..n {
            for c in 0..n {
                if self.pixel(r, c) > 0 {
                    out.push((r, c));
                    if out.len() == cap {
                        break 'outer;
                    }
                }
            }
        }
        out
    }

    /// Points where the normalised brightness is negative, raster order, capped.
    pub fn negative_points(&self, cap: usize) -> Vec<(u32, u32)> {
        let n = self.size;
        let mut out = Vec::with_capacity(cap);
        'outer: for r in 0..n {
            for c in 0..n {
                if self.pixel(r, c) < 0 {
                    out.push((r, c));
                    if out.len() == cap {
                        break 'outer;
                    }
                }
            }
        }
        out
    }

}

/// How two files were considered a match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MatchKind {
    /// Byte-for-byte identical content.
    Exact,
    /// Perceptually similar content.
    Similar,
    /// Same decoded media content (e.g. audio PCM) across different files.
    MediaContent,
}

impl MatchKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MatchKind::Exact => "exact",
            MatchKind::Similar => "similar",
            MatchKind::MediaContent => "media-content",
        }
    }
}

/// One accepted pair. Similarity is 0..100 (100 == identical).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Match {
    pub a: usize,
    pub b: usize,
    pub similarity: f32,
    pub kind: MatchKind,
}

/// A connected component of the match graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub number: usize,
    /// Item ids, ordered best-first according to the active keep policy.
    pub items: Vec<usize>,
    /// The item that should be kept.
    pub keep: usize,
    /// Items that the policy suggests removing/moving.
    pub remove: Vec<usize>,
    pub max_similarity: f32,
    /// How likely the kept member is the original (0..100 heuristic).
    pub keep_confidence: f32,
    /// Human-readable reasons for the originality score.
    pub keep_reasons: Vec<String>,
    /// Total bytes that would be reclaimed if `remove` is applied.
    pub reclaimable_bytes: u64,
}
