# Rolling image support

Proofstorm's support policy counts release families: three for Bitcoin Core,
Core Lightning and LND; two for released mint and wallet implementations. Within
each family, keep one validated patch. `proofstorm-core::release_policy` defines
the family rules and numeric ordering. CLN families are year/month pairs; LND,
Nutshell and CDK use `0.N` families before 1.0. LND's normal `-beta` releases
qualify, while release candidates and development snapshots do not.

CDK's support floor is **0.18.0** for all mint presets and the CLI wallet. For
now, only the 0.18 family participates. Once 0.19 qualifies, retain 0.18 alongside
it; subsequent releases roll over the newest two qualified families. Do not
backfill the window with 0.17 or older releases. The audit and catalog validation
both enforce this floor.

The installed Rust catalog remains the authority for supported images. A recipe,
successful build, or upstream release alone does not add official support.
Experimental Cocod, LDK Server, and payment processor snapshots and infrastructure
helpers do not participate in the rolling window.

## Discover and prepare releases

```sh
just catalog-image audit
just catalog-image list
scratch="$(mktemp -d)"
just catalog-image build nutshell-mint@0.21.0 linux/arm64 "$scratch/nutshell"
```

The audit reads paginated official GitHub release feeds, filters drafts and
prereleases, and reports the versions to qualify and retire after qualification.
It does not edit the catalog, build images, publish packages, or start cells.
Incomplete windows never propose retirement. A feed error fails the audit rather
than returning a partial proposal.

Builds select a reviewed `RECIPE@VERSION` and an explicit platform. Existing bare
recipe names retain their historical version selection. The selected version is
recorded in new build receipts, and exact native probes must match it. Old
receipts without a version retain their original recipe/probe interpretation.
The clean-source and publication checks described in [README.md](README.md) still
apply. Build both architectures and retain their receipts.

The added preparation recipes are Bitcoin Core 29.4/30.3 and Nutshell 0.21.0.
Their checksums, source identities, and recipe digests
are recorded beside the recipes. LND and CLN use upstream immutable images.
Preparation recipes are intentionally separate from the installed support list.
CDK adds versioned 0.18.1 mint and CLI wallet recipes. The 0.18.0 recipes
and existing unversioned receipt interpretation remain unchanged.

The release inventory reviewed on 2026-09-16, with CDK refreshed on 2026-09-18
and the CDK support floor applied:

| Implementation | Release families to qualify/retain |
| --- | --- |
| Bitcoin Core | 31.1, 30.3, 29.4 |
| Core Lightning | 26.06.7, 26.04.1, 25.12.1 |
| LND | 0.21.3-beta, 0.20.4-beta, 0.19.3-beta |
| Nutshell mint and wallet | 0.21.0, 0.20.3 |
| CDK mint presets and CLI wallet | 0.18.1; add a second family when 0.19 qualifies |

The existing CLN 26.06.7 catalog pin also needs requalification: upstream
[corrected its published images](https://github.com/ElementsProject/lightning/releases/tag/v26.06.7),
and the corrected multi-platform digest is
`sha256:0421a5f0d1b2e1ad639edfa17d777816040e3850d91bae7f2d32186d9c1e6da4`.
It differs from the installed catalog pin. The qualification workflow probes the
corrected image; it does not replace the existing pin or rewrite historical locks.

## Qualification and promotion

Run the relevant component contracts with explicit images:

```sh
just check-component-driver nutshell DRIVER_IMAGE NUTSHELL_IMAGE linux/arm64 0.21.0
just check-component-driver bitcoin DRIVER_IMAGE BITCOIN_IMAGE linux/arm64
just check-component-driver cdk DRIVER_IMAGE CDK_CLI_IMAGE linux/arm64
```

Nutshell tests exercise actual mint and wallet processes, mutual TLS, quota
isolation, native quote claiming, wallet isolation, and token accounting. Their
Lightning backend is FakeWallet; they do not prove LND/CLN payments. Bitcoin
tests mine a regtest chain, restart the process, and reopen the persistent wallet.
CDK's wallet component contract checks native noninteractive startup, independent
seed identities, reopening persistent state, and the helper boundary.
CDK's mint presets use the 0.18 configuration contract. The older 0.17 mint
configuration interface and wallet are outside the support and qualification scope.

The `Component image qualification` workflow runs these builds and contracts on
native Linux AMD64 and ARM64 runners when relevant pull requests change, or by
manual dispatch. It also runs all 18 Bitcoin/Lightning release pairings and the
two Nutshell mint and wallet families against every selected Lightning release,
plus the CDK 0.18 wallet against both Nutshell mints with the preferred LND release.
It saves exact image receipts, helper identity, version probes and structured
live results as artifacts. It has read-only repository permissions and does not
publish images. See [the compatibility runner](../tests/component-compat/README.md)
for local invocation and the boundaries of this evidence. PostgreSQL, Redis,
OIDC, CDK 0.18 mint/backend checks, and managed-cell acceptance remain separate
qualification requirements.

The workflow also builds `ldk-server@0.1.0-50fe752` and
`cdk-ldk-server-processor@0.1.0-fe468ca` in separate jobs for each native
architecture. Their offline probes verify the node and CLI versions, or the
processor executable and pinned source revision. These jobs retain build logs,
image receipts, image inspection, and probe output, including on failure.
The `ldk-server-processor` live gate separately checks gRPC readiness, payments,
quote recovery, and restart persistence; the image jobs do not establish those
integration behaviors.

Nutshell 0.21 uses the `nutshell-mint/0.21/v1` action contract. The renderer derives
the expected native version from that locked contract, including for candidates
that inherit it. Its CLN backend uses a restricted `xpay` rune saved separately
as `cln-xpay.rune`. Existing 0.20 cells keep their `pay` rune and startup command.
The settings observer rejects mismatched or unreviewed native versions.

Before catalog promotion, verify anonymous image availability, native amd64 and
arm64 behavior, actual Bitcoin/Lightning dependency pairings, mint/backend and
wallet/mint pairings, and all advertised storage/authentication capabilities.
Record evidence for each claimed edge. Emulated tests are useful preparation,
but do not establish native AMD64 qualification. Fresh-version support also does
not establish an in-place database migration or downgrade path.

The catalog selects CDK 0.18.1 for all three mint presets and the CLI wallet.
CDK 0.18.0 entries are deprecated and retained for existing locks. Nutshell
0.21.0 is preferred for both mint and wallet, with 0.20.3 still supported.
Both Nutshell releases share the configuration schema; the 0.21 mint selects
its versioned action contract for native version checking and CLN `xpay`.
The [verification record](../release/cashu-versions-20260918-verification.json)
records exact artifacts and completed checks. Native AMD64 and the remaining
managed-cell storage, authentication, and CDK backend gates still require
qualification before release.

When promoting subsequent versions, add the exact image/provenance and contracts to the catalog,
make the newest validated family preferred, and retire the oldest in the same
reviewed release change. Regenerate coverage, schemas and affected render
fixtures, then run `just check` and the affected live acceptance gates. Keep
candidate build baselines and profiles aligned with the promoted contracts.

## Retirement

Preferred and supported entries count toward the window. Deprecated entries
remain available for historical locks but do not appear in ordinary catalog
discovery; request the `deprecated` lifecycle filter to inspect them. New-cell
admission rejects retired versions, including previously published plans first
materialized after retirement. Existing instances retain their exact locks.

Keep archived image digests, provenance, runtime contracts, and registry content.
Retirement never rewrites a lock, upgrades a volume, or deletes an image. A failed
qualification retains the previous validated window. The current catalog can
temporarily contain fewer families while the initial inventory is being filled;
the audit reports the remaining qualification work.
