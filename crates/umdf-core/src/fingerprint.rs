//! Perceptual fingerprinting and similarity metrics.
//!
//! The pipeline:
//!
//! 1. decode the image (applying its EXIF orientation);
//! 2. convert to 8-bit grayscale;
//! 3. resample to `grid x grid`;
//! 4. subtract the mean brightness (mean normalisation);
//! 5. compare the normalised grids.
//!
//! The legacy sign metric is kept as `LegacySignAgreement` for comparison;
//! the default metric is `PixelTolerance`.

use std::path::Path;

use image::imageops::FilterType;
use image::{DynamicImage, GrayImage, ImageDecoder, ImageReader};

use crate::config::Metric;
use crate::model::Fingerprint;

/// Maximum number of sample points the legacy metric would keep per sign.
pub const LEGACY_POINT_CAP: usize = 250;
/// Aspect ratio gate observed in the original (max/min ratio must be <= 1.5).
pub const LEGACY_ASPECT_TOLERANCE: f64 = 1.5;

/// Decode an image applying its EXIF orientation.
///
/// Cameras and phones often store the pixels unrotated and put the rotation in
/// the EXIF tag. Without this step a tagged photo and its physically rotated
/// copy produce different fingerprints.
pub fn load_oriented(path: &Path) -> anyhow::Result<DynamicImage> {
    let reader = ImageReader::open(path)?.with_guessed_format()?;
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    Ok(img)
}

pub fn load_fingerprint(path: &Path, grid: u32) -> anyhow::Result<Fingerprint> {
    let img = load_oriented(path)?;
    Ok(from_dynamic_image(&img, grid))
}

pub fn from_dynamic_image(img: &DynamicImage, grid: u32) -> Fingerprint {
    let gray = img.to_luma8();
    from_gray_image(&gray, grid)
}

pub fn from_gray_image(gray: &GrayImage, grid: u32) -> Fingerprint {
    let n = grid.max(2);
    let resized = image::imageops::resize(gray, n, n, FilterType::Triangle);
    // Mean of the resampled grid (integer).
    let mut sum: u64 = 0;
    for p in resized.pixels() {
        sum += p.0[0] as u64;
    }
    let mean = (sum / (n as u64 * n as u64)) as i16;
    let mut values = Vec::with_capacity((n * n) as usize);
    for p in resized.pixels() {
        values.push(p.0[0] as i16 - mean);
    }
    Fingerprint {
        size: n,
        mean,
        values,
    }
}

/// Aspect ratio expressed as `max(w,h) / min(w,h)` (>= 1).
pub fn aspect_ratio(w: u32, h: u32) -> f64 {
    if w == 0 || h == 0 {
        return 1.0;
    }
    let (lo, hi) = if w < h { (w, h) } else { (h, w) };
    hi as f64 / lo as f64
}

/// Pairs whose aspect ratios differ by more than 1.5× are skipped.
pub fn aspect_ratio_compatible(a: (u32, u32), b: (u32, u32), tol: f64) -> bool {
    let ra = aspect_ratio(a.0, a.1);
    let rb = aspect_ratio(b.0, b.1);
    let hi = ra.max(rb);
    let lo = ra.min(rb);
    hi / lo <= tol
}

/// Returns `Some(similarity 0..=100)` when the pair passes the metric's own
/// acceptance rule, `None` otherwise.
pub fn similarity(a: &Fingerprint, b: &Fingerprint, metric: Metric, tolerance: u8) -> Option<f32> {
    debug_assert_eq!(a.size, b.size);
    match metric {
        Metric::PixelTolerance => Some(pixel_tolerance_similarity(a, b, tolerance)),
        Metric::Ssim => Some(ssim(a, b)),
        Metric::LegacySignAgreement => legacy_sign_agreement(a, b),
        Metric::NormalizedSignAgreement => Some(normalized_sign_agreement(a, b)),
        Metric::Pearson => Some(pearson(a, b)),
    }
}

/// Percentage of cells whose *raw* grey value differs by at most `tolerance`.
///
/// Pixel-difference tolerance: resample both pictures and count how many
/// cells are within the tolerance. UMDF compares luminance.
pub fn pixel_tolerance_similarity(a: &Fingerprint, b: &Fingerprint, tolerance: u8) -> f32 {
    let n = a.values.len().min(b.values.len());
    if n == 0 {
        return 100.0;
    }
    let tol = tolerance as i16;
    let mut matching = 0usize;
    for i in 0..n {
        let da = (a.values[i] + a.mean) as i32;
        let db = (b.values[i] + b.mean) as i32;
        if (da - db).abs() <= tol as i32 {
            matching += 1;
        }
    }
    (matching as f32 / n as f32) * 100.0
}

/// Dot product of two L2-normalised embeddings (= cosine similarity).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| x * y)
        .sum::<f32>()
        .clamp(-1.0, 1.0)
}

/// Cosine similarity mapped to 0..100.
pub fn cosine_percent(a: &[f32], b: &[f32]) -> f32 {
    (cosine_similarity(a, b).max(0.0)) * 100.0
}

/// Global structural similarity (SSIM) over the raw greyscale grids.
///
/// `C1 = (0.01·255)²` and `C2 = (0.03·255)²` are the standard stabilisation
/// constants. Returns 0..100 (`100` = identical).
pub fn ssim(a: &Fingerprint, b: &Fingerprint) -> f32 {
    let n = a.values.len().min(b.values.len());
    if n == 0 {
        return 100.0;
    }
    let nf = n as f64;
    let raw = |fp: &Fingerprint, i: usize| (fp.values[i] + fp.mean) as f64;

    let mut ma = 0.0;
    let mut mb = 0.0;
    for i in 0..n {
        ma += raw(a, i);
        mb += raw(b, i);
    }
    ma /= nf;
    mb /= nf;

    let mut va = 0.0;
    let mut vb = 0.0;
    let mut cov = 0.0;
    for i in 0..n {
        let da = raw(a, i) - ma;
        let db = raw(b, i) - mb;
        va += da * da;
        vb += db * db;
        cov += da * db;
    }
    va /= nf;
    vb /= nf;
    cov /= nf;

    let c1 = (0.01 * 255.0_f64).powi(2);
    let c2 = (0.03 * 255.0_f64).powi(2);
    let num = (2.0 * ma * mb + c1) * (2.0 * cov + c2);
    let den = (ma * ma + mb * mb + c1) * (va + vb + c2);
    if den == 0.0 {
        return 100.0;
    }
    ((num / den).clamp(0.0, 1.0) * 100.0) as f32
}

/// Legacy sign-agreement metric kept for comparison.
///
/// * `score` counts sign agreement over up to 250 positive and 250 negative
///   sample points of image A, using image B's normalised grid.
/// * `raw = score * 100 / 250`
/// * pairs with `raw < 50` are rejected;
/// * the displayed value is `clamp((raw - 50) * 2, 1, 100)`.
pub fn legacy_sign_agreement(a: &Fingerprint, b: &Fingerprint) -> Option<f32> {
    let pos = a.positive_points(LEGACY_POINT_CAP);
    let neg = a.negative_points(LEGACY_POINT_CAP);

    let mut score = 0usize;
    for (r, c) in &pos {
        if b.pixel(*r, *c) > 0 {
            score += 1;
        }
    }
    for (r, c) in &neg {
        if b.pixel(*r, *c) < 0 {
            score += 1;
        }
    }

    let raw = score as f32 * 100.0 / LEGACY_POINT_CAP as f32;
    if raw < 50.0 {
        None
    } else {
        Some(((raw - 50.0) * 2.0).clamp(1.0, 100.0))
    }
}

/// Corrected sign agreement: 50 % agreement (chance) maps to 0 %, 100 %
/// agreement maps to 100 %. This fixes the original's ~50 % floor.
pub fn normalized_sign_agreement(a: &Fingerprint, b: &Fingerprint) -> f32 {
    let mut considered = 0u64;
    let mut agree = 0u64;
    for (x, y) in a.values.iter().zip(b.values.iter()) {
        if *x == 0 && *y == 0 {
            continue;
        }
        considered += 1;
        let sx = x.signum();
        let sy = y.signum();
        if sx == sy {
            agree += 1;
        }
    }
    if considered == 0 {
        return 100.0;
    }
    let frac = agree as f32 / considered as f32;
    ((frac - 0.5) * 2.0 * 100.0).clamp(0.0, 100.0)
}

/// Pearson correlation of the two mean-normalised grids, mapped to 0..100.
pub fn pearson(a: &Fingerprint, b: &Fingerprint) -> f32 {
    let n = a.values.len().min(b.values.len());
    let mut sab = 0.0f64;
    let mut saa = 0.0f64;
    let mut sbb = 0.0f64;
    for i in 0..n {
        let x = a.values[i] as f64;
        let y = b.values[i] as f64;
        sab += x * y;
        saa += x * x;
        sbb += y * y;
    }
    if saa == 0.0 && sbb == 0.0 {
        return 100.0;
    }
    if saa == 0.0 || sbb == 0.0 {
        return 0.0;
    }
    let r = sab / (saa.sqrt() * sbb.sqrt());
    ((r.max(0.0)) * 100.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(values: Vec<i16>, size: u32) -> Fingerprint {
        Fingerprint {
            size,
            mean: 0,
            values,
        }
    }

    #[test]
    fn identical_is_100() {
        let a = fp(vec![10, -10, 5, -5, 0, 7, -7, 3, -3], 3);
        assert_eq!(similarity(&a, &a, Metric::NormalizedSignAgreement, 0), Some(100.0));
        assert_eq!(similarity(&a, &a, Metric::Pearson, 0), Some(100.0));
        assert_eq!(similarity(&a, &a, Metric::Ssim, 0), Some(100.0));
        // The legacy metric normalises by a fixed 250 samples, so a 3x3 grid
        // never reaches its acceptance threshold. Verify it on a full grid.
        let big = half_half();
        assert_eq!(similarity(&big, &big, Metric::LegacySignAgreement, 0), Some(100.0));
    }

    #[test]
    fn inverted_is_zero_for_sign_metric() {
        let a = fp(vec![10, -10, 5, -5, 1, -1, 7, -7, 2], 3);
        let b: Vec<i16> = a.values.iter().map(|v| -v).collect();
        let b = fp(b, 3);
        assert!(normalized_sign_agreement(&a, &b) < 1.0);
        assert!(pearson(&a, &b) < 1.0);
    }

    /// 20x20 grid: first 200 cells positive, last 200 negative.
    fn half_half() -> Fingerprint {
        let mut v = vec![100i16; 200];
        v.extend(vec![-100i16; 200]);
        fp(v, 20)
    }

    /// Deterministic pseudo-random +/-1 grid (uncorrelated with `half_half`).
    fn noise() -> Fingerprint {
        let mut state = 0x1234_5678u32;
        let mut v = Vec::with_capacity(400);
        for _ in 0..400 {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            v.push(if state & 0x10000 != 0 { 100 } else { -100 });
        }
        fp(v, 20)
    }

    #[test]
    fn random_like_pairs_do_not_score_100() {
        let (a, b) = (half_half(), noise());
        assert!(normalized_sign_agreement(&a, &b) < 10.0);
        assert!(pearson(&a, &b) < 10.0);
        assert!(ssim(&a, &b) < 20.0);
    }

    #[test]
    fn cosine_helpers() {
        let a = [1.0f32, 0.0, 0.0];
        let b = [1.0f32, 0.0, 0.0];
        let c = [0.0f32, 1.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);
        assert!(cosine_similarity(&a, &c).abs() < 1e-6);
        assert!((cosine_percent(&a, &b) - 100.0).abs() < 1e-4);
        assert!(cosine_percent(&a, &c).abs() < 1e-4);
    }

    #[test]
    fn pixel_tolerance_counts_close_values() {
        let a = fp(vec![100, 101, 102, 120], 2);
        let mut b = a.clone();
        b.values = vec![103, 100, 130, 118];
        // |100-103|=3 ok, |101-100|=1 ok, |102-130|=28 no, |120-118|=2 ok
        let sim = pixel_tolerance_similarity(&a, &b, 5);
        assert!((sim - 75.0).abs() < 0.01);
        assert_eq!(pixel_tolerance_similarity(&a, &a, 0), 100.0);
    }

    #[test]
    fn legacy_metric_has_a_high_floor() {
        // Documents the observed behaviour: unrelated sign patterns still
        // score far above zero with the legacy formula.
        let legacy = legacy_sign_agreement(&half_half(), &noise()).unwrap();
        assert!(legacy > 30.0);
    }
}
