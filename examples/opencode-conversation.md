# Agent cell demonstration

From your project's directory, open OpenCode with this installation's tools:

```sh
storm agent open opencode
```

Paste this request:

> Build a cell named demo with Bitcoin regtest, two LND nodes, one Core Lightning
> node, a compatible Cashu mint, and three Cashu wallets: one cdk-cli-wallet,
> one cocod-wallet and one nutshell-wallet. Discover the current catalog and
> supported configuration first; use explicit versions and correct backend links.
> Plan the topology, review the resolved connections and apply the returned digest.
> Inspect readiness and concrete startup conditions. If a component is blocked,
> fetch its logs immediately and report the actual failure and recovery needed.
> Do not assume Pending means an image is being built.
>
> Use native component CLIs to fund the regtest network, connect Lightning peers,
> open funded channels and demonstrate minting and transferring ecash between
> supported wallets. Discover invocation syntax from catalog hints and CLI help.
> Use the reachability oracle and partition/heal tools to demonstrate a wallet
> losing and recovering its mint connection. Verify effects with independent
> observations; distinguish successful command exit from confirmed settlement.
>
> Omit experiment_id and session_id for ordinary native commands and diagnostics;
> Proofstorm supplies attribution. Retain operation IDs and idempotency keys for
> retries. Wait for terminal results and report exit codes, errors and unresolved
> outcomes. Keep the website open so the cell and recorded activity update live.
>
> Add a second mint through a live edit: read the full cell, plan the updated
> configuration at its expected generation, review changes and apply. Preserve
> existing component IDs and links. Verify unchanged components keep their state.
>
> Leave the cell running for inspection. Summarize what worked, what failed, the
> relevant operation IDs and any behavior that remains unverified. Export any
> evidence you want to retain before subsequently closing the cell; deletion
> removes its local activity and releases the name.
