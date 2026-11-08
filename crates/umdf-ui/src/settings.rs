//! User configuration stored in `config.toml`.
//!
//! `config.toml` holds everything the user can change (models folder, device,
//! threshold, active model and the registry of installed models). Derived data
//! such as fingerprints and embeddings lives in [`crate::cache`] instead, so
//! the configuration stays small, diff-friendly and easy to edit by hand.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::models::InstalledModel;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Folder where downloaded `.onnx` models are stored.
    pub models_dir: PathBuf,
    /// `"cuda"` or `"cpu"`.
    pub device: String,
    /// UI language code (`en`, `es`, `fr`, `de`, `pt`, `it`, `zh`, `ja`, `ko`).
    pub language: String,
    /// Cosine-similarity threshold for semantic comparisons (0..1).
    pub similarity: f32,
    /// Absolute path of the active model, empty when none is selected.
    pub active_model: String,
    /// Models registered after a download (or added by hand).
    pub models: Vec<InstalledModel>,
}

impl Default for Settings {
    fn default() -> Self {
        let models_dir = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("models");
        Self {
            models_dir,
            // Release packages bundle a CPU ONNX Runtime; CUDA requires a
            // GPU-enabled runtime and an explicit opt-in.
            device: "cpu".to_string(),
            language: "en".to_string(),
            similarity: 0.90,
            active_model: String::new(),
            models: Vec::new(),
        }
    }
}

impl Settings {
    /// `<config dir>/umdf/config.toml`.
    pub fn path() -> PathBuf {
        let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        base.join("umdf").join("config.toml")
    }

    /// Load the configuration, migrating a legacy `config.db` the first time.
    /// A malformed file is reported and replaced by defaults instead of
    /// preventing the application from starting.
    pub fn load() -> Self {
        let path = Self::path();
        if path.is_file() {
            match std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| toml::from_str::<Settings>(&text).ok())
            {
                Some(settings) => return settings,
                None => eprintln!(
                    "UMDF: could not parse {}; using defaults",
                    path.display()
                ),
            }
        }

        let migrated = Self::migrate_legacy();
        let settings = migrated.unwrap_or_default();
        if let Err(error) = settings.save() {
            eprintln!("UMDF: could not write config.toml: {error}");
        }
        settings
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serialising config.toml")?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        if std::fs::rename(&tmp, &path).is_err() {
            // Windows does not replace an existing file with rename().
            std::fs::copy(&tmp, &path)
                .with_context(|| format!("writing {}", path.display()))?;
            let _ = std::fs::remove_file(&tmp);
        }
        Ok(())
    }

    pub fn device_index(&self) -> i32 {
        if self.device.eq_ignore_ascii_case("cpu") {
            1
        } else {
            0
        }
    }

    pub fn set_device_index(&mut self, index: i32) {
        self.device = if index == 0 { "cuda" } else { "cpu" }.to_string();
    }

    pub fn upsert_model(&mut self, model: InstalledModel) {
        match self
            .models
            .iter_mut()
            .find(|entry| entry.path == model.path)
        {
            Some(entry) => *entry = model,
            None => self.models.push(model),
        }
    }

    pub fn remove_model(&mut self, path: &Path) {
        self.models.retain(|model| model.path != path);
        if self.active_model == path.to_string_lossy() {
            self.active_model.clear();
        }
    }

    /// One-time import of the settings/models that older builds kept in
    /// `config.db`. The old file is renamed to `config.db.bak` once the new
    /// `config.toml` has been written.
    fn migrate_legacy() -> Option<Settings> {
        let legacy = Self::path().with_file_name("config.db");
        if !legacy.is_file() {
            return None;
        }
        let conn = rusqlite::Connection::open_with_flags(
            &legacy,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .ok()?;

        let get = |key: &str| -> Option<String> {
            conn.query_row(
                "SELECT value FROM settings WHERE key = ?1",
                rusqlite::params![key],
                |row| row.get(0),
            )
            .ok()
        };

        let mut settings = Settings::default();
        let mut found = false;
        if let Some(value) = get("models_dir") {
            settings.models_dir = PathBuf::from(value);
            found = true;
        }
        if let Some(value) = get("semantic_device") {
            settings.device = value;
            found = true;
        }
        if let Some(value) = get("semantic_threshold") {
            if let Ok(value) = value.parse() {
                settings.similarity = value;
                found = true;
            }
        }
        if let Some(value) = get("active_model") {
            settings.active_model = value;
            found = true;
        }

        if let Ok(mut stmt) = conn.prepare(
            "SELECT name, path, capabilities, preset, input_size, threshold,
                    COALESCE(source_url, '')
             FROM models",
        ) {
            if let Ok(rows) = stmt.query_map([], |row| {
                Ok(InstalledModel {
                    name: row.get(0)?,
                    path: PathBuf::from(row.get::<_, String>(1)?),
                    capabilities: row.get(2)?,
                    preset: row.get(3)?,
                    input_size: row.get(4)?,
                    threshold: row.get(5)?,
                    source_url: row.get(6)?,
                })
            }) {
                for model in rows.flatten() {
                    settings.upsert_model(model);
                    found = true;
                }
            }
        }

        if !found {
            return None;
        }
        if settings.save().is_ok() {
            let backup = legacy.with_extension("db.bak");
            let _ = std::fs::rename(&legacy, &backup);
            eprintln!(
                "UMDF: migrated settings to {} (legacy database kept as {})",
                Self::path().display(),
                backup.display()
            );
        }
        Some(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let settings = Settings::default();
        assert_eq!(settings.device, "cpu");
        assert!((settings.similarity - 0.90).abs() < f32::EPSILON);
        assert!(settings.models_dir.ends_with("models"));
        assert_eq!(settings.language, "en");
    }

    #[test]
    fn toml_round_trip() {
        let mut settings = Settings::default();
        settings.similarity = 0.83;
        settings.device = "cpu".to_string();
        settings.language = "fr".to_string();
        settings.models.push(InstalledModel {
            name: "CLIP".into(),
            path: PathBuf::from("/tmp/clip.onnx"),
            capabilities: "semantic".into(),
            preset: "clip".into(),
            input_size: 224,
            threshold: 0.83,
            source_url: "https://example.test/clip.onnx".into(),
        });
        let text = toml::to_string_pretty(&settings).unwrap();
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(back.device, "cpu");
        assert_eq!(back.language, "fr");
        assert!((back.similarity - 0.83).abs() < f32::EPSILON);
        assert_eq!(back.models.len(), 1);
        assert_eq!(back.models[0].path, PathBuf::from("/tmp/clip.onnx"));
        assert_eq!(back.models[0].source_url, "https://example.test/clip.onnx");
    }

    #[test]
    fn upsert_replaces_by_path() {
        let mut settings = Settings::default();
        let model = InstalledModel {
            name: "A".into(),
            path: PathBuf::from("/tmp/a.onnx"),
            capabilities: String::new(),
            preset: "imagenet".into(),
            input_size: 224,
            threshold: 0.90,
            source_url: String::new(),
        };
        settings.upsert_model(model.clone());
        settings.upsert_model(InstalledModel {
            name: "B".into(),
            ..model
        });
        assert_eq!(settings.models.len(), 1);
        assert_eq!(settings.models[0].name, "B");
    }
}
