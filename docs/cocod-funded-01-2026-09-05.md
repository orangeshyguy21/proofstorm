# Cocod first focused funded run

Run `cocod-funded-01-20260905` did not reach wallet funding. Assisted liquidity setup and protected cocod initialization succeeded, including observed session readiness and private passphrase permissions. Cocod created the requested 5,000-sat invoice. Three identical LND funding invocations exited 1; the mint recipient invoice remained OPEN at the last observation. No issuance, 700-sat payment or passive 4,300-sat balance was demonstrated.

The agent invoked `lncli payinvoice` without `--force` or `--json`, and never consulted that command's help. The existing deterministic cocod gate uses both flags. This is a concrete invocation difference and a leading hypothesis, not evidence of the graph-propagation problem the agent speculated about. Private payment output was not retrieved. After two equivalent failures, changing only output projection did not justify a third identical mutation.

Projection discovery also consumed steps: an unsupported fee field was rejected before execution; guessed invoice amount fields failed closed twice before a state-only projection succeeded. No raw payment-output fallback occurred. These are native CLI and observation-discovery findings, not a demonstrated cocod money-flow defect or a reason for a wallet-specific payment wrapper.

There were 23 journal actions: 21 native executions (17 exit 0 and four exit 1) and two typed actions (liquidity setup and restart). All native receipts had verified cleanup and complete, untruncated streams. The agent's final report incorrectly counted 16 native executions and omitted the failed tracking command from its exit count. Its financial non-claims were appropriately explicit, but its reporting gate remains failed.

The agent observed normal verified teardown and completed its report inside the unchanged 600-second/50-step cap, without operator rescue. Independent audit verified the cluster idle. Usage: 46 model steps, 52 tool calls, peak context 60,997 tokens, 1,621,666 processed tokens including reused context.

Evidence is retained under `dev/agent-usability-runs/cocod-funded-01-20260905/`. In `events.jsonl`, lines 22 and 70 establish setup, 77 invoice creation, 85/122/130 failed executions, 115/130 OPEN recipient state, 153 verified close and 156 the final report. `review.json`, `reviewer-audit.json`, `scorecard.json` and `operator-cluster-after.json` retain the independent review. Assessment: execution failed, target inconclusive, evidence insufficient, proficiency failed.

Next: narrowly confirm the noninteractive LND invocation difference, then review a corrected native CLI setup hint before another bounded funded run. Preserve this failure and the unchanged caps. Do not retry any old payment or expand wallet/fault scope to compensate.
