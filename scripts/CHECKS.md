# Checks

`just check` is the local and CI code gate: quick checks first, then strict Rust
lints and all workspace tests. It never starts Docker, opens an app, changes agent
configuration, or publishes anything.

The separate [merge qualification lane](../docs/merge-qualification.md) exercises
the supported catalog on native AMD64 and ARM64. It runs within `Checks`, reports
through `Merge qualification`, and includes exact-image receipts, payment and
persistence scenarios, cleanup verification and documented upstream exclusions.

## Prerequisites

Rust/rustup (pinned by `rust-toolchain.toml`), a native C toolchain, Git, Bash,
just, jq, and ShellCheck. Install just, jq, and ShellCheck through your package manager.
The check script does not install tools. Cargo may download dependencies on its
first run. Some tests need loopback listeners, child processes, and a pseudo-terminal;
a sandbox denying these cannot run the complete suite.

Python, Node, Docker, Helm, and Trunk are not code-check dependencies. Checks use
`target/check`, separate from registered development binaries and GUI assets.
The workspace lint/test gate explicitly enables `proofstorm-prober/runtime`, so
the shipped worker and its DNS/TCP/HTTP regressions are included in local and CI
checks. Library-only consumers can still omit the worker runtime dependencies.

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

Qualification flow tests run the real planner, matrix, verifier and shard script
with synthetic receipts and a stub acceptance process. They check failure
propagation and artifact handling; live payment evidence comes from the native
hosted qualification jobs.

The workflow guard rejects retired operational files, owned Python source, and
Python execution in maintained adapters, acceptance fixtures and tooling.
Historical evidence and explicit fixtures proving Python is absent remain allowed.
Upstream Nutshell retains its own runtime; Proofstorm calls its installed console
commands through the Rust driver. The logical
`proofstorm-registry.localhost:5000` image namespace remains valid.

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
| `runtime-lifecycle` | Three CLI stop/start cycles, funded Bitcoin wallet/chain and volume identity preservation, active-work admission, persistent MCP suspension/reconnection, GUI shutdown, individually stopped component, interrupted startup retry |
| `onboarding` (first gate) | Prepare-only twice, setup retry/controller reuse, exact on-demand image selection through CLI/MCP, ready cells, GUI stop preserving workloads |
| `gui` | Embedded HTTP, authentication/origin checks, reuse, stale-record recovery and session rotation; no browser or native app launch |
| `agent-config` | Private project/home fixtures for all three adapters, actual MCP probe, dry-run, backups, repeat, conflict/revocation and ambient-override protection |
| `agent-clients` | Opt-in installed OpenCode/Claude MCP discovery in private homes; prerequisites must already exist; no installs, models or trust bypass |
| `cli-progress` | Actual setup/GUI commands through a PTY, prompt animated feedback, clear line, human summaries and clean explicit JSON |
| `installation-isolation` | Two owned installations pull their own fixture image and reject the other's digest; no external registry upload |
| `slice5` | Native wallet funding, self-swap, invoice, payment and claim; exact fee accounting from separate wallet and mint observations; deterministic receipts and teardown |
| `quote-composition` | Native wallet and external Lightning payments to independent quotes, exact claim state, recipient balances and selected receipt privacy |
| `failed-melt` | Unroutable native payment leaves mint and wallet quotes unpaid, funds unspent and recipient empty; exported observations match |
| `ldk-server-processor` | CDK gRPC mutual TLS, funded BOLT11 mint/melt, unpaid quote recovery after processor restart, persistent node identity and BOLT12 payment recognition |
| `cross-implementation-wallet` | Native Nutshell wallet interaction with CDK and Nutshell mints, exact Nutshell fees, bounded CDK accounting without an unsupported fee claim, and Redis restart behavior |
| `cashu-double-spend` | Real CDK/Nutshell spent-proof replay/race and exact zero-fee 64-sat accounting |
| `channel-lifecycle` | Native Bitcoin/LND/CLN funding, peer connectivity, exact channel outpoints, circular payment balances, cooperative/force-close transactions, retry identity and exported receipts |
| `controller-recovery` | Supervised native execution survives controller restart without replay; lost/cancelled probes and component stop/start/restart remain covered |
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

`just test-native-supervisor` runs the Rust Linux execution-helper contracts.
It builds a Rust child-process fixture behind the `contract-tests` feature; that
fixture is absent from production builds. The tests create local process/private-I/O
fixtures, including descendants that escape their session, without cells or funds.
Set `PROOFSTORM_NATIVE_RUNNER` only to test an explicit trusted matching helper.

`just audit-mcp /absolute/path/to/proofstorm-mcp` runs the offline discovery and
planner-size diagnostic in Rust. It starts the explicitly selected trusted MCP
binary with a fresh temporary database and stripped runtime/authority overrides,
records request/response sizes and persisted drafts, and reaps the child even on
failure. It never materializes a cell. This replaces the exploratory
`dev/mcp-agent-audit-probes.py` host script.

`cargo test -p proofstorm-driver` covers passive wallet transactions, bounded
HTTP and Unix RPC, private rune handling, and readiness proxy/redirect isolation.
The transport tests use local sockets; they need permission to bind listeners.
They do not by themselves establish live mint quota isolation.

`just check-component-driver COMPONENT DRIVER_IMAGE COMPONENT_IMAGE PLATFORM`
runs the CDK, Coco or Nutshell contract against explicit local images. Each
contract runs with external networking disabled and a total deadline. These
cover native startup, Coco initialization and locked restart, and actual Nutshell
readiness/TLS and application quota preservation. See the
[native driver validation record](../crates/proofstorm-driver/VALIDATION.md) for
the verified boundaries and remaining release checks.

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
| `proofstorm-exec/tests/supervisor.rs` | Linux execution helper; `just test-native-supervisor` |
| `proofstorm-driver/tests` | Native protocol and passive wallet contracts; Rust workspace gate |
| `proofstorm-acceptance/examples/mcp_agent_audit.rs` | Offline MCP diagnostic; `just audit-mcp` |

Owned integration and test code is moving to Rust, with Bash/Just for tool
sequencing. A component's implementation language does not justify an additional
Proofstorm runtime or SDK dependency. Upstream Nutshell's own interpreter remains
part of that component; remaining owned Python drivers must be replaced while
preserving their protocol, privacy and failure contracts. See the driver crate's
README for migration status and outstanding validation.
