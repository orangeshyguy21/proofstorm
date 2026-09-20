# LDK Server payment processor

The first external CDK payment processor profile uses three independently
controlled components:

`cdk` → `cdk-ldk-server-processor` → `ldk-server` → `bitcoin-core`

The CDK mint remains the normal `cdk` implementation, with SQLite or PostgreSQL
storage. Its payment links select the gRPC transport. The existing `cdk-ldk`
implementation continues to select CDK's embedded node.

Use [the complete example](../../examples/ldk-server-cell.json) with the ordinary
cell planning and application workflow. It includes a second LDK node for
funding and receiving payments, and a CDK CLI wallet. No new MCP tools are needed.

## Exact profile

| Component | Version | Upstream source |
| --- | --- | --- |
| CDK mint | `0.18.1` | [CDK](https://github.com/cashubtc/cdk/tree/v0.18.1) |
| Processor | `0.1.0-fe468ca` | [CDK payment processors](https://github.com/cashubtc/cdk-payment-processors/tree/fe468cad486157683eddbc0df4ff87ba71b6c0a3/crates/ldk-server) |
| Node and CLI | `0.1.0-50fe752` | [LDK Server](https://github.com/lightningdevkit/ldk-server/tree/50fe7523be3529d86bfee0dfc35df9a52aca7310) |

Both new components are experimental and require an explicit version. The
processor implements CDK payment protocol `4.0.0`, matching this CDK mint release.
Each mint-to-processor and processor-to-node connection has two `payment_backend`
links: `bolt11/sat` and `bolt12/sat`, both targeting the same component. This fixed
profile advertises both methods; mixed native/gRPC endpoints and partial method
sets are rejected before deployment. Other processors and arbitrary remote
endpoints are outside this profile.

LDK reports `msat` internally; CDK converts the supported mint-facing `sat`
amounts. The processor supports amountless invoices and descriptions, but not
MPP. Fees are configured on the processor through `fee_reserve_min_sat`,
`fee_reserve_percent` (0.01 means 1%), and `max_payment_scan_pages`.

## Storage and authentication

LDK Server owns its persistent `/data` volume, including node identity, channels,
payment history, API key, and generated TLS certificate. The processor mounts
that volume read-only and loads the raw API key into its native process
environment; the key never enters authored configuration or command arguments.
The mint has no access to the node's volume.

Proofstorm creates a separate mutual-TLS identity for each processor. The mint
receives only the CA and client certificate/key. The processor receives separate
server and local readiness-client projections. Secrets are preserved on normal
reconciliation and restarts. The CA signing key is discarded. Endpoints come
from typed links; plaintext gRPC is disabled. Readiness performs a bounded,
authenticated `GetSettings` call with the protocol header and checks the expected
methods and unit.

The processor itself is ephemeral. Its upstream implementation consults durable
LDK history for payment status, subject to its configured history scan bound.
The live event stream does not replay payments missed during an outage. After
reconnect, check original mint and melt quotes through their normal Cashu status
endpoints to reconcile payments; a wallet waiting only for a notification can
time out, including while the subscription is reconnecting.
Do not treat a timeout as evidence that a payment failed, or retry a monetary
operation without reconciling the original quote and independent recipient state.

## Native controls

Run native commands through `cell_exec` on the selected component. On a node:

```sh
ldk-server-cli --config /config/config.toml --base-url 127.0.0.1:3536 --help
ldk-server-cli --config /config/config.toml --base-url 127.0.0.1:3536 get-node-info
ldk-server-cli --config /config/config.toml --base-url 127.0.0.1:3536 get-balances
```

The CLI reads its private credentials from the node configuration and storage.
Use explicit `sat` or `msat` suffixes in monetary arguments. Funding, peer and
channel operations, BOLT11/BOLT12, held invoices, and payment details use the
native CLI. A send acknowledgement is asynchronous; verify payment details and
recipient settlement. Ordinary component start/stop/restart and network fault
controls apply independently to all three services.

## Build and validation

Both recipes verify a pinned source archive checksum and build with that
project's own frozen Cargo.lock. They retain upstream binaries, run as UID 1000,
and record build provenance. The processor has no upstream version-only command;
its offline image probe checks the executable and source revision marker. Live
readiness separately verifies the actual gRPC service.

Maintainer selectors are `ldk-server@0.1.0-50fe752` and
`cdk-ldk-server-processor@0.1.0-fe468ca` with `just catalog-image`. Publishing
requires the existing explicit namespace confirmation; a local build is not a
published distribution artifact.

The `Component image qualification` workflow builds both selectors on native
Linux AMD64 and ARM64 runners, with a separate job for each image and architecture.
It retains build logs, image receipts, inspection, and offline probe output.
These jobs do not publish images or run the live payment scenario.

`just e2e ldk-server-processor` runs the fresh-runtime scenario: fund a channel,
mint 5,000 sat through BOLT11, melt to an independent recipient, recover an
unpaid quote through an explicit status check after a processor outage, preserve node identity across restarts,
and settle a BOLT12 mint quote. The BOLT12 check establishes payment recognition,
not ecash issuance. It compares the mint's credit to the recipient's actual
received amount: LDK can overpay a BOLT12 offer slightly.
Transport tests cover mutual authentication, wrong hostnames,
protocol headers, invalid settings, RPC failures, oversized replies, and timeouts.
