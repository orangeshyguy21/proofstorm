# CDK smoke retry after planning fixes

The authorized retry again stopped before materialization. CDK runtime behavior
remains untested by these two agent sessions. No further run was launched; the
retry's final cluster audit confirms verified idle with no blockers.

## Changes and validation

Plan receipts now include each component's authored `config`, including an empty
object when no overrides were submitted. These are not expanded backend defaults.
The tool description asks agents to verify configuration before applying a plan.

Draft creation now returns a domain conflict for a duplicate draft ID instead of
a raw SQLite constraint error. The plan tool explains that the original plan is
unchanged and tells the agent to use a new plan ID and idempotency key for changed
content. Exact request retries still replay successfully.

Tests verify preserved authored configuration, empty configuration visibility,
exact replay, rejection without overwriting the original, and successful recovery
with a new plan ID. All 65 tests in the selected MCP/store targets passed
(52 MCP unit, 3 stdio, 10 store). Strict Clippy, formatting, whitespace checks and
the release MCP build passed. No wallet image or controller deployment changed.

## Live result

Run `cdk-wallet-smoke-02-20260905` used fresh Kimi K3 with the same native scenario
and limits: 900 seconds, 60 steps, two equivalent plans. The agent was explicitly
told to begin cleanup by 720 seconds or step 48.

- Event 14: the first plan used correct backend bindings but omitted the mint's
  required zero-fee override. The receipt visibly showed `config: {}`.
- Event 17: the agent said it had forgotten `input_fee_ppk=0` and would replan.
- Event 18: the next request changed only plan ID and idempotency key. It still
  contained no configuration override, and the receipt again showed `{}`.
- The repeat guard correctly stopped the session with exit 143 after 104 seconds.

There were no tool errors, no lab creation and no wallet operations. The new
configuration visibility was verified live; the new conflict error was not
triggered in this retry. The original binding mistake did not recur, but two
sessions cannot establish why that changed. The wallet question is inconclusive,
and the overall agent benchmark fails.

## Cost and stopping decision

This retry used 5 completed steps, 7 tool calls and peak context of 18,168 tokens.
Reported processed tokens total 71,471: 50,432 cached input, 18,019 other input,
1,586 output and 1,434 reasoning. These exclude coordinator work and local checks.

Together, the two CDK attempts consumed 220 reported agent seconds, 18 tool calls,
11 completed steps and 169,435 processed tokens, with no laboratory materialized.
Completion was detected promptly in this retry using process status and the
runner's completion files, alongside incremental event inspection.

Another identical live attempt is not justified. The next useful diagnostic is a
bounded planning-only comparison: can another agent submit the required explicit
configuration through the same tool schema? Preserve these two failures and keep
the repeat guard. A successful setup can then justify another wallet smoke run;
adding wallet wrappers or expanding fault coverage does not address this blocker.
No such comparison or further smoke run has been dispatched.

Evidence is retained under
`dev/agent-usability-runs/cdk-wallet-smoke-02-20260905/`, including the source and
binary identities, events, independent `review.json`, evaluated scorecard and
`cluster-after.json`. Validation logs are in
`dev/agent-usability-runs/cdk-smoke-checkpoint-20260905/proofstorm-cdk-plan-*.log`.
The controller remains pinned to image digest
`sha256:4837bb105642b89189b32f5c4c3c8638f41b545e8adf7b1d4b809be93a3b675a`.
Changes remain uncommitted in the working tree.
