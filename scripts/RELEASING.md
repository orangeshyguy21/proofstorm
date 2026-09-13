# Preparing an alpha release

One release flow produces Linux AMD64 and macOS Apple Silicon downloads from the
same source commit. CI builds and publishes matching controllers; your GitHub login
authorizes a draft. Publishing the downloadable release remains a manual decision.

## Everyday flow

```sh
just release-prepare VERSION  # Replace VERSION with the next unused alpha version.
# Review the version changes, run just check, and merge.
# Once main's Checks run is green, update your local main:
just release
```

`release-prepare` updates workspace/lockfile versions, the installer default, tool
pin, chart version, and controller tag. It preserves dependency versions and image
digests. It does not commit, push, publish, or contact GitHub.

`just release` uses your existing `gh auth login` and the checkout's `origin`.
It requires a clean checkout and local main matching GitHub main. It selects that
exact commit's successful Checks run, verifies both platform artifacts and version
availability, then asks `[y/N]` before dispatching **Prepare alpha release**.

- `just release --preview`: show the selection without downloading or dispatching.
- `just release --yes`: authorize draft preparation without the prompt.
- Follow the printed workflow link. Dispatch success is not release completion.

The workflow downloads and verifies both bundles without executing or rebuilding
them, creates one draft prerelease, uploads the checked files, downloads them again,
and compares every byte. Review the draft in **Releases**, then publish explicitly.
No command marks it latest/stable or replaces an existing version.

## What CI builds

| Job | Runner | Output |
| --- | --- | --- |
| Formatting and shell | Ubuntu AMD64 | Fast tooling checks |
| Rust lints and tests | Ubuntu AMD64 | Workspace checks |
| Mac installer isolation | Apple Silicon macOS 15 | Native sandbox contract tests |
| Linux bundle and installer | Ubuntu AMD64 | AMD64 controller + Linux bundle + offline install/reinstall |
| ARM64 controller | Ubuntu ARM64 | ARM64 controller and verified receipt |
| Mac bundle and installer | Apple Silicon macOS 15 | Native Mac bundle + sandboxed install/reinstall |

Code checks run on PRs. Controller publication and full bundle builds run only for
pushes or manual Checks runs on the canonical repository's main. Both Linux
controller jobs receive `packages: write`; the Mac jobs have read-only repository
permissions and do not need Docker or registry credentials.

Each controller is built from the same clean source as its host bundle. CI tests
non-root startup and the execution helper, publishes a unique `ci-COMMIT-SUFFIX`
tag, then verifies anonymous registry identity and layer access. Bundles embed the
immutable digest and source-bound receipt. No generated-file commit is needed.

The existing `proofstorm/proofstormd` package must be **Public**, with this
repository granted **Write** under **Settings → Manage Actions access**.
See [GitHub package access](https://docs.github.com/en/packages/learn-github-packages/configuring-a-packages-access-control-and-visibility).
No personal access token is required in repository secrets.

Both bundles use the shared Bash/Rust build driver. Linux installation runs in
source-free Debian without networking or build tools. Mac installation runs
with a disposable home, source reads/network/compiler execution denied, and writes
restricted to its test directory. Mac tests prove those restrictions before
running the installer; unavailable or ineffective isolation fails the check.
This is not a claim that the Mac runner physically lacks source or build tools.

## Promotion checks and assets

All six jobs must succeed in the selected run attempt. Exactly one unexpired
artifact per platform must match its source SHA and attempt:

- `proofstorm-linux-amd64-COMMIT-ATTEMPT`
- `proofstorm-macos-arm64-COMMIT-ATTEMPT`

Artifacts expire after 14 days; diagnostics after 7. When rerunning, rerun all
required jobs so both artifacts belong to the same attempt. A missing Mac result
never silently becomes a Linux-only release.

Promotion checks safe archive extraction, payload checksums, optimized profiles,
clean source fingerprints, version/target/controller agreement, and successful
build/relocation/install evidence. Both installers must exactly match the selected
commit's installer and default to the release version. API errors are not treated
as evidence that a tag or draft is absent.

The draft has **seven assets**, with platform names intended for people downloading
the release. The complete compiler targets remain in bundle metadata:

```text
install.sh
release.json
proofstorm-VERSION-linux-amd64.tar.gz
proofstorm-VERSION-linux-amd64.tar.gz.sha256
proofstorm-VERSION-macos-arm64.tar.gz
proofstorm-VERSION-macos-arm64.tar.gz.sha256
verification-reports.tar.gz
```

`release.json` is generated automatically from the verified candidates. It records
the schema version, release version/tag, source commit, channel, platforms, and
every other asset's filename, byte size, and SHA-256, including the installer.
It is also verified before and after upload. The site consumes this
[download contract](../release/release-manifest.md), not release-note prose.

The reports archive retains all six original build, relocation, and installer
JSON reports, named by platform. It is deterministic and verified byte-for-byte
before and after upload. These checks are unchanged; the installer does not need
the reports archive.

Existing releases are not renamed or overwritten. The installer prefers friendly
filenames and supports old target-triple filenames when the automatically selected
archive is absent (HTTP 404 for downloads). Explicit `--archive` selections,
authentication/network failures, missing checksums, and integrity failures never
trigger a second-name download. Old archives remain usable for local installation
and verification. New promotion requires newly built, friendly-named candidates;
use a new release version if the previous version was already published.

Run and artifact identities are rechecked immediately before draft creation.
Uploaded assets must retain their names, exact bytes, and unpublished draft status.

These checks are **not full acceptance**; reports retain `release_ready: false`.
Before announcing a version, test public downloads and install → setup/doctor →
agent attachment → cell creation/read → reinstall → cleanup on fresh Linux and Mac
hosts. Test native apps and browser behavior separately. See
[Mac acceptance](../release/macos.md), including signing/Gatekeeper limitations.

## Manual alternatives and recovery

The GUI equivalent is **Actions → Prepare alpha release → Run workflow** on main.
Enter the successful Checks run ID and exact unused alpha tag. Leave **Create a
draft prerelease** unchecked for full payload verification without release changes;
check it to authorize a draft.

For local diagnosis, the lower-level equivalent is:

```sh
just release-promote \
  --repo orangeshyguy21/proofstorm \
  --run-id YOUR_SUCCESSFUL_MAIN_RUN_ID \
  --tag vVERSION \
  --work-dir /tmp/proofstorm-alpha-preview
```

It requires Bash, Rust, just, and an authenticated GitHub CLI, but no Docker or
Proofstorm runtime. Use a new external work directory and a reviewed checkout.
Add `--draft` only to authorize remote writes; verification evidence is retained.

If creation/upload fails, any draft remains **unpublished and retained**. Inspect
it before retrying: an uncertain API response may already have created it.
Existing tags and drafts are never overwritten. Repair or remove an incomplete
draft only by an explicit decision; never publish a partial or unverified draft.

Controller diagnosis remains available through `just release-controller-build
--platform linux/amd64` (or `linux/arm64`) and `just release-controller-publish`.
See the commands' help and [Mac build instructions](../release/macos.md).
CI does not rebuild workload images or delete older controller images.

## Workload images and helper pins

When a catalog image actually changes, use
[`just catalog-image`](../docker/README.md#build-or-publish-a-catalog-image).
Build or prepare a digest-preserving copy first; publish only after explicitly
confirming the GHCR namespace. Review the resulting digest/provenance before
editing the catalog. The regular main CI then rebuilds matching controllers.
There is no separate AMD64 development publisher or fixed local registry flow.

For k3d/kubectl/Helm updates, generate and review both platform manifests with
[`just tool-pins`](DEVELOPMENT.md#maintainer-host-tools). Setup and `just tools`
share pin validation, not installation directories. None of these maintenance
commands alter the seven release assets or authorize a GitHub Release.
