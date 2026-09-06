# Cocod focused restart checkpoint

Run `cocod-restart-01-20260905` independently verified the scoped restart target. Agent reporting proficiency remains failed because the final report attributed native exit/cleanup fields to all 25 journal actions. The correct count is 20 native executions and five typed actions. This reporting defect does not require repeating wallet mutations.

The agent observed a running protected session, passive zero balances and a structured identity hash; after component replacement it observed healthy daemon, locked/stopped session, unchanged identity and passive zero. After private explicit unlock, it actually observed running/available/passphrase-required status and all four passive balance categories at zero. Native exit success alone was not used to infer that final state.

All 20 native executions exited zero, with verified cleanup, complete streams and no truncation. The five typed actions were two component restarts and three passive balance reads. The agent observed normal lab close with verified absence and completed a final report without operator rescue. Independent cluster audit verified idle.

Evidence is retained under `dev/agent-usability-runs/cocod-restart-01-20260905/`: `events.jsonl` lines 88, 103, 109 and 116 establish the state sequence; line 135 establishes verified close; line 138 contains the final report and its overstatement. `review.json`, `reviewer-audit.json`, `operator-cluster-after.json` and `coordinator-decision.json` retain independent assessments. The evaluator records execution valid, target held, evidence insufficient and proficiency failed; the reporting gate was not waived.

The run used 40 model steps, 51 tool calls, peak context 54,624 tokens and 1,332,000 processed tokens (including reused context). Initial root help was hidden by private output and then repeated publicly, a small discovery inefficiency. No wallet/runtime defect was reproduced. Review inspected public evidence without retrieving private recovery material for a known-secret scan.

This is a separate focused case. Earlier incomplete full lifecycle runs remain incomplete. No funding, payment, second-wallet isolation or general crash-recovery conclusion is claimed. The next preview is `cocod-funded-preplanned`: assisted existing liquidity setup, native 5,000-sat issuance, one 700-sat payment, independent recipient settlement and passive 4,300-sat balance with no reserved/inflight proofs, under unchanged budgets.
