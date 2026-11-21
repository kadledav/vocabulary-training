# Releasing the TTS image — what release.yml must gain (M6 phase 2)

The M6 phase 1 branch does **not** touch the release workflow: the main
checkout has an uncommitted rewrite (`release.yml` + `release-image.yml` +
`tools/release-promote.py`: each platform built once natively, pushed by
digest, smoke-tested, merged into one index `sha-<short>`, then tagged
`X.Y.Z`; digests into the release notes). The second image
`ghcr.io/kadledav/vocabulary-training-tts` must follow exactly the same
path. Changes, against that rewrite:

## release-image.yml (one platform)

1. New inputs, so the same workflow builds either image:
   - `image`: `ghcr.io/kadledav/vocabulary-training` or
     `ghcr.io/kadledav/vocabulary-training-tts` (type string, required);
   - `dockerfile`: `docker/Dockerfile.server` or `docker/Dockerfile.tts`.
   `dep-age.py` requires `file:` to name a Dockerfile of the repository: a
   `${{ inputs.dockerfile }}` value is refused ("must name a Dockerfile in
   the repository"). Either (a) keep two literal build steps guarded by
   `if: inputs.image == '…'`, each with its literal `file:` and `outputs:`
   name, or (b) teach dep-age that `${{ inputs.dockerfile }}` is allowed when
   every caller passes a literal repository path (more work; (a) is enough).
2. TTS build step (literal values; everything else as for the server):
   `file: docker/Dockerfile.tts`,
   `outputs: type=image,name=ghcr.io/kadledav/vocabulary-training-tts,push-by-digest=true,name-canonical=true,push=true`,
   same `platforms`, `provenance: mode=max`, the same pinned SBOM generator,
   the same labels (`org.opencontainers.image.version`/`revision`).
   The build needs network for crates.io and the three `ADD --checksum`
   archives (sherpa-onnx libs for both platforms, the 350 MB model); the
   job's `timeout-minutes: 60` is enough (the amd64 runner builds the
   release binary with LTO in a few minutes).
3. Before the image build, a step running the crate's tests natively
   against the real libraries: the same action with `target: test`,
   `push: false`, `provenance: false`, `sbom: false` (as in ci.yml's
   `tts-image-smoke`). On arm64 this is the only place the arm64 libraries
   are exercised before release.
4. The smoke step must run `tools/tts-smoke.py` for the TTS image:
   `release-promote.py smoke` gains `--smoke tts` (or a `--tool` choice
   `image-smoke.py|tts-smoke.py`), passed by release-image.yml from a new
   input; `smoke()` then runs `HERE / "tts-smoke.py"` on the pulled
   `repository@digest`. tts-smoke.py takes the image reference as its only
   argument like image-smoke.py and pulls nothing itself (`--pull=never`),
   so the existing pull/architecture check stays as it is.

## release.yml

5. `prepare` checks both repositories: `registry_digest()` for the TTS
   repository too; `existing` must be the same for both (a version is
   either published for both or for none). Simplest: run `prepare` once per
   repository (two steps, outputs `existing` and `existing_tts`) and skip
   each image's jobs on its own `existing`.
6. Two more jobs `tts-amd64` (runner `ubuntu-24.04`) and `tts-arm64`
   (runner `ubuntu-24.04-arm`) calling release-image.yml with
   `image: ghcr.io/kadledav/vocabulary-training-tts`,
   `dockerfile: docker/Dockerfile.tts`, `smoke: tts`, `needs: [prepare, checks]`,
   permissions `contents: read`, `packages: write` (job level only).
7. `publish-tts`: like `publish`, with `--repository
   ghcr.io/kadledav/vocabulary-training-tts` and the two TTS digests;
   output `digest`.
8. `notes`: both references in the release notes. `notes_body()` in
   release-promote.py writes one "Server image" block; give it a label
   argument (`--label "TTS image"`) or call it twice with different
   repositories so the TTS block reads e.g.
   "TTS image (linux/amd64, linux/arm64), deploy by tag and digest:
   `ghcr.io/kadledav/vocabulary-training-tts:X.Y.Z@sha256:…`". The
   idempotency check (`reference in existing`) works per reference.
9. The GHCR package `vocabulary-training-tts` is created by the first push
   (private by default): the owner makes it public once, like the server
   package (owner step in deploy/README.md §5).

## Other files

- `tools/dep-age.py`: apply `docs/design/tts-dep-age.patch` (ADD
  `--checksum` of GitHub release assets) before Dockerfile.tts reaches
  main, else the `guards` job fails on its three `ADD` lines.
- `tools/licenses.py` (main checkout): it covers `vocab-server` only. For
  the TTS image, either extend it to a second package/Dockerfile pair
  (`vocab-tts` → `docker/Dockerfile.tts`, notices file for the TTS image,
  allowing `LGPL-3.0` for `mp3lame-*`) or keep the hand-written
  `docker/tts-licenses/THIRD-PARTY.md`; then the image's label check in
  `licenses.py --check` must not look at Dockerfile.tts.
- `renovate.json`: the custom regex manager for `ARG SHERPA_ONNX_VERSION`
  (design §14.3) does not match Dockerfile.tts (versions are in the URLs,
  checksums must change together) — replace it with a comment that
  sherpa-onnx, the model and their checksums are updated by hand (as
  design §14.3 already says for the checksums).
- Tests: `tools/tests/test_release_promote.py` for the new smoke choice and
  the second notes block.
