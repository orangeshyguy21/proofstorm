# One workflow: legacy consolidation plan

Status: all six coding slices implemented. Final local validation and the remaining
post-merge release-artifact gate are recorded below; hosted acceptance is not yet
complete.
Audited 2026-09-11 at `0be104b` on `polish-release`, including current working-tree entry points.
CLI/terminology baseline refreshed at `dfcea47` on `cleaning-house`; this refresh
checks the renamed interface and affected plan references, not a repeat of the full audit.

## Slice 1 implementation checkpoint

- `just e2e` / `just e2e smoke` now build verified checkout artifacts and run one
  Bitcoin MCP smoke gate in a new installation. Named gates share this runner;
  they do not inherit a development home, database, authority, or kubeconfig.
  CLI and MCP share the ordinary installation database, with scoped test actors.
- Setup is the normal CLI path. MCP mutations use installed image preparation;
  the raw-CRD `slice2` gate uses setup's explicit prefetch option. The legacy
  acceptance image restorer is no longer used by the live-gate runner.
- Rust setup records container IDs, Docker daemon identity, network identity,
  and volume creation metadata. Shared retirement checks for replaced resources,
  foreign network/volume users, and changed kubeconfig before deleting anything.
  Missing resources permit retries; missing receipts do not permit adoption.
- `just e2e-cleanup RUN_DIRECTORY` retries cleanup from retained run evidence.
  The internal CLI `runtime-delete --installation-id FULL_ID` uses the same
  implementation for the retained installed-bundle smoke script. It permanently
  retires that home, not the user's installation or an individual cell.
- Reports and private logs remain after cleanup. Before/after checks cover
  preexisting containers, networks, volumes, user configs, and selected checkout
  state. Image-cache growth is expected. Force-killing setup before it writes a
  resource receipt requires manual inspection, not automatic resource adoption.
- The installed smoke script uses current agent commands and saved runtime
  routing instead of deriving the old cluster name; its teardown now calls Rust.
- Validation: `just check` passed (quick checks, strict workspace Clippy, and
  hermetic workspace tests). The macOS `just e2e smoke` run passed setup, scoped
  MCP creation/read/deletion, CLI read-back from the same database, runtime
  cleanup, and preexisting-resource/configuration preservation. Evidence remains
  at `/private/var/folders/w7/t4mxkw6n48vd2dwyvdgrn73w0000gn/T/proofstorm-acceptance-WNPpfK/acceptance.json`.
  `just e2e-cleanup` also passed against that already-removed runtime. The quick
  lane now exercises the real Bash wrapper with stub Cargo, including its
  cleanup branch, to catch macOS Bash empty-array behavior without Docker.
  The retained Python installed-smoke script passed syntax validation but was
  not rerun against a bundle in this slice. Linux live validation, the full named
  gate suite, and release artifact lanes have not been run for these changes.

## Slice 2 implementation checkpoint

- Removed fixed-cluster recipes/config, the unused acceptance doctor/image
  restorer, and the acceptance kubectl helper's global-config fallback.
- Removed hidden `dev serve --replace` and process-discovery/replacement code.
  Managed GUI lifecycle, shared HTTP handlers and transport tests remain.
- Removed the three static OpenCode profiles and historical model-campaign
  scripts, proxy/plugin, scenario/seed fixtures and their framework-only tests.
  The assertion disposition ledger below distinguishes retired experiments from
  retained Rust product coverage. Ignored historical run data is untouched.
- Connected CLI/MCP startup now requires an installation or an explicit external
  context **and** kubeconfig. Missing selection fails before creating a database;
  offline/memory modes remain. No global kubeconfig fallback remains in this path.
- Updated affected launch docs and image-pull recovery advice to the current
  `storm` workflow. Added regressions for removed entry points, absent/partial
  runtime selection, private-path routing and no ambient-config adoption.
- Validation: `just check` passed on macOS: quick checks, strict workspace Clippy,
  and all hermetic workspace tests, including CLI/MCP selection refusal, managed
  GUI ownership/transport and agent-config conflict/backup contracts. Loopback
  access was enabled for local HTTP test fixtures. No Docker resources or
  personal agent configuration changed. Live runtime/GUI reuse against Docker,
  Linux, release-bundle and model/native-app tests were not rerun in this slice.

## Slice 3 implementation checkpoint

- Removed both Compose stacks, the Make wrapper/recipe, exclusive wallet and
  scenario helpers, regtest launchers, old image recipes/configs, and the
  Compose-only CDK entrypoint. These tracked files remain recoverable from Git;
  ignored `.env`, old run output and the checkout runtime are untouched.
- Added `just e2e cashu-double-spend` to the owned acceptance runner. It checks
  identical-token replay and two independent competing wallet processes against
  CDK and Nutshell, with explicit zero fees and exact 64-sat accounting. It reuses
  the shipped passive CDK balance observer: CLI balance can trigger recovery and
  mix recovery messages into its output. Tokens remain inside test storage.
- Retained the generated CDK configuration/initializer contract as
  `just check-cdk-config`, using exact public image digests and network-disabled
  containers. The quick lane exercises its dispatch and rejection paths with
  fake Docker; contributors do not need Make or Compose.
- The ledger below explicitly retires the FakeWallet population/conservation
  experiment and the broken quote-flood probe. Neither is silently represented
  as equivalent to existing real-Lightning or authentication tests.
- Validation: `just check`, `just check-cdk-config`, and
  `just e2e cashu-double-spend` passed on macOS. Both CDK and Nutshell rejected
  same-wallet/fresh-wallet replay, admitted exactly one competing redemption,
  and preserved the 64-sat total. The final run passed setup, cell/runtime/storage
  cleanup, and preservation of preexisting Docker resources and user configs.
  Evidence remains at
  `/private/var/folders/w7/t4mxkw6n48vd2dwyvdgrn73w0000gn/T/proofstorm-acceptance-Eo9uMG/acceptance.json`.
  Earlier fixture failures remain recorded, not relabeled as passes; their
  disposable runtimes were removed. The first attempt also detected a changed
  Claude config hash; it was not repaired or waived. The subsequent failed run
  and final successful run passed preservation. Linux live, other named gates,
  and release artifact lanes have not been rerun in this slice.

## Slice 4 implementation checkpoint

- Added `just catalog-image` for all six custom workload recipes, with explicit
  Linux AMD64/ARM64 selection, clean snapshots, immutable identities, restricted
  startup probes, and an explicit GHCR publication gate. Copies accept pinned
  GHCR or explicit loopback-registry sources; no port 5111 fallback remains.
- Controller and catalog publication share anonymous registry verification:
  hash-checked manifests/configs, descriptor/config platform agreement, layer
  availability, and config/manifest/index identity binding. Multi-platform copies
  preserve their digest; single-platform builds cannot acquire extra manifests.
- Receipts distinguish preparation, attempted upload, completed upload, and
  anonymous verification. Failed/moved-tag rechecks do not retain a current
  verified state. Uploads are not automatically retried or removed, and catalog,
  provenance, controller pins, running cells, and release assets are not edited.
- `just tools` and installed setup share Rust host-tool pin validation. Maintainer
  installs check existing executable hashes, payload hashes, and selected Helm
  members; linked/conflicting files are refused. `just tool-pins TARGET NEW_OUTPUT`
  writes a separately reviewed candidate from official checksum receipts.
- Removed seven tracked Python publisher/pin/controller implementation and test
  files. Their source is recoverable from Git. The old controller wrapper moved
  forward from slice 5 because it was the last caller of the removed registry
  publisher; the current controller flow and its tests remain.
- Updated affected image/pin/controller instructions to their Rust/Bash entry
  points. The website update workflow and historical JSON evidence are untouched.
- Validation: `just check` passed on macOS (quick checks, strict workspace
  Clippy, all hermetic workspace tests, including native installer isolation).
  The tooling suite includes 91 unit tests and seven Bash integration tests.
  Both real publisher-resolution runs succeeded; their candidate JSON exactly
  matches the reviewed Mac/Linux manifests. Candidates remain at
  `/private/tmp/proofstorm-tool-pin-check.FSiSQe/`; no tools or pins were replaced.
  The cached ARM64 CDK management image passed its offline version probe, which
  confirmed the management CLI reports `cdk-mint-rpc`. A Bitcoin probe could not
  run because that public image was not cached; no image was pulled for it.
  Catalog builds/pushes, other native image probes, Linux-host execution, live
  cells, and full release-artifact lanes were not run in this slice. Fixture
  publication tests do not substitute for those checks.

### Slice 4 assertion disposition

| Former assertions | Maintained destination / deliberate boundary |
| --- | --- |
| Pinned references, namespace confirmation, digest preservation, anonymous layers and required architectures | Shared Rust registry unit fixtures and real Rust/Bash catalog/controller integration with fake Docker/curl, covering both architectures and multi-platform copies |
| Local identity, native startup, non-root wallet/controller users, changed tags, partial upload receipts | Catalog/controller typed receipt tests and integration fixtures; failed pushes remain inspectable and are not retried automatically |
| AMD64-only three-image batch publication | Independent catalog-image receipts plus the existing controller flow; no cross-image atomicity is claimed |
| Old controller host-archive checksum, target and runtime-contract matching | Existing Rust archive, package, bundle and controller transport tests, including altered source/receipt and incompatible metadata |
| Mac-only publisher checksums and downloaded executables | Shared core pin validator plus Rust resolver/installer tests for both reviewed hosts, exact checksum rows, corrupt archives, links and install/reinstall |
| Historical reports / performance or live-cell acceptance | Retained as dated evidence, never synthesized from startup or publication checks |

## Slice 5 implementation checkpoint

- Removed the Python release implementation, Linux compatibility shim, their
  implementation/delegation tests, and the separate Python installer fixtures.
- Ported unmatched shell-installer boundaries into Rust integration tests:
  corrupt payload/checksum, traversal and linked archives, unsupported hosts,
  unsafe inputs, and valid local installation with compiler/network stubs.
  Existing Rust/Bash packaging, relocation, promotion and controller tests retain
  their safety and cross-platform contracts.
- The seven release assets and generated `release.json` are unchanged.
- Local full Rust checks passed, including the real installer negative tests and
  native Mac sandbox contract. A real Mac development build compiled embedded
  GUI, CLI/MCP and CRDs, then correctly refused packaging: the checked-in
  controller is alpha.1 with an older runtime contract than the current alpha.3
  host. No digest, receipt or validation was forged/relaxed to get past this.
- Full release-artifact lanes require the normal main CI source-matched controller
  receipts. This work does not authorize their publication or merging a branch;
  those lanes remain a post-merge requirement, not a claimed local pass.

### Slices 5–6 assertion disposition

| Former assertion family | Maintained destination / explicit boundary |
| --- | --- |
| Release metadata, archive traversal/links/size limits, corrupt gzip/checksums/modes, missing/extra members, source/target/controller drift | Rust xtask metadata, archive, bundle, package, smoke and controller suites; actual shell installer negatives in `tests/installer.rs` |
| Build/Linux/controller wrapper command delegation | Rust/Bash integration fixtures with real validators; the obsolete Python delegator is not retained |
| Install/reinstall without compilers or network | Linux source-free container and Mac verified sandbox distribution lanes; real installer fixtures in normal Rust checks |
| Prepare-only twice, helper hashes/mtimes, no early state, repeat setup/controller identity/receipt/database preservation | Owned `onboarding` gate; unregistered setup/doctor no-initialization regression in app CLI tests |
| CLI Bitcoin then MCP Bitcoin/CDK exact on-demand image selection, ready read-back and cell cleanup | `onboarding`, shared MCP/cell helpers, installation-aware kubectl and CLI |
| Codex/OpenCode/Claude dry-run, terminal defaults, backups, repeat, distinct actors, conflicting manual entries, revoked grants and poisoned ambient environment | `agent-config` with supported client fixtures and real MCP; ordinary product config/authorization unit tests remain |
| Installed OpenCode/Claude parser and MCP discovery in attached/unrelated folders | Explicit `agent-clients` gate with private homes; reports Claude's first-use approval separately from a connection, with no trust bypass, model call or native app claim |
| GUI HTTP/embedded assets, auth/origin, repeated start/stop, stale ownership and token rotation, unchanged cells | `gui`, also called by onboarding with real workloads; product HTTP and GUI ownership tests |
| Positive native launch, folder picker, browser focus and recent-project display | Explicit manual desktop checklist in CHECKS; retired the misleading headless script that opened apps. Existing generated-config, URL/argument and consent/HTTP tests remain. No automatic visual or handoff pass is inferred. |
| Externally written “model observed” file and 30-minute polling script | Retire that ad-hoc evidence-file protocol. Real model use requires separate authorization and actual recorded request/result evidence, never a boolean supplied by the harness. |
| PTY first feedback, animation, clear line, no timer/escape codes, readable setup/GUI and pure JSON | Hermetic real-PTY output test plus owned `cli-progress` live gate; current grammar, not obsolete exact message copy |
| Two registries with own-image success and foreign-digest rejection | `installation-isolation` owns two normal installations, verifies parent/peer receipt binding, and uses shared teardown; no raw k3d lifecycle |
| Old cleanup mock expecting three k3d calls / automatic curl-18 smoke retry | Retire implementation-specific expectations and the outer retry policy. Shared Rust retirement tests check exact identities, foreign users, partial retries and evidence; product downloads retain integrity enforcement. |
| Preservation of existing Docker/config/runtime state | Shared before/after inventory, byte hashes and resource receipts; any drift remains a failure, never repaired or waived |
| Optional observer of controller/cell UIDs through the developer's global Kubernetes context | Retired ambient-context access. Preservation inventories Docker resources and selected checkout files; onboarding/GUI verify workloads inside their own installation. This is not an inventory of unrelated Kubernetes workloads. |
| Unreferenced old light logo | Removed after source/docs reference scan; current SVG branding and site workflow untouched |
| Linux native supervisor process/private-I/O checks | Retained explicit opt-in diagnostic, documented with owner/caller; not swept away with Python host orchestration |

## Slice 6 implementation checkpoint

- Checkout and unpacked bundles now select the same owned acceptance suite through
  `just e2e` / `just e2e-bundle`; cleanup is `just e2e-cleanup`.
  New gates cover onboarding, GUI, private agent configs, optional real-client
  discovery, PTY output and two-installation registry isolation.
- Child operations have bounded output/deadlines. Failed workers' helper processes
  are reaped; owned GUI cleanup remains possible after failures. Peer teardown is
  bound to the parent receipt, refuses links, and does not prevent independent
  primary cleanup when a peer needs attention.
- Removed superseded Python smoke implementations after replacement checks and
  assertion mapping. Distribution isolation stays in its existing Bash/Rust lane.
- Release README is now an index; Linux/Mac docs and contributor guides agree on
  current commands and evidence boundaries. All retained scripts have documented
  owners/callers/lanes. Historical JSON/pins and ignored local output are untouched.
- Quick CI gains a narrow retired-workflow/reference guard; active Python runtime
  drivers, negative fixtures and logical catalog names are not blacklisted.
- An earlier onboarding/config/progress run passed all requested product gates
  and owned cleanup, but its final preservation check detected a
  Claude cache timestamp change. A matching existing Claude backup proved only
  `cachedGrowthBookFeaturesAt` changed; no MCP entries or Docker resources drifted.
  That run remains failed overall, and no personal file/process was modified to
  hide it. Its evidence is retained at
  `/private/var/folders/w7/t4mxkw6n48vd2dwyvdgrn73w0000gn/T/proofstorm-acceptance-mXiyHE/`.
- Final `just check` passed: quick dispatch/reference/shell checks, strict
  workspace Clippy, and all workspace test targets. New regression tests cover
  client-specific launch arguments, OpenCode's schema-only edit, and Claude
  approval status without misreporting a connection. Maintained documentation
  link checks and `git diff --check` passed.
- Live onboarding (including GUI with running cells), private agent config,
  PTY output, and two-installation registry isolation all passed in
  `/private/var/folders/w7/t4mxkw6n48vd2dwyvdgrn73w0000gn/T/proofstorm-acceptance-rl94Mh/`.
  First progress appeared in 11–15 ms; JSON output parsed separately. Both
  registries accepted their own fixture and rejected the other's image digest.
  Primary/peer cleanup and preservation passed. That run still records its
  later client-discovery assertion failure; it is not relabeled an overall pass.
- The corrected installed-client gate passed setup, all attachment assertions,
  owned cleanup and preservation in
  `/private/var/folders/w7/t4mxkw6n48vd2dwyvdgrn73w0000gn/T/proofstorm-acceptance-m0rMUW/`.
  Codex 0.153.4 passed its managed MCP probe and terminal preview (no client/model
  session). OpenCode 1.18.30 connected through its own MCP listing. Claude Code
  2.1.267 discovered the project entry as `pending_approval`, correctly reported
  as not client-connected; its direct managed MCP probe passed. Neither client
  found Proofstorm in the unrelated project. Private discovery output remains
  with the report. No personal agent configuration or preexisting Docker resource
  changed in this final run.
- Earlier runs exposed test-only mismatches: the CLI wait limit, Codex's explicit
  project argument, OpenCode's added schema annotation, and Claude's first-use
  approval requirement. The assertions were corrected with focused regressions,
  not product trust changes. Their failed evidence and diagnostics remain;
  each owned runtime was removed.
- No Linux live host, full distribution lane, fresh release-bundle runtime,
  native desktop handoff or model session pass is claimed. The source-bound
  release-artifact requirement from slice 5 remains outstanding.

## Current product baseline

- **Proofstorm** is the product; **`storm`** is the preferred CLI; **cells** are
  the environments users create. The CLI polish and cell rename are completed
  baseline work, not tasks to redo during consolidation.
- `proofstorm` remains a supported executable for the same CLI. The installer
  skips the short `storm` launcher if it conflicts with an existing executable.
  Keep this fallback and collision protection; they are not competing workflows.
- The MCP connection remains named **`proofstorm`**, and its executable remains
  `proofstorm-mcp`. The CLI rename does not rename the product, crates, installation
  paths, `PROOFSTORM_*` variables, or registry/release identities. Do not perform a
  blanket `proofstorm` → `storm` replacement.
- Use the current command tree: `storm setup`, `storm doctor`, `storm up`,
  `storm ls`, `storm status`, `storm rm`, and `storm ops …`. GUI controls are
  `storm gui` / `storm gui open`, `storm gui start`, `storm gui stop`, and
  `storm gui status`. Agent controls are `storm agent open` and
  `storm agent configure`; native launch uses `--desktop`.
- Preserve concise help, readable default results, progress feedback, and the
  global `--json` interface. Redirected output must remain usable by scripts.
  Adapt old test callers to the new grammar; do not revive removed top-level
  commands or old flags just to make a legacy test pass.

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

At the original audit, the tracked checkout contained 72 files under `scripts/`, with 45 Python files,
48 shell scripts, and one `.mjs` file across the repository. These are inventory
counts, not deletion targets: several Python drivers are part of the product.
No files under `dev/` were tracked before this plan; historical run output is
already ignored and is not repository bloat. Keep it that way.

| ID | Flow and evidence | Disposition |
| --- | --- | --- |
| A | Former `compose.yml`, `compose.regtest.yml`, `Makefile.compose`, and `just compose` | Removed in slice 3 after the assertion audit below. No remaining Make dependency in the just recipes. |
| B | Former `cluster-up`, `down`, `images`, `images-build`, `bitcoin-image-build`, `legacy-gate-build` recipes and `infra/k3d/proofstorm.yaml` | Removed in slice 2. They could recreate the retired cluster, reuse port 5111, and operate outside installation ownership. |
| C | [Acceptance runner](../crates/proofstorm-acceptance/src/bin/proofstorm-acceptance.rs), [gate context](../crates/proofstorm-acceptance/src/gate.rs), [kubectl wrapper](../crates/proofstorm-acceptance/src/kubectl.rs), former acceptance `doctor.rs` / `images.rs` | Slice 1 uses owned installations. Slice 2 removes unused diagnostic/image modules and makes installation-bound kubectl mandatory. Gate assertions remain. |
| D | Former hidden `storm dev serve --replace` and `server_restart.rs` | Removed in slice 2. Use managed `storm gui`, `storm gui start`, `storm gui stop`, and `storm gui status`. Shared HTTP implementation and transport tests remain. |
| E | [Default environment](../crates/proofstorm-app/src/config.rs), former `examples/opencode/` static profiles, [MCP startup](../crates/proofstorm-mcp/src/main.rs) | Slice 2 removes implicit connected-mode selection and static profiles. Explicit external context/kubeconfig, generated `proofstorm` attachments and offline/memory test identities remain. |
| F | Former Python release implementation, Linux shim, controller builder and their tests | Removed in slices 4–5 after the assertion audit. Rust/Bash owns build, controller, archive, relocation and promotion. New Rust integration tests run the real installer for unmatched negative cases. |
| G | Former Python image publishers and Mac-only pin resolver; [catalog-image wrapper](../scripts/catalog-image.sh), [maintainer tool wrapper](../tools/install-host-tools.sh) | Slice 4 consolidates publication into Rust/Bash with shared controller registry checks, explicit platforms and receipts. Host-tool validation is shared by setup and maintainer installs; their destinations remain separate. |
| H | Former agent benchmark runner/suite/cluster helper, prepare/seed/evaluate helpers, private-handoff/ecash campaign scripts, proxy and `.mjs` argument-audit plugin | Removed in slice 2 with framework-only fixtures/tests. See assertion disposition below. No replacement model benchmark platform; ignored historical run data is untouched. |
| I | Former installed/checkout/controller, GUI/attachment/progress/isolation Python smoke checks | Consolidated into artifact-selected Rust acceptance gates. Real-client discovery and manual desktop/model acceptance stay separate; see the slice 6 assertion ledger. |
| J | [Release README](../release/README.md), [Linux notes](../release/linux.md), [Mac notes](../release/macos.md), [original spec](../SPEC.md), historical verification JSON | Make the root README and `scripts/DEVELOPMENT.md`, `scripts/CHECKS.md`, `scripts/RELEASING.md` authoritative. Slice 3 removes the scenario instructions and marks the original spec historical. Audit each JSON consumer before moving/removing it. |
| K | Older Dockerfiles/configs, retained CDK config contract, old light logo | Compose-only leaves removed in slice 3; unused light logo removed in slice 6 after reference scanning. Current branding, drivers and generated config contracts remain. |
| L | [Website update workflow](../.github/workflows/update-site.yml), now tracked | Not legacy. Preserve website deployment behavior; coordinate any refactor. Prefer thin Bash orchestration and reusable Rust validation over a separate inline Python release implementation. Do not delete it as part of legacy retirement. |

### Important dependencies and exceptions

- The removed Compose regtest defaults pulled from `localhost:5111`, the retired
  registry. They are not a supported fallback for the current workflow.
- The owned Rust runner reads saved installation routing and shares product
  retirement. Version 2 uses `proofstorm-<8 ID characters>`; version 1 keeps its
  saved naming scheme. No naming migration or global kubeconfig fallback.
- Attachment checks use current `storm agent open` / `configure`, project scope,
  and explicit desktop selection. Old top-level grammar remains rejected.
- Python release/Linux shims and their delegation-only tests are removed.
  Bash orchestration and shared Rust verification are the only maintained path.
- Slice 2 removed the three OpenCode profiles along with campaign preparation
  and the old acceptance doctor; no remaining callers need those profiles.
- `tests/cdk18-config-contract.sh` now uses generated Kubernetes configs and their
  exact public image digests. The shipped initializer's edit/retry/failure checks
  remain; the unrelated Compose-only entrypoint and configs are removed.
- `proofstorm-registry.localhost:5000` also remains a **logical catalog image
  namespace** routed to each installation's registry. Do not globally replace
  it just because the identically named physical container is gone.
- Runtime Python under `proofstorm-kube/drivers/`, acceptance drivers, and
  Nutshell telemetry is actively included/called from Rust. It is not dead
  host orchestration. A later language-reduction project can assess it separately.
- Keep `--allow-development`, old published archive-name support, and version-1
  installation loading. These protect real artifacts/state, not obsolete dev flows.
  Use cells in current prose and examples, but do not mass-edit historical
  evidence or serialized identifiers as part of a terminology cleanup. Inspect
  the current contract before changing any remaining old-name field or fixture.
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

- Bring retained smoke-test invocations onto the current `storm` command grammar
  before moving their lifecycle logic. Keep the supported `proofstorm` executable
  path where a bundle test needs it; it uses the same grammar, not the old one.
- Refactor `GateContext`, MCP spawning, and kubectl selection around an explicit
  `Installation`, verified CLI/MCP artifacts, and a unique run identity.
- Make `just e2e smoke` prepare a disposable home using the ordinary setup path.
  Extend the same runner to named gates; no implicit use of the user's dev home.
- Reuse installation image provisioning rather than maintaining a second
  catalog downloader in acceptance. Keep genuinely necessary diagnostic checks.
- Centralize owned runtime teardown in Rust, reusing saved resource IDs and full
  ownership identity. The implementation exposes shared retirement through an
  internal CLI command and acceptance cleanup; no public runtime-reset command
  was added. Any future public entry point must use this same implementation.
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
- Retire the remaining hidden `storm dev serve --replace` lifecycle and its
  process-discovery module after consumer checks. Top-level `serve` is already
  gone; retain shared HTTP handlers and managed server ownership checks.
- Delete the three static OpenCode profiles and outdated copy/paste startup
  instructions. `storm agent open` / `storm agent configure` own configuration;
  the generated MCP connection name stays `proofstorm`.
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
The polished command tree and human/JSON output contracts remain covered.
No operation edits users' existing personal agent configuration during cleanup.

#### Slice 2 assertion disposition

The retired campaign tests exercise their own Python evaluator, proxy, proposal
writer and model-session scheduler—not the shipping CLI or controller. Their
removal is not a claim that a model benchmark has been ported to Rust.

| Retired assertion family | Disposition |
| --- | --- |
| Model scoring, unsupported conclusions, token/step budgets, role resumption, cleanup-only proxy admission | Retire with the real-model campaign framework. These were experiment constraints, not shipped product policy. No replacement model runner. |
| Proxy public-help allowlist and argument-audit redaction | Retire this benchmark-only restriction and its logs/plugin. It must not be mistaken for a product guarantee that public native commands are limited to help. Product private custody remains covered in `proofstorm-transfer/src/tests.rs` and the Rust `private_transfer` / `private_handoff` acceptance gates. |
| Recipient binding, command identity, private output and incomplete cleanup evidence | Keep the actual Rust transfer tests (cross-principal rejection, tampering, one-shot admission, completed-capture handoff and stale recipient fencing) and `private_handoff` gate. Retire the separate Python evidence scorer; do not equate successful native exit with settlement. |
| Prefunder JSON-RPC framing, timeout, operation identity and exact funding projection | Retire the script-specific client and fixed 5,000-sat fixture. Keep the Rust MCP stdio contract tests and owned acceptance worker's deadline. Real wallet funding/settlement remains in Rust wallet gates; no equivalence claim for the retired fixture or per-request transport timeout. |
| Campaign authority DB read-only/workspace filtering and disabled proposal writes | Retire with the campaign reader/writer. Product authorization and generated agent-config conflict/backup tests remain in Rust. No personal config is rewritten by this cleanup. |
| Global idle-cluster inventory, orphan storage and operator finalizer | Retire global-cluster campaign accounting. Slice 1 owns a complete disposable installation with exact container/network/volume receipts, preservation checks and retryable teardown; its tests protect foreign resources. |
| Cached-image restoration / static-profile doctor | Retire the unused acceptance helpers. Normal setup's immutable image validation and digest-preserving inventory tests remain; MCP discovery stays covered by stdio tests and the owned smoke gate. The unused cluster-schema pre-upgrade helper is not a migration API; typed CRD contract tests remain. |
| Checkout foreground-server PID matching | Retire with raw `dev serve --replace`. Managed GUI lifetime/session ownership, cross-origin protection and stop/reuse tests remain. |

### 3. Remove Compose and Make without discarding useful tests

Scope: A and Compose-only leaves in K. Depends on slice 1.

Assertion disposition, audited before removing the old scripts:

| Old assertion | Current candidate | Required decision/check |
| --- | --- | --- |
| Wallet funding, swaps, balance conservation | `cross_implementation_wallet`, `cdk_wallet`, `slice5` gates | Retire the configurable FakeWallet population test (N × 100 sats, 1-sat self-swaps, zero CDK loss / up to N sats Nutshell loss). It is not equivalent to real Lightning settlement. Existing CDK tests check 5,000-sat issuance, 700/300-sat payments, zero/100-ppk input fees and paid-invoice rejection fees; cross-implementation tests require 1,000-sat issuance and authoritative Nutshell conservation, and refuse a CDK conservation claim when fee evidence is unavailable. The new proof-spend gate adds exact zero-fee 64-sat accounting across source/recipient wallets. |
| Same-token sequential replay rejected | New Rust `cashu-double-spend` gate | Preserve against CDK and Nutshell. Require initial redemption, reject the identical token both in the recipient and a fresh wallet, require a spent-proof rejection (not arbitrary failure), and verify unchanged balances. |
| Concurrent same-proof spend admits exactly one | New Rust `cashu-double-spend` gate | Preserve two independently initialized CDK wallet states and two processes released by a common barrier against the same mint/token. Require one success, one spent-proof rejection, winner credit, zero loser credit and exact total balance. This is not a command-idempotency test or proof of simultaneous HTTP arrival. |
| Quote flood preserves honest-client service | No equivalent replacement | Explicitly retire this experimental load/SLA test. Its `stop_probe=$(mktemp)` creates the stop file before the probe loop, so it can pass without a single concurrent observation. OIDC rate-limit tests do not replace it. No sustained quote-load availability claim remains. |
| CDK configuration validate/init/edit/retry/failure recovery | Retained opt-in CDK image contract | Validate only generated Kubernetes configs against their pinned public images; use the rendered initializer and generated config for state-preservation checks. Remove Compose fixture checks and the Compose-only entrypoint, which imports once and is not used by shipped images. |

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
- The controller wrapper and its publisher callers were removed in slice 4;
  preserve the shared Rust registry and existing controller/bundle assertions.
- Remove `release.py` and its implementation-specific tests only after no
  maintained import/caller remains.
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
- Preserve the current CLI grammar/help and output tests. Live progress checks
  should assert prompt feedback, readable results, clean JSON, and no elapsed
  timer rather than pinning obsolete command names or superseded message copy.
- Retire the Python live-smoke scripts only when their meaningful assertions have
  a maintained destination. Optional real-model tests stay opt-in and bounded.
- Replace `release/README.md`'s historical procedure narrative with a short index
  pointing to `scripts/RELEASING.md`. Date historical evidence; don't rewrite it
  to claim current acceptance. Remove stale Make/profile/alpha-unpublished advice.
- Standardize current user-facing instructions on `storm` and cells. Explain the
  `proofstorm` executable fallback once; do not rename MCP or internal identities.
- Remove unreferenced asset/config leaves. Keep provenance/test fixtures that
  have live consumers. Preserve the current branding and site deployment behavior.

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
5. Treat the merged naming, branding, site workflow, CLI polish, and cell rename
   as the baseline. Preserve newer concurrent work; keep each cleanup commit
   narrowly scoped and reverify affected behavior.

## Definition of done

- No Compose or Make dependency in maintained Proofstorm workflows.
- No commands recreate or implicitly target the retired shared cluster.
- One managed GUI launcher and one generated agent-attachment path.
- Current instructions use `storm` and cells; the polished CLI, supported
  `proofstorm` executable fallback, and `proofstorm` MCP identity remain intact.
- Live tests exercise the same runtime lifecycle as installed users, with their
  own home and verified cleanup; checkout state is not adopted or destroyed.
- One authoritative Bash/Rust build and release implementation; unique catalog
  publication and pin maintenance remain supported through that tooling.
- Every removed test assertion has a replacement or an explicit retirement
  reason. Active runtime drivers and published-install compatibility remain intact.
- Fewer operational entry points and clearer docs, not a new framework replacing
  each old framework. Git history is sufficient recovery for deleted source.

Next gate: review the completed consolidation branch and run hosted checks. After
merge, normal main CI creates the source-matched controller receipts for both
platform bundle/installer lanes. Those artifact lanes and fresh release-bundle
runtime acceptance remain required before release; local code checks do not imply
their success. No further consolidation coding slice remains, and this work does
not commit, merge, publish images, or create a release.
