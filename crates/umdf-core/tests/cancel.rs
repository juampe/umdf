//! Integration test for the "Stop" path: a cancelled scan must return
//! promptly with partial results instead of blocking the caller forever.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::{Duration, Instant};

use umdf_core::{Progress, ScanConfig};

#[test]
fn stop_returns_partial_results_promptly() {
    let dir = std::env::temp_dir().join(format!("umdf-cancel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let base = dir.join("base.png");
    let img = image::GrayImage::from_fn(8, 8, |x, y| image::Luma([((x * 7 + y * 3) % 256) as u8]));
    img.save(&base).unwrap();
    for i in 0..200 {
        std::fs::copy(&base, dir.join(format!("img_{i:03}.png"))).unwrap();
    }

    let flag = Arc::new(AtomicBool::new(false));
    let cfg = ScanConfig {
        folders: vec![dir.clone()],
        cancel: Some(flag.clone()),
        ..Default::default()
    };

    let (tx, rx) = channel::<Progress>();
    let started = Instant::now();
    let handle = std::thread::spawn(move || umdf_core::run_with_progress(&cfg, Some(&tx)));

    // Wait until at least one file has been loaded, then request a stop.
    loop {
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Progress::Loaded { .. }) => {
                flag.store(true, Ordering::Relaxed);
                break;
            }
            Ok(_) => continue,
            Err(_) => panic!("the scan produced no Loaded progress event"),
        }
    }

    let report = handle
        .join()
        .expect("scan thread panicked")
        .expect("scan returned an error");

    assert!(
        started.elapsed() < Duration::from_secs(30),
        "cancelled scan did not return promptly"
    );
    assert!(report.stats.files_scanned >= 1);

    let _ = std::fs::remove_dir_all(&dir);
}
