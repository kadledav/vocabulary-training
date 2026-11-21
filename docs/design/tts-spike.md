# M6 spike: Kokoro with sherpa-onnx 2.0.1 from Rust

Date: 2026-11-21. Scope: design §17 row M6 ("spike first"), §10. Result: the
design's choice works; three findings change details of §10 (below).

## Setup

- Machine: Apple M5 Pro (arm64), measured natively on macOS and in a
  Podman 6.1.4 VM (linux/arm64, 7 vCPUs, 5.5 GiB) running the final image
  hardened as in Compose (read-only, `cap_drop ALL`, 2 CPUs, 1280 MB).
  amd64 is measured by the CI smoke test (`tools/tts-smoke.py`), not here.
- sherpa-onnx 2.0.1 (crates `sherpa-onnx`/`sherpa-onnx-sys` 2.0.1,
  2026-10-27; static libraries of the GitHub release v2.0.1, 2026-10-26),
  model `kokoro-multi-lang-v1_0` (fp32, sha256 `60703707…`) and
  `kokoro-int8-multi-lang-v1_0` (sha256 `72c34723…`), both uploaded
  2026-10-26; all checksums matched GitHub's asset digests.
- 2 inference threads (`VOCAB_TTS_THREADS=2`), voice `af_heart` (speaker 3)
  unless noted; `bf_emma` is speaker 21 (model metadata `speaker_names`).
- Texts: synthetic English sentences (3–40 words), single words, numbers,
  invented words; one 598-character text (the API maximum is 600).

## Numbers

| Measure | fp32 | int8 |
|---|---|---|
| Model load | 0.3–0.5 s | 0.5 s |
| RTF, sentences (13–40 words), macOS | 0.33–0.37 | 0.80–0.90 |
| RTF, single words, macOS | 0.44–0.57 | 1.1–1.4 |
| RTF overall (9 items, 30 s of audio), macOS | **0.36** | 0.88 |
| 19-word sentence (design threshold: ≤ 2 s) | **1.6–1.7 s** | 4.5 s |
| Peak RSS, macOS, spike program | 0.82 GB | 0.61 GB |
| RTF overall in the container (11 items, 42 s of audio) | **0.36** | – |
| 598-character text in the container | 10.6 s for 31 s of audio | – |
| Peak container memory (`docker stats`, incl. the 598-char text) | **0.91 GB** | – |
| Threads/PIDs in the container | 5 | – |
| Image size (linux/arm64) | 433 MB | – |
| MP3 size | ≈ 7.5 KB/s (≈ 60 kbit/s VBR q5): word ≈ 5 KB, sentence ≈ 25–40 KB | |
| Loudness of the clips (ffmpeg `ebur128` on the decoded MP3s) | −16.1 … −16.9 LUFS, true peak ≤ −1.4 dBTP | |

## Decisions

- **fp32** (`model.onnx`), not int8: int8 is 2.4× slower on this CPU (dynamic
  quantisation kernels) for 0.2 GB less memory; fp32 is well inside the
  design threshold. On x86 with VNNI int8 might be faster; the CI smoke
  test on amd64 shows the time per clip in the log (`synthesis_ms`), and the
  switch is a Dockerfile change (file name) only.
- **Voices**: `af_heart` (US, default) and `bf_emma` (UK), as designed.
- **Limits** (Compose, smoke test): memory 1280 MB without swap (peak
  0.91 GB + 40 %), 2 CPUs, 64 PIDs; request timeout 60 s (the longest text
  takes ~11 s here).
- **MP3 encoder**: LAME 3.100 through `mp3lame-encoder` 0.2.5 /
  `mp3lame-sys` 0.1.11 (bundled source, built with autotools; LAME is
  LGPL-2.0-or-later, the Rust bindings LGPL-3.0), VBR quality 5, mono
  24 kHz, with a Xing/LAME tag frame so players know the exact duration.

## Findings that change the design (§10)

1. **sherpa-onnx 2.0 has no espeak-ng any more** (upstream PR 4055: "sherpa-onnx
   is GPL-free"). Kokoro's front-end is lexicon-only: every word not in the
   lexicon is **silently dropped** (in the spike: "1990", "3rd" and invented
   words; "dogs" and "wanted" are not in the US lexicon either) and
   **printed to stderr** ("OOV word skipped:
   '…'") — words of book sentences would land in the container log (R-24).
   Also, one engine instance merges the US and UK lexicons (last wins), so
   the UK voice would not get UK pronunciations (R-42).
   → `vocab-tts` does grapheme-to-phoneme itself (`phonemize` module): the
   lexicon of the voice's accent (Kokoro v1.0's misaki lexicons, 178k US /
   192k UK entries), numbers/years/ordinals read as words, accents folded,
   possessive and regular suffixes (-s, -ed, -ing, -ly) derived from the
   stem with the right ending sound, hyphenated parts, and letter-by-letter
   spelling as the last resort (counted in the log as `spelled_out`). The
   engine gets only phoneme code points (`GenerationConfig.phoneme_codepoints`);
   nothing it logs can contain text. Same speed and duration as the
   engine's own front-end in the spike.
2. **The image is not a GPL-3.0 combined work.** Without espeak-ng (and
   without copying the archive's `espeak-ng-data`) the image is
   `MIT AND Apache-2.0 AND BSD-3-Clause AND Unicode-3.0 AND LGPL-3.0-only
   AND LGPL-2.0-or-later` (LAME and its bindings are the only copyleft
   parts). `/licenses` carries those texts, `THIRD-PARTY.md` and `SOURCE.md`
   instead of the GPL text; the label says the same. The repository stays MIT.
3. **Loudness**: scaling the whole clip down to the −1.5 dBTP ceiling (design
   §10.3 step 6) leaves sentences at −18 … −20 LUFS (speech has a high crest
   factor). `postprocess` uses a look-ahead peak limiter (5 ms look-ahead,
   80 ms release) with make-up passes instead; the scaling remains as a final
   safety net. Result: every clip −16.1 … −16.9 LUFS.

## Other differences to §10.2 (from the M6 phase 1 brief)

- API `POST /v1/speak` (not `/v1/synthesize`), **no token**: the container is
  only on the `internal: true` network shared with the app; every input is
  validated anyway (text ≤ 600 characters, no control/invisible formatting
  characters, no `<`/`>` — no SSML; voice from the list; speed 0.5–1.5;
  JSON object with exactly those fields; body ≤ 4 KiB). Responses carry
  `X-Duration-Ms`, `X-Loudness-Lufs` and `X-Voice`; `422 nothing_to_say` when
  nothing in the text is speakable, `503 busy` (with `Retry-After`) when one
  synthesis runs and four wait. `GET /v1/voices`, `GET /healthz`.
- Configuration `VOCAB_TTS_LISTEN` (default `0.0.0.0:8090`),
  `VOCAB_TTS_MODEL_DIR` (`/model`), `VOCAB_TTS_THREADS` (2),
  `VOCAB_TTS_LOG_LEVEL` (info); unknown `VOCAB_TTS_*` = startup error. At
  start the service speaks "Hello." with both voices (self-test) before it
  listens.
- The phoneme symbols the engine receives and the clip identity in the app
  (design §10.3 step 2: `v1|kokoro-multi-lang-v1_0|voice|1.0|text`) are
  unaffected; phase 2 may add the front-end version to the hash input so
  that a front-end change regenerates clips.

## Building

`vocab-tts` is a workspace member but not a default member. Its dependency
`sherpa-onnx-sys` links the sherpa-onnx static libraries; **without
`SHERPA_ONNX_LIB_DIR` its build script downloads them from GitHub without a
checksum**. Therefore:

- `docker/Dockerfile.tts` adds the archives with `ADD --checksum` and sets
  `SHERPA_ONNX_LIB_DIR`; `--target test` runs the crate's tests there.
- CI (`rust` job) sets `SHERPA_ONNX_LIB_DIR` to an empty directory: clippy
  checks the code without linking; `cargo test` excludes `vocab-tts`, whose
  tests run in the `tts-image-smoke` job (Docker `test` stage).
- Locally: download the archive of your platform from the v2.0.1 release,
  compare `shasum -a 256` with GitHub's digest (e.g. macOS arm64
  `sherpa-onnx-v2.0.1-osx-arm64-static-lib.tar.bz2`
  `d7a9b14f337958e7373c8c650f046df33d210bbaff2f33cb8322dda663ee5e1c`), unpack it
  and `export SHERPA_ONNX_LIB_DIR=<dir>/lib` before `cargo … --workspace`
  (or point it at an empty directory for clippy only).
