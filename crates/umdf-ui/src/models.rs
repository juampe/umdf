//! Model catalog, Hugging Face downloads and SQLite persistence.

use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

pub struct CatalogModel {
    pub id: &'static str,
    pub name: &'static str,
    pub url: &'static str,
    pub capabilities: &'static str,
    pub preset: &'static str,
    pub input_size: u32,
    pub threshold: f32,
    pub size_mb: u32,
}

/// Curated image-embedding models. All are single-input vision encoders.
pub static CATALOG: &[CatalogModel] = &[
    CatalogModel {
        id: "resnet18",
        name: "ResNet-18",
        url: "https://huggingface.co/Xenova/resnet-18/resolve/main/onnx/model.onnx",
        capabilities: "General image similarity: resized, recompressed and lightly edited photos",
        preset: "imagenet",
        input_size: 224,
        threshold: 0.90,
        size_mb: 45,
    },
    CatalogModel {
        id: "resnet50",
        name: "ResNet-50",
        url: "https://huggingface.co/Xenova/resnet-50/resolve/main/onnx/model.onnx",
        capabilities: "General image similarity with a larger, higher-quality model",
        preset: "imagenet",
        input_size: 224,
        threshold: 0.90,
        size_mb: 98,
    },
    CatalogModel {
        id: "clip",
        name: "CLIP ViT-B/32 (vision)",
        url: "https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/onnx/vision_model.onnx",
        capabilities: "Semantic similarity: different shots of the same scene or person (threshold ~0.83)",
        preset: "clip",
        input_size: 224,
        threshold: 0.85,
        size_mb: 335,
    },
    CatalogModel {
        id: "siglip",
        name: "SigLIP base 16-224 (vision)",
        url: "https://huggingface.co/Xenova/siglip-base-patch16-224/resolve/main/onnx/vision_model.onnx",
        capabilities: "Semantic similarity, SigLIP alternative to CLIP (threshold ~0.83)",
        preset: "clip",
        input_size: 224,
        threshold: 0.85,
        size_mb: 355,
    },
];

#[derive(Debug, Clone)]
pub struct InstalledModel {
    pub name: String,
    pub path: PathBuf,
    pub capabilities: String,
    pub preset: String,
    pub input_size: u32,
    pub threshold: f32,
}

pub struct Db {
    conn: Connection,
    pub path: PathBuf,
}

impl Db {
    pub fn open() -> Result<Self> {
        let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        let dir = base.join("umdf");
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join("config.db");
        let conn = Connection::open(&path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS models (
                 path TEXT PRIMARY KEY,
                 name TEXT NOT NULL,
                 capabilities TEXT NOT NULL,
                 preset TEXT NOT NULL,
                 input_size INTEGER NOT NULL,
                 threshold REAL NOT NULL,
                 source_url TEXT,
                 created_at TEXT NOT NULL DEFAULT (datetime('now'))
             );",
        )?;
        // Older builds stored a Hugging Face token; the catalogue no longer
        // needs one, so remove any leftover value.
        let _ = conn.execute("DELETE FROM settings WHERE key = 'hf_token'", []);
        Ok(Self { conn, path })
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .ok()
    }

    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn models(&self) -> Result<Vec<InstalledModel>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, path, capabilities, preset, input_size, threshold
             FROM models ORDER BY name",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(InstalledModel {
                name: row.get(0)?,
                path: PathBuf::from(row.get::<_, String>(1)?),
                capabilities: row.get(2)?,
                preset: row.get(3)?,
                input_size: row.get(4)?,
                threshold: row.get(5)?,
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn upsert_model(&self, model: &InstalledModel, url: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO models(path, name, capabilities, preset, input_size, threshold, source_url)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(path) DO UPDATE SET
                 name = excluded.name,
                 capabilities = excluded.capabilities,
                 preset = excluded.preset,
                 input_size = excluded.input_size,
                 threshold = excluded.threshold,
                 source_url = excluded.source_url",
            params![
                model.path.to_string_lossy(),
                model.name,
                model.capabilities,
                model.preset,
                model.input_size,
                model.threshold,
                url
            ],
        )?;
        Ok(())
    }

    pub fn remove_model(&self, path: &Path) -> Result<()> {
        self.conn.execute(
            "DELETE FROM models WHERE path = ?1",
            params![path.to_string_lossy()],
        )?;
        Ok(())
    }
}

/// Download `url` to `dest`, creating parent directories. Follows Hugging Face
/// redirects. The catalogue only contains public models, so no token is used.
pub fn download(url: &str, dest: &Path) -> Result<u64> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let response = ureq::get(url)
        .call()
        .map_err(|e| anyhow::anyhow!("download failed: {e}"))?;
    let mut reader = response.into_reader();

    let tmp = dest.with_extension("onnx.part");
    let mut file = File::create(&tmp)?;
    let bytes = std::io::copy(&mut reader, &mut file)?;
    file.sync_all()?;
    std::fs::rename(&tmp, dest)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Network test: downloads a tiny Hugging Face file (not a model).
    #[test]
    #[ignore]
    fn download_small_file() {
        let dir = std::env::temp_dir().join(format!("umdf-dl-{}", std::process::id()));
        let dest = dir.join("config.json");
        let bytes = download(
            "https://huggingface.co/Xenova/resnet-18/resolve/main/config.json",
            &dest,
        )
        .expect("download");
        assert!(bytes > 0);
        assert!(dest.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
