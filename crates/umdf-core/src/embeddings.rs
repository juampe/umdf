//! Optional semantic image embeddings with ONNX Runtime.
//!
//! Enabled with the `embeddings` cargo feature. The runtime library is loaded
//! dynamically (`load-dynamic`), so the default build has no ONNX Runtime
//! dependency. A release package can bundle a CPU ONNX Runtime in `lib/` next
//! to the executable; otherwise point `ORT_DYLIB_PATH` at an ONNX Runtime build
//! with CUDA (or CPU) support. Provide any image-embedding `.onnx` model
//! (ResNet, CLIP, SigLIP…).
//!
//! Pipeline: decode + EXIF orientation + resize to `input_size` + per-channel
//! normalisation (Rayon) → batched `[N,3,S,S]` tensor on the GPU → L2-normalised
//! embeddings → cosine similarity.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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

/// Look for a bundled CPU runtime next to the executable (release packages put
/// it in `lib/`). Falls back to `ORT_DYLIB_PATH` / the system loader when no
/// bundled copy is present.
fn bundled_runtime() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let mut candidates = vec![
        dir.to_path_buf(),
        dir.join("lib"),
        dir.join("../lib/umdf/lib"),
    ];
    if let Some(contents) = dir.parent() {
        candidates.push(contents.join("Frameworks"));
        candidates.push(contents.join("Resources"));
    }
    candidates.iter().find_map(|candidate| runtime_in(candidate))
}

fn runtime_in(dir: &Path) -> Option<PathBuf> {
    let mut fallback = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().map(|name| name.to_string_lossy().to_ascii_lowercase()) else {
            continue;
        };
        let preferred = name == "onnxruntime.dll"
            || name == "libonnxruntime.so"
            || name == "libonnxruntime.dylib";
        let matches = preferred
            || (name.starts_with("libonnxruntime")
                && (name.ends_with(".dylib")
                    || name.starts_with("libonnxruntime.so.")));
        if preferred {
            return Some(path);
        }
        if matches && fallback.is_none() {
            fallback = Some(path);
        }
    }
    fallback
}

/// Directory that contains the ONNX Runtime library in use.
fn runtime_dir() -> Option<PathBuf> {
    if let Some(path) = bundled_runtime() {
        return path.parent().map(Path::to_path_buf);
    }
    std::env::var_os("ORT_DYLIB_PATH")
        .map(PathBuf::from)
        .and_then(|path| path.parent().map(Path::to_path_buf))
}

/// ONNX Runtime CPU builds accept the CUDA execution-provider request but
/// silently fall back to CPU. The CUDA provider ships as a separate library;
/// its presence is the reliable signal that GPU execution is actually available.
fn cuda_provider_available() -> bool {
    let Some(dir) = runtime_dir() else {
        return false;
    };
    #[cfg(windows)]
    let name = "onnxruntime_providers_cuda.dll";
    #[cfg(target_os = "linux")]
    let name = "libonnxruntime_providers_cuda.so";
    #[cfg(target_os = "macos")]
    let name = "libonnxruntime_providers_cuda.dylib";
    dir.join(name).is_file()
}

/// True when a bundled or explicitly configured ONNX Runtime is present.
/// Used by the UI to disable semantic search with a helpful message.
pub fn runtime_available() -> bool {
    if bundled_runtime().is_some() {
        return true;
    }
    std::env::var_os("ORT_DYLIB_PATH")
        .map(PathBuf::from)
        .map(|path| path.is_file())
        .unwrap_or(false)
}

fn environment() -> &'static ort::environment::Environment {
    ENVIRONMENT.get_or_init(|| {
        let builder = match bundled_runtime() {
            Some(path) => match ort::init_from(&path) {
                Ok(builder) => builder,
                Err(error) => {
                    crate::log_error(format_args!(
                        "UMDF: could not load bundled ONNX Runtime at {}: {error}",
                        path.display()
                    ));
                    ort::environment::init()
                }
            },
            None => ort::environment::init(),
        };
        builder
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
    used_cuda: bool,
}

impl EmbeddingEngine {
    pub fn new(cfg: &EmbeddingConfig) -> Result<Self> {
        let env = environment();

        let mut forced_cpu = false;
        let mut used_cuda = false;
        let cuda_requested = cfg.use_cuda && cuda_provider_available();
        if cfg.use_cuda && !cuda_requested {
            crate::log_error(format_args!(
                "UMDF: CUDA requested but onnxruntime_providers_cuda is not next to the runtime; using CPU"
            ));
            forced_cpu = true;
        }
        let session = if cuda_requested {
            match Self::build_session(&env, cfg, true) {
                Ok(session) => {
                    used_cuda = true;
                    session
                }
                Err(error) => {
                    crate::log_error(format_args!(
                        "UMDF: CUDA execution provider unavailable ({error}); falling back to CPU"
                    ));
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
            used_cuda,
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

    /// True when the CUDA execution provider was actually selected.
    pub fn uses_cuda(&self) -> bool {
        self.used_cuda
    }

    /// Embed every image in batches. Files that cannot be decoded are skipped
    /// (their slot is `None`), so a single corrupt image never aborts a scan.
    /// A batch-level model/inference error is logged and skipped as well.
    pub fn embed_paths(&mut self, paths: &[PathBuf]) -> Result<Vec<Option<Vec<f32>>>> {
        let mut out: Vec<Option<Vec<f32>>> = vec![None; paths.len()];
        let never = AtomicBool::new(false);
        self.embed_paths_incremental(paths, &never, None, |start, batch| {
            for (offset, value) in batch.into_iter().enumerate() {
                out[start + offset] = value;
            }
        })?;
        Ok(out)
    }

    /// Like [`Self::embed_paths`], but hands each finished batch to
    /// `on_batch(start, values)` immediately. `start` is the offset in `paths`;
    /// each value is `None` when that file could not be decoded or inferred.
    ///
    /// This lets callers persist vectors as they are computed and report real
    /// progress. A long scan interrupted halfway keeps the batches already
    /// stored instead of losing all of them.
    pub fn embed_paths_incremental<F>(
        &mut self,
        paths: &[PathBuf],
        cancel: &AtomicBool,
        pause: Option<&AtomicBool>,
        mut on_batch: F,
    ) -> Result<()>
    where
        F: FnMut(usize, Vec<Option<Vec<f32>>>),
    {
        let size = self.input_size;
        let mean = self.mean;
        let std = self.std;
        let batch_size = self.batch_size.max(1);

        for (chunk_index, chunk) in paths.chunks(batch_size).enumerate() {
            if let Some(pause) = pause {
                while pause.load(Ordering::Relaxed) && !cancel.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let start = chunk_index * batch_size;
            let mut chunk_out: Vec<Option<Vec<f32>>> = vec![None; chunk.len()];
            let prepared: Vec<Result<Vec<f32>>> = chunk
                .par_iter()
                .map(|path| {
                    // A panic inside an image decoder must skip only this file.
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        preprocess(path, size, mean, std)
                    }))
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("decoder panic")))
                })
                .collect();
            let mut batch = Vec::with_capacity(prepared.len());
            let mut offsets = Vec::with_capacity(prepared.len());
            for (offset, result) in prepared.into_iter().enumerate() {
                match result {
                    Ok(tensor) => {
                        batch.push(tensor);
                        offsets.push(offset);
                    }
                    Err(error) => crate::log_error(format_args!(
                        "UMDF: skipping embedding for {}: {error}",
                        chunk[offset].display()
                    )),
                }
            }
            if batch.is_empty() {
                on_batch(start, chunk_out);
                continue;
            }
            let n = batch.len();
            let s = size as usize;
            let mut data = Vec::with_capacity(n * 3 * s * s);
            for tensor in batch {
                data.extend_from_slice(&tensor);
            }
            let shape = [n as i64, 3, s as i64, s as i64];
            let tensor = match ort::value::Tensor::from_array((shape, data)) {
                Ok(tensor) => tensor,
                Err(error) => {
                    crate::log_error(format_args!("UMDF: embedding tensor error: {error}"));
                    on_batch(start, chunk_out);
                    continue;
                }
            };
            let name = self.input_name.clone();
            let outputs = match self.session.run(ort::inputs![name.as_str() => tensor]) {
                Ok(outputs) => outputs,
                Err(error) => {
                    crate::log_error(format_args!("UMDF: embedding batch failed: {error}"));
                    on_batch(start, chunk_out);
                    continue;
                }
            };
            let (shape, values) = match outputs[0].try_extract_tensor::<f32>() {
                Ok(values) => values,
                Err(error) => {
                    crate::log_error(format_args!("UMDF: embedding output error: {error}"));
                    on_batch(start, chunk_out);
                    continue;
                }
            };
            let dims: Vec<usize> = shape.iter().map(|&d| d.max(0) as usize).collect();
            let mut vectors = match split_output(&dims, values, n) {
                Ok(vectors) => vectors,
                Err(error) => {
                    crate::log_error(format_args!("UMDF: embedding shape error: {error}"));
                    on_batch(start, chunk_out);
                    continue;
                }
            };
            for vector in &mut vectors {
                l2_normalise(vector);
            }
            for (offset, vector) in offsets.into_iter().zip(vectors) {
                chunk_out[offset] = Some(vector);
            }
            on_batch(start, chunk_out);
        }
        Ok(())
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
