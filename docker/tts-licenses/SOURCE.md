# Corresponding source of the vocabulary-training-tts image

Everything in this image is built from public sources. To rebuild it
exactly, use:

1. **This repository** — https://github.com/kadledav/vocabulary-training at
   the release tag of the image (`vX.Y.Z` for the image tag `X.Y.Z`; a
   `sha-<short>` image is the commit `<short>`). `docker/Dockerfile.tts`
   is the build recipe; `Cargo.lock` (also in the image at
   `/usr/share/doc/vocab-tts/Cargo.lock`) pins every Rust crate, whose
   sources are on https://crates.io (MIT; the crates' own licences in
   their sources).
2. **sherpa-onnx v2.0.1** static libraries (Apache-2.0; they contain ONNX
   Runtime 1.28.2, MIT, and the libraries listed in `THIRD-PARTY-NOTICES.txt`):
   - https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.1/sherpa-onnx-v2.0.1-linux-x64-static-lib.tar.bz2
     sha256 `55cfadf65ba50d0afc1a802b7a337942819e51c94a5b5a384def1978f7c70871`
   - https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.1/sherpa-onnx-v2.0.1-linux-aarch64-static-lib.tar.bz2
     sha256 `b6c6090e40fd18d6812f04fe21260602cd1223d7b5e83ad39dab87ad7c1c031d`
   - their source: https://github.com/k2-fsa/sherpa-onnx/tree/v2.0.1
3. **Kokoro-82M v1.0** as packaged by sherpa-onnx (weights, voices,
   lexicons; Apache-2.0):
   - https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-multi-lang-v1_0.tar.bz2
     sha256 `60703707bc05ccd48d60e1eb723f38b07d6e474795a0b6a588d7220400e0f7a4`
   - upstream: https://huggingface.co/hexgrad/Kokoro-82M
4. **CMUdict** (BSD-2-Clause), the fallback pronunciation dictionary:
   - https://raw.githubusercontent.com/cmusphinx/cmudict/74790861f652b15e4ac49015a90074ad62a27690/cmudict.dict
     sha256 `81917843c7f44ce2b094ac63873c2c7a4cf802040792c455ba3ca406891c3d22`
5. **LAME 3.100** (LGPL-2.0-or-later), compiled from the source bundled in
   the crate `mp3lame-sys` 0.1.11 (https://crates.io/crates/mp3lame-sys);
   statically linked: the object files can be rebuilt from 1. and 5. and
   relinked with a modified LAME.
6. **Base images** (glibc, libstdc++, libgcc, CA certificates and tzdata
   from Debian 13, under their own licences in `/usr/share/doc` of the
   image): `gcr.io/distroless/cc-debian13:nonroot` and, for the build,
   `rust:1.99.0-slim-trixie` with Debian packages from
   `snapshot.debian.org` — digests and timestamp in `docker/Dockerfile.tts`.

No espeak-ng code or data is in this image: sherpa-onnx 2.x dropped
espeak-ng, and `vocab-tts` turns text into phonemes itself with the
Kokoro lexicons (docs/design/tts-spike.md).
