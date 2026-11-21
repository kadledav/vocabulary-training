# Third-party components of the vocabulary-training-tts image

The image is a combined work under these licences (SPDX):
`MIT AND Apache-2.0 AND BSD-3-Clause AND Unicode-3.0 AND LGPL-3.0-only AND LGPL-2.0-or-later`,
plus the Debian runtime of the base image (glibc LGPL-2.1-or-later,
libstdc++/libgcc GPL-3.0-or-later WITH GCC-exception-3.1; their notices are
in `/usr/share/doc` of the image). The project's own code (`/vocab-tts`) is
MIT (`LICENSE.txt`). Sources: `SOURCE.md`.

| Component | Version | Licence | Text in /licenses |
|---|---|---|---|
| vocab-tts (this repository) | see image tag | MIT | `LICENSE.txt` |
| Kokoro-82M v1.0 weights, voices; misaki-derived US/UK lexicons (sherpa-onnx packaging `kokoro-multi-lang-v1_0`) | 1.0 | Apache-2.0 | `kokoro-model-LICENSE.txt` |
| sherpa-onnx (C/C++ engine, static libraries; Rust crates `sherpa-onnx`, `sherpa-onnx-sys`) | 2.0.1 | Apache-2.0 | `sherpa-onnx-LICENSE.txt` |
| ONNX Runtime (in the sherpa-onnx static libraries) | 1.28.2 | MIT | `onnxruntime-LICENSE.txt` |
| kaldi-native-fbank, kaldi-decoder, kaldifst, OpenFst, simple-sentencepiece (in the sherpa-onnx static libraries) | as pinned by sherpa-onnx v2.0.1 | Apache-2.0 | `sherpa-onnx-LICENSE.txt` (same terms) |
| kissfft (in the sherpa-onnx static libraries) | as pinned by sherpa-onnx v2.0.1 | BSD-3-Clause | see https://github.com/mborgerding/kissfft |
| utf8proc (in the sherpa-onnx static libraries) | 2.12.0 | MIT (+ Unicode data licence) | `utf8proc-LICENSE.md` |
| LAME MP3 encoder (crate `mp3lame-sys`, bundled source) | 3.100 | LGPL-2.0-or-later | `lame-COPYING.txt` |
| Rust bindings `mp3lame-sys`, `mp3lame-encoder` | 0.1.11, 0.2.5 | LGPL-3.0-only | `mp3lame-sys-LICENSE.txt` |
| Rust crates (axum, tokio, hyper, serde, ebur128, …) | `Cargo.lock` | MIT / Apache-2.0 / BSD-3-Clause / Unicode-3.0 / Zlib / Unlicense (each crate's choice) | their sources on crates.io |

No component is GPL-only: sherpa-onnx 2.x no longer contains espeak-ng
(GPL-3.0), and the model archive's `espeak-ng-data` is not copied into the
image.
