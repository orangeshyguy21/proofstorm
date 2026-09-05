# CDK wallet smoke with verified planning

**The native funding, payment and restart milestones succeeded. The overall
agent benchmark fails its budget, process-supervision and output-confidentiality
requirements. Teardown completed and the cluster is idle.**

Run: `cdk-wallet-preplanned-01-20260905`, fresh Kimi K3, native toolset. The harness
preloaded the verified topology through MCP and supplied its exact plan ID and
digest. It matched the earlier verified draft except for the unique plan name.
The agent could apply and operate the plan but could not create or edit plans.
This is an assisted-setup wallet test, not an autonomous lab-design pass.

## Functional observations

| Milestone | Evidence-backed result |
| --- | --- |
| Provisioning | Agent applied the seeded plan; one lab reached ready |
| Lightning setup | Native Bitcoin/LND commands funded regtest and opened an active direct channel |
| Initialization | Both CDK wallets initialized with different seed fingerprints |
| Funding | 5,000-sat Lightning payment succeeded; native mint log reached ISSUED and wallet balance 5,000 |
| Passive baseline | 5,000 available; reserved, pending and pending-spent all zero |
| First payment | Native PAID, 700 sat, fee_paid 0; independent LND invoice SETTLED for 700 |
| First balance | Native and passive balances both 4,300 sat |
| Restart | Wallet restarted; same seed fingerprint and native balance 4,300 |
| Second payment | Native PAID, 300 sat, fee_paid 0; independent LND invoice SETTLED for 300 |
| Final balances | Wallet 1 native/passive 4,000 sat; wallet 2 passive zero, with unchanged distinct fingerprint |
| Cleanup | Evidence exported, lease/experiment closed, verified-absence receipt received; independent audit idle |

The configuration was CDK CLI 0.18.0, CDK mint 0.18.0, LND 0.20.0-beta,
BOLT11/sat and explicit `input_fee_ppk=0`. The wallet image remained
`sha256:bc4ec6943eb505bb7eb5a6d43ddebf0297fe00f70775378e33ae85c26eb6a5a8`.
The controller remained
`sha256:4837bb105642b89189b32f5c4c3c8638f41b545e8adf7b1d4b809be93a3b675a`.
No wallet/controller changes were needed during the run.

## Why this is not an overall pass

1. **Budget discipline:** the agent continued wallet work after the step-48
   cleanup checkpoint. It reached the 60-step hard cap after obtaining teardown
   confirmation, and exited 143 without a final report. The cap was not extended.
2. **Process supervision:** the initial mint ran under `nohup` with CDK's own
   wait-duration, without an OS timeout or captured child exit status. The later
   `ps` inspection failed because the utility was absent. No successful final
   child-process check followed. Restart and namespace teardown establish final
   cleanup, but do not satisfy the requested supervision evidence.
3. **Output confidentiality:** a JSON-specific redactor was applied to LND's
   table-formatted payment output. A payment preimage appeared in the retained
   native output at event 135. Its value is not reproduced in this report.

The agent's intermediate “all checks pass” statement therefore exceeded what it
had verified. Functional settlement and balance observations remain valid.

## Native-tool findings

No wallet mutation wrappers were needed. The agent performed initialization,
funding, issuance and payment through native CLIs, then used portable passive
balance observations and platform lifecycle operations to check the results.

Friction came from guessed data paths, absent `python3` on LND and `ps` on the
wallet, interactive payment confirmation, and parsing the wrong output format.
Some shell pipelines reported an outer exit code that differed from the native
command's printed `EXIT` value. The review used actual outputs and settlement
checks rather than the operation's success phase alone.

The background mint's original log showed issuance. Running `mint-pending`
afterward is not itself evidence that this command completed issuance; the
native balance and passive observation established the resulting funds.

## Budget and next checkpoint

Limits were 900 seconds and 60 steps, with cleanup requested by 720 seconds or
step 48. The measured run was 644 seconds, 60 steps, 80 tool calls and peak
context 59,885 tokens. Reported processed tokens were 2,165,657, including
2,078,976 cached input, 66,077 other input, 16,236 output and 4,368 reasoning.
These exclude coordinator work and are not dollar-cost estimates.

Stop here. Another full smoke run is not the best next investment. First make
native command supervision, output-format handling/redaction and the cleanup
checkpoint reliable. A compact reusable native execution pattern can help
without adding a wallet-specific MCP mutation API. Do not expand into faults,
nonzero fees or concurrency on the strength of a normal restart test.

## Retained evidence

Artifacts live under `dev/agent-usability-runs/cdk-wallet-preplanned-01-20260905/`.
Key event lines: 4/8 (apply/ready), 77/85 (channel and initialization), 135/142/149
(funding, issuance and passive baseline), 163/172 (first payment/settlement),
179/187 (restart and persistence), 194/204/211 (second payment/final balances),
221/225 (export and verified teardown). The independent review is applied to
`scorecard.json`; `cluster-after.json` has `verified_idle: true` and no blockers.

The new fixture and seeding client are stored under `scripts/fixtures/` and
`scripts/seed-agent-usability-plan.py`; the actual request, receipt and client are
copied into each seeded run. Shell syntax, Python compilation, actual seed-plan
creation, equality against the verified topology and whitespace checks passed.
No unchanged runtime suites were rerun. No additional agent session is running.
