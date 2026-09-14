# Managed self-update contract (v1)

Only `proofstorm update` (alias `upgrade`) contacts
`https://proofstorm.com/release.json`. `--check` performs discovery without writing
installation/runtime files. There is no background check, channel switch,
automatic downgrade, runtime migration, or rollback operation.

## Website feed

This is a separate schema from the GitHub release asset manifest. Schema 1 has:

- `schema_version: 1`, `repository: "orangeshyguy21/proofstorm"`, positive
  `release_id`, canonical semantic `version`, exact `tag: "v<version>"`,
  `channel: "alpha" | "release"`, and `published_at`.
- `installer: {name: "install.sh", url, bytes, sha256}`.
- `platforms: [{id, archive: {name,url,bytes,sha256}, checksum: {...}}]`,
  with unique `linux-amd64` / `macos-arm64` IDs. Archive names are
  `proofstorm-<version>-<id>.tar.gz`; checksum names append `.sha256`.

Additional fields are ignored. The installed channel must match the feed. Alpha
versions use numeric `alpha.N` identifiers; comparison uses semantic precedence.
Asset URLs must match the exact repository/tag/name on HTTPS GitHub. HTTPS
redirects are limited to GitHub's release delivery hosts and the official site.
No remote value is evaluated as shell syntax. The feed is limited to 64 KiB,
installers to 1 MiB, checksums to 4 KiB, and archives to 512 MiB. Download time,
redirect count, retries, and child output are bounded.

The updater captures one feed response, downloads all three selected assets,
and verifies their advertised lengths and SHA-256 values. The existing installer
also checks the archive against its checksum sidecar. These hashes share the
existing HTTPS release trust model; they are not independent release signatures.

## Installer interface: preserve in every subsequent release

The verified versioned script must accept these literal arguments together:

```text
--prefix ABSOLUTE_PREFIX
--version SELECTED_VERSION
--archive SELECTED_ARCHIVE_NAME
--artifact-dir PRIVATE_DOWNLOAD_DIRECTORY
--expected-sha256 ARCHIVE_SHA256
--expected-bytes ARCHIVE_LENGTH
--expected-current OBSERVED_BUNDLE_ID
--report-json
```

`--expected-current` reaches the new bundle's `internal install-bundle` command.
Under the installation lock it must still equal `current` before any activation.
The parent never holds that lock while awaiting the installer. The new binary
verifies and installs its own embedded release metadata. All legacy installer
arguments remain valid.

With `--report-json`, stdout contains one JSON receipt with `installed: true`,
`version`, canonical `prefix`, and the activated manifest hash in `bundle_id`.
Errors exit nonzero; diagnostics go to stderr. Primary launcher ownership is
checked before activation. Old version directories remain intact.

## Result and recovery

The CLI emits result schema 1. Status is `update_available`, `up_to_date`,
`installed_newer`, `updated`, `failed`, or `activated_with_error`. Fields include
previous/available/installed versions, channel/platform/prefix, selected release
ID, `activation_changed`, `activation_observed`, `verification_passed`,
`runtime_refreshed` (always false in v1), `required_actions`, and optional `error`
with a stage code and readable message. Exit status is nonzero for failures.
`activation_observed: false` means activation could not be fully inspected; do
not interpret it as proof that nothing changed.

After the installer ends—even after cancellation or failure—the updater inspects
activation and verifies both installed CLI/MCP metadata against the bundle.
The receipt must identify that exact bundle. A same-version invocation verifies
bundle and launcher health before reporting `up_to_date`; missing launchers
trigger installer repair. Corrupted or foreign files may require explicit
recovery through the official installer and are never reported healthy.

## Runtime and process boundary

The installation prefix is derived from the canonical running managed executable
and owner record. `PROOFSTORM_HOME` selects runtime advice, never executable
installation. File updates do not open the shared database, call setup, restart
GUI/MCP processes, or change cells, grants, or agent configuration. Follow-up
commands retain the selected home and prefix.

A managed client checks its recorded controller image before opening shared
state; attached MCP additionally checks actual runtime compatibility before
opening the database. These checks refuse mismatches, not migrate them. Existing
old processes continue using retained files until stopped. Disconnect old MCP
sessions and stop the GUI before explicit `setup`, then reconnect/start them.
The passive deployment receipt is a version gate, not a runtime health result.

Coordinated process refresh remains separate work. The first release cannot
retrofit safeguards into already-running older binaries. Packaging checks and
cross-version validation are described in [RELEASING.md](../scripts/RELEASING.md).
