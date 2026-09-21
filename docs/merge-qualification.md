# Merge qualification

`Checks` starts on every pull request, main push, manual run and merge-group
event. Its stable `Merge qualification` result requires formatting, Rust tests,
the Mac installer isolation contract and native catalog qualification to succeed.
Every main SHA keeps its own run; only obsolete PR runs are cancelled. A failed
qualification therefore also prevents the existing release-promotion validator
from accepting that `Checks` run.

The planner reads both platform catalogs. It enumerates preferred and supported
versions and the experimental LDK processor relationship exposed by CDK. Plans
record exact image sources, component versions, storage/authentication choices,
wallet pairings and coverage claims. A full run exercises each planned case on
native Linux AMD64 and ARM64. Only a nonempty diff consisting entirely of the
explicitly allowed documentation paths may omit cases outside the fixed baseline.
All other changes select the full matrix. The baseline includes native image and
Lightning contracts, preferred external mint/wallet payment paths with SQLite
and PostgreSQL, Redis, authenticated Nutshell 0.20, workspace persistence and
control-plane lifecycle checks.

Qualification pulls published images anonymously by digest and records the
selected platform manifest and config digest. A source build cannot replace a
catalog artifact. `Candidate component packaging` separately rebuilds images on
recipe/provenance changes or manual dispatch. Host binaries and the controller
are built once per architecture and transferred within the current CI run;
controller reuse verifies its source identity, platform and executable metadata.
PR jobs have no package-write permissions.

Both native architectures must restore their build artifacts on fresh runners,
start an owned runtime, pass the Bitcoin smoke scenario and verify cleanup before
the full matrix begins. Linux ARM64 checkout
setup uses its own checksum-verified host-tool pins; this does not add a published
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
- Embedded BDK covers on-chain deposits, confirmation and dust handling,
  concurrent quote addresses and persistent settled quotes. Its catalog no
  longer advertises the BOLT11-only Nutshell wallet as an on-chain wallet pairing.
- Nutshell 0.21's NUT-21/NUT-22 integration is excluded from supported claims
  because the existing upstream blind-auth database defect prevents issuance.
  The supported 0.20.3 authentication contract remains scheduled. Restore newer
  authenticated support only with a qualifying upstream artifact and passing
  positive, negative and replay/persistence tests; do not patch around it in CI.
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

To inspect a complete plan:

```sh
cargo run -p proofstorm-qualification -- plan "$(git rev-parse HEAD)" 0 1 full /tmp/qualification-plan.json
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
receipt for recovery. CI uploads only the plan and redacted execution receipts,
never native command output, credentials, seeds or runtime homes. Missing
receipts indicate an interrupted or failed setup/execution. Local cleanup can be
retried using `proofstorm-acceptance --cleanup WORK_DIRECTORY`; it only acts on
that run's recorded resources. Re-run the entire hosted workflow after a failure:
receipts from an earlier attempt intentionally cannot satisfy a newer attempt.

Console diagnostics include a fixed setup-stage/error category, the current mint
test stage and operation elapsed time, available memory and disk, and Linux load,
task counts and cumulative OOM kills. Unavailable readings are null. Preservation
failures report counts of added/removed/changed resources.
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
Double-spend stages distinguish CDK and Nutshell startup from their replay checks.
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
