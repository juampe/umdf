//! Applying the plan: trash, permanent delete or move.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{Action, ScanConfig};
use crate::model::{Group, MediaItem};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResult {
    pub path: PathBuf,
    pub action: String,
    pub ok: bool,
    pub destination: Option<PathBuf>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActionReport {
    pub results: Vec<ActionResult>,
    pub deleted: usize,
    pub moved: usize,
    pub trashed: usize,
    pub failed: usize,
    pub bytes_freed: u64,
}

pub fn execute(
    cfg: &ScanConfig,
    items: &[MediaItem],
    groups: &[Group],
) -> anyhow::Result<ActionReport> {
    let mut report = ActionReport::default();

    for group in groups {
        for id in &group.remove {
            let item = &items[*id];
            let result = match &cfg.action {
                Action::None => continue,
                Action::Trash => {
                    let ok = trash::delete(&item.path).is_ok();
                    if ok {
                        report.trashed += 1;
                        report.bytes_freed += item.size;
                    }
                    ActionResult {
                        path: item.path.clone(),
                        action: "trash".into(),
                        ok,
                        destination: None,
                        error: (!ok).then(|| "could not move to trash".to_string()),
                    }
                }
                Action::Delete => {
                    let ok = fs::remove_file(&item.path).is_ok();
                    if ok {
                        report.deleted += 1;
                        report.bytes_freed += item.size;
                    }
                    ActionResult {
                        path: item.path.clone(),
                        action: "delete".into(),
                        ok,
                        destination: None,
                        error: (!ok).then(|| "could not delete file".to_string()),
                    }
                }
                Action::MoveTo(dir) => {
                    if let Err(e) = fs::create_dir_all(dir) {
                        report.failed += 1;
                        report.results.push(ActionResult {
                            path: item.path.clone(),
                            action: "move".into(),
                            ok: false,
                            destination: None,
                            error: Some(e.to_string()),
                        });
                        continue;
                    }
                    let dest = unique_destination(dir, &item.path);
                    match fs::rename(&item.path, &dest) {
                        Ok(()) => {
                            report.moved += 1;
                            ActionResult {
                                path: item.path.clone(),
                                action: "move".into(),
                                ok: true,
                                destination: Some(dest),
                                error: None,
                            }
                        }
                        Err(e) => ActionResult {
                            path: item.path.clone(),
                            action: "move".into(),
                            ok: false,
                            destination: Some(dest),
                            error: Some(e.to_string()),
                        },
                    }
                }
            };
            if !result.ok {
                report.failed += 1;
            }
            report.results.push(result);
        }
    }

    Ok(report)
}

/// Avoid clobbering an existing file in the destination folder.
pub fn unique_destination(dir: &Path, source: &Path) -> PathBuf {
    let name = source
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| "unnamed".into());
    let mut candidate = dir.join(&name);
    if !candidate.exists() {
        return candidate;
    }
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let ext = source.extension().map(|s| s.to_string_lossy().into_owned());
    for n in 1..10_000 {
        let file_name = match &ext {
            Some(ext) => format!("{stem} ({n}).{ext}"),
            None => format!("{stem} ({n})"),
        };
        candidate = dir.join(file_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_destination_appends_counter() {
        let dir = std::env::temp_dir().join(format!("umdf-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let existing = dir.join("photo.jpg");
        fs::write(&existing, b"x").unwrap();
        let dest = unique_destination(&dir, Path::new("/tmp/other/photo.jpg"));
        assert_eq!(dest.file_name().unwrap(), "photo (1).jpg");
        let _ = fs::remove_dir_all(&dir);
    }
}
