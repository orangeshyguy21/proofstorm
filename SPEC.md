# proofstorm adversarial spec

**Adversarial agent attempts fund theft and denial-of-service against a
sandboxed CDK/Nutshell deployment.**

This is security testing of our own regtest infrastructure — the same category
as chaos engineering or pentesting your own nodes. Nothing here is softened:
the goal is to make a mint mis-issue value ("steal funds") or stop serving
honest clients ("DoS"), on a throwaway regtest network, so we find the bug
before someone does it on mainnet. The phases below (V0–V3) that ship the
FakeWallet wallet-population runner are documented in `README.md`; this file
covers the adversarial expansion (Phase 6+).

---

## 1. Why this is not already covered by CDK/Nutshell tests

Both upstream suites already test double-spend and concurrency — but **in
process**, not as a network attacker against a deployed daemon. That distinction
is the whole reason proofstorm exists.

Grounded in the actual suites:

| Property tested | CDK | Nutshell | proofstorm |
|---|---|---|---|
| Double-spend rejection | `mint.rs` swap saga, `fake_wallet.rs` | `test_concurrent_swap_same_proofs` (`test_mint_db_operations.py:1027`) | replay/race attacks over HTTP |
| Concurrency / race | `test_concurrent_duplicate_payment_handling` (`mint.rs:128`, `JoinSet`) | `test_db_verify_spent_proofs_and_set_pending_race_condition` (`asyncio.gather`) | N independent clients racing over the wire |
| Melt-quote pending race | melt saga tests | `test_concurrent_set_melt_quote_pending_same_*` | melt/swap race against real LN backend |
| Rate limiting / DoS | — (config-level) | `test_mint_limit.py` (in-process) | connection + quote floods against deployed mint |
| Cross-implementation | `nutshell_wallet.rs` (cdk wallet ↔ nutshell mint, happy path) | — | cdk wallet **attacking** nutshell mint and vice versa |

The upstream race tests call `ledger.db_write._verify_spent_proofs_and_set_pending(...)`
or spawn tokio tasks **inside one process against one in-memory `Ledger`**. They
prove the locking primitive is correct. They do **not** exercise:

1. **The black-box HTTP surface.** An attacker is an untrusted client speaking
   NUT-XX over the network, not an in-process caller. Request parsing, the HTTP
   framework, the deployed rate limiter, and the real DB connection pool are all
   in scope over the wire and out of scope for a unit test.
2. **Distributed concurrency.** Real double-spend races are N independent
   wallets racing over TCP against a mint fronted by a real LN node, real DB
   locking, and network latency — not `asyncio.gather` in one event loop.
3. **Deployment-level DoS.** Connection floods, unbounded quote creation, and LN
   liquidity exhaustion depend on how the mint is *deployed* (limiter config, LN
   node, bitcoind), which a library test cannot see.
4. **Long-duration behaviour** (Category B below) — infeasible in CI timeouts.
5. **Cross-implementation divergence.** A token minted at CDK and redeemed at
   Nutshell (or an attack crafted by one implementation's primitives fired at
   the other's mint) catches spec-interpretation bugs neither suite sees alone.

---

## 2. Topology

Single regtest network, all local, all throwaway:

```
                         ┌───────────────┐
                         │   bitcoind    │  regtest, txindex, zmq
                         │  (+block-miner)│  mines 1 block / interval
                         └───────┬───────┘
                    ┌────────────┴────────────┐
              ┌─────┴─────┐              ┌─────┴─────┐
              │  lnd-a    │◄── channel ──►│  lnd-b    │
              └─────┬─────┘              └─────┬─────┘
              gRPC  │ :10009            REST  │ :8080
              ┌─────┴─────┐              ┌─────┴─────┐
              │ cdk-mintd │              │ nutshell  │  (mint mode)
              │  :3338    │              │  :3338    │
              └─────┬─────┘              └─────┬─────┘
                    │ NUT HTTP                 │ NUT HTTP
                    └────────────┬─────────────┘
                          ┌──────┴───────┐
                          │  adversary   │  wallets + raw HTTP attacker
                          └──────────────┘
```

**Decision: one combined network, not two isolated stacks.** Because both mints
share one chain and their LN nodes are channel peers, this single topology
supports both testing modes the "against each other vs in parallel" question
poses:

- **Cross-implementation.** A melt at the CDK mint makes `lnd-a` pay a bolt11
  issued by `lnd-b` (Nutshell's node), moving real regtest sats between the two
  implementations' backends. A cdk-cli wallet can mint at Nutshell and attack
  CDK, exercising interop and spec-divergence bugs.
- **Parallel comparison.** Run any attack scenario against `MINT=cdk`, then
  `MINT=nutshell`, and diff the rejection behaviour. Both mints are up at once,
  so no teardown/rebuild between comparisons.

A single balanced channel (`lnd-a ⇄ lnd-b`, half pushed) gives both mints
outbound liquidity, so melt works in both directions.

Isolated-stack mode (each mint on its own bitcoind + LN) is available by running
the CDK and Nutshell mint services on separate compose projects, but is not the
default because it forfeits the cross-implementation tests for no benefit on a
sandbox that we trust to be non-adversarial at the hypervisor level.

---

## 3. What "steal funds" means, concretely

"Steal funds" is not one test. Each mechanism below is a distinct attack with a
distinct oracle (the property that must hold for the mint to be correct). The
oracle is what we assert; the attack is what we run.

| # | Attack | Mechanism | Oracle (mint must…) | Status |
|---|---|---|---|---|
| A1 | **Replay double-spend** | Redeem the same token (proofs) twice, serially | reject the 2nd; population total not inflated | **built** |
| A2 | **Concurrent double-spend** | Fire the same token at the mint from N clients at once | accept exactly one; total inflation == one redemption | **built** |
| A3 | **Cross-mint replay** | Redeem a CDK-issued token at the Nutshell mint (and vice versa) | reject (unknown keyset / bad signature), no issuance | spec'd |
| A4 | **Forged blind signature** | Tamper C_/amount in a swap request | reject with signature/DLEQ failure, no proofs issued | spec'd |
| A5 | **Melt/swap race** | Swap proofs while a melt of the same proofs is in flight | one path wins; no proof both melts and swaps | spec'd |
| A6 | **Overspend split** | Request outputs summing to more than inputs in a swap | reject (amount mismatch), no net issuance | spec'd |
| A7 | **Amountless/underpay melt** | Melt for less LN than the proofs' value implies | conserve value; no free LN out | spec'd |

"Status: built" = a runnable scenario in `scenarios/` with an automated oracle.
"spec'd" = mechanism and oracle defined here; scenario is a documented next step
(some, e.g. A4, require a raw-protocol client that crafts BDHKE messages, which
is deliberately not shipped as half-working crypto).

## 4. What "DoS" means, concretely

| # | Attack | Mechanism | Oracle (mint must stay…) | Status |
|---|---|---|---|---|
| D1 | **Mint-quote flood** | Create mint quotes as fast as possible | live: `/v1/info` answers within SLA throughout | **built** |
| D2 | **Connection flood** | Open many concurrent idle/slow connections | live: honest client still served | spec'd |
| D3 | **LN liquidity exhaustion** | Melt repeatedly to drain the mint's channel | fail melts cleanly once illiquid; no crash, no negative balance | spec'd |
| D4 | **Large-payload / deep-batch** | Oversized swap batches, huge proof arrays | bound the request; reject over limit, stay live | spec'd |

The DoS oracle is **liveness of an honest client**, measured concurrently with
the attack. A mint that rejects the attack but also stops serving honest clients
has still failed.

---

## 5. The three test categories

- **(A) Gaps in existing integration tests** — §1 is the grounded gap list from
  reading both suites. proofstorm adds the black-box HTTP + distributed +
  cross-implementation dimensions the in-process suites structurally cannot.
- **(B) Tests infeasible in CI** — long-duration: multi-hour/day mint liveness,
  keyset rotation under sustained load, slow-drip double-spend across many
  blocks, channel-exhaustion over time. Gated behind `PROOFSTORM_LONG=1` and a
  duration budget; never run in CI. See `scenarios/README.md`.
- **(C) Red-team agent tests** — §3 and §4. An agent (or script) with wallet
  access and raw HTTP tries the attacks above; the harness asserts the oracle.

---

## 6. Oracles reuse conservation

Every fund-theft oracle reduces to value conservation, which proofstorm already
computes (`scripts/check-conservation.sh`, `wallet_population_total_sat` in
`scripts/lib/wallet.sh`). An attack "succeeds" (mint is broken) iff the
population's spendable total increases without a corresponding paid mint quote,
or the mint's LN backend pays out more than the melted proofs' value. Attacks
therefore assert: command failed **and** population total unchanged (or changed
by exactly the one legitimate redemption).
