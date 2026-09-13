# Native driver migration validation

Local validation recorded on September 13, 2026. This working tree contains the
native-driver migration and the preceding prober refactor. These are local test
results, not publication receipts or evidence of a fresh installed release.

## Passed local checks

| Check | Result and boundary |
| --- | --- |
| Complete host check | Formatting, workflow/Just/shell checks, strict workspace Clippy and all 52 workspace test targets passed: 707 tests, 0 failures, 3 intentionally ignored subprocess fixtures; final command exited 0 |
| Linux driver contracts | 32 tests passed, including passive SQLite/WAL reads, quote recovery and concurrent-update refusal, child-process deadlines, HTTP/Unix RPC, authentication primitives and management TLS |
| Linux execution supervisor | 7 tests passed, including cancellation, escaped descendants, private input/output, bounded capture and deadline cleanup |
| ARM and x86 production controller builds | Both built successfully; controller release information, driver/prober self-checks and native supervisor startup passed with networking disabled and read-only filesystems |
| CDK and Coco images | Both architectures report the expected upstream CLI version, run as UID 1000 and contain neither `python` nor `python3` |
| Actual ARM CDK image | Native driver startup passed |
| Actual ARM Coco daemon | Initialization, encrypted configuration, locked restart and recovery-identity continuity passed |
| Actual ARM Nutshell daemon | Native mutual TLS readiness and settings checks passed; missing/wrong/plaintext client identities were rejected |
| Actual Nutshell quota preservation | After 100 native readiness checks and 100 TCP checks, a non-exempt client retained all 3 global requests and another retained both transaction requests; the following requests returned 429 |

The Nutshell contract uses the real pinned daemon and SQLite ledger with its
fake payment backend. It does not contact Lightning nodes or an external network.
Non-exempt clients bind distinct loopback source addresses other than exactly
`127.0.0.1`; they send no forwarding headers. This checks application quotas,
not fleet latency, connection-rate limits, CPU overhead or shared accept queues.

The full host check log is retained locally at
`/private/tmp/proofstorm-python-purge-final-check.log`. Linux-only process checks
are separate from the macOS workspace tests.

## Local image identities

| Image tag | Manifest digest |
| --- | --- |
| `proofstorm-controller:python-purge-local-03` (ARM) | `sha256:14cf60958bfdf9f915ba5c72c4c45effed1bea873f933a748609c10ac43d71b7` |
| `proofstorm-controller:python-purge-amd64-03` (x86) | `sha256:028bb8561cfca2edf3d46715c8f3796cc8028f52c8fbe551e0ea7ba1c671e32f` |
| `proofstorm-driver-contracts:python-purge-02` (ARM) | `sha256:cfd770547d3162d18c2c265e093da9b77c09093acd7e5f0cba3f7c8ada51eb71` |
| `proofstorm-native-contracts:python-purge-02` (ARM) | `sha256:eaa4276d0e11efb24aafd6294b5cf5255fe9b9a3fc2f7e066ebf4e840c6f9991` |

The ARM controller reports runtime contract
`f1246c7c3c50003c3170959f9ff9d65e5e6ab2dd78f4a12cac434bd57c748f58`;
the x86 controller reports
`34f683f1cf231ffd72ced3d57a8ee496349d6a5db1d8e2485be7ea4eb9fcd6f0`.
Both report `source_sha256: development`: they were built locally and do not
provide the release workflow's clean-source attestation. Subsequent changes in
this validation pass corrected test fixtures and documentation only.

The six component manifest pins are recorded in
[`wallet_builds.rs`](../proofstorm-core/src/wallet_builds.rs). They are local build
candidates. Their presence in the catalog does not prove public availability.

## Reproduction

`Dockerfile.proofstormd` has `driver-contracts` and `native-contracts` targets for
the Linux suites. Build the production target for each required platform and run
the controller's `--release-info`, the driver/prober `--self-check`, and the
supervisor's expected sanitized failure with no arguments.

Run real-image contracts with explicit matching helper and component images:

```sh
just check-component-driver cdk proofstorm-native-contracts:python-purge-02 proofstorm-native-cdk:python-purge-01 linux/arm64
just check-component-driver coco proofstorm-native-contracts:python-purge-02 proofstorm-native-cocod:python-purge-01 linux/arm64
just check-component-driver nutshell proofstorm-native-contracts:python-purge-02 proofstorm-native-nutshell:python-purge-arm64-01 linux/arm64
```

These contracts force fresh execution rather than reusing a cached test layer.
Their generated images contain disposable fixture state and are not release
artifacts.

## Remaining release gates

1. Produce clean-source publication receipts and verify anonymous access to all
   six new component pins and matching controller images.
2. Run fresh mixed-component acceptance on the release candidate, including
   mint/pay/claim/recovery, authentication, management, Redis and PostgreSQL gates.
3. Measure real application latency and connection-limit interference under
   healthy and failing probe load before making stronger fleet-isolation claims.

The existing live user cell was not changed during these checks. Older Nutshell
locks and candidate builds need the explicit `native_cli_entrypoints` packaging
guarantee; they must be rebuilt/re-resolved before replacement workloads launch.
