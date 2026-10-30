# Experimental Spark integration

## Current status

The images build from pinned sources with recorded patches. They are not yet catalog
components, nothing is published, and there is no managed acceptance gate.

A Docker Compose probe (2026-10-29, native ARM64) passed:
- a real BOLT11 mint and melt through a CDK 0.18.2 mint, this processor, three Spark
  operators, open-ssp and LDK Server
- an unreachable melt refused
- an unpaid quote surviving a processor restart
- mint and melt after an SSP restart

Every payment was confirmed independently on the counterparty LDK node. Images built
from these exact recipes then passed the same checks.

The intended topology:

`cdk` → `cdk-spark-processor` → `open-ssp` → `ldk-server@0.1.0-762bc47` → Lightning peer,
with the processor and `open-ssp` both using a three-operator `spark-operator`
federation backed by `bitcoin-core` and PostgreSQL.

The processor serves only `bolt11` in `sat`: no amountless invoices, descriptions or
MPP. No component needs Esplora.

## Pinned inputs

| Recipe | Selection | Source | Archive SHA-256 |
| --- | --- | --- | --- |
| [Dockerfile.spark-operator](Dockerfile.spark-operator) | `spark-operator@0.0.0-5e0c879` | [benthecarman/spark](https://github.com/benthecarman/spark/tree/5e0c879de2775f6a34aea6d3b0c455d065a3a70f) | `af3ba244…2fe1f3` |
| [Dockerfile.open-ssp](Dockerfile.open-ssp) | `open-ssp@0.1.0-9c25f69` | [benthecarman/open-ssp](https://github.com/benthecarman/open-ssp/tree/9c25f69924490c22a66f03a98e5840015383dc79) | `57e8b0b7…18bd85` |
| [Dockerfile.cdk-spark](Dockerfile.cdk-spark) | `cdk-spark-processor@0.1.0-d7244e2` | [cdk-payment-processors `crates/spark`](https://github.com/cashubtc/cdk-payment-processors/tree/d7244e261506b51d5b309a558bf19cd4ffc32f13/crates/spark) | `54cd1fba…f75792` |
| [Dockerfile.ldk-server-762bc47](Dockerfile.ldk-server-762bc47) | `ldk-server@0.1.0-762bc47` | [ldk-server](https://github.com/lightningdevkit/ldk-server/tree/762bc473342c8f945475785ec6743a9b26f56aff) | `b3cb9756…3045ed` |

Full digests, lockfile digests and base images are in each `*-provenance.json`.
`catalog-image` checks every identity they name against its recipe.

- **Operator image.** The operator is a fork. open-ssp needs its SSP-only gRPC service
  (`spark_ssp_internal.proto`), which public `buildonspark/spark` does not have. The
  image also carries the Rust FROST signer, built with the Rust 1.92.0 toolchain its
  `rust-toolchain.toml` selects, and `atlas` with both migration directories.
- **open-ssp's vendored sources.** It builds against the exact submodule revisions it
  records: the SDK fork `benthecarman/spark-sdk@2472fdc` and the operator fork above.
- **Spark source archive.** The operator archive is about 120 MB and downloads slowly
  from codeload. Expect several minutes per cold build.

## Processor patches

The processor uses `spark-wallet` 0.26.0 from Breez. Unmodified, it does not work with
open-ssp. Two patches are applied with zero fuzz. Each image records them in
`/usr/local/share/processor-patch-sha256`.

1. [`cdk-spark-no-auto-optimize.patch`](patches/cdk-spark-no-auto-optimize.patch): one
   line. It turns off automatic leaf optimization (hard-coded on, multiplicity 5).
   - **Why:** after any payment that creates change, the optimizer asks the SSP to
     split it into many leaves. open-ssp rejects swaps of more than 32 targets, but only
     after the transfer has started. That leaves the change `SENDER_KEY_TWEAK_PENDING`
     for about 5 minutes, and melts fail with `InsufficientFunds` in that window.
2. [`cdk-spark-wait-for-send.patch`](patches/cdk-spark-wait-for-send.patch): after the
   SSP accepts a send, `make_payment` polls its state every 250 ms for up to 30 s.
   - **Why:** the processor emits no `PaymentSuccessful`/`PaymentFailed` events, and
     open-ssp settles Lightning asynchronously. CDK 0.18.2 therefore left every melt
     `PENDING` after its 60 s wait, and cdk-cli's `melt` never returned.

Both belong upstream, along with open-ssp checking the target count before the transfer
starts. Drop each patch when its fix is released.

## LDK Server versions

open-ssp's LDK client authenticates with macaroons, so it needs LDK Server `762bc47`.
Both the pinned and current CDK LDK Server processors use `ldk-server-client@50fe752`
with an API key, so `ldk-server@0.1.0-50fe752` stays for them.

Against `50fe752`, open-ssp exits at startup with `Invalid macaroon size or version`.
The two versions are not interchangeable. Phase 2 must record which consumers link to
which version.

## Known limitations

- Melts carry no `payment_preimage`, so wallets get no payment proof.
- A failed Lightning send can leave the processor's Spark funds locked:
  - In the probe, ldk-server refused an unroutable invoice and open-ssp logged
    `Lightning submission outcome unknown`.
  - The mint correctly marked the melt unpaid.
  - But the 500-sat preimage swap to the SSP stayed pending, with a 16-day expiry.
- AMD64 has only the CI native builds below. The payment probe ran only on ARM64.

## Runtime notes for the managed components

- **Processor.** It reads `config.toml` from its working directory. The operator list
  and SSP settings need that file, since environment variables cover only scalar
  settings, so start it from the directory holding its configuration.
- **Operator.** Each operator applies both `atlas` migration directories to its own
  main and ephemeral databases. It then starts `spark-frost-signer` on a Unix socket
  and runs `spark-operator` with that socket, its identity key, the shared operator
  list, the threshold, and `-ssp-grpc-port`. The image's `CMD` is only a default.
- **Federation readiness.** The federation is usable only once every operator has
  `AVAILABLE` signing keyshares. In the probe that took 16 s.
- **open-ssp.** It refuses to start without `SPARK_ADMIN_TOKEN`. After the first boot,
  set `SPARK_MNEMONIC_REQUIRED=1` so a missing seed cannot silently create a new
  identity.

## Build and validation

Native builds and offline probes run in the
[component image workflow](../../.github/workflows/component-images.yml) on AMD64 and
ARM64 for pull requests that change these recipes. Locally, on a clean checkout:

```sh
bash scripts/catalog-image.sh build spark-operator@0.0.0-5e0c879 linux/arm64 ../spark-operator-work
```

Use a native builder for each platform; emulation does not establish native support.
Nothing is published by these commands.
