//! Model catalog and Hugging Face downloads.
//!
//! Persistence lives in [`crate::settings`] (configuration) and
//! [`crate::cache`] (derived values); this module only describes the curated
//! catalogue and performs the HTTP download.

use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub struct CatalogModel {
    pub id: &'static str,
    pub name: &'static str,
    pub name_es: &'static str,
    pub url: &'static str,
    pub capabilities: &'static str,
    pub capabilities_es: &'static str,
    pub preset: &'static str,
    pub input_size: u32,
    /// Recommended cosine-similarity threshold when this model is activated.
    pub threshold: f32,
    pub size_mb: u32,
}

impl CatalogModel {
    pub fn name(&self, es: bool) -> &'static str {
        if es {
            self.name_es
        } else {
            self.name
        }
    }

    pub fn capabilities(&self, es: bool) -> &'static str {
        if es {
            self.capabilities_es
        } else {
            self.capabilities
        }
    }
}

/// Curated image-embedding models. All are single-input vision encoders.
pub static CATALOG: &[CatalogModel] = &[
    CatalogModel {
        id: "resnet18",
        name: "ResNet-18",
        name_es: "ResNet-18",
        url: "https://huggingface.co/Xenova/resnet-18/resolve/main/onnx/model.onnx",
        capabilities: "General image similarity: resized, recompressed and lightly edited photos",
        capabilities_es: "Similitud general: fotos redimensionadas, recomprimidas y con retoques ligeros",
        preset: "imagenet",
        input_size: 224,
        threshold: 0.90,
        size_mb: 45,
    },
    CatalogModel {
        id: "resnet50",
        name: "ResNet-50",
        name_es: "ResNet-50",
        url: "https://huggingface.co/Xenova/resnet-50/resolve/main/onnx/model.onnx",
        capabilities: "General image similarity with a larger, higher-quality model",
        capabilities_es: "Similitud general con un modelo mayor y de mayor calidad",
        preset: "imagenet",
        input_size: 224,
        threshold: 0.90,
        size_mb: 98,
    },
    CatalogModel {
        id: "clip",
        name: "CLIP ViT-B/32 (vision)",
        name_es: "CLIP ViT-B/32 (visión)",
        url: "https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/onnx/vision_model.onnx",
        capabilities: "Semantic similarity: different shots of the same scene or person (threshold ~0.83)",
        capabilities_es: "Similitud semántica: tomas distintas de la misma escena o persona (umbral ~0.83)",
        preset: "clip",
        input_size: 224,
        threshold: 0.83,
        size_mb: 335,
    },
    CatalogModel {
        id: "siglip",
        name: "SigLIP base 16-224 (vision)",
        name_es: "SigLIP base 16-224 (visión)",
        url: "https://huggingface.co/Xenova/siglip-base-patch16-224/resolve/main/onnx/vision_model.onnx",
        capabilities: "Semantic similarity, SigLIP alternative to CLIP (threshold ~0.83)",
        capabilities_es: "Similitud semántica, alternativa a CLIP (umbral ~0.83)",
        preset: "clip",
        input_size: 224,
        threshold: 0.83,
        size_mb: 355,
    },
];

/// A model registered in `config.toml` after a successful download (or after
/// the user points the manager at an existing file).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstalledModel {
    pub name: String,
    pub path: PathBuf,
    pub capabilities: String,
    pub preset: String,
    pub input_size: u32,
    pub threshold: f32,
    pub source_url: String,
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
    std::fs::rename(&tmp, dest)
        .with_context(|| format!("moving {} to {}", tmp.display(), dest.display()))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_are_unique() {
        for (i, a) in CATALOG.iter().enumerate() {
            for b in &CATALOG[i + 1..] {
                assert_ne!(a.id, b.id);
                assert_ne!(a.url, b.url);
            }
        }
    }

    #[test]
    fn semantic_models_use_a_lower_threshold() {
        for model in CATALOG {
            let expected = if model.preset == "clip" { 0.83 } else { 0.90 };
            assert!(
                (model.threshold - expected).abs() < f32::EPSILON,
                "{} has threshold {}",
                model.name,
                model.threshold
            );
        }
    }

    #[test]
    fn descriptions_are_localised() {
        for model in CATALOG {
            assert!(!model.capabilities(false).is_empty());
            assert!(!model.capabilities(true).is_empty());
            assert_ne!(model.capabilities(false), model.capabilities(true));
        }
    }

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
