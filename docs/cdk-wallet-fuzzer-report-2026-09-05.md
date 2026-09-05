# First CDK wallet agent smoke checkpoint

Follow-up: the bounded retry and planning fixes are documented in
[the retry report](cdk-wallet-fuzzer-retry-2026-09-05.md).

Result: **failed before materialization; wallet interoperability remains
untested by this agent run**. No second run was launched. The cluster is idle.

Run: `cdk-wallet-smoke-01-20260905`, scenario `cdk-wallet-native-smoke`, fresh
Kimi K3 session, default native profile. Limits were 900 seconds, 60 model
steps and two equivalent plans. The runner now tells the agent those limits
explicitly and requests export/cleanup by 720 seconds or step 48. This prompt
change does not enforce a separate token cap or cleanup watchdog.

## Observed sequence

1. The agent read the catalog and correctly recognized that the CDK wallet
   supports native CLI operations and passive balance observation.
2. Its first plan contained `input_fee_ppk=0`, but used network paths instead
   of backend bindings. The agent noticed this before applying the plan.
3. A corrected request reused the stored plan ID and received a raw SQLite
   UNIQUE constraint error. It recovered by choosing a new ID.
4. The corrected topology omitted the zero-fee configuration. The agent noticed
   and said it would restore that value, but submitted an identical plan that
   still omitted it.
5. The existing repeat-plan guard stopped the session. Events 24 and 28 are
   identical after removing plan ID and idempotency key. This was a valid guard
   activation, not a wallet failure or two successful experiments.

The session exited 143 after 116 seconds with
`repeated_equivalent_lab_plan:2`. No lab, native CLI invocation, payment,
balance observation, restart or wallet-isolation check occurred. Session export
succeeded, but no experiment evidence bundle or teardown receipt exists because
no experiment or lab was created. Independent cluster audits report verified idle;
no operator resource cleanup was needed.

## Findings and next decision

- **Discovery:** the native-versus-passive distinction was understood initially.
  Its practical usability has not yet been exercised.
- **Agent planning:** backend bindings and preservation of configuration across
  full plan rewrites were the blocking issues. The final claimed correction was
  absent from the submitted request.
- **Platform diagnostics:** a plan-ID collision surfaced an internal database
  constraint rather than a useful domain error. A clearer diagnostic is warranted.
- **Harness:** the repeat guard worked as configured. Do not relax it simply to
  turn this attempt into a pass.
- **Wallet:** this run establishes no new CDK defect or compatibility result.
  The handoff's deterministic gate remains separate evidence.

Before another authorized run, improve the plan-ID collision diagnostic and
consider making selected configuration visible in the compact plan receipt.
Then repeat this same bounded smoke question. Native CLI wrappers, fault
campaigns and additional wallet implementations are not justified by this result.

## Budget and monitoring

The benchmark used 6 completed model steps and 11 tool calls. Peak context was
21,310 tokens. Provider counters report 97,964 processed tokens: 71,168 cached
input, 21,407 other input, 3,456 output and 1,933 reasoning. These counters exclude
the coordinating Codex work and are not a dollar-cost estimate.

The coordinator initially mistook the unchanged event log for a pending model
response and continued polling for several minutes after the runner had ended.
Checking the process result and completion files established the real cause.
No model/provider stall is demonstrated, and the proposed manual stop sent no
signal because the process was already absent. Future monitoring must check
terminal status and limit-reason files before interpreting event silence.

## Provenance and evidence

The deployed controller matched the handoff:
`sha256:4837bb105642b89189b32f5c4c3c8638f41b545e8adf7b1d4b809be93a3b675a`.
The catalog advertised CDK CLI 0.18.0, source commit
`d3dec24c784e8fec1fd65f853241c7a2261c7abd`, wallet image
`sha256:bc4ec6943eb505bb7eb5a6d43ddebf0297fe00f70775378e33ae85c26eb6a5a8`.
That wallet image was not launched by this session. Existing wallet-agent edits
were retained; no wallet or controller code was changed for this attempt.

Local artifacts are under
`dev/agent-usability-runs/cdk-wallet-smoke-01-20260905/`: manifest, events,
metrics, session export, limit reason, independent review and evaluated scorecard.
Event references: 15 (surface discovery), 16 (first plan), 19–24 (binding and
identity correction), 27–28 (claimed but absent configuration change).
Final audit:
`dev/agent-usability-runs/cdk-smoke-checkpoint-20260905/cluster-final.json`.
The runner edit passed `bash -n`, prompt preview and whitespace checks. No
unchanged runtime test suites were rerun for this reporting checkpoint.
