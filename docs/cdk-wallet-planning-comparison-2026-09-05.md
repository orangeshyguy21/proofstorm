# CDK configuration: bounded planning-only comparison

**Kimi passed the simplified planning diagnostic. Qwen failed on a different
argument-format error. Both included `input_fee_ppk=0` in their tool arguments.**
This does not reproduce a universal client/tool-path failure that drops config.
The earlier full-smoke omissions remain observed failures, with their precise
cause unresolved.

| Session | Outcome | Seconds | Steps / tools | Peak context | Processed tokens |
| --- | --- | ---: | ---: | ---: | ---: |
| `cdk-plan-k3-01-20260905` | Pass | 71 | 5 / 4 | 13,263 | 47,589 |
| `cdk-plan-qwen-01-20260905` | Fail; no stored plan | 164 | 6 / 8 | 12,098 | 55,776 |

## Controls

Fresh OpenCode sessions ran serially using `kimi-for-coding/k3` and the available
local alias `ollama/qwen3.8:27b-mlx`. They received identical prompts except for
run identifiers, the same release MCP binary and native toolset, and identical
limits: 180 seconds, 12 steps, two equivalent plans. Those equalities were checked
against the retained manifests. Actual server-side grants were restricted to
`catalog.read,lab.create,lab.read`, so provisioning was not authorized.

The new `cdk-wallet-plan-config` scenario asks only for a topology and verification
of its explicit zero-fee configuration. It requires no lab apply, runtime action,
experiment export or teardown. The runner now accepts a scenario-scoped capability
override; existing scenarios retain their original grants. Shell syntax, prompt
preview, generated effective permissions and whitespace checks passed.

## Kimi observations

Both calls (events 9 and 13) included `config: {"input_fee_ppk": 0}` on the CDK
0.18.0 mint. The first used incorrect/duplicate links. Kimi corrected these in a
second request while preserving the configuration. The final receipt and an
independent read-only SQLite query confirmed the setting in the stored draft,
along with two LND chain bindings, their peer link and the mint's LND backend.
The final report identified the correct plan and digest. The independent reviewer
and evaluator mark this planning diagnostic passed.

## Qwen observations

Both calls (events 19 and 24) also included the zero-fee configuration, but sent
`policy` as an encoded JSON string where the schema requires an object. The server
rejected parameter deserialization. Qwen recognized this in its text, then
submitted the same arguments again. The repeat guard stopped the run with exit
143; the database contains no draft.

It also chose `cdk-ldk`, the embedded Lightning implementation, despite the prompt
requiring the mint to bind to an LND node. Parameter parsing failed before semantic
validation, so no server-side topology validation was reached. This is not the
same config-omission failure seen in the earlier smoke sessions.

## Interpretation and next step

Successful submission and persistence by Kimi demonstrate that the current tool
path can carry the required setting. Qwen's rejected arguments further show that
its requests also contained the setting. These observations argue against a
universal configuration-stripping defect; they do not rule out conditional
model/provider/client behavior or isolate why the larger task failed.

The simpler prompt, narrower objective and reduced grants differ from the full
smoke scenario. Tool discovery is capability-filtered, so reduced grants also
narrowed the advertised tool menu. This comparison does not isolate prompt
complexity from tool-menu size. One trial per model/provider path is insufficient for a model
ranking. The local model alias is recorded as served, not independently verified
as a weight-level identity. No wallet funding, payment, restart or recovery was
tested.

The next useful experiment would separate plan verification from runtime work:
require a valid stored plan with the intended backend and fee configuration before
starting a bounded native wallet session. Any assisted setup should be reported as
such; it would test wallet operation rather than fully autonomous lab design.
No further experiment has been dispatched.

## Cost and evidence

The comparison used 235 reported agent seconds and 103,365 processed tokens,
including 32,256 cached input tokens. Other input was 64,813, output 5,807 and
reasoning 489. Counters exclude coordinator work and are not dollar-cost estimates.

Each run directory under `dev/agent-usability-runs/` retains its manifest, events,
metrics, reviewed scorecard and `stored-plan-verification.json`. The latter records
an independent read-only query of the drafts after session completion. Both final
cluster audits report verified idle, with no resources to clean up. Both benchmark
processes completed, and no replacement session remains running.
