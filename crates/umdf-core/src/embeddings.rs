//! Optional semantic image embeddings with ONNX Runtime.
//!
//! Enabled with the `embeddings` cargo feature. The runtime library is loaded
//! dynamically (`load-dynamic`), so the default build has no ONNX Runtime
//! dependency. Point `ORT_DYLIB_PATH` at an ONNX Runtime build with CUDA (or
//! CPU) support and provide any image-embedding `.onnx` model (ResNet, CLIP,
//! SigLIP…).
//!
//! Pipeline: decode + EXIF orientation + resize to `input_size` + per-channel
//! normalisation (Rayon) → batched `[N,3,S,S]` tensor on the GPU → L2-normalised
//! embeddings → cosine similarity.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use image::imageops::FilterType;
use ort::ep;
use ort::session::Session;
use ort::value::ValueType;
use rayon::prelude::*;

use crate::config::EmbeddingConfig;
use crate::fingerprint;

/// ONNX Runtime allows only one environment per process.
static ENVIRONMENT: OnceLock<ort::environment::Environment> = OnceLock::new();

fn environment() -> &'static ort::environment::Environment {
    ENVIRONMENT.get_or_init(|| {
        ort::environment::init()
            .with_name("umdf-embeddings")
            .build()
            .expect("ONNX Runtime environment")
    })
}

/// Facts discovered from the model's input tensor.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub input_name: String,
    pub input_size: Option<u32>,
    pub channels: Option<u32>,
    pub output_dim: Option<usize>,
}

/// Load the model just enough to read its input/output shapes.
pub fn inspect_model(path: &Path) -> Result<ModelInfo> {
    let session = Session::builder(environment())
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .commit_from_file(path)
        .map_err(|e| anyhow::anyhow!("could not load {}: {e}", path.display()))?;

    let input = session
        .inputs()
        .first()
        .context("the ONNX model has no inputs")?;
    let (channels, input_size) = match input.dtype() {
        ValueType::Tensor { shape, .. } => {
            let dims: Vec<i64> = shape.iter().copied().collect();
            let channels = dims.get(1).copied().filter(|v| *v > 0).map(|v| v as u32);
            let height = dims.get(2).copied().filter(|v| *v > 0);
            let width = dims.get(3).copied().filter(|v| *v > 0);
            let size = match (height, width) {
                (Some(h), Some(w)) if h == w => Some(h as u32),
                _ => None,
            };
            (channels, size)
        }
        _ => (None, None),
    };
    let output_dim = session.outputs().first().and_then(|output| match output.dtype() {
        ValueType::Tensor { shape, .. } => shape
            .last()
            .copied()
            .filter(|v| *v > 0)
            .map(|v| v as usize),
        _ => None,
    });

    Ok(ModelInfo {
        input_name: input.name().to_string(),
        input_size,
        channels,
        output_dim,
    })
}

pub struct EmbeddingEngine {
    session: Session,
    input_name: String,
    input_size: u32,
    batch_size: usize,
    mean: [f32; 3],
    std: [f32; 3],
    forced_cpu: bool,
}

impl EmbeddingEngine {
    pub fn new(cfg: &EmbeddingConfig) -> Result<Self> {
        let env = environment();

        let mut forced_cpu = false;
        let session = if cfg.use_cuda {
            match Self::build_session(&env, cfg, true) {
                Ok(session) => session,
                Err(error) => {
                    eprintln!(
                        "UMDF: CUDA execution provider unavailable ({error}); falling back to CPU"
                    );
                    forced_cpu = true;
                    Self::build_session(&env, cfg, false)?
                }
            }
        } else {
            Self::build_session(&env, cfg, false)?
        };

        let input_name = session
            .inputs()
            .first()
            .map(|input| input.name().to_string())
            .context("the ONNX model has no inputs")?;

        Ok(Self {
            session,
            input_name,
            input_size: cfg.input_size.max(32),
            batch_size: cfg.batch_size.max(1),
            mean: cfg.mean,
            std: cfg.std,
            forced_cpu,
        })
    }

    fn build_session(
        env: &ort::environment::Environment,
        cfg: &EmbeddingConfig,
        cuda: bool,
    ) -> Result<Session> {
        let builder = Session::builder(env).map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut builder = if cuda {
            builder
                .with_execution_providers([ep::CUDA::default().build()])
                .map_err(|e| anyhow::anyhow!("{e}"))?
        } else {
            builder
        };
        builder
            .commit_from_file(&cfg.model_path)
            .map_err(|e| anyhow::anyhow!("could not load {}: {e}", cfg.model_path.display()))
    }

    pub fn forced_cpu(&self) -> bool {
        self.forced_cpu
    }

    /// Embed every image, in batches, returning L2-normalised vectors.
    pub fn embed_paths(&mut self, paths: &[PathBuf]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(paths.len());
        let size = self.input_size;
        let mean = self.mean;
        let std = self.std;

        for chunk in paths.chunks(self.batch_size) {
            let batch: Vec<Vec<f32>> = chunk
                .par_iter()
                .map(|path| preprocess(path, size, mean, std))
                .collect::<Result<Vec<_>>>()?;
            let n = batch.len();
            if n == 0 {
                continue;
            }
            let s = size as usize;
            let mut data = Vec::with_capacity(n * 3 * s * s);
            for tensor in batch {
                data.extend_from_slice(&tensor);
            }
            let shape = [n as i64, 3, s as i64, s as i64];
            let tensor = ort::value::Tensor::from_array((shape, data))
                .map_err(|e| anyhow::anyhow!("tensor: {e}"))?;
            let name = self.input_name.clone();
            let outputs = self
                .session
                .run(ort::inputs![name.as_str() => tensor])
                .map_err(|e| anyhow::anyhow!("inference: {e}"))?;
            let (shape, values) = outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow::anyhow!("output: {e}"))?;
            let dims: Vec<usize> = shape.iter().map(|&d| d.max(0) as usize).collect();
            let mut vectors = split_output(&dims, values, n)?;
            for vector in &mut vectors {
                l2_normalise(vector);
            }
            out.extend(vectors);
        }
        Ok(out)
    }
}

fn preprocess(path: &Path, size: u32, mean: [f32; 3], std: [f32; 3]) -> Result<Vec<f32>> {
    let image = fingerprint::load_oriented(path)
        .with_context(|| format!("decoding {}", path.display()))?;
    let rgb = image.to_rgb8();
    let resized = image::imageops::resize(&rgb, size, size, FilterType::Triangle);
    let n = size as usize;
    let mut out = vec![0f32; 3 * n * n];
    for (x, y, pixel) in resized.enumerate_pixels() {
        for channel in 0..3 {
            let value = pixel[channel] as f32 / 255.0;
            out[channel * n * n + y as usize * n + x as usize] =
                (value - mean[channel]) / std[channel];
        }
    }
    Ok(out)
}

fn split_output(dims: &[usize], values: &[f32], batch: usize) -> Result<Vec<Vec<f32>>> {
    if batch == 0 {
        return Ok(Vec::new());
    }
    match dims {
        // [N, D]
        [n, d] if *n == batch && *d > 0 => Ok((0..batch)
            .map(|i| values[i * d..(i + 1) * d].to_vec())
            .collect()),
        // [N, tokens, D] -> use the first (CLS) token
        [n, tokens, d] if *n == batch && *tokens > 0 && *d > 0 => Ok((0..batch)
            .map(|i| {
                let start = i * tokens * d;
                values[start..start + d].to_vec()
            })
            .collect()),
        // [N, C, H, W] -> global average pool
        [n, c, h, w] if *n == batch && *c > 0 && *h > 0 && *w > 0 => Ok((0..batch)
            .map(|i| {
                let base = i * c * h * w;
                (0..*c)
                    .map(|ch| {
                        let slice = &values[base + ch * h * w..base + (ch + 1) * h * w];
                        slice.iter().sum::<f32>() / (h * w) as f32
                    })
                    .collect()
            })
            .collect()),
        _ => {
            let per = values.len() / batch;
            if per == 0 {
                anyhow::bail!("unexpected model output shape {:?}", dims);
            }
            Ok((0..batch)
                .map(|i| values[i * per..(i + 1) * per].to_vec())
                .collect())
        }
    }
}

fn l2_normalise(vector: &mut [f32]) {
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in vector {
            *value /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_two_dim_output() {
        let values = vec![1.0, 2.0, 3.0, 4.0];
        let out = split_output(&[2, 2], &values, 2).unwrap();
        assert_eq!(out, vec![vec![1.0, 2.0], vec![3.0, 4.0]]);
    }

    #[test]
    fn split_three_dim_takes_cls() {
        let values = vec![10.0, 11.0, 20.0, 21.0, 30.0, 31.0];
        let out = split_output(&[1, 3, 2], &values, 1).unwrap();
        assert_eq!(out, vec![vec![10.0, 11.0]]);
    }

    #[test]
    fn normalise_unit_length() {
        let mut v = vec![3.0, 4.0];
        l2_normalise(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
    }
}
