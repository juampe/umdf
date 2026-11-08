//! Path formatting helpers for everything that is shown in the UI.

use std::path::{Path, PathBuf};

/// Remove the Windows extended-length prefix (`\\?\` / `\\?\UNC\`) so paths
/// can be shown to the user. Extended paths remain valid for filesystem
/// operations; this is display-only.
pub fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{}", rest)
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        text.into_owned()
    }
}

/// A cleaned path for comparisons such as "is this a filesystem root?".
pub fn clean_path(path: &Path) -> PathBuf {
    PathBuf::from(display_path(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_extended_drive_prefix() {
        assert_eq!(
            display_path(Path::new(r"\\?\B:\ownCloud\Album")),
            r"B:\ownCloud\Album"
        );
    }

    #[test]
    fn strips_extended_unc_prefix() {
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\server\share\photos")),
            r"\\server\share\photos"
        );
    }

    #[test]
    fn leaves_normal_paths_alone() {
        assert_eq!(display_path(Path::new("/home/me/Pictures")), "/home/me/Pictures");
        assert_eq!(display_path(Path::new(r"C:\Users\me")), r"C:\Users\me");
    }
}
