# Third-party notices

UMDF is distributed under the **GNU General Public License, version 3 only**
(`GPL-3.0-only`). See [LICENSE](LICENSE).

## Slint

The desktop UI (`umdf-ui`) is built with
[Slint](https://slint.dev), used under the **GNU GPLv3** option.
Slint is Copyright (C) SixtyFPS GmbH and contributors.

- Upstream: <https://github.com/slint-ui/slint>
- License options: GPL-3.0-only, Slint Royalty-free 2.0, or Slint Commercial.

## ONNX Runtime (optional)

The `embeddings` cargo feature uses [ONNX Runtime](https://onnxruntime.ai),
Copyright (c) Microsoft Corporation, under the MIT License.

Release binaries are built **without** this feature, so they do not bundle
ONNX Runtime or CUDA libraries. If you build with `--features embeddings`, the
engine loads ONNX Runtime dynamically from the path in `ORT_DYLIB_PATH`; obtain
that library separately from Microsoft or your distribution.

## Rust crates

A complete dependency list, including versions and licenses, can be generated
with:

```bash
cargo metadata --format-version 1
cargo tree
```

Most dependencies are dual-licensed MIT/Apache-2.0. `option-ext` is MPL-2.0
(file-level copyleft; no changes are made to it in this project).

## Models

The optional image-embedding models downloaded by the Models manager are not
part of this repository. Each model keeps the license of its upstream Hugging
Face repository (for example ResNet, CLIP and SigLIP model weights).
