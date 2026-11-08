//! The optional scan cache must return identical results and reuse values
//! across runs, invalidating them when the file changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use umdf_core::{run_with_progress_cached, Fingerprint, ScanCache, ScanConfig};

#[derive(Default)]
struct MemoryCache {
    fingerprints: Mutex<HashMap<PathBuf, (u64, Option<i64>, u32, Fingerprint)>>,
    hits: AtomicUsize,
    misses: AtomicUsize,
    stores: AtomicUsize,
}

impl ScanCache for MemoryCache {
    fn fingerprint(
        &self,
        path: &Path,
        size: u64,
        modified_ms: Option<i64>,
        grid: u32,
    ) -> Option<Fingerprint> {
        let guard = self.fingerprints.lock().unwrap();
        let found = guard.get(path).and_then(|(cached_size, cached_modified, cached_grid, fp)| {
            (*cached_size == size && *cached_modified == modified_ms && *cached_grid == grid)
                .then(|| fp.clone())
        });
        if found.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        found
    }

    fn store_fingerprint(
        &self,
        path: &Path,
        size: u64,
        modified_ms: Option<i64>,
        grid: u32,
        fingerprint: &Fingerprint,
    ) {
        self.stores.fetch_add(1, Ordering::Relaxed);
        self.fingerprints.lock().unwrap().insert(
            path.to_path_buf(),
            (size, modified_ms, grid, fingerprint.clone()),
        );
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

#[test]
fn cached_scan_returns_the_same_groups_and_reuses_values() {
    let dir = std::env::temp_dir().join(format!("umdf-cache-scan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let img = image::GrayImage::from_fn(32, 32, |x, y| {
        image::Luma([((x * 11 + y * 5) % 256) as u8])
    });
    let a = dir.join("a.png");
    let b = dir.join("b.png");
    img.save(&a).unwrap();
    std::fs::copy(&a, &b).unwrap();

    let mut cfg = ScanConfig::default();
    cfg.folders = vec![dir.clone()];
    cfg.pixel_tolerance = 0;
    cfg.similarity_threshold = 100.0;

    let cache = MemoryCache::default();

    let first = run_with_progress_cached(&cfg, None, Some(&cache)).expect("first scan");
    assert_eq!(first.groups.len(), 1, "identical images must group");
    assert_eq!(cache.stores.load(Ordering::Relaxed), 2);
    assert_eq!(cache.hits.load(Ordering::Relaxed), 0);
    assert_eq!(cache.misses.load(Ordering::Relaxed), 2);

    let second = run_with_progress_cached(&cfg, None, Some(&cache)).expect("second scan");
    assert_eq!(second.groups.len(), 1);
    assert_eq!(second.items.len(), 2);
    assert_eq!(
        cache.stores.load(Ordering::Relaxed),
        2,
        "a second scan must not rewrite cached fingerprints"
    );
    assert_eq!(
        cache.hits.load(Ordering::Relaxed),
        2,
        "both files must be served from the cache"
    );

    // Replacing one file invalidates only that entry.
    let other = image::GrayImage::from_fn(32, 32, |x, y| {
        image::Luma([((x * 3 + y * 17) % 256) as u8])
    });
    other.save(&b).unwrap();

    let third = run_with_progress_cached(&cfg, None, Some(&cache)).expect("third scan");
    assert_eq!(third.groups.len(), 0, "different images must not group");
    assert_eq!(cache.hits.load(Ordering::Relaxed), 3);
    assert_eq!(cache.misses.load(Ordering::Relaxed), 3);
    assert_eq!(cache.stores.load(Ordering::Relaxed), 3);

    let _ = std::fs::remove_dir_all(&dir);
}
