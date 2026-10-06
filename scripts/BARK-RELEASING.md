# Bark image release handoff

Bark needs three images on each native platform: `bark-server@0.7.0-6188e2d`,
`cdk-bark-processor@0.1.0-fe468ca`, and `cln-hold@26.06.7-hold.0.3.3`.
Native image builds, managed qualification, public distribution, and catalog
promotion are distinct steps. Nothing here commits or pushes a branch.

## Qualify retained native images

Commit and push the reviewed qualification tooling. The existing **Candidate
component packaging** entry point can run it from that branch before merging:

```sh
gh workflow run component-images.yml --repo orangeshyguy21/proofstorm \
  --ref REVIEWED_BRANCH -f family=bark-qualification -f architecture=amd64 \
  -f candidate_run=AMD64_BUILD_RUN
```

Repeat for `architecture=arm64` with its native build run. A build run must be
successful, from this repository's manually dispatched Bark image workflow, and
still have all three artifacts from the same revision and attempt. The restore
checks every handoff checksum, frozen source fingerprint, recipe and provenance,
native architecture, image identity, and offline probe. Application-only source
changes may reuse existing images; changed recipes or patches require new builds.

To build a fresh set, dispatch `component-images.yml` with `family=bark` and the
desired architecture. Record its run ID after it succeeds. Builds retain artifacts
for 14 days. Expired or partial artifacts must be rebuilt, never substituted.

The managed workflow has read-only repository/Actions permissions and no package
write token. On a fresh native Linux runner it loads the exact saved images,
repeats offline probes, stages their verified manifest pins in the disposable
checkout, builds matching host/controller artifacts, and runs `bark-processor`.
That gate checks settlement, original-quote recovery, retained identities/state,
nine mTLS refusals, owned storage cleanup, and preservation of unrelated resources.
No model benchmark runs.

Read `status.json`, `qualification.json`, and `bark-run-identity.json` in the
result artifact. Require exit status zero, `stage: complete`,
`managed_qualification: true`, and both cleanup/preservation flags true. The result
binds native image config/manifest digests, original build checksums, and the gate's
actual cell lock. The controller source fingerprint and staged catalog patch are
retained too. Candidate receipts still say unpublished; a passed gate does not
imply public distribution. Raw runtime homes and gate output are intentionally
excluded from public artifacts.

## Publish the qualified images

After both native gates pass, retain their receipts and restore the corresponding
image/work artifacts on matching native hosts. Use the existing `catalog-image`
publication guard against the qualified image IDs, without rebuilding. Re-run its
offline verification first, then publish the frozen work directory:

```sh
just catalog-image verify-local /absolute/path/to/restored/work
just catalog-image publish /absolute/path/to/restored/work \
  --confirm-namespace ghcr.io/orangeshyguy21/proofstorm
```

Publication needs GHCR write access for `proofstorm/bark-server`,
`proofstorm/cdk-bark-processor`, and `proofstorm/cln-hold`. Use a separate publishing
job/session; never add registry credentials to the managed qualification runner.
The three packages must be **Public**. If a newly created package is private,
change visibility and resume `just catalog-image verify-work WORK`; do not repush.
The helper verifies anonymous access to manifests/configs/layers and records each
immutable image reference. Verify each published image's config digest against
its successful native qualification receipt before promoting catalog pins.

## Promote the catalog before preparing an alpha

Review a separate change that replaces local preview pins with the verified
public platform pins, enables Bark in the distributed catalog, and adds its
qualification obligations. Preserve experimental lifecycle and the BOLT11/sat
scope. Require native managed qualification and anonymous distribution evidence
for both architectures. Do not merely enable AMD64 or remove distribution guards
because the image builds passed.

Once that change is merged and green, follow [the normal release flow](RELEASING.md)
to prepare the next alpha version. The qualification workflow introduced here
does not itself publish images, promote catalog support, or cut a release.
