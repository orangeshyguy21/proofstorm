# Native component drivers

Proofstorm integration logic belongs in Rust. Bash/Just sequences native tools.
A component's implementation language does not determine the language of its
Proofstorm adapter. The helper speaks HTTP, gRPC and Unix JSON-RPC, or invokes an
upstream CLI. Passive wallet observations read pinned SQLite schemas directly.

The controller image carries a static Linux `proofstorm-driver`. A restricted
init container copies it into an empty directory mounted read-only by the mint
and wallet workloads that need it. Lightning nodes and the credential-free
protocol worker do not acquire this helper. Release verification checks its
startup and binds its version into the runtime contract.

Observation commands never start SDKs, recover wallet operations, or expose
proofs. They use read-only SQLite transactions compatible with live WAL state.
Missing, busy, incompatible, ambiguous or overflowing state fails closed.
Readiness avoids ambient proxies and redirects; Nutshell checks authenticated
management RPC and the exact loopback `/v1/info` path. These checks consume no
blind-auth tokens and make no payment requests.

Protocol sources: Nutshell 0.20.3 from the pinned upstream image
`sha256:f039b0e61f64d67c7212f5472eb5d021c3703cd9e72170aa924906ce6bd1f2ed`,
especially `cashu/mint/management_rpc/protos/management.proto`,
`cashu/wallet/wallet.py`, and `pyproject.toml`. Upstream source is reference
material, not an additional owned runtime. Cashu cryptographic primitives use
the pinned Rust `cashu` crate.

Run `cargo test -p proofstorm-driver` for wallet and local transport contracts.
Run `just test-native-supervisor` on Linux for process/private-I/O contracts.
Run `just check-component-driver COMPONENT DRIVER_IMAGE COMPONENT_IMAGE PLATFORM`
for disposable real-image checks (`COMPONENT` is `cdk`, `coco`, or `nutshell`).
The helper argument is the Dockerfile's `native` or `native-contracts` build target,
which carries `/proofstorm-driver`. Use explicit locally verified image references.
These checks disable external networking, enforce a total deadline, and neither
contact a cluster nor publish images.
Transport tests require local socket permission. Production image builds and
fresh mixed-component acceptance must pass before release; earlier prober
measurements do not validate this migration.

Wallet observations, readiness, rune bootstrap, authentication, quote recovery,
and acceptance helpers now use Rust or shell commands. Quote recovery follows
the pinned Nutshell wallet's explicit recovery behavior, with atomic local
updates and rejection of concurrent changes. It does not reconstruct lost change
signatures; the pinned upstream wallet does not implement that recovery either.

Nutshell configuration checks corroborate public metadata, active keyset fees and
advertised amount limits through HTTP. Non-public settings are projections of the
explicitly rendered process environment. These projections alone do not establish
rate-limit enforcement, database persistence or cache behavior; live acceptance
retains independent checks for those behaviors.

The owned Python migration is implemented and local checks pass. Publication
evidence and full live acceptance still need to pass before release; see
[the validation record](VALIDATION.md). `scripts/test-workflow-surface.sh` rejects
owned Python source and execution while preserving explicit tests that Python
is absent on installed hosts.

Local validation on September 13, 2026 passed real ARM CDK helper startup, Coco
initialization/locked restart/recovery-identity continuity, and Nutshell's complete
daemon startup with its fake payment backend. The latter accepted native mutual
TLS, rejected missing/wrong/plaintext client identities, and preserved all three
global HTTP requests and both transaction requests after 100 native readiness
checks and 100 TCP checks. Excess requests returned 429. Application clients used
source addresses outside Nutshell's exact loopback exemption; no proxy forwarding
headers were used. This is an actual daemon contract, not a fleet latency result.

The six new per-platform catalog pins are local build candidates pending registry
publication verification. Fresh installation must not be released with inaccessible
pins. Both CDK and Coco images were verified without Python on ARM and x86; Nutshell
retains its own upstream runtime and installs the upstream console entrypoints.

The library's default `runtime` feature builds the complete helper. Catalog and
controller consumers disable default features to read only its protocol constants;
host observation tests enable `observation` for passive SQLite access. They do not
need the helper's cryptographic or management clients.

Nutshell locks and candidate builds carry `native_cli_entrypoints` as an explicit
packaging guarantee. Old images cannot inherit it from a newer base catalog.
Upgrading a mint or wallet with an older lock requires rebuilding candidates and
resolving a new revision before the controller will launch the replacement.
