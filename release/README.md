# Release reference

Proofstorm has one Bash/Rust release flow for Linux x86-64 and macOS Apple Silicon.
Installed users download prebuilt binaries, the embedded GUI, and pinned container
images; they do not need the repository or a compiler.

- [Prepare and promote a release](../scripts/RELEASING.md): the normal maintainer flow.
- [Checks and diagnostic commands](../scripts/CHECKS.md): code, packaging, install, and live test lanes.
- [Linux](linux.md) and [macOS](macos.md): platform boundaries and fresh-host acceptance.
- [Agent attachments](agent-attachments.md): project configuration and desktop limitations.
- [Website download contract](release-manifest.md): the generated `release.json` asset.
- [Development](../scripts/DEVELOPMENT.md): the same product lifecycle using checkout artifacts.

`release/ghcr.json`, bootstrap-tool manifests, and controller receipts are build
inputs, not clutter. Catalog/provenance and controller compatibility checks remain
required. The seven release assets are listed in the release guide.

## Historical evidence

The checked-in `*-verification.json`, publication/container reports,
[alpha.1 notes](alpha-1-notes.md), and [alpha.2 Linux smoke report](alpha-2-linux-smoke.md)
record the September 8–10, 2026 bring-up. Their versions, hashes, and limitations
apply to those runs only; they are not acceptance results for the current checkout.

New owned live runs retain private logs and `acceptance.json` in their printed
work directory. CI artifacts retain build/install evidence. Neither a historical
pass nor an HTTP 200 establishes current public-download, model, or desktop behavior.
