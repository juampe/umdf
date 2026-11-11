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
    /// Times captured by the directory walk. Reusing them avoids a second
    /// filesystem round trip per file, which matters on network shares.
    pub modified: Option<chrono::DateTime<chrono::Utc>>,
    pub created: Option<chrono::DateTime<chrono::Utc>>,
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

        let excluded: Vec<String> = cfg
            .exclude_dirs
            .iter()
            .map(|name| name.trim().trim_matches(['/', '\\']).to_ascii_lowercase())
            .filter(|name| !name.is_empty())
            .collect();
        let entries = walker.into_iter().filter_entry(|entry| {
            // Never skip the folder the user explicitly selected.
            if entry.depth() == 0 || !entry.file_type().is_dir() {
                return true;
            }
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            !excluded.contains(&name)
        });
        for entry in entries.filter_map(Result::ok) {
            visited += 1;
            if visited.is_multiple_of(256) && cancel.load(Ordering::Relaxed) {
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
            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let size = metadata.len();
            if size < cfg.min_size {
                continue;
            }
            if let Some(max) = cfg.max_size {
                if size > max {
                    continue;
                }
            }
            let modified = metadata
                .modified()
                .ok()
                .and_then(crate::metadata::system_time_to_utc);
            let created = metadata
                .created()
                .ok()
                .and_then(crate::metadata::system_time_to_utc);
            let canonical = path.to_path_buf();
            if seen.insert(canonical.clone()) {
                out.push(Candidate {
                    path: canonical,
                    size,
                    extension: ext,
                    modified,
                    created,
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
    out.sort_by_key(|entry| entry.0.to_lowercase());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_trash_directories_case_insensitively() {
        let root = std::env::temp_dir().join(format!("umdf-walk-exclude-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("keep")).unwrap();
        std::fs::create_dir_all(root.join(".Trash")).unwrap();
        std::fs::create_dir_all(root.join(".dtrash")).unwrap();
        std::fs::create_dir_all(root.join("$RECYCLE.BIN")).unwrap();
        std::fs::write(root.join("keep").join("a.jpg"), b"a").unwrap();
        std::fs::write(root.join(".Trash").join("b.jpg"), b"b").unwrap();
        std::fs::write(root.join(".dtrash").join("c.jpg"), b"c").unwrap();
        std::fs::write(root.join("$RECYCLE.BIN").join("d.jpg"), b"d").unwrap();

        let cfg = ScanConfig {
            folders: vec![root.clone()],
            extensions: vec!["jpg".to_string()],
            exclude_dirs: vec![".dtrash".into(), ".trash".into(), "$recycle.bin".into()],
            ..Default::default()
        };
        let files = collect_candidates(&cfg);
        assert_eq!(files.len(), 1);
        assert!(files[0].path.ends_with("keep/a.jpg"));

        let _ = std::fs::remove_dir_all(&root);
    }

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
