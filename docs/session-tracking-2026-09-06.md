# Session tracking

Proofstorm starts protocol labs, lets applications and agents use them, and records what happened.

Sessions describe activity. They are created automatically for CLI/MCP clients and record the principal, lab, run, start, last recorded activity and finish. Separate clients using the same principal get distinct automatic session IDs. Actions and their results retain the session ID and actor. Ordinary lab execution is available to other authorized principals without taking ownership of the lab.

Sessions have no exclusivity, expiry, renewal, quota or access scope. An unfinished or overlapping session never blocks work or lab closure. Finishing a session does not cancel accepted operations or revoke permissions. Further work starts a fresh interval; exact retries retain the original operation and attribution. Clean client shutdown finishes its intervals. Abrupt process death leaves an unfinished interval; last activity is an observation, not proof of liveness. Command completion can advance last activity after a client disconnects.

## Reading activity

`proofstorm status NAME` and MCP `proofstorm_lab_inspect` include a bounded session page and activity with principal/session IDs. Use `proofstorm_session_list` to page through sessions for `instance_id`. Supply `session_id` instead to list its temporal overlaps. Both support `cursor` and `limit` (1–100, default 20), and return `next_cursor` and `observed_at_unix`. Overlap uses the recorded intervals at one-second resolution, including unfinished intervals up to observation time; it is advisory and does not establish simultaneous mutation of the same component. Activity summaries list component IDs; raw command arguments stay in individual operation records.

The advanced surface also has `session_start`, `session_read` and `session_finish`; normal callers need none of them. Operation requests may omit `session_id`. Supplied IDs are attribution hints: finished intervals or another actor's ID resolve to a fresh interval for the authenticated actor.

## Permissions and private transfers

Ordinary workspace capabilities remain independent from tracking. `lab.operate` replaces the former ability to acquire control of a lab; it permits operating alongside other authorized principals. Each operation still requires its specific capability. Actual lab shutdown and finite runtime capacity remain enforced.

The private ecash handoff feature uses separate `private_access_issue/read/revoke` permissions. A grant names the issuer, recipient, lab, one wallet/mint/reference, and an approved receive command. It grants no general lab operation capability. Sessions neither grant nor revoke this access. The source must still bind ready custody to the grant using `private_transfer` handoff. Private payload retention and native command timeouts remain independent controls.

## Automatic run context and alpha storage

Ordinary native commands and diagnostics may also omit `experiment_id`. The
implicit run is scoped to workspace, lab incarnation and actor. Reconnecting
continues that run with a new session; a live edit retains it. Explicit experiment
IDs keep their validation and lifecycle rules. A closed run is never silently
reopened. Session finish remains benign and never closes the run.

Upgrade CLI/MCP and controller together when their contracts change. Proofstorm
is unreleased and initializes the current schema directly; it has no historical
database migration support. Tests use fresh databases. Explicit lab deletion
purges its runs, sessions and action history; export wanted evidence first.

## Validation

Core, application, MCP, store, private custody, Kubernetes contract/rendering, and controller tests pass. Historical validation below describes the initial session change. Current session regressions cover concurrent clients, overlaps, explicit finish, and independent grant revocation. Formatting, strict workspace Clippy, and Helm lint pass.

The initial session change did not deploy a controller or run a live cluster gate. The subsequent [environment API verification](environment-api-2026-09-06.md#live-verification--september-6-2026) upgraded the local controller and verified concurrent operation by two principals.
