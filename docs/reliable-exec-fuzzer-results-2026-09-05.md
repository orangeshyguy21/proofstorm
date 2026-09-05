# Reliable native execution: first fuzzer result

The observed execution contract held, but the overall agent benchmark **failed**:
the 600-second cap arrived before a final report and agent-confirmed teardown.
The lab was subsequently verified absent by the independent cluster audit.
Normal finalizer cleanup completed without operator rescue; the cluster is idle.

One `kimi-for-coding/k3` session ran scenario `reliable-native-execution`, with
600 seconds, 50 steps, and two equivalent attempts as hard limits. It used the
verified, preplanned Bitcoin/LND/CDK-wallet lab without funding or payments.
Run ID: `reliable-exec-fuzzer-01-20260905`.

| Check | Observation |
| --- | --- |
| Native discovery | Safe public LND version and CDK help succeeded in their live components. |
| Exit semantics | Direct exit 7 and shell exit 9 retained their distinct scopes. |
| Private output | Component-generated canary stayed behind empty ordinary streams; selected output exposed only the allowed status and amount. |
| Invalid projection | Malformed JSON failed closed while preserving command exit 3. |
| Cancellation | A marker proved the long command started; cancellation retained signal 15 and verified cleanup. |
| Execution deadline | A separate command timed out with signal 15 and verified cleanup. |
| Owned processes | All 13 accepted executions reached terminal receipts with complete streams, no truncation, and `cleanup_verified: true`. |
| Cleanup boundary | The new-execution probe was refused by the benchmark proxy. |
| Completion | Evidence export, lease release, experiment close and lab-close request succeeded; final report and agent-observed closed receipt were missing. |

The three nonzero exits were deliberate probes. The seven observation-wait
timeouts were distinct from the tested command deadline. No wallet-specific
mutation tools or host commands were needed by the benchmark agent. Shared
execution handles, output policy and cleanup receipts justified the MCP layer in
these cases; no additional wallet wrappers are indicated by this round.

The cleanup threshold was 480 seconds. A wait crossed that threshold; the next
call latched cleanup at 491 seconds but was itself an allowed 45-second wait.
Its response did not announce cleanup mode. The agent probed admission at 543
seconds, cancelled its remaining timer, and requested lab close at 574 seconds.
Only 26 seconds remained for teardown confirmation and reporting.

This supports a bounded harness/usability fix: publish absolute cleanup and hard
deadlines, expose the cleanup transition on allowed observations, and make waits
respect the remaining reporting margin. The missing notification is a plausible
contributor, not a proven sole cause. Keep the existing cap. Repeat this small
reliability checkpoint after that fix before resuming the funded CDK smoke test.
No second run or money-flow test was launched here.

Cost: 600 seconds, 29 completed model steps, 38 tool calls, 651,855 processed
tokens including 608,768 cache-read tokens; peak context 31,029. These are runner
metrics and exclude coordinating-agent usage.

Evidence is retained under
`dev/agent-usability-runs/reliable-exec-fuzzer-01-20260905/`, including
`review.json`, `reviewer-audit.json`, `scorecard.json`, `events.jsonl`,
`cleanup-phase.json` and `cluster-after.json`. The reviewed score is execution
**failed**, scoped target property **held**, evidence **sufficient**.

Limits: no independent descendant inspection, restart fault, or funded flow was
tested. The canary assessment uses request/receipt structure and matching private
capture hashes; its random plaintext was not retrieved for an exhaustive scan.
The complete 13-entry journal export did not embed artifact bodies; the transcript
retains execution receipts. The post-teardown operation table is empty and cannot
independently establish action absence at the moment of admission refusal.

The controller remained pinned to
`sha256:f42a7d9ab93ae4920f8cbf5b3d9933ecec03b19ee406b55de09ba2cff9dfb5c0`;
all 13 receipts used runner
`sha256:a503353d7e028fc3794d72a1229767e0586ef4e384030c99a637994536da75b9`.
This was an uncommitted workspace build; the run retains its source diff and
harness digests.
