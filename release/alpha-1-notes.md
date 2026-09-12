# Proofstorm 0.1.0-alpha.1 — Linux x86-64

Early alpha for testing the GitHub installer and full runtime on a fresh Linux VM.
This prerelease ships Linux x86-64 binaries for glibc-based distributions. A Mac
archive is not included in this prerelease yet.

## Install

Run as your ordinary user, not root:

```sh
curl --fail --location --proto '=https' --proto-redir '=https' \
  https://github.com/orangeshyguy21/proofstorm/releases/download/v0.1.0-alpha.1/install.sh \
  --output /tmp/proofstorm-install.sh
sh /tmp/proofstorm-install.sh
export PATH="$HOME/.local/bin:$PATH"
proofstorm setup
proofstorm doctor
```

Docker Engine with Buildx must already be installed, running, and accessible to
your user. The installer needs curl, tar/gzip, and sha256sum (or shasum). It does
not install Docker, modify your shell profile, or build Proofstorm from source.
No alpha/development override flag is required.

From your project directory, `proofstorm agent open codex`, `proofstorm agent open opencode`,
or `proofstorm agent open claude` connects Proofstorm and starts the corresponding
installed agent CLI. On a headless VM, `proofstorm gui start` keeps the web UI
on loopback; use an SSH tunnel rather than exposing it to the internet.

## Known limitations

- The GitHub download and full runtime flow still need their fresh-VM test.
- Full catalog/helper image coverage and lab lifecycle are not yet certified.
- These host binaries use a debug build and a recorded dirty source snapshot.
- The controller is a digest-pinned preview. Host/controller compatibility and
  bundle integrity passed; these are not claims of production readiness.

The earlier development bundle passed a source-free install/reinstall test.
That result is not a test of this newly rebuilt alpha or its GitHub download.

## Artifact identity

- Archive: `proofstorm-0.1.0-alpha.1-x86_64-unknown-linux-gnu.tar.gz`
- Archive SHA-256: `9d81714bd782214f08b0d1860b34be4a8d12713c471e679821c6df7b93571517`
- Installer SHA-256: `9a6ed7944f350b161170c55ab3a845beb37d6cb1f22ebac42692a4970f5120a3`
- Source base revision: `9bdba429f3f9f4b48ba7c762981548d5f948f496`
- Exact dirty source snapshot SHA-256: `37f9e3a0545107fd550df13531c09f10e87412f5e0d15371a975cb4e99491482`

The base revision alone does not reproduce the dirty snapshot. Checksums detect
changed downloads; they are not publisher signatures. Use only the official
repository's release assets.
