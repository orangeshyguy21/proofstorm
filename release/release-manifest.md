# Release download contract

Each new release includes `release.json`. The release-preparation flow generates
it from the verified Linux and Mac candidates; maintainers do not edit it or run
another command. The site can use this file without parsing release notes.

## Schema version 1

The document is a UTF-8 JSON object with these required fields:

| Field | Type | Meaning |
| --- | --- | --- |
| `schema_version` | integer | `1`. Breaking changes require a new schema version. |
| `version` | string | Release version without `v`, such as `0.1.0-alpha.4`. |
| `tag` | string | Exact GitHub release tag: `v` followed by `version`. |
| `source_commit` | string | Full 40-character lowercase Git commit SHA shared by both builds. |
| `channel` | string | `alpha`. This flow currently produces only alpha prereleases. |
| `repository` | string | GitHub `OWNER/REPO` used for this release. |
| `installer` | string | `install.sh`, a key in `assets`. |
| `verification_reports` | string | `verification-reports.tar.gz`, a key in `assets`. |
| `platforms` | object | Platform ID → platform description (below). |
| `assets` | object | Exact download filename → byte size and SHA-256 (below). |

Consumers should reject unsupported schema versions and tolerate additional
fields. Platform IDs are inventory keys, not a claim that every Linux distribution
or macOS version has passed acceptance testing.

### Platforms

Version 1 currently lists `linux-amd64` and `macos-arm64`. Each description contains
`os`, `arch`, the full Rust `target`, and `archive` / `checksum` filenames that
reference entries in `assets`. For example:

```json
{
  "os": "linux",
  "arch": "amd64",
  "target": "x86_64-unknown-linux-gnu",
  "archive": "proofstorm-0.1.0-alpha.4-linux-amd64.tar.gz",
  "checksum": "proofstorm-0.1.0-alpha.4-linux-amd64.tar.gz.sha256"
}
```

The Mac entry uses `os: "macos"`, `arch: "arm64"`, and
`target: "aarch64-apple-darwin"`. Sites should read filenames from the manifest,
not reconstruct them from compiler targets.

### Assets

`assets` inventories the six other release downloads: the shared installer, two
platform archives, their two checksum sidecars, and the verification reports
archive. Every value has:

- `size_bytes`: a non-negative integer, measuring the actual downloadable file
  (compressed size for archives).
- `sha256`: exactly 64 lowercase hexadecimal characters, hashing those same bytes.

Keys are filenames only, never local paths or URLs. The installer entry hashes the
exact script tested on both platforms and matched against the selected commit.
Checksum sidecar entries hash the sidecar files themselves; archive entries hash
the archives.

`release.json` deliberately does not inventory itself: a file cannot contain its
own final hash. Its hash is held in the private promotion receipt and checked
before upload and after downloading the draft assets again, like every other
asset. Generation is deterministic and adds no timestamps or machine-local paths.

## Using it on the site

Select a published release from the trusted Proofstorm repository, then fetch its
`release.json` asset. Check that `repository`, `tag`, and `version` match the
selected release. Build download links using the trusted repository, that exact
tag, and the listed filenames:

```text
https://github.com/OWNER/REPO/releases/download/TAG/FILENAME
```

Use `installer` for the installation script and each platform's `archive` for
manual downloads. Each archive already includes the CLI, MCP executable, embedded
UI, and supporting files; they do not need separate website downloads.

Earlier releases may not have this asset. Do not infer their inventories from
release-note prose or silently mix files from different versions. This manifest
describes downloads and integrity, not fresh-host acceptance or an independent
signature. Keep runtime, signing, and GUI acceptance checks separate.

See [Preparing an alpha release](../scripts/RELEASING.md) for the maintainer flow.
