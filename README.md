# UMDF — Universal Media Duplicate Finder

<p align="center"><img src="assets/icon-256.png" width="128" alt="UMDF icon"></p>

[![CI](https://github.com/juampe/umdf/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/juampe/umdf/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/juampe/umdf)](https://github.com/juampe/umdf/releases)
[![License: GPL-3.0-only](https://img.shields.io/badge/license-GPL--3.0--only-blue.svg)](LICENSE)

UMDF finds duplicate and visually similar **photos, videos, audio and files**.
It combines exact byte comparison with perceptual fingerprints, groups the
matches, and proposes which copy to keep using an explainable
"most likely original" score.

* **Exact duplicates** — size + streaming SHA-256 (full content).
* **Perceptual duplicates** — resampled greyscale fingerprints with a
  configurable gray tolerance, plus three alternative metrics.
* **Video** — ffprobe metadata and one frame extracted with ffmpeg, compared
  as an image.
* **Audio** — decoded PCM hashed, so re-muxed or retagged files match.
* **Groups and decisions** — union-find groups and six keep policies
  (`original` by default).
* **Safe actions** — trash, move or delete with a dry run, confirmation for
  bulk actions and a hard "leave one" guarantee.
* **Exports** — CSV and XML.
* **Desktop UI in Slint** — light/dark theme, nine languages (English,
  Spanish, French, German, Portuguese, Italian, Simplified Chinese, Japanese
  and Korean), folder picker, thumbnails, side-by-side preview and
  keyboard-driven chained deletion. Noto Sans CJK is embedded for CJK text.

## Layout

```
crates/umdf-core   engine: discovery, hashing, fingerprints, grouping,
                   policies, actions, CSV/XML export, ffmpeg integration
crates/umdf-cli    `umdf` headless CLI
crates/umdf-ui     `umdf-ui` Slint desktop app
```

## Install (prebuilt binaries)

Every [release](https://github.com/juampe/umdf/releases) ships:

- **Standard archives** — small, no ONNX Runtime; pixel metrics only.
- **AI archives / installers** — include a bundled **CPU ONNX Runtime 1.31.1**
  and the `embeddings` feature, so semantic search and the Models manager work
  out of the box.

### Linux

| Format | AI | Command |
|---|:---:|---|
| `.deb` | yes | `sudo apt install ./umdf_0.3.9_amd64.deb` |
| `.rpm` | yes | `sudo dnf install ./umdf-0.3.9-1.x86_64.rpm` |
| `.AppImage` | yes | `chmod +x umdf-0.3.9-x86_64.AppImage && ./umdf-0.3.9-x86_64.AppImage` |
| `.tar.gz` | optional | extract and run `umdf` / `umdf-ui` |

The packages install `umdf` and `umdf-ui`, a desktop entry, icons and the
licenses. They are built against **glibc 2.35** (Ubuntu 22.04); the RPM targets
Fedora 36+. The AppImage bundles its non-graphics libraries but uses the host
OpenGL drivers.

### Windows

| Format | AI | Notes |
|---|:---:|---|
| `.msi` | yes | Start Menu + Desktop shortcuts, includes `onnxruntime.dll` |
| `.zip` | optional | portable; the `-ai` archive includes `lib\onnxruntime.dll` |

### macOS

| Format | AI | Notes |
|---|:---:|---|
| `.tar.gz` (Intel) | no | `UMDF.app` + `umdf`, macOS 11+ |
| `.tar.gz` (Apple Silicon) | optional | `-ai` archive bundles ONNX Runtime; macOS 14+ |

The macOS builds are not notarised. If Gatekeeper blocks the app, right-click →
**Open**, or run `xattr -d com.apple.quarantine UMDF.app`.

### AI and CUDA

The bundled runtime is **CPU-only**; it works on every supported platform and
needs no installation. Semantic search falls back to CPU automatically. To use
an NVIDIA GPU, point `ORT_DYLIB_PATH` at a CUDA-enabled ONNX Runtime and select
**CUDA** in the Models manager.

## Build from source

```bash
cargo build --workspace
cargo test --workspace
```

## CLI

```bash
# Report only (no changes on disk)
umdf --folder ~/Pictures

# Keep the copy most likely to be the original and move the rest
umdf --folder ~/Pictures --selectby=original --moveto ~/Duplicates --apply

# Only byte-identical files, export both formats
umdf --folder ~/Pictures --only-identical --csv out.csv --xml out.xml

# Images + video + audio by content (needs ffmpeg/ffprobe on PATH)
umdf --folder ~/Media --all-media --threshold 92

# Limit the worker pool (0 = one per CPU)
umdf --folder ~/Pictures --threads 8
```

Defaults: `--metric dpf --grid 75 --tolerance 10 --threshold 90
--selectby original`.

## Which copy is kept? (`--selectby original`, default)

Duplicates are usually derivatives of an original that was resized,
re-compressed, renamed or copied to another folder. UMDF scores every member of
a group with a transparent heuristic and keeps the highest score:

| Signal | Weight | More original when… |
|--------|-------:|---------------------|
| resolution | 0.22 | more pixels (photos/video) |
| quality | 0.20 | more bytes per pixel (or per second of audio) |
| file age | 0.20 | older modification date (derivatives are written later) |
| metadata | 0.14 | richer EXIF, camera make/model present |
| file name | 0.14 | no `copy`, `(1)`, `edited`, `resized`, `whatsapp`… |
| location | 0.10 | `DCIM`/`Camera`/`Pictures` rather than `Downloads`/`Desktop`/temp |

The score and the reasons are shown in the UI (`Original · 63% likely —
better quality per pixel, older file date, …`) and in the CLI. The other
policies remain available: `dimensions`, `filesize`, `earlierdate`,
`earlierexifdate`, `oldest`, `name`, `path`.

## Metrics

| value | meaning |
|---|---|
| `dpf` (default) | resample + pixel gray tolerance |
| `ssim` | structural similarity; robust to local retouching |
| `sign` | corrected sign agreement of the normalised grid |
| `pearson` | correlation of the normalised grids |
| `legacy` | legacy sign-agreement metric kept for comparison |

`dpf` resamples both pictures to 75×75 and counts the fraction of cells whose
gray level differs by at most `--tolerance` (default 10). Raise it for edited
copies (red-eye correction, recompression); lower it for strict archives.

## Video and audio

Exact duplicates always work (SHA-256). Content comparison needs
`ffmpeg`/`ffprobe` on `PATH` (or `--ffmpeg <path>`):

* **video**: `ffprobe` metadata + a frame extracted at `--frame-position`
  (default 1 s) that is fingerprinted like an image;
* **audio** (and audio tracks inside video): 5 s of decoded PCM hashed, so
  re-muxed/retagged files match while re-encoded ones do not.

## Semantic embeddings and the models manager (optional, GPU)

UMDF can compare images by their **content embedding** instead of (or in
addition to) the pixel fingerprint. This helps with crops, brightness changes,
recompression and different photos of the same scene. It is an opt-in cargo
feature, so the default build has no ONNX Runtime dependency.

### Models manager

Open **Models…** from *Options → Semantic (AI)*. The manager shows a catalogue
with the comparison each model enables:

| Model | Size | Best for |
|---|---:|---|
| ResNet-18 | ~45 MB | general image similarity: resize, recompression, light edits |
| ResNet-50 | ~98 MB | same, with a larger/higher-quality model |
| CLIP ViT-B/32 (vision) | ~335 MB | semantic similarity: different shots of the same scene/person |
| SigLIP base 16-224 (vision) | ~355 MB | semantic alternative to CLIP |

**Which model for which problem**

| Problem | Recommended |
|---|---|
| Exact duplicates | SHA-256 (no model) |
| Resized / recompressed | `dpf` (default) or ResNet-18/50 |
| Crops / different framing | CLIP or SigLIP |
| Same scene/person, different shots | CLIP/SigLIP, threshold ~0.83 |
| Rotated with EXIF | automatic (orientation is applied) |
| Rotated without EXIF (baked into pixels) | semantic models; review manually |
| Red-eye or small retouching | `dpf` with tolerance 14–18 or `ssim` |
| Video / audio | frame at `Frame position` / decoded PCM |

* Downloads come from Hugging Face into the **models folder** (default
  `./models`). The catalogue only contains public models, so **no token is
  needed**.
* After a download the model is inspected (`session.inputs`) to discover the
  input size and suggest the preprocessing (ImageNet vs CLIP).
* **CUDA/CPU** and the similarity threshold are selectable. AI packages bundle
  a CPU ONNX Runtime and default to CPU; selecting CUDA with a CPU-only runtime
  falls back to CPU automatically. Point `ORT_DYLIB_PATH` at a CUDA-enabled
  runtime to use the GPU.
* Activating a model sets its **recommended threshold** automatically:
  **0.90** for ResNet-18/50 and **0.83** for CLIP/SigLIP. The slider can still
  be adjusted afterwards.
* Model files are not part of the repository (`/models/` and `*.onnx` are
  ignored).

### Configuration and cache

* `<config dir>/umdf/config.toml` stores user configuration: `models_dir`,
  `device`, `similarity` (semantic threshold), `active_model`, the
  `[[models]]` registry, and all scan/UI choices: `folders`, `move_dest`,
  `recursive`, `only_identical`, `compare_media`, `pixel_similarity`,
  `pixel_tolerance`, `metric`, `keep_policy`, `threads`, `dark_mode`,
  `exclude_trash` and `trash_dirs` (Linux `~/.config/umdf/config.toml`, Windows
  `%APPDATA%\umdf\config.toml`). It is plain TOML, easy to review or edit by
  hand.

  ```toml
  folders = ["/home/me/Pictures"]
  move_dest = "/home/me/Duplicates"
  recursive = true
  only_identical = false
  compare_media = true
  pixel_similarity = 90.0
  pixel_tolerance = 10
  metric = "dpf"                 # dpf | ssim | sign | pearson | legacy
  keep_policy = "original"       # original | dimensions | filesize | ...
  threads = 0                    # 0 = one per CPU
  dark_mode = false
  exclude_trash = true
  trash_dirs = [".dtrash", ".Trash", ".Trash-1000", "$RECYCLE.BIN", "RECYCLER", ".Trashes"]
  ```

* Trash/recycle directories are skipped by default. `trash_dirs` entries are
  matched **case-insensitively against directory names**; add or remove names
  in `config.toml`. The folder explicitly selected as a scan root is never
  skipped. Disable the **Exclude trash folders** checkbox to scan them.
* Older builds kept this in `config.db`; it is migrated automatically on first
  start and the legacy file is kept as `config.db.bak`.
* `<config dir>/umdf/cache.db` on Linux/macOS and
  `%LOCALAPPDATA%\umdf\cache.db` on Windows is a **disposable SQLite cache** of
  derived values: `fingerprint_cache` (perceptual grids) and
  `embedding_cache` (semantic vectors). Entries are keyed by absolute path and
  validated against file size, modification time and the grid/model that
  produced them, so an edited or replaced file is recomputed automatically.
  Deleting `cache.db` is always safe and only makes the next scan slower.
  On Windows the cache is kept out of the roaming profile; an older cache in
  `%APPDATA%\umdf` is migrated automatically.
* Windows cache keys are normalised (drive/UNC form, separators and case), so
  the canonicalised path used by the folder browser and the display path saved
  in `config.toml` share the same cache entries.
* The status bar reports how many fingerprints came from the cache
  (`N desde caché`), and the CLI prints `fingerprinted N (M from cache)`.
* The Models manager shows the cache size (fingerprints, embeddings, bytes)
  and a **Clear cache** button.

### Build and run

```bash
# Build with the feature (UI and/or CLI)
cargo build --release -p umdf-cli --features embeddings
cargo build --release -p umdf-ui  --features embeddings

# A dynamic ONNX Runtime is required unless the release package bundles it.
# For CUDA, provide a GPU build:
export ORT_DYLIB_PATH=/path/to/libonnxruntime.so   # e.g. onnxruntime-gpu

# CLI: point it at a downloaded model
umdf --folder ~/Pictures \
     --embeddings-model models/resnet18.onnx \
     --embeddings-device cuda \
     --embeddings-threshold 0.90
```

Pipeline: EXIF-aware decode, 224×224 resize and per-channel normalisation run
on Rayon; images go to the model in batches (`--embeddings-batch`, default 32)
as `[N, 3, 224, 224]` tensors; embeddings are L2-normalised and compared by
cosine similarity. For CLIP/SigLIP use
`--embeddings-mean 0.48145466,0.4578275,0.40821073 --embeddings-std 0.26862954,0.26130258,0.27577711`
and usually a lower threshold (0.85). The UI's **Models…** button hides the
pixel metric/tolerance controls while semantic search is enabled and shows a
compact summary (`resnet18.onnx · 224²`) and the actual runtime (`CUDA`/`CPU`) once embeddings start.

## UI

```bash
cargo run -p umdf-ui
```

Layout is a modern light/dark card design:

* **Top bar** in three aligned blocks: title + Add/Remove/Clear; Help, settings,
  sun/moon theme toggle and language selector; separator; Start and Stop.
* **Add folder…** opens a centred modal with a dimmed backdrop, folder icons,
  hover, a path field and **Show hidden**.
* **Folders to scan** always shows the added folders with their full path.
* **Match list** with stage, progress, bulk actions, thumbnails, 38 px rows and
  a soft selection tint plus a left accent bar.
* **Preview** with both pictures, file information, the originality score and
  reasons, a compact move destination with **Browse…**, and Trash/Move/Delete.
* **Delete in chain**: after Trash / Move / Delete the next row is selected
  automatically (and its preview loaded), so you can keep comparing and
  deleting. `↑`/`↓` move the selection and `Supr`/`Delete` removes the selected
  duplicate. Counters update live.
* **Help** button with a formatted, scrollable guide (including what the gray
  tolerance means).
* **Parallel by default**: decoding, hashing and comparison use a dedicated
  Rayon pool (one worker per CPU). The **Threads** option in *Options* (and
  `--threads N`) overrides it.
* **Previews and thumbnails are decoded off the UI thread**, with
  latest-request-wins coalescing and a small cache.
* **Language selector** switches the whole interface and Help between nine
  languages at runtime: English, Español, Français, Deutsch, Português,
  Italiano, 简体中文, 日本語 and 한국어. The choice is persisted in
  `config.toml` (`language = "fr"`, ...). Noto Sans CJK is embedded, so no
  extra font installation is needed.
* **Light / dark mode** with a single `Theme` global; native widgets follow
  `Palette.color-scheme`.

Self-test hooks used for automated UI checks (no mouse needed):

```bash
umdf-ui --demo /path/to/folder --demo-select
umdf-ui --demo /path/to/folder --demo-stop-ms 900 --demo-restart-ms 1500
umdf-ui --demo /path/to/folder --demo-lang es
umdf-ui --demo /path/to/folder --demo-lang zh
umdf-ui --demo-browser --demo-lang es   # drive/root view
umdf-ui --demo /path/to/folder --demo-bulk trash
umdf-ui --demo /path/to/folder --demo-delete 3
umdf-ui --demo-help
umdf-ui --demo /path/to/folder --semantic-model models/resnet18.onnx  # needs --features embeddings

```

## Design notes

* Exact duplicates: streaming SHA-256, computed only for files sharing a size.
* Perceptual duplicates: mean-normalised greyscale grid (75×75 by default)
  with a 1.5× aspect-ratio gate.
* Groups: union-find over accepted pairs.
* Decisions: six keep policies plus a hard `leave_one` guarantee.
* Safety: dry-run unless `--apply`; OS trash instead of permanent delete.
* Cancellation: checked during discovery, every 1 MiB of hashing, every 256
  comparisons and with timeouts for ffmpeg/ffprobe.
* Rotated photos: EXIF orientation is applied when decoding, so a photo with an
  orientation tag matches its physically rotated copy.

## Disclaimer

UMDF is provided **“as is”**, without warranty of any kind. The authors are not
responsible for any loss, damage, data loss or other consequences resulting
from the use of this software, including deleting, moving or modifying files.
Always keep backups and review the results before applying actions.

## Author

Juan Pedro Paredes Caballero <juanpedro.paredes@gmail.com>

## License

Copyright (C) 2026 Juan Pedro Paredes Caballero.

UMDF is free software: you can redistribute it and/or modify it under the terms
of the **GNU General Public License, version 3 only** (`GPL-3.0-only`), as
published by the Free Software Foundation. See [LICENSE](LICENSE).

This program is distributed in the hope that it will be useful, but **without
any warranty**; without even the implied warranty of merchantability or fitness
for a particular purpose. See the GNU General Public License for details.

The desktop UI uses [Slint](https://slint.dev) under its GPLv3 option, so the
combined application is GPLv3. Third-party components and their licenses are
listed in [THIRD_PARTY.md](THIRD_PARTY.md); the generated
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md) contains the full license
texts of the Rust dependencies.

The AI installers bundle the CPU ONNX Runtime (MIT). Its license and
`ThirdPartyNotices.txt` are included in every AI package as
`ONNXRUNTIME-LICENSE` and `ONNXRUNTIME-THIRD-PARTY-NOTICES.txt`. Every package
also contains `SOURCE.txt` with the exact source tag, satisfying the GPLv3
source-availability requirement.
