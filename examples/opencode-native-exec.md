# Native command smoke test with OpenCode

Run `storm setup`, then open OpenCode from your project's directory:

```sh
storm agent open opencode
```

Give the agent this request:

> Use the Proofstorm MCP tools to create one temporary cell with a single
> Bitcoin Core regtest node. Discover the available tools and catalog first;
> use the advertised schemas rather than guessing tool names or arguments.
> Wait for readiness, then run `bitcoin-cli getblockchaininfo` inside that
> component. Wait for the terminal operation result and confirm it reports the
> regtest chain. Do not use host Docker, Kubernetes or component commands.
>
> Report the cell name, operation ID, exit code and observed chain. Distinguish
> a successful command from any unverified transaction behavior. Remove only
> the cell you created and confirm teardown. Do not print credentials or secrets.

This is a manual agent smoke test, not a scored benchmark. For repeatable
runtime acceptance without a model, use `just e2e smoke` from the checkout.
