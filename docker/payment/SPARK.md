# Experimental Spark integration

## Current status

The images build from pinned sources with recorded patches. Proofstorm models,
validates and renders the stack, but its catalog entries appear only for platforms
pinned in `crates/proofstorm-core/src/spark_images.json`, which stays empty until the
images are published. The managed `spark-processor` gate and its native image
pipeline are described under [Build and validation](#build-and-validation).

The gate passed a local managed qualification on native ARM64 (Docker Desktop on Apple
silicon) on 2026-10-30, at commit `692da71`. It covered:
- settings and transport refusals
- SSP funding and the channel
- an unpaid quote, a mint paid while the processor was stopped, and two melts, across
  two full-stack restarts with every identity unchanged
- an unreachable melt that was never paid
- cleanup

Melts carried no payment preimage, as documented. AMD64 qualification needs a native
AMD64 runner.

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

The Proofstorm driver starts each service; the images' `CMD` is only a default.

- **Processor.** It reads `config.toml` from its working directory. The operator list
  and SSP settings need that file, since environment variables cover only scalar
  settings. The driver writes it, with the SSP identity read from `/identity`, into a
  memory volume and starts the processor there.
- **Operator.** Each operator pod creates its own main and ephemeral databases, sealed
  to its public key by the database comment (atlas refuses a first migration into a
  database with any extra schema). It applies both `atlas` migration directories and
  runs `spark-operator` with its identity key, the shared operator list, the threshold
  and `-ssp-grpc-port`. `spark-frost-signer` runs beside it on a shared Unix socket.
- **Federation readiness.** An operator is ready when its gRPC health service reports
  its databases ready. The federation is usable only once every operator has
  `AVAILABLE` signing keyshares, about 16 s later in the probe; nothing exposes that
  outside the databases.
- **open-ssp.** It refuses to start without `SPARK_ADMIN_TOKEN`, and it chmods its
  seed file to 0600, which a read-only Secret mount refuses. The driver gives it a
  private copy in a memory volume and always sets `SPARK_MNEMONIC_REQUIRED=1`, so a
  missing seed cannot silently create a new identity. Its `/status` always reports
  `ldk_mode` as `live`.

## Funding

Funding is an explicit step, never part of readiness. Agents run these through
`cell_exec`, and the acceptance gate runs the same steps. open-ssp's admin API is
loopback-only. Its token is passed as a header file the driver writes, so it never
appears in a command:

```sh
curl -sS --fail-with-body -X POST -H @/run/open-ssp/admin.header \
  http://127.0.0.1:5000/admin/spark/deposit-address
```

1. **Spark liquidity.**
   1. Request a deposit address as above. It fails until the operators finish key
      generation, so retry.
   2. Send to it from `bitcoin-core` and mine 3 blocks.
   3. Claim it with `POST /admin/spark/claim-deposit` and the body
      `{"transaction_hex": …, "vout": …}`, taken from `getrawtransaction`.
   4. Check `spark.available_sats` in `/status`.
2. **Lightning liquidity.** On the SSP's `ldk-server` 762bc47:
   1. Fund it on-chain.
   2. Run `open-channel <peer node_id> <peer>:9735 2000000sat --push-to-counterparty 1000000sat --announce-channel`.
   3. Mine 6 blocks.
   4. Wait for `list-channels` to show the channel ready and usable.

The processor's wallet starts empty. Paid mint quotes fund it, from the SSP's liquidity.

## Build and validation

Spark uses the same native image and qualification pipeline as Bark. The scripts
take a stack name, and everything Spark-specific is one row of the stack table in
[`catalog_images/stack.rs`](../../crates/proofstorm-xtask/src/release/catalog_images/stack.rs):

| Recipe | Key in `spark_images.json` | Catalog entry |
| --- | --- | --- |
| `spark-operator@0.0.0-5e0c879` | `operators` | `spark-operators` |
| `open-ssp@0.1.0-9c25f69` | `ssp` | `open-ssp` |
| `cdk-spark-processor@0.1.0-d7244e2` | `processor` | `cdk-spark-processor` |
| `ldk-server@0.1.0-762bc47` | `ldk_server` | `ldk-server` |

The gate is `spark-processor`. Its evidence files are `spark-result.json`,
`spark-plan.json` and `spark-storage-cleanup.json`.

### Native builds

[`spark-images.yml`](../../.github/workflows/spark-images.yml) builds each recipe on a
native AMD64 or ARM64 runner with `scripts/stack-image-build.sh spark`. A manually
dispatched job keeps the complete handoff for 14 days; a pull request keeps only the
build, inspection and probe receipts, because only dispatched builds can be qualified:
- `image.tar`, exported by image ID
- the frozen source in `work.tar.gz`
- the native host record
- build, inspection and probe receipts
- `SHA256SUMS`, written only when the handoff is complete

Nothing is published. The
[component image workflow](../../.github/workflows/component-images.yml) runs it in
three ways:
- For pull requests that change these recipes, and for `family=standard`, on both
  architectures.
- For `family=spark`, on one architecture:

  ```sh
  gh workflow run component-images.yml --ref REVIEWED_BRANCH -f family=spark -f architecture=arm64
  ```

### Managed qualification

`family=spark-qualification` runs
[`spark-qualification.yml`](../../.github/workflows/spark-qualification.yml). It uses
the images of one successful, manually dispatched build run without building them
again. That run can be `spark-images.yml`, or `component-images.yml` with `family=spark`
or `standard`. Pull-request runs are not accepted.

```sh
gh workflow run component-images.yml --ref REVIEWED_BRANCH \
  -f family=spark-qualification -f architecture=arm64 -f candidate_run=BUILD_RUN
```

On a fresh runner, `scripts/stack-qualification.sh spark`:
1. Restores the four handoffs. It rechecks each checksum, the frozen source, the recipe,
   provenance and patches, the native host, the image ID and the offline probe.
2. Starts a loopback registry. It loads each image, probes it again and pushes it, and
   checks that the registry config runs as `1000:1000` with the same root filesystem.
3. Writes those manifest digests into `spark_images.json` for the build platform, in
   that disposable checkout only. `catalog.patch` records the change.
4. Builds the checkout artifacts with `just dev-build`, and the matching controller image.
5. Checks that the cache holds every image that
   `proofstorm-acceptance --image-cache-inputs spark-processor` lists. It copies the
   gate's other images in once.
6. Runs `spark-processor` with `--qualification-image-cache`.
7. Writes `qualification.json`. The gate list must be exactly `[spark-processor]`,
   cleanup and preservation must pass with no volumes left, and the gate's lock must
   use the staged images.

The public artifact holds `status.json` (it must show `stage: complete` and exit 0),
`qualification.json`, the staged pins and candidate receipts, `catalog.patch`,
`controller-source.json`, `cache-inputs.json` and `spark-run-identity.json`. Raw gate
output and runtime homes are never uploaded.

### Local run on an ARM64 Mac

On Apple silicon, Docker Desktop's Linux engine runs ARM64 natively. The same
scripts can therefore build and qualify `linux/arm64` with `--local`, without CI:
- Builds record the real host (`Darwin`/`arm64`) in `native.json`. Only a `--local`
  restore accepts it; CI qualification never does.
- Qualification clones the committed `HEAD` into its work directory, then stages pins,
  builds and runs the gate there. The pins files and `.proofstorm-dev` installation of
  your own checkout are only read.

You need:
- a clean, committed checkout; `HEAD` is what gets built and qualified
- Docker Desktop with Buildx, plus Rust, Git, `jq` and `just`
- GNU `timeout` and `sha256sum` on `PATH`, for example Homebrew `coreutils`

The gate creates its own disposable installation in the same Docker engine as the dev
cluster, and its preservation check compares every existing container before and
after. Run it only when no live lab sessions are using the cluster.

```sh
rev=$(git rev-parse HEAD)
base=$HOME/spark-qualification-01   # new, outside the checkout
mkdir -p "$base/artifacts"
for selection in spark-operator@0.0.0-5e0c879 open-ssp@0.1.0-9c25f69 \
    cdk-spark-processor@0.1.0-d7244e2 ldk-server@0.1.0-762bc47; do
  bash scripts/stack-image-build.sh spark --local "$selection" linux/arm64 "$base/build-$selection"
  mv "$base/build-$selection/artifacts" "$base/artifacts/spark-image-$selection-arm64-$rev-1"
done
bash scripts/stack-qualification.sh spark --local linux/arm64 "$rev" 1 \
  "$base/artifacts" "$base/qualification"
```

The cold Spark builds and the clone's own first `just dev-build` take a while. The
results are in `$base/qualification`:
- `public/` holds the public receipts.
- `run/` holds the gate's private evidence.
- `checkout/` is the disposable clone.

If a run is interrupted, retry its runtime cleanup with
`just e2e-cleanup "$base/qualification/run"`. Delete `$base` when you are done.

Use a native builder for each platform; emulation does not establish native support.
None of these commands publishes an image or changes catalog pins in your checkout.
