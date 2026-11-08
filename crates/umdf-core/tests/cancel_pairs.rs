//! Stop must be responsive during the (potentially O(n²)) comparison stage.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::{Duration, Instant};

use umdf_core::{Progress, ScanConfig};

#[test]
fn stop_during_comparison_is_prompt() {
    let dir = std::env::temp_dir().join(format!("umdf-pairs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 300 images with the same aspect ratio => ~45k candidate pairs.
    let img = image::GrayImage::from_fn(8, 8, |x, y| image::Luma([((x * 13 + y * 7) % 256) as u8]));
    for i in 0..300 {
        img.save(dir.join(format!("img_{i:03}.png"))).unwrap();
    }

    let flag = Arc::new(AtomicBool::new(false));
    let mut cfg = ScanConfig::default();
    cfg.folders = vec![dir.clone()];
    cfg.cancel = Some(flag.clone());
    cfg.similarity_threshold = 99.0;

    let (tx, rx) = channel::<Progress>();
    let started = Instant::now();
    let handle = std::thread::spawn(move || umdf_core::run_with_progress(&cfg, Some(&tx)));

    // Wait until the comparison stage starts, then request Stop.
    loop {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(Progress::Comparing { .. }) => {
                flag.store(true, Ordering::Relaxed);
                break;
            }
            Ok(_) => continue,
            Err(_) => panic!("the scan never reached the comparison stage"),
        }
    }

    let report = handle
        .join()
        .expect("scan thread panicked")
        .expect("scan returned an error");

    assert!(
        started.elapsed() < Duration::from_secs(30),
        "cancelled comparison did not return promptly"
    );
    assert!(report.stats.cancelled);
    assert!(report.stats.files_scanned >= 1);

    let _ = std::fs::remove_dir_all(&dir);
}
