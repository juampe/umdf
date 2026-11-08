//! File discovery: recursive walking with extension and size filters.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use walkdir::WalkDir;

use crate::config::ScanConfig;

/// One file discovered on disk, before any decoding happens.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: PathBuf,
    pub size: u64,
    pub extension: String,
}

/// Walk every configured folder and return the candidate files.
///
/// * `extensions` is compared case-insensitively against the file extension.
/// * `min_size`/`max_size` are applied here (cheap, avoids decoding).
/// * Symlinked directories are not followed to avoid cycles.
pub fn collect_candidates(cfg: &ScanConfig) -> Vec<Candidate> {
    collect_candidates_cancellable(cfg, &NEVER).0
}

static NEVER: AtomicBool = AtomicBool::new(false);

/// Cancellable walk. Returns the files found so far and whether it stopped
/// early because the caller pressed Stop.
pub fn collect_candidates_cancellable(
    cfg: &ScanConfig,
    cancel: &AtomicBool,
) -> (Vec<Candidate>, bool) {
    let allowed: BTreeSet<String> = cfg
        .extensions
        .iter()
        .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .collect();

    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut visited = 0usize;

    for folder in &cfg.folders {
        let walker = if cfg.recursive {
            WalkDir::new(folder).follow_links(false)
        } else {
            WalkDir::new(folder).max_depth(1).follow_links(false)
        };

        for entry in walker.into_iter().filter_map(Result::ok) {
            visited += 1;
            if visited % 256 == 0 && cancel.load(Ordering::Relaxed) {
                return (out, true);
            }
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if !allowed.is_empty() && !allowed.contains(&ext) {
                continue;
            }
            let size = match entry.metadata() {
                Ok(m) => m.len(),
                Err(_) => continue,
            };
            if size < cfg.min_size {
                continue;
            }
            if let Some(max) = cfg.max_size {
                if size > max {
                    continue;
                }
            }
            let canonical = path.to_path_buf();
            if seen.insert(canonical.clone()) {
                out.push(Candidate {
                    path: canonical,
                    size,
                    extension: ext,
                });
            }
        }
    }

    out.sort_by(|a, b| a.path.cmp(&b.path));
    (out, false)
}

/// True when `path` lives inside `root` (used by the "shortest path" policy).
pub fn is_under(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
}

/// List the immediate sub-directories of `path`, sorted case-insensitively.
///
/// Used by the UI folder browser. Errors (missing folder, no permission) yield
/// an empty list instead of failing.
pub fn list_directories(path: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if !is_dir {
                continue;
            }
            out.push((
                entry.file_name().to_string_lossy().to_string(),
                entry.path(),
            ));
        }
    }
    out.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_directories_is_sorted_and_dirs_only() {
        let root = std::env::temp_dir().join(format!("umdf-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("bravo")).unwrap();
        std::fs::create_dir_all(root.join("Alpha")).unwrap();
        std::fs::write(root.join("file.txt"), b"x").unwrap();

        let dirs = list_directories(&root);
        let names: Vec<&str> = dirs.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "bravo"]);

        let _ = std::fs::remove_dir_all(&root);
    }
}
