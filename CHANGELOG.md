# Changelog

All notable changes to UMDF are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [0.6.6] - 2026-11-12

### Added

- Shift shortcuts act on both files of the selected row: `Shift+T` trashes,
  `Shift+M` moves and `Shift+D` deletes the original and the duplicate. The
  lowercase keys keep acting only on the duplicate. Rows that referenced either
  removed file are dropped so the list never shows missing paths.
- The Move destination is excluded from scans even when it is a custom folder
  not listed in `trash_dirs`, so moved duplicates never reappear.
- The walk log records how many trash directories were skipped and which
  names/paths were used, making exclusion problems visible in `umdf.log`.

### Fixed

- The semantic-search option is now persisted in `config.toml`
  (`semantic_enabled`). Disabling it survives a restart instead of being
  re-enabled whenever a model is registered.
- An empty `trash_dirs` list falls back to the built-in defaults instead of
  scanning `.dtrash` and other trash folders.
- The semantic checkbox saves immediately when toggled, and choosing or
  removing the active model keeps the stored flag in sync.

### Changed

- Shortcut hints and Help in all ten languages document the Shift actions.

## [0.6.5] - 2026-11-11

### Added

- Preview rotation for visual comparison: `1` rotates the original and `2`
  the duplicate (90° steps). Rotation is display-only and is never written to
  the file; the lightbox and the per-pane ⟳ buttons use the same state.
- "Remove from group" (`o`, or the button under the preview) takes a copy out
  of its group when it does not look like the reference. The file stays on
  disk; the decision is stored in `cache.db` and applied on later scans.
- Star and out choices now persist in `cache.db` across scans and restarts.
  Starred references are loaded into the scan configuration, so the chosen copy
  remains the group reference even for videos and after switching rows.
  "Clear results" keeps these manual choices; "Clear cache" removes them.
- Russian UI: full interface, dynamic phrases and Help translation, bringing
  the app to ten languages.

### Fixed

- The preview splitter width is saved and restored correctly. Window geometry
  is applied before the saved preview width, and the splitter is re-applied
  after the window manager finishes resizing.
- Video previews and clicks on video comparisons no longer flash console
  windows on Windows: every helper process (ffmpeg, nvidia-smi, PowerShell,
  `cmd /c start`, Explorer) is spawned with `CREATE_NO_WINDOW`.
- The perceptual comparison index no longer scans aspect-ratio-incompatible
  pairs. The list stays sorted by aspect ratio, and a cheap mean-difference
  bound prunes pairs that cannot reach the threshold, restoring the previous
  comparison speed without changing accepted matches.

### Changed

- Keyboard hints and Help now document `o`, `1`/`2`, persistent star/out
  decisions and the Russian interface in every language.
- Formatting and lints were cleaned up for the current Rust toolchain.

## [0.6.4] - 2026-11-10

### Added

- A perceptual index for the default `dpf` metric. Fingerprints are sorted by
  their global mean and only a safe mean-difference window is compared; flat
  fingerprints (std < 3) are skipped. This removes most of the O(n²) pairs
  before the pixel loop starts, without changing the accepted matches.
- A whole-library comparison cache. The match list is stored per exact
  library/settings signature (paths, sizes, mtimes, fingerprints, embeddings,
  metric and thresholds), so a warm rescan reuses it instead of comparing every
  pair again.
- "Clear results" in Models deletes only the comparison cache; fingerprints,
  embeddings and metadata stay cached. "Clear cache" still deletes everything.
- Star reference: press `s` or the ★ button to use a duplicate as the reference
  of its group, even when the policy prefers another copy. The previous
  reference becomes a duplicate and the group rows follow the new reference.
- Keyboard operations in the main list and the enlarged preview: `t` trash,
  `m` move, `d` delete and `s` star, in addition to ↑/↓, Enter and Esc.
- Window size and maximized state are saved in `config.toml` and restored on
  start, together with the existing layout settings.

### Changed

- The Models dialog is more compact: a one-row AI engine bar, a single models
  folder row, all four supported models with their capabilities, an execution
  card for device/similarity, and a footer with the cache counters, "Open data
  folder", "Clear cache", "Clear results" and "Close".
- Cache counters in Models show the number of stored comparison results and
  include their size in the cache total.
- Help now documents the star/reference workflow, the new keyboard operations
  and the independent comparison cache. It also recommends VLC for video
  previews because it plays more codecs without extra installations.
- The Settings window geometry is persisted only in `config.toml`; `cache.db`
  remains disposable derived data.

### Fixed

- Language changes keep manually chosen references labelled as such instead of
  reverting them to the policy score.

## [0.6.3] - 2026-11-09

### Added

- Video thumbnails and previews. The list and the preview use the same frame
  as the scanner (1 s by default); a ▶ button in each preview opens the video in
  the system player. Videos now show a real thumbnail instead of a blank tile.
- Models → AI engine panel: "Check" shows the ONNX runtime, CUDA provider,
  NVIDIA driver, cuDNN/cuBLAS PATH and ffmpeg status; "Install CUDA runtime"
  downloads ONNX Runtime 1.31.1 GPU (CUDA 13 or 12, detected with nvidia-smi),
  extracts it into `%LOCALAPPDATA%\umdf\runtime\cuda` and tells the user to
  restart; "Open runtime folder" opens that folder. CUDA/cuDNN themselves remain
  NVIDIA installers.
- "Save layout" button in the preview header; the layout is also saved when the
  window closes.
- Keyboard: Enter opens the enlarged preview; Esc closes it. The existing
  ↑/↓, Delete and mouse-wheel navigation remains.

### Changed

- The Models… button is available even when no model is selected or the runtime
  is missing, so the AI engine can be configured before enabling semantics.
- A user-installed runtime in `%LOCALAPPDATA%\umdf\runtime\cuda` takes
  precedence over the bundled CPU runtime.
- Help now lists keyboard shortcuts, video frame/playback behaviour, the 32x32
  grid and the CUDA/cuDNN/GPU-runtime steps.

## [0.6.2] - 2026-11-09

### Fixed

- Grouping no longer chains unrelated images together. Union-find could put A
  and C in the same group through B even when A and C did not match. Every
  member must now match the chosen keep directly, and the list shows the
  keep-to-duplicate score instead of the group maximum. This removes the
  "different photos at 100 %" rows. Exact content/media matches keep their
  100 %/99 % score even when they have no perceptual fingerprint (videos).
- Files of 0 bytes are reported as `empty file (0 bytes)` and skipped before
  any decode. The first 0.6.2 scan rewrites the old decoder messages in the
  failure cache.
- The list generates thumbnails lazily for the visible viewport, not only the
  first 300 rows, so scrolling no longer leaves blank thumbnails.

### Added

- `corrupt.log` next to the configuration lists every skipped file with its
  path, byte size and reason. On Windows:
  `%LOCALAPPDATA%\umdf\corrupt.log`.
- The preview column width is saved in `config.toml` and restored at startup.
- The AI runtime search path also includes
  `%LOCALAPPDATA%\umdf\runtime\cuda` (and the platform equivalent), so a
  GPU ONNX Runtime can live outside `Program Files`.
- Help updated: 32x32 grid, no "standard builds", dark/low-contrast guidance,
  and explicit CUDA/cuDNN/GPU-runtime instructions.

### Changed

- The preview lightbox uses 1280x960 preview images (already in 0.6.1).
- Match-list columns are slightly narrower and the preview can grow without
  overflowing a 1280 px window.

## [0.6.1] - 2026-11-09

### Fixed

- `dpf` no longer groups nearly black or flat images just because every cell
  is within the grey tolerance. A match now also needs a minimum contrast and
  a positive Pearson correlation, so two unrelated night photos are rejected.
  The same contrast check protects SSIM from flat-grid false positives.
- Decode failures are cached by path, size and modification time. Empty or
  corrupt files are no longer decoded again on every scan; they are retried
  automatically when the file changes.
- The comparison stage no longer counts every pair in a separate first pass.
  It uses the theoretical maximum for the progress bar, reports progress while
  comparing, and avoids billions of duplicate iterations on large libraries.

### Added

- UI: draggable splitter between the match list and the preview, an
  expand/restore button, automatic side-by-side preview when the panel is wide
  enough, and a full-window lightbox when clicking either preview image.
- Preview images are generated at 1280x960 for the lightbox instead of
  480x360; the small cache still bounds memory.
- The preview explanation now aligns to the top, removing the empty vertical
  gap above the images.
- `umdf.log` next to the configuration records scan phase timings (extract,
  hash, compare, group, total) and cache preload timings. A Windows GUI has no
  console, so previously these diagnostics were lost.

### Changed

- Cache preload is skipped when the in-session warm view already covers at
  least 90 % of the selected files.
- Video-frame fingerprints are preloaded with the rest of the cache instead of
  issuing one SQLite query per video.
- The match list columns are slightly narrower so the preview can grow without
  overflowing a 1280px window.

## [0.6.0] - 2026-11-09

### Changed

- Perceptual fingerprints are now 32x32 instead of 75x75. Each cached
  fingerprint drops from 11,250 to 2,048 bytes (5.5x smaller), which reduces
  disk reads, RAM and comparison cost. A new `fp_version` column invalidates
  old rows automatically and rebuilds them once, without clearing the cache.
- Before extraction, the cache loads the fingerprint, metadata, content-hash,
  audio-signature and active-model embedding rows for the scanned files into
  memory. Extraction no longer issues one SQLite query per file; on a warm
  cache the workers only decode new or changed files.
- All cache writes go through a dedicated writer thread that commits batches of
  up to 256 rows (or every 200 ms) in a single transaction. Reads use the WAL
  connection pool with `mmap_size` and the prepared-statement cache, so writers
  never block the Rayon workers.
- Extraction keeps `2 x threads` images in flight instead of a fixed batch of
  12, so every core is used and a slow image does not idle the pool.
- File modification/creation times already collected by the directory walk are
  reused during extraction, removing one filesystem `stat` per file. This is
  especially noticeable on network shares.
- When semantic search is enabled, each image is decoded once for both the
  perceptual fingerprint and the model tensor. A bounded worker runs GPU
  batches while the CPU decodes the next chunk; the stage label shows
  `CPU + GPU` while both are active.
- The comparison stage rejects a pair as soon as it cannot reach the required
  similarity, precomputes aspect ratios and parallelises pair counting. A
  90 % threshold on a dissimilar pair now inspects roughly 10 % of the grid
  instead of all 1,024 cells.

## [0.5.2] - 2026-11-09

### Fixed

- `cache.db` used a single SQLite connection protected by a mutex. Extraction
  workers (up to 12) queued for every metadata/fingerprint read, so CPU and disk
  stayed almost idle while the scan appeared stuck. The cache now uses a pool of
  up to 32 WAL connections with `busy_timeout`; reads run concurrently and writes
  no longer serialize the whole scan.
- The pool is shared by fingerprint, metadata, content-hash, audio-signature and
  video-frame lookups.

## [0.5.1] - 2026-11-09

### Changed

- The stage label now shows the active resource: `Cargando … · CPU` during
  extraction, `Calculando embeddings … · CPU` or `· CUDA` during semantic
  inference, and `· CPU` for hashing/comparison. This makes it obvious that the
  GPU is not involved in extraction, hashing or comparison.

## [0.5.0] - 2026-11-09

### Added

- Cached video frame fingerprints, audio PCM signatures and whole-file SHA-256
  values. Audio/video rescans no longer run ffprobe/ffmpeg hashing for
  unchanged files.
- Pause/Resume button. The worker loops sleep between extraction, embedding,
  hashing and comparison batches; the progress bar and elapsed time stay
  visible.
- If ONNX Runtime is missing, semantic search is disabled and the UI explains
  how to obtain it and set `ORT_DYLIB_PATH`.

### Changed

- All release packages are AI builds. The lite portable archives are gone;
  installers and portables include the embeddings feature.
- macOS Intel is an AI build without a bundled ONNX Runtime (there is no
  1.31.1 x86_64 macOS runtime); the UI guides the user to install it.
- Models modal: cache paths and counters are on separate lines so the counters
  are never elided; lighter backdrop.
- Left folder actions are `Quitar`/`Limpiar` (and localized equivalents).

## [0.4.0] - 2026-11-09

### Added

- `metadata_cache` in `cache.db`: width, height, EXIF date, EXIF field count,
  camera flag, creation time, media duration and audio flag. An unchanged file
  no longer re-reads its header, EXIF or ffprobe output on the next scan.
- The Models manager cache line and the CLI JSON include metadata-cache counts.

### Changed

- Dark theme contrast: brighter primary/secondary text, bordered cards and
  fields, and disabled buttons with a visible outline instead of faded blobs.
- Main columns have a subtle border; the left folder buttons stretch to fit.
- The progress bar is explicitly anchored at the left edge and grows to the
  right.

### Performance

- One `stat` per file provides both modification and creation times.
- A second scan of an unchanged folder skips fingerprint decoding, image
  headers, EXIF and ffprobe: in the test folder it went from 10.0 s to 0.3 s
  with 38/39 files served from cache.

## [0.3.9] - 2026-11-09

### Fixed

- The runtime label no longer claims `CUDA` when the loaded ONNX Runtime is
  CPU-only. CUDA is shown only when `onnxruntime_providers_cuda` is present
  next to the runtime; the bundled CPU packages show `CPU` in red when CUDA was
  requested.
- The model summary no longer hardcodes `CUDA`; it shows the model and input
  size, and the actual runtime appears once embeddings start.
- A panic in the scan thread now reports its message in the status bar and on
  stderr instead of only `scan thread panicked`.
- Pressing Stop during embeddings keeps the progress bar at the real position
  instead of jumping to 100 %.

## [0.3.8] - 2026-11-09

### Fixed

- Semantic embeddings now report progress **per batch** instead of only at 0
  and 100 %. The progress bar grows from left to right, the stage label shows
  `done/total`, and the status bar shows the current percentage.
- Embeddings are stored in `cache.db` as each batch finishes. Stopping during
  the semantic phase no longer throws away all the work: already computed
  vectors are kept and the next scan resumes from there.
- Stop now interrupts the embedding loop after the current batch.
- The UI shows the execution provider actually selected by ONNX Runtime:
  `CUDA` or `CPU` in red when CUDA was requested but unavailable.
- The elapsed time in the status bar updates live during a scan.

### Changed

- Progress weights: extraction 0–35 %, embeddings 35–75 %, hashing 75–80 %,
  comparison 80–98 %.

## [0.3.7] - 2026-11-08

### Fixed

- Windows cache keys are now normalised. The folder browser canonicalises
  paths to `\\?\B:\...`, while `config.toml` stores the display form
  `B:\...`; both now map to the same cache entry. Existing databases are
  merged automatically on first start (one row per file, newest entry wins).
- The Windows cache moved from Roaming (`%APPDATA%\umdf`) to
  `%LOCALAPPDATA%\umdf`, away from roaming-profile/OneDrive sync. An existing
  cache is migrated automatically.
- The status bar now reports fingerprints restored from the cache
  (`N desde caché`), and the CLI prints `fingerprinted N (M from cache)`.
  JSON output includes `fingerprints_from_cache`, `fingerprints_computed`,
  `embeddings_from_cache` and `embeddings_computed`.

### Added

- Regression tests for cache-key normalisation, the one-time migration and
  cache-hit accounting on a second scan.

## [0.3.6] - 2026-11-08

### Fixed

- A decoder panic in a third-party codec can no longer abort a scan: the file
  is skipped and reported like any other undecodable file.
- If the semantic model cannot be loaded or inference fails, the scan continues
  with perceptual fingerprints instead of stopping. The skipped embeddings are
  counted in the final status.
- Added regression tests for high-entropy/corrupt `.jpg` files and for JPEG
  recovery behind a zero-filled header.

### Added

- The application version is shown next to the title, so reports always
  identify the build in use.

## [0.3.5] - 2026-11-08

### Added

- **Exclude trash folders** checkbox (enabled by default). The default list is
  `.dtrash`, `.Trash`, `.Trash-1000`, `$RECYCLE.BIN`, `RECYCLER` and
  `.Trashes`, matched case-insensitively against directory names. It is also
  available as `exclude_trash` and `trash_dirs` in `config.toml` and can be
  edited by hand.
- All scan/UI options are now persisted in `config.toml`: `folders`,
  `move_dest`, `recursive`, `only_identical`, `compare_media`,
  `pixel_similarity`, `pixel_tolerance`, `metric`, `keep_policy`, `threads`,
  `dark_mode`, `exclude_trash` and `trash_dirs`.
- The Help includes a configuration section in all nine languages.

## [0.3.4] - 2026-11-08

### Fixed

- A corrupt or undecodable image no longer aborts a semantic scan. Failed
  images are skipped and counted; batch-level embedding errors are logged and
  skipped.
- Images that failed extraction are excluded from the embeddings stage.
- JPEG files with a zero-filled/corrupt header are recovered by locating an
  embedded SOI marker (`FF D8 FF`) and decoding from there.
- The final status reports the number of skipped files (`N file(s) skipped`).
- Error messages no longer show the Windows `\\?\` prefix.
- Added tests for JPEG header recovery and diagnostic path cleaning.

## [0.3.3] - 2026-11-08

### Fixed

- Windows extended-length prefixes (`\\?\` and `\\?\UNC\`) are stripped
  from every path shown in the UI: folder list, browser, status messages,
  destination fields and model paths. Filesystem operations still use the
  canonical path.
- The folder browser now has a **drive/root view**. Pressing **Up** at a drive
  root (or **Computer**) lists the available Windows drives; Up and “Use this
  folder” are disabled at that level, and drives use a dedicated icon.
- Added the `--demo-browser` self-test hook.

## [0.3.2] - 2026-11-08

### Added

- Help now shows the **version**, the author
  (Juan Pedro Paredes Caballero <juanpedro.paredes@gmail.com>) and a legal
  disclaimer in all nine languages.
- A fixed version/author line in the Help footer.
- Author/copyright metadata updated across Cargo, Debian, RPM, MSI and the
  Windows executable.

## [0.3.1] - 2026-11-08

### Fixed

- Windows MSI license now embeds the real GPL/ONNX license instead of WiX's
  "Lorem ipsum" placeholder (`WixVariable WixUILicenseRtf`).
- The desktop UI is built as a **Windows GUI subsystem** binary, so it no longer
  opens an extra console window. The CLI remains a console program.
- Windows diagnostics use non-panicking logging when there is no console.
- `ffmpeg`/`ffprobe` child processes are started with `CREATE_NO_WINDOW`.
- The MSI registers as **UMDF — Universal Media Duplicate Finder** in Apps &
  Features, with **UMDF Media Finder** shortcuts.

## [0.3.0] - 2026-11-08

### Added

- UI and Help translations for **French, German, Portuguese, Italian,
  Simplified Chinese, Japanese and Korean**.
- Language selection is persisted in `config.toml` (`language = "fr"`, ...).
- **Noto Sans CJK** is embedded in the UI binary, so Chinese/Japanese/Korean
  render without installing extra fonts.
- Noto OFL copyright/notice in every package.

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
