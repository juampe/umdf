//! # umdf-core
//!
//! Engine of the **Universal Media Duplicate Finder (UMDF)**.
//!
//! Pipeline:
//!
//! 1. discover files (folder list, recursive walk, type/size filters);
//! 2. extract metadata and a mean-normalised greyscale fingerprint;
//! 3. exact comparison (size + SHA-256);
//! 4. perceptual comparison with an aspect-ratio gate;
//! 5. group matches and apply a keep policy;
//! 6. optionally trash, move or delete;
//! 7. optionally export CSV/XML.
//!
//! ## Example
//!
//! ```no_run
//! use umdf_core::{ScanConfig, scan};
//! use std::path::PathBuf;
//!
//! let mut cfg = ScanConfig::default();
//! cfg.folders.push(PathBuf::from("/home/me/Pictures"));
//! let report = scan(&cfg).unwrap();
//! for g in &report.groups {
//!     println!("group {} => {} duplicates", g.number, g.remove.len());
//! }
//! ```

/// Write a diagnostic to stderr and to `umdf.log` next to the configuration.
///
/// A Windows GUI has no console, so stderr alone loses every warning. The log
/// file also records the phase timings of each scan, which makes it possible
/// to tell a slow walk, extraction, comparison or grouping apart.
pub fn log_error(args: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let text = format!("{args}");
    let _ = writeln!(std::io::stderr(), "{text}");

    let path = log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = writeln!(file, "[{timestamp}] {text}");
    }
}

pub fn log_path() -> std::path::PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(std::path::PathBuf::from));
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".config"))
        });
    base.unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("umdf")
        .join("umdf.log")
}

pub mod action;
pub mod config;
#[cfg(feature = "embeddings")]
pub mod embeddings;
pub mod export;
pub mod fingerprint;
pub mod group;
pub mod hash;
pub mod media;
pub mod metadata;
pub mod model;
pub mod originality;
pub mod scan;
pub mod walk;

pub use config::{Action, EmbeddingConfig, KeepPolicy, Metric, ScanConfig};
pub use model::{CachedMetadata, Fingerprint, Group, Match, MatchKind, MediaItem, MediaKind};
pub use scan::{
    run as scan, run_with_progress, run_with_progress_cached, Progress, ScanCache, ScanReport,
    Stats,
};

/// Convenience: run a scan and return only the groups.
pub fn find_groups(cfg: &ScanConfig) -> anyhow::Result<Vec<Group>> {
    Ok(scan(cfg)?.groups)
}
