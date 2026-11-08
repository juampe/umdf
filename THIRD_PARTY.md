# Third-party notices

UMDF is distributed under the **GNU General Public License, version 3 only**
(`GPL-3.0-only`). See [LICENSE](LICENSE).

MIT, Apache-2.0, BSD and MPL-2.0 components are compatible with GPLv3. They
keep their own licenses; the UMDF project does not relicense them.

## Slint

The desktop UI (`umdf-ui`) is built with
[Slint](https://slint.dev), used under the **GNU GPLv3** option.
Slint is Copyright (C) SixtyFPS GmbH and contributors.

- Upstream: <https://github.com/slint-ui/slint>
- License options: GPL-3.0-only, Slint Royalty-free 2.0, or Slint Commercial.

Slint is the reason the combined application is distributed as GPLv3.

## ONNX Runtime (bundled in AI packages)

The `embeddings` cargo feature uses [ONNX Runtime](https://onnxruntime.ai),
Copyright (c) Microsoft Corporation, under the **MIT License**.

The **AI archives and installers** (Linux `.deb`/`.rpm`/`.AppImage`, Windows
`.msi`/`-ai.zip`, macOS Apple Silicon `-ai`) bundle the official **CPU**
ONNX Runtime 1.31.1 shared library. The following files are included with those
packages:

- `ONNXRUNTIME-LICENSE` — MIT license of ONNX Runtime.
- `ONNXRUNTIME-THIRD-PARTY-NOTICES.txt` — notices for the third-party
  components inside the ONNX Runtime binary.
- `ONNXRUNTIME-PRIVACY.md` — ONNX Runtime privacy notes.

CUDA/cuDNN libraries are **not** bundled. To use a GPU, point `ORT_DYLIB_PATH`
at a CUDA-enabled ONNX Runtime and select CUDA in the Models manager. Standard
archives do not include ONNX Runtime.

## Corresponding source

Every binary package includes `SOURCE.txt` with the exact source tag, and the
source is publicly available at:

<https://github.com/juampe/umdf>

This satisfies the GPLv3 requirement to make the Corresponding Source available
for the distributed object code.

## ffmpeg and ffprobe

UMDF invokes `ffmpeg`/`ffprobe` as **external, optional** programs for video and
audio comparison. They are not bundled or linked. Install them from your
distribution (`Recommends: ffmpeg` in the Linux packages). Their licenses are
independent of UMDF.

## Rust crates

The generated [`THIRD_PARTY_LICENSES.md`](THIRD_PARTY_LICENSES.md) contains the
full license texts and the crates that use them. It is produced with
`cargo about generate about.hbs` and is included in every package.

Most dependencies are dual-licensed MIT/Apache-2.0. `option-ext` is MPL-2.0
(file-level copyleft; it is used unmodified). CSS/JS assets, if any, keep their
upstream licenses.

## Models

The optional image-embedding models downloaded by the Models manager are not
part of this repository and are not bundled with the installers. Each model
keeps the license of its upstream Hugging Face repository (for example ResNet,
CLIP and SigLIP model weights). Review the model card before redistributing a
model.

## Noto Sans CJK

The UI embeds **Noto Sans CJK Regular** (Copyright 2010-2023 Google LLC and
contributors) as a fallback for Chinese, Japanese and Korean text. It is
licensed under the **SIL Open Font License 1.1**. The full copyright and
license text is included in every package as `NOTO-COPYRIGHT.txt` and in the
repository at `assets/fonts/NOTO-COPYRIGHT.txt`.

## App icon

The UMDF icon (`assets/icon*`) is part of the project and is distributed under
the same GPL-3.0-only license.
