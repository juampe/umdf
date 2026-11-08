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

pub mod action;
pub mod config;
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

pub use config::{Action, KeepPolicy, Metric, ScanConfig};
pub use model::{Fingerprint, Group, Match, MatchKind, MediaItem, MediaKind};
pub use scan::{run as scan, run_with_progress, Progress, ScanReport, Stats};

/// Convenience: run a scan and return only the groups.
pub fn find_groups(cfg: &ScanConfig) -> anyhow::Result<Vec<Group>> {
    Ok(scan(cfg)?.groups)
}
