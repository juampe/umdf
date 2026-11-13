//! "Which member of this group is most likely the original?" heuristic.
//!
//! Duplicates usually come from an original that was resized, re-compressed,
//! renamed, copied to another folder or re-exported by an editor. The signals
//! below are individually weak, so they are combined with fixed weights into a
//! 0..100 score. It is a *heuristic estimate*, not a calibrated probability;
//! the reasons are kept so the UI/CLI can explain the choice.
//!
//! | Signal | Weight | More original when… |
//! |--------|-------:|---------------------|
//! | resolution | 0.20 | more pixels (photos/video) |
//! | quality    | 0.34 | more bytes per pixel (or per second of audio) |
//! | age        | 0.12 | older file time, but only when the gap is weeks/months |
//! | metadata   | 0.10 | richer EXIF, camera make/model present |
//! | name       | 0.14 | no "copy", "(1)", "edited", "resized", "whatsapp"… |
//! | location   | 0.10 | DCIM/Camera/Pictures rather than Downloads/Desktop/temp |
//!
//! Quality is the heaviest signal by design: it is direct evidence of
//! recompression. At identical dimensions a file that is several times larger
//! is almost always the source, while file dates, extra EXIF (editors often add
//! fields) and copy-marked names are weaker and easily rewritten. Its weight is
//! intentionally larger than the resolution weight and larger than the usual
//! combined age/metadata/name advantage, so a dramatic quality gap wins.

use std::collections::HashMap;

use crate::model::{MediaItem, MediaKind};

// Resolution alone must not beat the fingerprint of a derivative (copy name,
// download folder, lower bytes-per-pixel), so it is weighted below the sum of
// the "is this a copy?" signals. Quality is deliberately the heaviest signal:
// at identical dimensions a 7x smaller file is overwhelmingly a recompressed
// derivative, whereas an older file date is weak evidence (copies, sync tools
// and editors often preserve or rewrite timestamps).
const W_RESOLUTION: f32 = 0.20;
const W_QUALITY: f32 = 0.34;
const W_AGE: f32 = 0.12;
const W_METADATA: f32 = 0.10;
const W_NAME: f32 = 0.14;
const W_LOCATION: f32 = 0.10;

#[derive(Debug, Clone, Default)]
pub struct Signals {
    pub resolution: f32,
    pub quality: f32,
    pub age: f32,
    pub metadata: f32,
    pub name: f32,
    pub location: f32,
}

/// Stable reason codes. The CLI/UI translate them, so the core stays
/// language-neutral.
pub mod reason {
    pub const RESOLUTION: &str = "resolution";
    pub const QUALITY: &str = "quality";
    pub const AGE: &str = "age";
    pub const METADATA: &str = "metadata";
    pub const NAME: &str = "name";
    pub const LOCATION: &str = "location";
    pub const CONSENSUS: &str = "consensus";
}

#[derive(Debug, Clone, Default)]
pub struct Assessment {
    /// 0..100 heuristic score.
    pub confidence: f32,
    pub signals: Signals,
    pub reasons: Vec<String>,
}

/// Assess every member of a group. Values that are only meaningful relative to
/// the group (resolution, bytes per pixel, age) are normalised inside it.
pub fn assess_group(items: &[MediaItem], members: &[usize]) -> HashMap<usize, Assessment> {
    let n = members.len();
    let resolutions: Vec<f64> = members.iter().map(|id| resolution(&items[*id])).collect();
    let qualities: Vec<f64> = members.iter().map(|id| quality(&items[*id])).collect();
    let ages: Vec<Option<i64>> = members.iter().map(|id| age_days(&items[*id])).collect();
    let metadata: Vec<f64> = members.iter().map(|id| metadata_raw(&items[*id])).collect();

    let res_range = range(&resolutions);
    let qual_range = range(&qualities);
    let meta_range = range(&metadata);
    let qual_mean = if qualities.is_empty() {
        0.0
    } else {
        qualities.iter().sum::<f64>() / qualities.len() as f64
    };

    let mut out = HashMap::with_capacity(n);
    for (pos, id) in members.iter().enumerate() {
        let item = &items[*id];
        let signals = Signals {
            resolution: normalise(resolutions[pos], res_range),
            quality: normalise_log(qualities[pos], qual_range),
            age: age_score(ages[pos], &ages),
            metadata: normalise(metadata[pos], meta_range),
            name: name_score(item),
            location: location_score(item),
        };
        let confidence = 100.0
            * (W_RESOLUTION * signals.resolution
                + W_QUALITY * signals.quality
                + W_AGE * signals.age
                + W_METADATA * signals.metadata
                + W_NAME * signals.name
                + W_LOCATION * signals.location);
        out.insert(
            *id,
            Assessment {
                confidence: confidence.clamp(0.0, 100.0),
                signals,
                reasons: Vec::new(),
            },
        );
    }

    // Explain the best member relative to the others.
    if let Some(best) = out
        .iter()
        .max_by(|a, b| {
            a.1.confidence
                .partial_cmp(&b.1.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(id, _)| *id)
    {
        let mut reasons = Vec::new();
        let res_max = res_range.map(|(_, hi)| hi).unwrap_or(0.0);
        let qual_max = qual_range.map(|(_, hi)| hi).unwrap_or(0.0);
        let pos = members.iter().position(|m| *m == best).unwrap_or(0);
        if res_max > 0.0 && resolutions[pos] >= res_max {
            reasons.push(reason::RESOLUTION.to_string());
        }
        if qual_max > 0.0
            && qualities[pos] >= qual_max
            && qual_mean > 0.0
            && qualities[pos] > qual_mean * 1.05
        {
            reasons.push(reason::QUALITY.to_string());
        }
        if age_score(ages[pos], &ages) >= 0.9 {
            reasons.push(reason::AGE.to_string());
        }
        if out.get(&best).map(|a| a.signals.metadata).unwrap_or(0.0) >= 0.75 {
            reasons.push(reason::METADATA.to_string());
        }
        if name_score(&items[best]) >= 0.95 {
            reasons.push(reason::NAME.to_string());
        }
        if location_score(&items[best]) >= 0.9 {
            reasons.push(reason::LOCATION.to_string());
        }
        if reasons.is_empty() {
            reasons.push(reason::CONSENSUS.to_string());
        }
        reasons.truncate(4);
        if let Some(assessment) = out.get_mut(&best) {
            assessment.reasons = reasons;
        }
    }

    out
}

fn resolution(item: &MediaItem) -> f64 {
    match (item.width, item.height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => w as f64 * h as f64,
        _ => 0.0,
    }
}

fn quality(item: &MediaItem) -> f64 {
    match item.kind {
        MediaKind::Image | MediaKind::Video => {
            let pixels = resolution(item);
            if pixels > 0.0 {
                item.size as f64 / pixels
            } else {
                item.size as f64
            }
        }
        MediaKind::Audio => match item.duration_ms {
            Some(ms) if ms > 0 => item.size as f64 / (ms as f64 / 1000.0),
            _ => item.size as f64,
        },
        MediaKind::Other => item.size as f64,
    }
}

/// File age, deliberately *not* the EXIF capture date: a derivative usually
/// keeps the capture date but is written to disk later.
fn age_days(item: &MediaItem) -> Option<i64> {
    item.modified
        .or(item.created)
        .map(|d| d.timestamp() / 86_400)
}

fn range(values: &[f64]) -> Option<(f64, f64)> {
    let mut iter = values.iter().copied();
    let first = iter.next()?;
    let mut lo = first;
    let mut hi = first;
    for v in iter {
        if v < lo {
            lo = v;
        }
        if v > hi {
            hi = v;
        }
    }
    if (hi - lo).abs() < f64::EPSILON {
        None
    } else {
        Some((lo, hi))
    }
}

fn normalise(value: f64, range: Option<(f64, f64)>) -> f32 {
    match range {
        Some((lo, hi)) if hi > lo => ((value - lo) / (hi - lo)) as f32,
        _ => 0.5,
    }
}

/// Quality is compared on a logarithmic scale: each doubling in bytes per
/// pixel is a comparable step in compression, no matter whether the files are
/// tiny thumbnails or large camera JPEGs. A linear min/max scale makes the same
/// relative difference look negligible when one outlier dominates the group.
fn normalise_log(value: f64, range: Option<(f64, f64)>) -> f32 {
    match range {
        Some((lo, hi)) if hi > lo => {
            // `ln_1p` keeps zero/negative values finite while preserving order.
            let value = value.max(0.0).ln_1p();
            let lo = lo.max(0.0).ln_1p();
            let hi = hi.max(0.0).ln_1p();
            if hi > lo {
                ((value - lo) / (hi - lo)) as f32
            } else {
                0.5
            }
        }
        _ => 0.5,
    }
}

fn age_score(day: Option<i64>, all: &[Option<i64>]) -> f32 {
    let Some(day) = day else { return 0.5 };
    let mut lo = i64::MAX;
    let mut hi = i64::MIN;
    let mut seen = false;
    for value in all.iter().flatten() {
        seen = true;
        lo = lo.min(*value);
        hi = hi.max(*value);
    }
    if !seen || hi == lo {
        return 0.5;
    }
    // Oldest day -> 1.0, newest -> 0.0, but only once the group spans a
    // meaningful amount of time. A copy written an hour (or a day) after the
    // original is normal, and sync/editing tools rewrite timestamps, so small
    // gaps are weak evidence. The signal saturates at 30 days.
    let span = (hi - lo) as f32;
    let strength = (span / 30.0).clamp(0.0, 1.0);
    let rank = ((hi - day) as f32 / span).clamp(0.0, 1.0);
    0.5 + 0.5 * strength * rank
}

/// Absolute metadata richness (0..1). Normalised inside the group so that a
/// group where nobody has EXIF is neutral instead of uniformly penalised.
fn metadata_raw(item: &MediaItem) -> f64 {
    if item.kind != MediaKind::Image {
        return 0.0;
    }
    let mut score = (item.exif_fields as f64 / 40.0).min(1.0) * 0.6;
    if item.exif_camera {
        score += 0.4;
    }
    score.clamp(0.0, 1.0)
}

/// 1.0 = clean/camera name, lower = looks like a derivative.
pub fn name_score(item: &MediaItem) -> f32 {
    let name = item.file_name().to_ascii_lowercase();
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(&name);

    let camera_prefixes = [
        "img_", "dsc_", "dscf", "dscn", "pxl_", "gopr", "g001", "dji_", "mvimg", "vid_", "p1000",
        "cimg", "sam_", "photo_",
    ];
    let strong = [
        "copy",
        "copia",
        "edited",
        "editado",
        "resized",
        "resize",
        "small",
        "thumb",
        "thumbnail",
        "compressed",
        "compress",
        "whatsapp",
        "telegram",
        "facebook",
        "instagram",
        "screenshot",
        "captura",
        "capture",
        "duplicate",
        "dup",
    ];

    let hits = strong.iter().filter(|m| stem.contains(**m)).count();
    let suffix = has_copy_suffix(stem);

    // A clean camera-style name is the strongest possible signal.
    if hits == 0 && !suffix && camera_prefixes.iter().any(|p| stem.starts_with(p)) {
        return 1.0;
    }

    let mut score = 1.0f32;
    score -= 0.30 * hits as f32;
    if suffix {
        score -= 0.20;
    }
    score.clamp(0.0, 1.0)
}

fn has_copy_suffix(stem: &str) -> bool {
    let bytes = stem.as_bytes();
    if let Some(open) = stem.rfind('(') {
        if stem.ends_with(')')
            && stem[open + 1..stem.len() - 1]
                .chars()
                .all(|c| c.is_ascii_digit())
        {
            return true;
        }
    }
    // trailing "-2", "_3", " 4"
    let mut idx = bytes.len();
    while idx > 0 && bytes[idx - 1].is_ascii_digit() {
        idx -= 1;
    }
    idx > 0
        && idx < bytes.len()
        && matches!(bytes[idx - 1], b'-' | b'_' | b' ')
        && (bytes.len() - idx) <= 2
}

pub fn location_score(item: &MediaItem) -> f32 {
    let path = item.path.to_string_lossy().to_ascii_lowercase();
    let original = [
        "/dcim/",
        "/camera/",
        "/cameras/",
        "/photos/",
        "/pictures/",
        "/fotos/",
    ];
    let derivative = [
        "downloads",
        "descargas",
        "desktop",
        "escritorio",
        "/tmp/",
        "temp/",
        "whatsapp",
        "telegram",
        "export",
        "exports",
        "backup",
        "backups",
    ];
    if original.iter().any(|m| path.contains(m)) {
        return 1.0;
    }
    if derivative.iter().any(|m| path.contains(m)) {
        return 0.25;
    }
    0.6
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn item(id: usize, name: &str, size: u64, w: u32, h: u32) -> MediaItem {
        let mut it = MediaItem::new(
            id,
            PathBuf::from(format!("/photos/DCIM/{name}")),
            MediaKind::Image,
            size,
        );
        it.width = Some(w);
        it.height = Some(h);
        it
    }

    #[test]
    fn camera_original_beats_resized_copy() {
        let mut original = item(0, "IMG_1234.JPG", 3_000_000, 4000, 3000);
        original.exif_fields = 42;
        original.exif_camera = true;
        let mut copy = item(1, "IMG_1234 - Copy (1).jpg", 300_000, 1200, 900);
        copy.exif_fields = 0;
        copy.exif_camera = false;

        let scores = assess_group(&[original, copy.clone()], &[0, 1]);
        assert!(scores[&0].confidence > scores[&1].confidence);
        assert!(scores[&0].reasons.iter().any(|r| r == reason::RESOLUTION));
    }

    #[test]
    fn copy_markers_reduce_name_score() {
        let clean = item(0, "IMG_0001.JPG", 1, 10, 10);
        let copy = item(1, "IMG_0001 - copia.jpg", 1, 10, 10);
        let thumbs = item(2, "thumb_IMG_0001_small.jpg", 1, 10, 10);
        assert_eq!(name_score(&clean), 1.0);
        assert!(name_score(&copy) < 0.8);
        assert!(name_score(&thumbs) < name_score(&copy));
    }

    #[test]
    fn downloads_are_less_original_than_camera_folder() {
        let a = item(0, "a.jpg", 1, 10, 10);
        let mut b = item(1, "b.jpg", 1, 10, 10);
        b.path = PathBuf::from("/home/me/Downloads/b.jpg");
        assert!(location_score(&a) > location_score(&b));
    }

    fn dated(mut it: MediaItem, timestamp: i64) -> MediaItem {
        it.modified = chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0);
        it
    }

    #[test]
    fn much_higher_quality_wins_at_equal_resolution() {
        // Reproduces the reported pair: same 4624x3472 photo, but one copy is
        // 945 KB and the other 6.56 MB. The smaller file is older and has no
        // copy marker in its name; that must not make it the original.
        let mut small = dated(
            item(0, "2021-12-11 13.47.57.jpg", 945_000, 4624, 3472),
            1_638_900_000, // 2021-12-07
        );
        // The recompressed file may even have richer EXIF (editors add fields)
        // and a cleaner name. Quality must still win.
        small.exif_fields = 15;
        small.exif_camera = true;
        let mut big = dated(
            item(1, "2021-12-11 14.47.57_14.jpg", 6_560_000, 4624, 3472),
            1_654_000_000, // 2022-06-01
        );
        big.exif_fields = 12;
        big.exif_camera = true;

        let scores = assess_group(&[small, big.clone()], &[0, 1]);
        assert!(
            scores[&1].confidence > scores[&0].confidence,
            "larger file must win: {} vs {}",
            scores[&1].confidence,
            scores[&0].confidence
        );
        assert!(scores[&1].reasons.iter().any(|r| r == reason::QUALITY));
    }

    #[test]
    fn hour_apart_dates_are_not_strong_evidence() {
        let a = dated(item(0, "a.jpg", 1_000_000, 4000, 3000), 1_638_000_000);
        let b = dated(item(1, "b.jpg", 1_000_000, 4000, 3000), 1_638_003_600);

        let scores = assess_group(&[a, b], &[0, 1]);
        assert!((scores[&0].signals.age - 0.5).abs() < 0.01);
        assert!((scores[&1].signals.age - 0.5).abs() < 0.01);
        // No age reason can be justified by a one-hour gap.
        assert!(!scores[&0].reasons.iter().any(|r| r == reason::AGE));
        assert!(!scores[&1].reasons.iter().any(|r| r == reason::AGE));
    }

    #[test]
    fn older_file_wins_when_quality_matches_and_gap_is_large() {
        let old = dated(item(0, "old.jpg", 1_000_000, 4000, 3000), 1_600_000_000);
        let new = dated(item(1, "new.jpg", 1_000_000, 4000, 3000), 1_650_000_000);

        let scores = assess_group(&[old, new], &[0, 1]);
        assert!(scores[&0].confidence > scores[&1].confidence);
        assert!(scores[&0].reasons.iter().any(|r| r == reason::AGE));
    }
}
