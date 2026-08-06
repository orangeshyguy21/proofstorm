# scenarios/

Adversarial scenarios for the proofstorm regtest harness. Each scenario is a
concrete attack with an automated **oracle** (the property the mint must
uphold). Full threat model, topology, and the attack/oracle catalog are in
[`../SPEC.md`](../SPEC.md).

## Running

The regtest stack must be up and funded first:

```bash
make regtest-build   # first run (or after docker/adversary changes)
make regtest-up      # bitcoind + 2 LND + cdk-mintd + nutshell + adversary
make regtest-fund    # mine chain, fund LND, open channel, start block-miner
make attack                      # all built scenarios vs the CDK mint
make attack MINT=nutshell        # same, vs the Nutshell mint
make attack SCENARIO=double-spend-race MINT=cdk   # one scenario
```

`MINT=cdk|nutshell` selects which mint is under attack; both are up at once, so
you can run each scenario against both and compare (SPEC §2).

## Built scenarios (automated oracle)

| File | SPEC | Attack | Mint must… |
|---|---|---|---|
| `double-spend-replay.sh` | A1 | redeem the same token twice, serially | reject the 2nd; no balance inflation |
| `double-spend-race.sh` | A2 | two independent clients redeem the same proofs at once | accept exactly one |
| `mint-quote-flood.sh` | D1 | flood `/v1/mint/quote/bolt11` | keep serving an honest `/v1/info` prober |

Exit code is the result: `0` = mint upheld the oracle; non-zero = oracle
violated (or, for A2, an inconclusive both-failed run — reported distinctly).

## Spec'd, not yet built

A3 cross-mint replay, A4 forged blind signature, A5 melt/swap race, A6 overspend
split, A7 underpay melt, D2 connection flood, D3 LN-liquidity exhaustion, D4
large-payload. See `../SPEC.md` §3–§4 for each mechanism and oracle. The
signature/crypto ones (e.g. A4) need a raw-protocol client that crafts BDHKE
messages and are intentionally not shipped as half-working crypto.

## Category B — long-duration (not for CI)

Multi-hour/day liveness, keyset rotation under sustained load, slow-drip
double-spend across many blocks, channel exhaustion over time. These are the
tests that CI timeouts make infeasible. Gate any long-runner behind
`PROOFSTORM_LONG=1` and a duration budget; never wire them into CI.

## Adding a scenario

1. Create `scenarios/<name>.sh`, `source lib/attack.sh`.
2. Run the attack via `adv <workdir> <cdk-cli args…>` (wallet-driven) or `curl`
   against `${MINT_HOST_URL}` (raw HTTP).
3. Assert the oracle with `assert_fails` / `assert_eq`, or a custom check.
4. Add the name to `BUILT` in `../scripts/run-attack.sh` and the table above.
