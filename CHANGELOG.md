# Changelog

All notable changes to UMDF are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [0.2.0] - 2026-11-08

### Added

- App icon and desktop integration (Linux `.desktop`, Windows executable icon,
  macOS `.app` bundle).
- Linux installers: `.deb`, `.rpm` and `.AppImage`.
- Windows `.msi` installer with Start Menu and Desktop shortcuts.
- AI variants of the Linux/Windows/macOS ARM packages with a bundled CPU
  **ONNX Runtime 1.31.1**, so semantic search works without extra setup.
- Automatic discovery of a bundled `libonnxruntime` next to the executable.
- Generated `THIRD_PARTY_LICENSES.md`, ONNX Runtime license/notices and
  `SOURCE.txt` in every package.
- `bundled_runtime` fallback search for AppImage and `.app` layouts.
- Help text now documents the real limitations (heuristic originality, aspect
  gate, crops/rotations, one-frame video, lossy audio, CPU/GPU runtime, model
  licenses and destructive actions).

### Changed

- New installations default to the CPU execution provider; CUDA remains
  selectable when a GPU ONNX Runtime is supplied.
- Version bumped to 0.2.0.

## [0.1.0] - 2026-11-08

Initial public release.

### Added

- Rust engine (`umdf-core`): folder discovery, exact SHA-256 comparison,
  perceptual fingerprints, SSIM/sign/Pearson metrics, EXIF-oriented decoding,
  grouping and an explainable "most likely original" keep policy.
- Headless CLI (`umdf`) with keep policies, dry-run/apply, trash/move/delete,
  CSV/XML export, thread control and video/audio content comparison through
  ffmpeg/ffprobe.
- Slint desktop UI (`umdf-ui`) with light/dark themes, English/Spanish,
  folder browser, thumbnails, side-by-side preview, keyboard navigation and
  chained deletion.
- Optional semantic embeddings via ONNX Runtime/CUDA (`embeddings` feature).
- Models manager with a Hugging Face catalogue, download folder, CUDA/CPU
  choice, automatic recommended thresholds and model activation.
- Configuration in `config.toml` and a disposable SQLite cache (`cache.db`)
  for fingerprints and embeddings.
- Prebuilt archives for Linux, Windows and macOS in GitHub Releases.

[0.1.0]: https://github.com/juampe/umdf/releases/tag/v0.1.0
