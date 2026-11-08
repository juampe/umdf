//! Scan configuration and the decision policies.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Similarity metric used for "same content" detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Metric {
    /// Pixel tolerance: percentage of cells whose grey value is within
    /// `pixel_tolerance` levels of the other image. Images are resampled first,
    /// so it tolerates recompression, resizing and small edits.
    PixelTolerance,
    /// Global structural similarity (SSIM) over the resampled grids. Robust to
    /// local retouching (red-eye correction, small patches) and compression.
    Ssim,
    /// Legacy sign-agreement metric kept for comparison only; its raw scale is
    /// very permissive.
    LegacySignAgreement,
    /// Corrected sign agreement: 0 % = uncorrelated, 100 % = identical.
    NormalizedSignAgreement,
    /// Pearson correlation of the mean-normalised grids.
    Pearson,
}

impl Metric {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "dpf" | "pixel" | "tolerance" => Some(Metric::PixelTolerance),
            "ssim" => Some(Metric::Ssim),
            "legacy" | "legacy-sign" => Some(Metric::LegacySignAgreement),
            "sign" | "sign-agreement" => Some(Metric::NormalizedSignAgreement),
            "pearson" | "correlation" => Some(Metric::Pearson),
            _ => None,
        }
    }
}

/// Which item of a group should be kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeepPolicy {
    /// Keep the member that is most likely the original (resolution, quality,
    /// age, EXIF, file name and location). Default.
    MostLikelyOriginal,
    /// Keep the largest file (bytes).
    Largest,
    /// Keep the highest pixel count.
    HighestResolution,
    /// Keep the newest file (EXIF date, else mtime).
    Newest,
    /// Keep the oldest file (EXIF date, else mtime).
    Oldest,
    /// Keep the file whose name sorts first (stable, predictable).
    FirstName,
    /// Keep the shortest path (closest to the scan root).
    ShortestPath,
}

impl KeepPolicy {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "original" | "likely-original" | "auto" => Some(KeepPolicy::MostLikelyOriginal),
            "filesize" | "largest" => Some(KeepPolicy::Largest),
            "dimensions" | "resolution" => Some(KeepPolicy::HighestResolution),
            "newest" | "laterdate" => Some(KeepPolicy::Newest),
            "earlierdate" | "oldest" => Some(KeepPolicy::Oldest),
            "earlierexifdate" => Some(KeepPolicy::Oldest),
            "name" => Some(KeepPolicy::FirstName),
            "path" => Some(KeepPolicy::ShortestPath),
            _ => None,
        }
    }
}

/// What to do with the selected duplicates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    None,
    /// Send to the OS trash / recycle bin.
    Trash,
    /// Permanent delete.
    Delete,
    /// Move to this folder.
    MoveTo(PathBuf),
}

/// Optional semantic embeddings with ONNX Runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// Path to a `.onnx` image-embedding model (ResNet, CLIP, SigLIP…).
    pub model_path: PathBuf,
    /// Square model input size (224 for ResNet/CLIP ViT-B/32).
    pub input_size: u32,
    pub batch_size: usize,
    /// Cosine-similarity threshold in 0..1.
    pub threshold: f32,
    /// Use the CUDA execution provider when available.
    pub use_cuda: bool,
    /// Per-channel normalization mean (RGB).
    pub mean: [f32; 3],
    /// Per-channel normalization std (RGB).
    pub std: [f32; 3],
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            model_path: PathBuf::new(),
            input_size: 224,
            batch_size: 32,
            threshold: 0.90,
            use_cuda: true,
            mean: [0.485, 0.456, 0.406],
            std: [0.229, 0.224, 0.225],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanConfig {
    pub folders: Vec<PathBuf>,
    pub recursive: bool,
    /// Directory names to skip while walking (trash/recycle bins, ...).
    pub exclude_dirs: Vec<String>,

    /// Lower-case extensions without the dot. Empty = every configured kind.
    pub extensions: Vec<String>,

    pub min_width: u32,
    pub min_height: u32,
    pub min_size: u64,
    pub max_size: Option<u64>,

    /// Grid edge used for the perceptual fingerprint (75 is the default
    /// resample size for the `dpf` metric).
    pub grid: u32,
    pub metric: Metric,
    /// Grey-level tolerance for `Metric::PixelTolerance` (0..=255).
    pub pixel_tolerance: u8,
    /// 0..100. Pairs below this are discarded.
    pub similarity_threshold: f32,

    /// Skip the perceptual stage and only report byte-identical files.
    pub only_identical: bool,

    /// Compare the *content* of media containers (video/audio) instead of the
    /// whole file, so re-muxed files match.
    pub compare_media_content: bool,
    /// Worker threads for decoding/hashing/comparing. `0` = one per available
    /// CPU (rayon default).
    pub threads: usize,
    /// Optional semantic embeddings (requires the `embeddings` feature).
    pub embeddings: Option<EmbeddingConfig>,
    /// Position (seconds) used to grab the video frame for the image metric.
    pub frame_position_secs: f64,
    /// External ffmpeg/ffprobe. `None` disables media content comparison.
    pub ffmpeg: Option<PathBuf>,

    pub keep_policy: KeepPolicy,
    /// After the policy, keep one item per group even if the policy marked
    /// everything.
    pub leave_one: bool,
    pub action: Action,
    /// When false nothing is modified on disk; only a plan is produced.
    pub apply: bool,

    /// Cooperative cancellation flag (not serialised).
    #[serde(skip)]
    pub cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl ScanConfig {
    pub fn is_cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .map(|c| c.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(false)
    }
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            folders: Vec::new(),
            recursive: true,
            exclude_dirs: Vec::new(),
            extensions: default_image_extensions(),
            min_width: 0,
            min_height: 0,
            min_size: 0,
            max_size: None,
            grid: 75,
            metric: Metric::PixelTolerance,
            pixel_tolerance: 10,
            similarity_threshold: 90.0,
            only_identical: false,
            compare_media_content: false,
            threads: 0,
            embeddings: None,
            frame_position_secs: 1.0,
            ffmpeg: None,
            keep_policy: KeepPolicy::MostLikelyOriginal,
            leave_one: true,
            action: Action::None,
            apply: false,
            cancel: None,
        }
    }
}

pub fn default_image_extensions() -> Vec<String> {
    [
        "bmp", "jpg", "jpeg", "gif", "png", "tif", "tiff", "cr2", "webp", "jp2", "j2k", "tga",
        "dds", "hdr", "exr", "ppm", "pgm", "pbm", "ico", "avif",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

pub fn default_video_extensions() -> Vec<String> {
    ["mp4", "m4v", "mkv", "ts", "mpg", "mpeg", "avi", "mov", "webm", "3gp", "3g2", "wmv"]
        .into_iter()
        .map(String::from)
        .collect()
}

pub fn default_audio_extensions() -> Vec<String> {
    ["m4a", "m4b", "alac", "aac", "mp3", "flac", "ogg", "opus", "wav", "wma", "aiff"]
        .into_iter()
        .map(String::from)
        .collect()
}

/// Extensions that we treat as "media content can be probed with ffmpeg".
pub fn is_video_ext(ext: &str) -> bool {
    default_video_extensions().iter().any(|e| e == ext)
}

pub fn is_audio_ext(ext: &str) -> bool {
    default_audio_extensions().iter().any(|e| e == ext)
}

/// Extensions decoded by the `image` crate.
pub fn is_image_ext(ext: &str) -> bool {
    matches!(
        ext,
        "bmp"
            | "jpg"
            | "jpeg"
            | "gif"
            | "png"
            | "tif"
            | "tiff"
            | "webp"
            | "ico"
            | "tga"
            | "dds"
            | "hdr"
            | "exr"
            | "ppm"
            | "pgm"
            | "pbm"
            | "avif"
            | "jp2"
            | "j2k"
            | "qoi"
    )
}
