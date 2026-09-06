# Cocod structured invoice checkpoint

Run `cocod-structured-01-20260905` verified structured native invoice relay and the scoped 5,000 → 4,300-sat wallet flow. Both invoice producers used direct native commands and shared output projections. No grep extraction, invoice-copy scripts, payment retries or operator rescue were used. The lab was normally closed and the cluster independently verified idle.

| Boundary | Independently verified evidence |
| --- | --- |
| Funding invoice | Direct `cocod receive bolt11 5000`, output mode `bolt11`; 5,000,000 msat, bcrt, valid expiry at payment submission |
| Recipient invoice | Direct LND `addinvoice --amt=700`, output mode `lnd_invoice`; 700,000 msat, bcrt, valid expiry at payment submission |
| Funding and issuance | Exact selected request matched LND payer argv; selected SUCCEEDED/5,000; passive balance 5,000 with reserved/inflight zero |
| Payment | Exact selected recipient request matched native cocod send argv; selected hash matched independent LND lookup; SETTLED/true |
| Final balance | 4,300 available and total ready; reserved/inflight zero |
| Native execution | 12 receipts, all exit 0, cleanup verified, complete untruncated streams |
| Teardown | Agent observed verified absence; independent cluster audit idle |

Raw invoice and payment streams remained private. Invoice projections intentionally expose small validated BOLT11 requests and fixed metadata; they do not provide private Cashu delivery. The producer receipts carried the released runner digest, successful projection and native exit/cleanup evidence. Both selected invoices were checked against exact expected amount, network and submission time, then linked to separate payer operations.

Protected initialization used a private passphrase and native configuration reload before funding. The agent kept the numeric stat result private; independent review confirmed its sole four-byte stdout digest exactly matched the harmless expected text `600\n`, without retrieving private output. Exposing only the safe numeric stat result would make this evidence easier for the agent to interpret.

The agent's report still has an accounting defect: it says 11 native receipts and 11 typed actions. The journal contains 16 actions: 12 native executions and four typed actions (liquidity setup, restart and two passive balances). It also attributes exit 0 to a typed restart, whose receipt instead records `restarted:true`. The financial claims are supported, but the claims gate remains failed. Evaluator result: execution valid, target held, evidence insufficient, proficiency failed. This reporting failure is preserved; it is not a reason to replay payments.

Final harness usage: 32 model steps, 39 tool calls, peak context 44,015 tokens and 928,189 processed tokens including reused context. The existing 600-second/50-step limits and cleanup reserve remained unchanged. Earlier failed and incomplete runs retain their original scores.

Evidence is retained under `dev/agent-usability-runs/cocod-structured-01-20260905/`. In `events.jsonl`, line 62 establishes both projections, 68 funding, 75 issuance balance, 81 native send, 89 recipient settlement/final balance, 108 verified close and 111 the final report. `payment-linkage-review.json` retains amount/network/expiry, exact request/hash association and permission-hash assertions; `reviewer-audit.json` retains native/typed counts. `review.json`, `scorecard.json` and `operator-cluster-after.json` retain the independent assessments.

Frozen build: controller `sha256:da2dc871bc2a6ad53932645e411aa43967405b5640e30c72acb66034c1ad1b49`; runner `sha256:0eb7342a3d18d776ecca4b18f8c0602fbd757e8293bbf55524123f0f26cf32fc`; release MCP `sha256:4e0797b3285a77d44dbb31166138f24e4fa5ebb04b8a44d4a75ef84b05931d00`. Cocod remains the experimental exact source `44e5101cbea370132af6e68f88e01b47e39431c4` and wallet image `sha256:88dc907f64530788280b0ba603b1bd7f361c58281171e74ca25b0676fadfcdc7`.

Stop here. This checkpoint establishes assisted planning/provisioning, native wallet execution and structured small-invoice relay for this zero-fee pairing. Private ecash delivery, mixed-wallet transfers, nonzero fees, interruption and recovery remain separate work. No further scenario was dispatched.
