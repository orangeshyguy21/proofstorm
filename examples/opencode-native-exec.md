# OpenCode native-protocol acceptance conversation

Run `make setup`, then start OpenCode from this repository:

```bash
OPENCODE_CONFIG=examples/opencode/proofstorm-only.json opencode .
```

Give the agent this request:

> Use Proofstorm MCP only; do not invoke host Docker, Kubernetes, Helm, or local
> component CLIs. Generate one unique run suffix and use it consistently in
> every plan, instance, operation, and idempotency ID.
> Discover compact catalog identities, read only the exact selected entries and
> configuration schemas, then create and materialize a minimal cell
> with two Bitcoin Core nodes, one LND node linked to the first Bitcoin node, a
> CDK mint linked to LND, and a Nutshell wallet. Include `component.exec_live` in the
> cell policy. Omit experiment_id and session_id for ordinary native commands;
> Proofstorm supplies attribution automatically. Use
> `storm_component_exec_live` to run `bitcoin-cli --help`, then use the native
> Bitcoin CLI with the cell-provided RPC environment to call
> `getblockchaininfo` on each Bitcoin node. Prove explicit multi-node selection
> by executing a command in each selected live Bitcoin component. Run
> `lncli --help` and `lncli getinfo` in the LND component and the native
> wallet CLI help in the wallet component. Inspect every terminal operation
> artifact and report its exit code and a concise output summary. Also prove
> the exec workload has no Kubernetes service-account token at
> `/var/run/secrets/kubernetes.io/serviceaccount/token`; treat the expected
> missing file as experiment data. Use `storm_cell_wait` for readiness and
> teardown, and use the paged component-status tool only when exact component
> conditions are needed. Use `storm_operation_wait` for every submitted
> command; do not tightly poll status tools. Do not print mnemonics, macaroons, proofs, or
> private keys. Export evidence before closing
> the cell, since verified deletion purges its local history. Read
> the canonical journal through `storm_action_list` and confirm its
> object-wrapped `actions` page. Reuse an idempotency key only for an identical
> retry. Get expected_instance_key from storm_cell_status, then pass it to storm_cell_close
> and storm_cell_wait with target_phase=closed. Report the exported evidence digest
> and verified teardown receipt.

Passing evidence:

- each native command ran in the target component's immutable locked image;
- artifacts contain an exit code and bounded combined output;
- Bitcoin RPC and native CLI help succeed without a normalized protocol API;
- the first Bitcoin CLI can explicitly target the second Bitcoin node;
- the canonical action list is accepted as object-shaped MCP structured content;
- absence of a Kubernetes token is observed from inside the exec workload;
- the action journal contains every exec request and content-hashed artifact;
- teardown reports `verified_absent: true`.
