# Run limit removal — September 6, 2026

> Superseded coordination design: leases are replaced by nonblocking [session tracking](session-tracking-2026-09-06.md); private-transfer permissions are separate. Historical results below describe their recorded version.


Labs remain usable until explicitly closed. Runs and leases have no lifetime or action-count quota, and status has no remaining-budget counter.

Removed the duration/action options from `proofstorm up`, developer MCP requests, advanced lease acquisition/delegation requests, application state, core lease types, generated schemas, runtime authorization and store admission. No optional limit settings or artificial “unlimited” values replace them. Updated acceptance gates, benchmark setup scripts and current usage documentation.

Ownership, explicit release, parent/recipient scope checks, shutdown admission fences, idempotent retries and the operation journal remain. Releasing a root lease atomically releases its children. Replaying an old acquisition receipt cannot revive released authority. Individual command deadlines, concurrent-operation bounds and private-payload retention are separate controls and remain.

## Existing installations

Opening an existing database transactionally removes the obsolete lease and named-lab columns and upgrades lease receipts and their idempotency hashes. Active records lose their old cutoff. Records already marked expired become released; inactive parents cannot reactivate their children. Action history and operation results remain intact. The Kubernetes annotation reader accepts persisted leases from before the upgrade, drops retired fields and maps expired authority to released. New writes use the current model.

Upgrade the CLI/MCP and controller together; old clients must stop sending duration and action-limit fields. This is a one-way storage migration, so preserve a database backup if an older binary must be restored. Historical exported evidence is left unchanged.

## Verification

The storage upgrade regression opens an old database with a past deadline and a one-action allowance, admits and completes 1,001 actions, verifies the old columns are absent, then verifies explicit release blocks new work and acquisition replay. It also reopens the database and checks the final operation result survived. Separate tests cover inactive-parent migration and old Kubernetes annotations.

Validation passed: `cargo test --workspace --all-targets` (266 passing tests, one ignored subprocess fixture exercised by its parent test), strict workspace Clippy, formatting, Helm lint, checked-in schema/CRD contracts, Python syntax, JSON documentation examples and diff whitespace checks. The rebuilt CLI help exposes neither removed option. No live cluster gate or controller deployment was performed for this change.
