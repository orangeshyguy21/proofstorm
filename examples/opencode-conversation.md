# OpenCode proof-of-concept conversation

Run `tools/proofstorm-cluster setup`, then start OpenCode from the Proofstorm
repository with:

```bash
OPENCODE_CONFIG=examples/opencode.json opencode .
```

Give the agent this request:

> Use only the Proofstorm MCP tools for laboratory and network control. Do not
> invoke Docker, Kubernetes, Helm, or component CLIs. Discover the installed
> component catalog and network-fault backend. Do not request latency or packet
> loss unless that backend explicitly advertises the feature, direction, and
> required bound. Then create an empty lab draft containing Bitcoin Core, two LND
> nodes, one Core Lightning attacker node, one CDK mint, and two independent
> Nutshell wallets. Use
> separate logical IDs and the catalog's advertised service and configuration
> versions. Add the required chain-backend links from all three Lightning nodes
> to Bitcoin Core and the Lightning-backend link from the mint to its LND node. Validate,
> publish, and materialize the lab. Wait until it is ready, create an experiment,
> and acquire an exclusive lease with enough time and action budget for the
> workflow. Bootstrap regtest liquidity, explicitly connect the Lightning
> peers, and open a bounded reverse channel. Initialize the Cashu wallet, inspect
> its sanitized balance, fund it with 1,000 sat through the payer LND node, and
> inspect the balance again. Initialize the second wallet, create a bounded
> private receive quote for it, pay that quote from the funded wallet, and wait
> for both the payment action and receive quote to settle. Inspect the sanitized
> quote lifecycle without requesting the Lightning invoice. Stop the payer LND
> node and confirm that its logical status is stopped without making the lab
> unusable, start it and wait for readiness, then restart it and confirm the
> lifecycle artifact reports a completed restart. Use the bounded Proofstorm
> reachability oracle—not a shell or component CLI—to prove both persistent
> wallets can reach the mint's advertised HTTP service, then partition each
> wallet from the same mint with two separate operations and use the same oracle
> to verify both connections are blocked. Heal the first partition by its
> operation ID, prove only that wallet recovers while the second remains blocked,
> then heal the second partition and prove both connections work. Build a
> three-node Lightning
> cycle involving the mint LND, select distinct incoming and outgoing channels
> by their opaque Proofstorm handles, and rebalance 100,000 sat with a bounded
> fee. Verify the returned balance deltas without requesting native channel IDs
> or payment material, then close the temporary bridge. Cooperatively close both
> existing channels using their opaque Proofstorm channel handles, disconnect
> the Lightning peers and verify both endpoints are disconnected, then reconnect
> them. Open a fresh bounded channel and force-close it; report confirmed closure
> separately from pending CSV resolution. Connect the Core Lightning attacker
> to the mint's LND node, open and cooperatively close a mixed channel,
> disconnect and reconnect the mixed peers, then open and force-close a second
> mixed channel. Read the ordered action
> journal and run a conservation oracle using the observed balance. Finally release the lease,
> close the experiment, and export its Proofstorm evidence bundle with all oracle
> artifacts plus the wallet payment artifact. Verify its revision and lock
> digests and canonical journal before closing the lab. Then report the evidence
> digest and verified teardown receipt. Reuse an idempotency key only
> when retrying the exact same request.

Expected outcome:

- every topology and runtime change is represented by a typed Proofstorm tool;
- the agent never receives Kubernetes access or Bitcoin, Lightning, mint, or
  wallet credentials;
- balance artifacts contain amounts and logical IDs, not proofs or mnemonics;
- quote and payment artifacts contain no BOLT11 invoice or adapter quote ID;
- node lifecycle uses logical component IDs and exposes no node credentials;
- overlapping partitions use logical component IDs plus distinct opaque
  operation handles; targeted healing restores only the selected fault;
- reachability observations accept logical endpoints and an advertised service,
  never an arbitrary hostname, port, image, command, or credential;
- network shaping is attempted only after backend discovery, and unsupported
  delay/loss never creates an operation or consumes the lease budget;
- channel artifacts expose opaque Proofstorm handles, never LND funding
  outpoints or CLN channel IDs, and distinguish cooperative settlement from
  pending force-close resolution across mixed implementations;
- rebalance artifacts prove bounded outgoing and incoming balance deltas without
  exposing the private self-invoice, payment hash/preimage, or native IDs;
- the final action journal is monotonically sequenced; and
- the closed experiment exports a bounded content-hashed evidence bundle without
  runtime resource names or private payment material; and
- lab close returns a teardown receipt with `verified_absent: true`.
