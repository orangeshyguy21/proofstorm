# Checks

`just check` is the local and CI code gate: quick checks first, then strict Rust
lints and all workspace tests. It never starts Docker, opens an app, changes agent
configuration, or publishes anything.

## Prerequisites

Rust/rustup (pinned by `rust-toolchain.toml`), a native C toolchain, Git, Bash,
just, and ShellCheck. Install just and ShellCheck through your package manager.
The check script does not install tools. Cargo may download dependencies on its
first run. Some tests need loopback listeners, child processes, and a pseudo-terminal;
a sandbox denying these cannot run the complete suite.

Python, Node, Docker, Helm, and Trunk are not code-check dependencies. Checks use
`target/check`, separate from registered development binaries and GUI assets.

| Command | Purpose |
| --- | --- |
| `just check` | Quick checks, strict workspace Clippy, hermetic workspace tests |
| `just check-quick` | Recipe/wrapper contracts, retired-path guard, formatting, shell syntax and scoped ShellCheck |
| `just check-rust` | Strict workspace Clippy, then tests with `--no-fail-fast` |
| `just test` | Hermetic workspace tests |
| `just lint` | Quick checks and strict Clippy |
| `just lint-helm` | Opt-in chart validation using pinned Helm |
| `just check-cdk-config` | Opt-in generated CDK config/initializer checks with pinned images and networking disabled |

Tests cover CLI grammar/help, human and JSON output, PTY animation without an
elapsed timer, MCP compatibility, authorization/revocation, generated config
conflicts/backups, controller rendering, and owned runtime/GUI cleanup.
Both AMD64 and ARM64 catalog snapshots and Kubernetes goldens are checked on
either host. After an intentional rendering change, regenerate with
`UPDATE_GOLDENS=1 cargo test --locked -p proofstorm-kube --test golden_rendering`
and review the diff.

Bash fixtures use fake commands in temporary directories. Rust tooling tests use
real validators, archives, and disposable Git fixtures—not real publication.
They cover quoting, failure propagation, dirty/stale source, platform/controller
binding, traversal/symlinks/size limits, altered hashes/modes, installer conflicts,
reinstall, and exact upload/download evidence. Installer negative tests run the
real shell installer with compiler/network stubs. These replace the old Python
release implementation and its duplicate fixtures.

The narrow workflow guard rejects retired operational files and executable
references to them. It does not ban Python drivers, historical evidence, negative
fixtures, or the logical `proofstorm-registry.localhost:5000` image namespace.

## Live and manual checks

```sh
just e2e                         # Small Bitcoin smoke; fresh runtime
just e2e onboarding agent-config cli-progress
just e2e installation-isolation  # Two fresh runtimes; registry separation
just e2e cashu-double-spend
just e2e --list                  # All named gates
just e2e-bundle /absolute/unpacked/bundle onboarding agent-config cli-progress
just e2e-cleanup /absolute/path/to/retained/run
```

Checkout and bundle inputs share the same Rust runner, MCP client, ordinary setup,
and receipt-checked teardown. Bundle tests do not rebuild checkout artifacts.
Add `--allow-development` only for an explicitly selected development bundle.
`--work-dir` chooses a new absolute evidence directory; `--timeout` bounds each
gate. Run live gates serially and avoid concurrent Docker/config edits: preservation
drift is a failure, not something the runner repairs.

| Gate or lane | Evidence / boundary |
| --- | --- |
| `smoke` | Scoped MCP Bitcoin creation/read/deletion, denied mutation, same-database CLI read-back |
| `onboarding` (first gate) | Prepare-only twice, setup retry/controller reuse, exact on-demand image selection through CLI/MCP, ready cells, GUI stop preserving workloads |
| `gui` | Embedded HTTP, authentication/origin checks, reuse, stale-record recovery and session rotation; no browser or native app launch |
| `agent-config` | Private project/home fixtures for all three adapters, actual MCP probe, dry-run, backups, repeat, conflict/revocation and ambient-override protection |
| `agent-clients` | Opt-in installed OpenCode/Claude MCP discovery in private homes; prerequisites must already exist; no installs, models or trust bypass |
| `cli-progress` | Actual setup/GUI commands through a PTY, prompt animated feedback, clear line, human summaries and clean explicit JSON |
| `installation-isolation` | Two owned installations pull their own fixture image and reject the other's digest; no external registry upload |
| `cashu-double-spend` | Real CDK/Nutshell spent-proof replay/race and exact zero-fee 64-sat accounting |
| Source-free installation | Separate Linux container / Mac sandbox checks below; does not establish runtime or public download readiness |
| Browser/native app | Manual desktop session: default-browser reuse, folder picker/cancel, vendor buttons, project handoff and MCP status; record prompts and limitations |
| Real model | Separately authorized session, actual tool-call response and cell read-back; never inferred from MCP discovery |

Reports explicitly distinguish these boundaries. Test state/logs remain in the
printed private directory; only recorded runtime containers/network/storage are
removed. A forced kill before setup writes its resource receipt requires manual
inspection, not adoption of a discovered cluster. Cleanup retries use the retained
run identity and stop only that run's GUI.

The client gate records exact private discovery output. OpenCode may add its
standard schema annotation when reading a config; the test allows only that
annotation/formatting and still rejects any changed MCP entry. Revocation checks
compare the bytes immediately before and after Proofstorm's refused operation.
Claude may report a discovered project server as `pending_approval`; the report
keeps that distinct from `connected`. The gate never approves it for the user.

For Linux execution-helper contracts, the retained
`tests/native_supervisor_contract.py` is an opt-in diagnostic:
`PROOFSTORM_NATIVE_RUNNER=/absolute/path/to/proofstorm-exec python3 -m unittest discover -s tests -p native_supervisor_contract.py`.
Run on Linux with a trusted matching helper. This creates local process/private-I/O
fixtures, not cells or live funds; it is not a normal build dependency.

## Distribution diagnostics

The normal release flow is [RELEASING.md](RELEASING.md). These lower-level commands
are for maintainers, not first-time installers. Build inputs must be trusted:
packaging and relocation execute the selected CLI/MCP binaries.

| Command | Purpose |
| --- | --- |
| `just release-check FILE [--alpha] [--json]` | Offline typed release metadata validation; no runtime or readiness claim |
| `just release-verify DIRECTORY [--json]` | Unpacked inventory, file modes/hashes, metadata and source/controller binding |
| `just release-extract ARCHIVE NEW_DESTINATION` | Adjacent checksum, bounded safe extraction and bundle verification |
| `just release-smoke ARCHIVE NEW_DESTINATION [--deny-source DIRECTORY] [--json]` | Execute trusted relocated binaries after verification; source-denial option is Mac-only |
| `just release-package SOURCE BINARIES PROVENANCE_JSON OUTPUT [--development] [--json]` | Assemble, verify and archive trusted build outputs |
| `just release-pack DIRECTORY OUTPUT [--json]` | Archive a verified unpacked bundle |
| `just release-build --work-dir NEW_DIRECTORY --output DIRECTORY` | Isolated native source build; optional `--development --debug`, external `--target-dir`, matching `--controller-receipt` |
| `just release-build-linux --work-dir NEW_DIRECTORY` | Isolated AMD64 Debian build/relocation, no host mounts; optional development/debug/receipt |
| `just release-install-linux --archive FILE --installer FILE --work-dir NEW_DIRECTORY [--development]` | Non-root, source-free install/reinstall with networking disabled |
| `just release-install-macos --archive FILE --installer FILE --snapshot SOURCE_DIRECTORY --work-dir NEW_DIRECTORY` | Native Apple Silicon install/reinstall with verified source/network/compiler/write denial; no development bypass |
| `just release-ci-linux --work-dir NEW_DIRECTORY --controller-receipt FILE` | Matching Linux build + installer artifact lane |
| `just release-ci-macos --work-dir NEW_DIRECTORY --controller-receipt FILE` | Matching Mac build + installer artifact lane |
| `just release-controller-build --platform linux/amd64 --work-dir NEW_DIRECTORY` | Source-bound local controller build/probe; also supports `linux/arm64` |
| `just release-controller-publish --work-dir DIRECTORY --confirm-namespace ghcr.io/orangeshyguy21/proofstorm` | Explicit authenticated publication + anonymous verification |
| `just catalog-image list` | [Catalog image maintenance](../docker/README.md); publication requires explicit namespace confirmation |
| `just tool-pins TARGET NEW_OUTPUT` | Resolve official checksum-verified candidate pins; review before editing shipped manifests |

Work directories must be new and external to the checkout. Rust owns snapshots,
metadata, verification and receipts; Bash only sequences tools. Archive checks
reject unlisted/non-regular files, unsafe paths, invalid modes, corrupt/truncated
gzip and overstated readiness. Limits include 10,000 files, 1 GiB payload,
64 directory levels and 4 MiB metadata. Passing integrity is not authenticity or
fresh-host acceptance.

Main-only CI builds matching controllers and both platform bundles after code
checks. PRs run quick/Rust checks and the native Mac isolation contract; they do
not publish. Promotion requires both successful artifacts from the same commit
and run attempt, all six original reports, and exact installer bytes. It verifies
the seven downloads and generated `release.json` before and after draft upload.
No cleanup in this plan changes that release gate.

## Script ownership

| Surface / caller | Owner and lane |
| --- | --- |
| `install.sh` | Product distribution; installer shell/Rust fixtures and platform isolation lanes |
| `scripts/check.sh`, `test-just.sh`, `test-workflow-surface.sh` | Contributor tooling; `just check-quick` / CI |
| `scripts/develop.sh`, `tools/install-trunk.sh` | Checkout artifact selection; `just dev*` / `web*`, Rust+Bash fixtures |
| `scripts/acceptance.sh` | Live acceptance; `just e2e*`, wrapper fixtures + explicitly selected live gates |
| `scripts/release.sh`, `release-promote.sh` | Release preparation/promotion; `just release*`, Rust+Bash/GitHub-shaped fixtures |
| `scripts/release-build.sh`, `linux-build.sh`, `linux-build-worker.sh` | Bundle construction; native/container build drivers and integration fixtures |
| `scripts/ci-*-bundle.sh` | Main artifact orchestration; Checks workflow, quick sequencing fixtures |
| `scripts/{linux,macos}-install-{smoke,check}.sh` | Distribution isolation; artifact lanes, Rust+Bash fixtures and real platform sandbox |
| `scripts/controller-build.sh`, `catalog-image.sh` | Image maintenance; explicit maintainer/main CI calls, fake-registry/Docker tests |
| `tools/install-host-tools.sh` | Reviewed host helpers; `just tools` / `tool-pins`, shared Rust pin tests |
| Other `scripts/test-*.sh` | Fixtures for the matching wrapper; quick lane or `proofstorm-xtask/tests/wrapper.rs` |
| `tests/cdk18-config-contract.sh` | Generated CDK config contract; opt-in image-only lane |
| Acceptance and Kubernetes drivers | Their Rust `include_str!`/execution consumers; named live gates / backend tests |
| `tests/native_supervisor_contract.py` | Linux execution helper; opt-in command above |

Runtime Python drivers remain because the shipped product or explicit contracts
use them. Retiring host orchestration is not a blanket language rewrite.
