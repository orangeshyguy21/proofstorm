# One workflow: legacy consolidation plan

Status: proposed; repository audit complete, implementation not started.
Audited 2026-09-11 at `0be104b` on `polish-release`, including current working-tree entry points.

## Outcome

Proofstorm should have one runtime lifecycle, one agent-attachment path, one GUI
launcher, and one release pipeline. Development changes where artifacts come
from, not how the product operates. Tests use the same lifecycle with disposable
installation homes; they must not borrow the developer's running cluster.

Delete superseded workflows rather than translating every old script. Preserve
useful assertions, not historical command compatibility. No migration of old
cells, benchmark runs, or experimental workflows is required.

This plan does not authorize further machine cleanup, image publication, model
sessions, or Git history rewriting. The old local `proofstorm` cluster and its
registry have already been retired; the current checkout runtime stays intact.

## What the audit found

The tracked checkout contains 72 files under `scripts/`, with 45 Python files,
48 shell scripts, and one `.mjs` file across the repository. These are inventory
counts, not deletion targets: several Python drivers are part of the product.
No files under `dev/` were tracked before this plan; historical run output is
already ignored and is not repository bloat. Keep it that way.

| ID | Flow and evidence | Disposition |
| --- | --- | --- |
| A | [Compose wallet stack](../compose.yml), [regtest stack](../compose.regtest.yml), [Make wrapper](../Makefile.compose), `just compose` | Delete after mapping the few unique test assertions. This also removes the remaining Make dependency from the just recipes. |
| B | [Legacy just recipes](../justfile): `cluster-up`, `down`, `images`, `images-build`, `bitcoin-image-build`, `legacy-gate-build`; [fixed cluster config](../infra/k3d/proofstorm.yaml) | Retire. They can recreate the cluster just removed, reuse port 5111, and operate outside installation ownership. |
| C | [Acceptance runner](../crates/proofstorm-acceptance/src/bin/proofstorm-acceptance.rs), [gate context](../crates/proofstorm-acceptance/src/gate.rs), [kubectl wrapper](../crates/proofstorm-acceptance/src/kubectl.rs), [image provisioner](../crates/proofstorm-acceptance/src/images.rs) | Consolidate, not delete the suite. Most gates hard-code `k3d-proofstorm`; `--home` currently works only for images, image checks, and cluster-schema checks. |
| D | [Raw `serve --replace`](../crates/proofstorm-app/src/main.rs), [process replacement](../crates/proofstorm-app/src/server_restart.rs) | Remove the alternate user-facing GUI lifecycle. Use managed `gui`, `gui --no-open`, and `stop`. Keep the HTTP implementation and transport tests shared by managed GUI. |
| E | [Default environment](../crates/proofstorm-app/src/config.rs), [three OpenCode profiles](../examples/opencode/README.md), [MCP startup](../crates/proofstorm-mcp/src/main.rs) | Remove implicit connected-mode fallback to a CWD database and the fixed cluster. Delete obsolete profiles after retiring their benchmark/doctor callers. They are identical, name the server `pst`, and reference checkout binaries. Keep generated `proofstorm` attachments and explicit test identities. |
| F | [Python release implementation](../scripts/release.py), [Linux shim](../scripts/linux_container.py), [old controller builder](../scripts/controller_release.py) and their tests | Delete in dependency order after moving any unmatched negative/security fixtures to existing Rust/Bash tests. Current normal build, controller, relocation, and promotion paths already have Rust/Bash implementations. |
| G | [Image publisher](../scripts/publish_images.py), [AMD64 one-off publisher](../scripts/publish_linux_images.py), [Mac-only pin resolver](../scripts/bootstrap_pins.py), [maintainer tool downloader](../tools/install-host-tools.sh) | Consolidate selectively. Controller CI is not a replacement for every catalog-image publication operation. Preserve digest/platform/publisher verification before removing these tools. |
| H | [Agent benchmark runner](../scripts/run-agent-usability-benchmark.sh), [suite](../scripts/run-agent-usability-suite.sh), [cluster helper](../scripts/agent-usability-cluster.py), `prepare/seed/evaluate-agent-*`, private-handoff/ecash campaign scripts, [proxy](../scripts/native-execution-proxy.py), [argument-audit plugin](../scripts/private-transfer-argument-audit.mjs) | Retire the historical campaign framework by default. It depends on old profiles, checkout paths, fixed-cluster assumptions, and local scenario/run material. Preserve only useful product regression assertions in Rust. Do not replace it with another general benchmark platform in this pass. |
| I | [Installed smoke](../scripts/test_installed_setup.py), [checkout smoke](../scripts/test_checkout.py), [controller retry](../scripts/test_checkout_controller.py), GUI/attachment/progress/isolation Python tests | Consolidate into a shared installation-aware acceptance harness. These contain valuable current checks; do not delete them merely because they are Python or absent from default CI. |
| J | [Release README](../release/README.md), [Linux notes](../release/linux.md), [Mac notes](../release/macos.md), [old scenario instructions](../scenarios/README.md), [original spec](../SPEC.md), historical verification JSON | Make the root README and `scripts/DEVELOPMENT.md`, `scripts/CHECKS.md`, `scripts/RELEASING.md` authoritative. Remove obsolete instructions; retain dated evidence separately from current requirements. Audit each JSON consumer before moving/removing it. |
| K | Older Dockerfiles/configs and [CDK config contract](../tests/cdk18-config-contract.sh); [old light logo](../crates/proofstorm-web/assets/proofstorm-logo-light.svg) | Leaf cleanup after consumer checks. Several files are Compose-only candidates; the old logo has no reference in the searched active source. Do not delete entire `docker/`, `tests/`, or asset directories. |
| L | New, untracked [website update workflow](../.github/workflows/update-site.yml) | Not legacy: concurrent work. Coordinate with its owner. Prefer thin Bash orchestration and reusable Rust validation over adding a new inline Python release implementation. Do not delete or edit it as part of this audit. |

### Important dependencies and exceptions

- The Compose regtest defaults still pull from `localhost:5111`, the retired
  registry. They are not a supported fallback for the current workflow.
- The installed smoke helper derives `k3d-pst-` plus 28 ID characters and checks
  the old development context. It must consume saved installation metadata
  instead. This overlaps the in-flight shorter-container-name change.
- `release.py` is still imported by old controller/publication tooling and its
  own tests. Removing the shim alone does not remove this dependency family.
- The three OpenCode profiles are inputs to campaign preparation and the old
  acceptance doctor. Removing them requires removing or changing those callers.
- `tests/cdk18-config-contract.sh` uses the Compose mint configs. Decide whether
  its entrypoint/config assertions also protect shipped images before deletion.
- `proofstorm-registry.localhost:5000` also remains a **logical catalog image
  namespace** routed to each installation's registry. Do not globally replace
  it just because the identically named physical container is gone.
- Runtime Python under `proofstorm-kube/drivers/`, acceptance drivers, and
  Nutshell telemetry is actively included/called from Rust. It is not dead
  host orchestration. A later language-reduction project can assess it separately.
- Keep `--allow-development`, old published archive-name support, and version-1
  installation loading. These protect real artifacts/state, not obsolete dev flows.
- Keep charts, CRDs, schemas, catalog provenance, `release/ghcr.json`, bootstrap
  tool manifests, and currently compiled controller receipts. Generated files
  and JSON are not automatically disposable.
- Keep Docker Engine, Buildx, k3d, and Rust/WASM UI builds. Removing Compose is
  not removing Docker or Kubernetes. Release/install isolation is a legitimate
  distribution test, not another product runtime to collapse into dev setup.

## Execution plan

Each slice is a reviewable PR. Update affected instructions and tests in that PR;
do not wait until the end to fix broken references. The command names below are
the target interface, not commands implemented by this document.

### 1. Give live tests an owned installation

Scope: C and the shared lifecycle needed by I.

- Refactor `GateContext`, MCP spawning, and kubectl selection around an explicit
  `Installation`, verified CLI/MCP artifacts, and a unique run identity.
- Make `just e2e smoke` prepare a disposable home using the ordinary setup path.
  Extend the same runner to named gates; no implicit use of the user's dev home.
- Reuse installation image provisioning rather than maintaining a second
  catalog downloader in acceptance. Keep genuinely necessary diagnostic checks.
- Centralize owned runtime teardown in Rust, reusing saved resource IDs and full
  ownership identity. The current product CLI has no runtime-reset command;
  do not document one as already available. Add an explicit runtime-reset command
  only as the public entry point to this same implementation, not a test-only
  competing cleanup engine.
- Remove default context/port/name derivation from live-test helpers. Preserve
  per-test actor scopes and revocation tests; do not give every session full access.

Acceptance: hermetic tests refuse wrong/missing homes, foreign/replaced resource
IDs, changed kubeconfig, collisions, and inherited overrides. A disposable live
smoke must create/read/delete one cell and remove only its own runtime. Existing
dev containers, configuration, and storage remain unchanged. Interrupted setup
and cleanup retain accurate failure evidence and are safely retryable.

### 2. Delete the old entry points and campaign scaffolding

Scope: B, D, E, H; update C's old doctor/launch consumers first.

- Delete fixed-cluster recipes/config and the old image-restoration wrappers.
  Keep `e2e` only on the new installation-aware path.
- Remove raw user-facing `serve --replace` and its process-discovery module;
  retain shared HTTP handlers and managed server ownership checks.
- Delete the three static OpenCode profiles and outdated copy/paste startup
  instructions. `proofstorm open/attach` owns configuration and the MCP name.
- Retire benchmark suite/campaign commands, their proxy/plugin, preparations,
  seed/evaluation helpers, and tests that exist solely for that retired framework.
  Record any still-relevant security assertion and its Rust destination first.
- Stop implicitly selecting `k3d-proofstorm` in connected mode. Keep offline and
  in-memory test modes. If explicit external-cluster support remains, require
  explicit context/kubeconfig selection and keep it out of the alpha happy path;
  do not silently remove that separate capability without a decision.

Acceptance: current setup, GUI reuse/stop, and agent-attachment tests pass. Missing
installation selection cannot contact a legacy/global cluster. Fresh-clone docs
contain no runnable instructions requiring local ignored benchmark files.
No operation edits users' existing personal agent configuration during cleanup.

### 3. Remove Compose and Make without discarding useful tests

Scope: A and Compose-only leaves in K. Depends on slice 1.

Before deleting the old assertions, complete this small coverage ledger:

| Old assertion | Current candidate | Required decision/check |
| --- | --- | --- |
| Wallet funding, swaps, balance conservation | `cross_implementation_wallet`, `cdk_wallet`, `slice5` gates | Compare fees, expected totals, and failure handling; overlapping names do not establish equivalent coverage. |
| Same-token sequential replay rejected | Existing wallet/authentication tests | Authentication-token replay is not Cashu proof replay. Add a dedicated Rust acceptance case if the latter is missing. |
| Concurrent same-proof spend admits exactly one | Native execution and wallet tests | Command idempotency is not a double-spend race oracle. Preserve/add the real race assertion if missing. |
| Quote flood preserves honest-client service | Nutshell OIDC rate-limit tests | Different endpoint and property; either retain a bounded dedicated test or explicitly retire this experimental coverage. |

- Delete `compose.yml`, `compose.regtest.yml`, `Makefile.compose`, and `just compose`.
- Remove Compose-only `.env.example` variables, `.proofstorm-active` handling,
  `scripts/lib/wallet.sh`, funding/balance/watch/smoke/wait helpers, `regtest/`
  orchestration, and shell scenario launchers once their consumers/assertions
  have been classified. Do not delete any user's ignored `.env` or output files.
- Remove Dockerfiles and mint configs proven exclusive to those stacks. Preserve
  shared CDK entrypoint behavior until its shipped consumers are accounted for.
- Rewrite dispatch tests to assert the old recipes are absent. A fresh-clone
  check must not require Make or Compose.

Acceptance: `just check` passes; named replacement oracles have real live evidence
or an explicit retirement record. No Docker Compose calls or Compose-only fixed
container names remain in executable maintained paths. No broad Docker cleanup.

### 4. Consolidate image and tool-pin maintenance

Scope: G, with the website workflow owner handling L separately or by agreement.

- Give catalog image rebuild/mirror/publication operations one maintainer path
  using thin Bash and typed Rust helpers. Reuse existing registry identity checks.
  This is a prerequisite for deleting publishers that still provide unique work.
- Remove AMD64 development-publication special cases and dependence on a registry
  at port 5111. Inputs are explicit source snapshots, image references, platforms,
  and verified receipts; publication stays an explicit authenticated action.
- Replace the Mac-only pin-generation helper with a platform-aware Rust command.
  Share pin validation across setup and maintainer tools; keep separate download
  destinations when their owners and purposes differ.
- Keep actual toolchain installation (Trunk/Rust targets) distinct from installing
  runtime helpers; it is not a second product workflow.

Acceptance: Linux AMD64 and Linux ARM64 image receipts and both host tool manifests
validate. Wrong architecture, changed digest/tag, missing layers, and wrong
namespace still fail. No existing catalog/provenance hash is silently rewritten.
No new Python/Node requirement for first-time installation or normal build/release.

### 5. Finish removing the superseded release implementations

Scope: F, after resolving publication callers in slice 4.

- Remove `linux_container.py` and its delegation-only tests first.
- Compare Python release/install/controller negative cases with existing Rust
  archive, packaging, smoke, controller, and Bash installer tests. Port missing
  fixtures, not the old implementation.
- Replace callers of `controller_release.py` with the existing controller flow.
- Remove `release.py`, old controller tooling, and their implementation-specific
  tests only after no maintained import/caller remains.
- Keep one documented command family: `just release-prepare`, `just release`, and
  the existing lower-level `release-*` diagnostics. Do not add another wrapper CLI.

Acceptance: local quick/Rust checks and both platform bundle/installer lanes pass.
Retain traversal/symlink/oversize rejection, checksums, source/target/controller
binding, anonymous image verification, reinstall safety, and upload/download
verification. The seven release assets and `release.json` contract stay unchanged.
No publication is performed merely to validate a cleanup PR.

### 6. Unify remaining smoke checks and finish documentation hygiene

Scope: I, J, remaining K.

- Parameterize one installed-runtime smoke suite by artifact source (checkout or
  release). Reuse the Rust MCP client and lifecycle/cleanup from slice 1 instead
  of several custom Python protocol clients and cleanup implementations.
- Keep GUI/native desktop checks, CLI progress/PTY checks, and source-free
  installer isolation as distinct test kinds with shared helpers where useful.
  A headless HTTP response is not a visual/native-app pass.
- Retire the Python live-smoke scripts only when their meaningful assertions have
  a maintained destination. Optional real-model tests stay opt-in and bounded.
- Replace `release/README.md`'s historical procedure narrative with a short index
  pointing to `scripts/RELEASING.md`. Date historical evidence; don't rewrite it
  to claim current acceptance. Remove stale Make/profile/alpha-unpublished advice.
- Remove unreferenced asset/config leaves. Keep provenance/test fixtures that
  have live consumers. Do not edit the concurrent branding or site changes blindly.

Acceptance: documentation links and documented commands resolve from a fresh
checkout; Linux and macOS instructions agree with the selected published release.
Each surviving script has a clear caller, purpose, test lane, and owner. Historical
run output stays ignored; no mass `git add -f dev` or history rewrite.

## CI and merge rules

Keep feedback ordered: fast static checks, hermetic Rust tests, then expensive
live/build checks. Do not add Docker startup or model calls to `check-quick`.

1. Add a small workflow-contract check to the quick lane as each path is retired:
   obsolete recipes absent, no imports of deleted scripts, no new fixed-cluster
   defaults, and no undocumented second setup/GUI path. Use narrow, documented
   exceptions for negative fixtures and logical catalog names; never ban every
   occurrence of words like `compose`, `legacy`, or `python`.
2. Run `just check` before each merge. The static audit did not run tests or prove
   feature parity. Preserve all current CI jobs; don't make CI green by dropping
   security/compatibility assertions or converting failures into skips.
3. Live gates run serially in owned disposable environments. Reports distinguish
   not run, unsupported, failed, and passed. PR checks remain non-publishing; real
   model calls require a separate explicit request and budget.
4. Verify Linux and macOS artifact lanes after relevant main merges. Do not publish
   an alpha until the unchanged release gates and intended runtime checks pass.
5. Keep the in-flight naming, README branding, and site workflow edits separate
   from cleanup commits unless deliberately combined and reverified.

## Definition of done

- No Compose or Make dependency in maintained Proofstorm workflows.
- No commands recreate or implicitly target the retired shared cluster.
- One managed GUI launcher and one generated agent-attachment path.
- Live tests exercise the same runtime lifecycle as installed users, with their
  own home and verified cleanup; checkout state is not adopted or destroyed.
- One authoritative Bash/Rust build and release implementation; unique catalog
  publication and pin maintenance remain supported through that tooling.
- Every removed test assertion has a replacement or an explicit retirement
  reason. Active runtime drivers and published-install compatibility remain intact.
- Fewer operational entry points and clearer docs, not a new framework replacing
  each old framework. Git history is sufficient recovery for deleted source.

Recommended first implementation slice: **owned live-test setup and teardown**.
It unlocks deletion of the fixed cluster, Compose harness, and duplicate smoke
cleanup without sacrificing the checks that protect the alpha.
