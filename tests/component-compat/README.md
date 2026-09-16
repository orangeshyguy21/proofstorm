# Pre-merge image compatibility

These tests run exact images before they are admitted to the installed catalog.
They create fresh, private Docker regtest networks, publish no ports, and remove
only resources with their own run label. Existing Proofstorm installations are
not used or modified. Evidence directories must be new and absolute.

```sh
just check-component-compat /absolute/matrix.json linux/arm64 /absolute/new-evidence
```

The matrix contains `bitcoin` and `lightning` arrays. Every entry names an exact
`version` and `image`; Lightning also names `implementation` (`lnd` or `cln`). Use
`lightning.json` for the reviewed upstream pins. Locally built Bitcoin image IDs
can be read from `catalog-image` build receipts. The runner resolves all inputs
once, verifies their architecture, executes immutable image IDs, and checks
native versions. It runs every Bitcoin/Lightning pair with the arguments from
the checked-in render fixtures: fund, open a channel, settle payments, restart
Bitcoin and the sending node, verify persisted identity/state, and settle again.
CLN exercises both `pay` and the Nutshell 0.21 `xpay` contract.

To qualify Nutshell, select one Bitcoin version and add:

- `driver`: the locally built `Dockerfile.proofstormd` `native` target.
- `mints`: exact Nutshell images with `implementation: "nutshell"`.
- `wallets`: exact Nutshell images with `implementation: "nutshell-wallet"`.

The supported test contracts are 0.20.3 and 0.21.0. Every selected mint runs
against every selected Lightning backend and every selected wallet. Tests use
rendered configuration, SQLite, native mint/wallet entrypoints, the actual Rust
driver, management mutual TLS, and the restricted CLN rune. They pay a real
regtest invoice to issue tokens, transfer tokens between separate wallets, melt
to an independently observed Lightning invoice, and verify balances and rune
identity after process restart. Their scope does not include PostgreSQL, Redis,
OIDC, Kubernetes network policies, or controller admission and mutation flows.
CDK 0.18 mint/backend qualification remains separate from this Nutshell matrix.

Wallet entries can also select `implementation: "cdk-cli-wallet"` at 0.18.0.
Those cases check native quote persistence and resumption, real issuance,
token transfer, the driver's native melt receipt, independent invoice settlement,
and identity/balance preservation after restart. CI covers the CDK 0.18 wallet with
both Nutshell mints over the preferred LND release. This does not qualify a CDK
mint or claim every possible wallet/mint/storage topology.

CDK 0.17 is excluded for both mints and wallets. The support window starts at 0.18
and expands to two families when 0.19 qualifies, retaining 0.18 at that point.

The pull-request workflow runs on native Linux ARM64 and AMD64, using build
receipts as inputs and retaining structured results even after failures. No
images are published. Local AMD64 emulation does not replace the native CI gate.
A failed case or failed owned-resource cleanup makes the command fail; inspecting
a partial directory is not sufficient evidence that a matrix passed.

Private raw logs can contain disposable seeds, invoices, tokens and runes. The
workflow uploads only selections, image identity and structured pass/fail
receipts from these live tests. Keep raw local evidence private. Successful
artifact compatibility is required for promotion but is not, by itself, full
Proofstorm product qualification or an upgrade/downgrade claim.
