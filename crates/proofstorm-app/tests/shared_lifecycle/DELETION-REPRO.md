# Removal reporting regressions

Originally reproduced on checkout `dd5fe3d` on 2026-09-14, before the product fix.
Both tests run the real application removal path used by `storm rm`, with the
existing in-process Kubernetes fixture controlling controller timing. They do
not launch the CLI executable, a real Bitcoin process, Docker, or a fresh VM.

The two original reproductions are now enabled regressions in
`deletion_reporting.rs`. The shared removal loop retains the original identity
when background cleanup removes its local record. A Kubernetes DELETE returning
404 triggers the same exact resource/namespace checks. Remaining resources mean
Closing without a success receipt; failed checks remain errors.

From the checkout root, with checkout web assets already built:

```sh
CARGO_TARGET_DIR=/tmp/proofstorm-native-headline-target \
PROOFSTORM_WEB_DIST="$PWD/.proofstorm-dev/web" \
PROOFSTORM_REQUIRE_WEB_ASSETS=1 \
cargo test --locked --offline -p proofstorm-app --test lifecycle \
  deletion_reporting -- --nocapture
```

Before the fix, both original tests verified absence of the original runtime
resource and namespace, then failed their assertions that removal should succeed.
Both reproduced in all five additional repetitions after the initial run.
After the fix, both return success with a verified receipt for the original
incarnation. All seven tests in this module pass without ignoring any tests.

Additional coverage checks remaining namespaces and later cleanup, DELETE
403/503 errors, failed verification reads, missing local records with incomplete
or unverifiable cleanup, and replacement cells created between removal polls.
Both alias reuse and reuse of the canonical cell ID are covered.

Final validation: 61 lifecycle tests, 73 MCP unit tests, and 12 MCP stdio tests
passed. Strict Clippy for the application and MCP targets, formatting, and
whitespace checks passed. HTTP tests used local loopback permission. The first
stdio run was blocked by sandbox certificate-store access; its rerun with normal
OS permissions passed all 12 tests.

## Background reconciliation wins the polling interval

`remove_reports_success_when_background_sweep_finishes_cleanup`:

1. Create one cell with a single Bitcoin component specification.
2. Begin one removal call. The fixture completes Kubernetes deletion and writes
   its verified teardown receipt.
3. At the removal loop's first waiting notification, run the real
   `lifecycle::sweep` through a separately opened handle to the same temporary
   database. The sweep verifies absence, removes the receipt and purges the
   cell's local records under the normal lifecycle guard.
4. The original removal resumes its next poll and cannot resolve the now
   absent local record. It now checks the captured incarnation's runtime absence
   and returns its verified removal receipt.

Before the fix: `ErrorKind::Missing`, code `not_found`, message
`cell "cell-<generated-id>" was not found`.

The original failure was in `Cells::close_with_progress`, at
`self.resolve_instance(reference)?`. The outer removal loop retained the
original incarnation but re-resolves its deleted record on each poll.
The normal GUI observer and control-client recovery tasks call this same sweep.

## Controller finalization wins a repeated DELETE

`remove_reports_success_when_finalizer_wins_repeated_delete`:

1. Create the same one-component specification and begin one removal call.
2. The first internal Kubernetes DELETE succeeds. The fixture leaves the cell
   terminating, as a real finalizer can do while cleanup is underway.
3. During the next poll, both status/identity lookups still see that cell.
4. The fixture completes finalization just before the repeated DELETE is
   handled: it removes the resource, writes the verified receipt, and returns
   Kubernetes `NotFound` / HTTP 404 to that DELETE.
5. Removal now verifies runtime absence and reconciles the remaining local
   record before returning success. Independent assertions confirm absence.

Before the fix: `ErrorKind::Failure`, code `runtime_failure`, `http_status: 404`.

The original failure was in `Runtime::close`, where the DELETE error was propagated by
`.map_err(kube_error)?`. There is one caller removal request and two internal
DELETE requests, so this does not require an agent to retry `storm rm`.

## Scope of evidence

These establish and cover two Proofstorm races matching the VM report's error
types. They do not identify which interleaving caused each of its four failures
or reproduce their observed frequency. No component behavior or mining is needed
to trigger either controlled reproduction. The fix is in the shared application
and runtime code used by CLI and MCP; it adds no public tools or persistent state.
The existing MCP fallback for a retry beginning after removal remains necessary
because that request has no initial local cell view.

Local execution logs:

- `/tmp/proofstorm-deletion-repro.log`: initial failures and independent checks.
- `/tmp/proofstorm-deletion-repeated.log`: five repetitions of both failures.
- `/tmp/proofstorm-deletion-baseline-unsandboxed.log`: default lifecycle suite.
- `/tmp/proofstorm-deletion-fixed.log`: the seven focused tests after the fix.
- `/tmp/proofstorm-deletion-lifecycle-final.log`: full lifecycle suite after the fix.
- `/tmp/proofstorm-deletion-mcp-final.log`: passing MCP unit tests and the initial sandbox-restricted stdio attempt.
- `/tmp/proofstorm-deletion-stdio-final.log`: passing stdio rerun with normal OS permissions.
- `/tmp/proofstorm-deletion-clippy-final.log`: strict application/MCP lint checks.
