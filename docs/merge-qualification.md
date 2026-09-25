# Merge qualification

`Checks` starts on every pull request, main push, manual run, merge-group
event and weekly schedule (Mondays 07:17 UTC). Its stable `Merge qualification` result requires formatting, Rust tests,
the Mac installer isolation contract and native catalog compatibility to succeed.
Every main SHA keeps its own run; only obsolete PR runs are cancelled. A failed
qualification therefore also prevents the existing release-promotion validator
from accepting that `Checks` run.

The planner reads both platform catalogs. It enumerates preferred and supported
versions and the experimental LDK processor relationship exposed by CDK. Plans
record exact image sources, component versions, storage/authentication choices,
wallet pairings and coverage claims. The required compatibility suite exercises
each supported pairing on native Linux AMD64 and ARM64. Main pushes, the weekly
schedule and manual runs always select the complete compatibility matrix, and
release promotion requires a successful main run. Pull requests select the small
`pull` suite (smoke, runtime lifecycle, native exec and one CDK/LND/SQLite round
trip per architecture) unless they change catalog definitions, component images
or their Kubernetes rendering (`docker/`, `proofstorm-core` catalogs,
`proofstorm-kube`, `scripts/catalog-image.sh`); those, and a failed or empty
diff, select the complete compatibility matrix. The fixed baseline includes
native image and Lightning contracts, preferred external mint/wallet payment paths
with SQLite and PostgreSQL, Redis, Keycloak, workspace persistence and control-plane
lifecycle checks.

Required CI checks Proofstorm's integration contract: image identity, startup,
configuration, supported component pairings, ordinary payment/deposit flows,
persistence, cleanup, and Proofstorm regressions such as incorrect settlement
reporting or controller recovery. It does not gate merges or deployments on
upstream adversarial races or load testing.

`Upstream behavioral qualification` is a separate, manually dispatched workflow.
It selects `full`, adding CDK/Nutshell double-spend replay/races and 24 concurrent
BDK quote requests on both database backends to the compatibility suite. Failures
remain failures in that workflow, but it is not a dependency of `Checks` or release
promotion. Required BDK cases use four sequential quote requests and retain
configuration, valid deposits, invalid-input/dust checks, restart persistence and
cleanup. No `continue-on-error` hides failures. A failed case is retried once in a
fresh work directory; a pass on retry is listed as "Passed only on retry" with the
first attempt's public diagnostic, so flaky cases stay visible without blocking.

Plans explicitly bind their suite (`pull`, `documentation`, `compatibility`, or
`full`), so receipts cannot be reused between suites. The planner verifies every
catalog claim is covered by scheduled cases in both compatibility and full plans.

Qualification pulls published images anonymously by digest and records the
selected platform manifest and config digest. A source build cannot replace a
catalog artifact. `Candidate component packaging` separately rebuilds images on
recipe/provenance changes or manual dispatch. Host binaries and the controller
are built once per architecture and transferred within the current CI run;
controller reuse verifies its source identity, platform and executable metadata.
PR jobs have no package-write permissions.

Both native architectures must restore their build artifacts on fresh runners,
start an owned runtime, pass the Bitcoin smoke scenario and verify cleanup before
the selected matrix begins. Linux ARM64 checkout setup uses its own checksum-verified host-tool pins; this does not add a published
Linux ARM64 installer or update channel.

Each live case owns a disposable installation. Its fixture must instantiate all
planned component versions. The runner checks teardown and compares preexisting
resources before and after execution. Failures and preservation drift are not
waived. The verifier requires exactly one successful receipt per required case,
bound to the commit, catalog, plan, architecture, workflow run and attempt.
Failed, missing, duplicate, skipped or stale evidence cannot qualify a merge.

## Coverage boundaries and upstream gaps

- External CDK/Nutshell scenarios exercise issuance, self-swap, melting with
  independent recipient settlement, and a second payment after restart. CDK's
  wallet debit is bounded rather than described as exact fee conservation.
- Embedded LDK exercises paid BOLT11 mint/melt through Nutshell and BOLT12
  payment recognition, then checks persistent identity/quote state. BOLT12
  ecash issuance is not covered. The experimental gRPC processor has the same
  explicit BOLT12 payment-recognition boundary.
- Embedded BDK compatibility covers on-chain deposits, confirmation and dust
  handling, sequential quote addresses and persistent settled quotes. Concurrent
  quote generation is reserved for the opt-in behavioral suite. Its catalog no
  longer advertises the BOLT11-only Nutshell wallet as an on-chain wallet pairing.
- The unified `cdk` entry covers linked Lightning, embedded LDK, embedded BDK,
  and linked LND with embedded BDK. The `cdk-oidc` gate exercises NUT-21/22 on
  both SQLite and PostgreSQL auth stores in one cell. The PostgreSQL mint's
  primary and auth databases share a server with Keycloak. Each mint must pass
  valid/invalid CAT and BAT checks, BAT issuance and DLEQ verification, the
  issuance limit, a protected request, and spent-token rejection after restart.
  Controller restart must preserve generated identity/database credentials.
- Nutshell 0.21.0 auth is covered by `nutshell-oidc`, using SQLite primary
  storage and both SQLite and PostgreSQL auth stores. It verifies the upstream
  NUT-21/22 error codes and CAT rate limit in addition to issuance, DLEQ,
  protected requests and spent-token rejection across restart. The retained
  0.20.3 release remains unauthenticated because its recorded live run fails
  blind-auth issuance. No local schema workaround or candidate image substitutes
  for released-image qualification. Keycloak also remains independently
  qualified for discovery, valid/invalid logins, generated credentials, signing
  keys and restart persistence.
- CLN uses the corrected 26.06.7 digest published in the
  [upstream release notes](https://github.com/ElementsProject/lightning/releases/tag/v26.06.7).
  The previous image reported the same version without the release fixes.
  Historical release receipts are not evidence for the corrected image.

## Local use and diagnostics

Hermetic checks work without a runtime:

```sh
just check
cargo test -p proofstorm-qualification
```

To inspect the required compatibility plan (use `full` for the opt-in suite):

```sh
cargo run -p proofstorm-qualification -- plan "$(git rev-parse HEAD)" 0 1 compatibility /tmp/qualification-plan.json
```

For a native Linux live case, prepare matching checkout artifacts with
`just dev-build`, build `proofstorm-acceptance`, `proofstorm-qualification` and
`proofstorm-xtask` into `target/check`, then select a case ID from the plan:

```sh
target/check/debug/proofstorm-acceptance --checkout-home "$PWD/.proofstorm-dev/state" \
  --qualification-plan /tmp/qualification-plan.json --qualification-case CASE_ID \
  --work-dir /tmp/qualification-case qualification
```

The work directory must be new. It retains private logs and the owned runtime
receipt for recovery. CI uploads the plan and redacted execution receipts, never
plaintext native command output, credentials, seeds or runtime homes. When
`.github/qualification-logs.age.pub` holds an age public key, each failed
attempt's `*.log` and `*.json` files are encrypted to it and uploaded as
`qualification-private-logs-*` (7-day retention). Decrypt with the matching
private key: `age -d -i KEY_FILE CASE-attempt-N.tar.gz.age | tar -xz`. Missing
receipts indicate an interrupted or failed setup/execution. Local cleanup can be
retried using `proofstorm-acceptance --cleanup WORK_DIRECTORY`; it only acts on
that run's recorded resources. Re-run the entire hosted workflow after a failure:
receipts from an earlier attempt intentionally cannot satisfy a newer attempt.

Console diagnostics include a fixed setup-stage/error category, the current mint
test stage and operation elapsed time, available memory and disk, and Linux load,
task counts and cumulative OOM kills. Unavailable readings are null. Preservation
failures report counts of added/removed/changed resources. A failed MCP tool call
reports the tool, JSON-RPC code, the server's typed problem code and any
Kubernetes HTTP status (for example `tool cell_remove; code runtime_failure;
http 409`); its message and arguments stay in the private log.
Embedded LDK also reports configuration, version, peer connection, BOLT12 quote
and payment, and teardown stages. Its channel setup seeds both directions and
requires observed capacity above reserves before exercising issuance and melting.
It also funds the embedded node's on-chain wallet through the upstream loopback
dashboard and waits for spendable funds before opening an anchor channel. The
channel balance and this on-chain emergency reserve are separate requirements.
Dashboard cookies and CSRF values stay in private temporary files and memory.
Proofstorm disables CLN's automatic reconnect, so after mint replacement the
fixture reconnects CLN through the service name and checks the original peer's
usable channel before requiring another settled melt.
Each shard continues after individual case failures and ends with a list of failed
case IDs and their exit codes or missing/copy-failed receipts. These failures also
create individual CI annotations. A passing last case does not clear an earlier
failure; inspect that case's preceding qualification summary for its failing stage.
These diagnostics never include native output, resource identities or configuration contents.
Failed gates additionally print `Gate failure` with repository-relative Rust
source locations for the failing assertion. Native failures include numeric exit
and RPC codes, execution/cleanup flags and a fixed error category where recognized.
These summaries come from captured backtraces and selected typed status fields;
arbitrary error messages and native output remain private. Read this console
summary when a hosted runner's local `gate-0-qualification.log` is unavailable.
The shard's final failure list and CI annotations repeat the failing stage,
safe category and fixture source location when available, beside the case ID.
Readiness failures preserve the specific fixed blocker category, including
container exit, image pull, configuration, and scheduling failures.
An operation that reaches a terminal failure reports the runtime's own failure
code as a category, with the terminated container's kubelet termination reason
and exit code where both are recognized values. `OOMKilled` there means the
container's memory limit, not the component, ended the operation. The native
error tail and wallet diagnostic reason that can accompany that code stay in the
private log. Startup
failures retain private pod status and current/previous logs from every component
and initializer before runtime cleanup, including Lightning and identity-provider
dependencies. Capture has a bounded time budget; these are not public CI artifacts.
All CDK payment backends and Keycloak wait for their linked PostgreSQL service
before initializing the application. PostgreSQL readiness probes TCP loopback so
its temporary socket-only bootstrap server cannot prematurely expose a ready
Service endpoint. SQLite startup has no database wait; configuration failures
after PostgreSQL is available still fail without retrying initialization.
CDK also waits for its linked LND or CLN service before reading shared credentials
or opening the native RPC connection. Standalone Lightning failures report their
payment, funding, channel or restart stage from the private compatibility result,
instead of treating every failure in that runner as an image download failure.
Registry manifest lookup, image download, local image inspection and executable
version probes also have separate fixed stage labels.
The opt-in double-spend gate distinguishes CDK and Nutshell startup from replay
checks. A race loser must report a recognized spent/pending rejection and remain
uncredited; exactly one wallet must win. After both commands complete, an
independent fresh wallet must reject those same proofs as spent. Ordinary replay
checks still require spent errors. Timeouts, unknown failures, duplicate winners
and balance drift fail the gate. Public failure summaries include only the typed
numeric/boolean receipt, including both race exits, balances and post-race replay
results; raw wallet logs and tokens remain private. Deterministic shell-fixture
tests exercise both winner orderings and spent/pending/error outcomes.
Nutshell waits for its linked LND REST service before starting, avoiding a fatal
first backend check while LND is still initializing. Failed double-spend fixtures
retain private mint/dependency startup logs and pod status before removing the
cell; those logs remain excluded from public CI artifacts.
Acceptance workers use a separate process group. Completion, failure, timeout
and cancellation terminate that group through the OS signal API, including
descendants left by an exited worker; cleanup does not depend on parsing a
negative PID with an external `kill` command. Process-group signalling failures
fail the operation instead of being silently ignored.
Standalone Lightning cleanup removes the anonymous volumes declared by upstream
LND/CLN images as well as the fixture's explicitly owned named volumes.

Hosted wall time and runner minutes still need to be measured. The 30–45 minute
PR feedback target is a rollout goal, not a measured guarantee. Optimize repeated
setup and sharding before reducing the asserted coverage.

## Enforcement rollout

The repository ruleset must require the real `Merge qualification` check with
up-to-date branches and without a routine bypass. Enable it only after both
native architectures pass and a deliberately failed hosted scenario demonstrably
fails the aggregate. This code does not by itself change repository settings.
