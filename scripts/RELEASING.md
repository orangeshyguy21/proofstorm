# Preparing a Linux alpha release

The **Prepare alpha release** workflow promotes an existing, tested Linux CI
artifact into a **draft prerelease**. It never rebuilds Proofstorm, executes the
downloaded binaries, replaces a version, or publishes automatically. A small
Rust verifier is built from the workflow checkout; Bash handles GitHub calls.
No Python, Docker, or Proofstorm runtime is needed for promotion.

## Everyday commands

Prepare a new alpha from a clean checkout, normally on your release branch:

```bash
just release-prepare 0.1.0-alpha.2
```

This updates the workspace version, workspace packages in `Cargo.lock`, installer
default, tool version pin, chart version/appVersion, and default controller image
tag. It preserves dependency versions, file permissions, and all existing image
digests/verification receipts. It does not commit, push, tag, or contact GitHub.
Invalid, unchanged, older, or inconsistent versions are rejected before editing.

**Main CI builds the matching Linux controller automatically.** After code checks
pass, it builds from that same clean commit, verifies the controller and execution
helper, publishes a uniquely tagged image to GHCR, and checks anonymous access.
It embeds that image's exact digest and verification record in the Linux bundle.
No receipt copying or generated-file commit is needed; old checked-in image
records are neither relabelled nor used by this automated lane.

Review the source version changes, run checks, and merge normally. Once the
**Checks** run for current `main` is green, update your local main checkout and run:

```bash
just release
```

The command uses your existing GitHub CLI login (`gh auth login` if needed) and
the repository selected by that checkout. It finds **current main's exact build**,
checks all three jobs, its artifact, and version availability, then displays the
repository/version/commit/build and asks `[y/N]` before dispatching **Prepare alpha
release** on `main`. GitHub Actions performs the full bundle and uploaded-byte
verification. Follow the printed workflow link, then review and publish the draft
in Releases. Dispatch success is not reported as release completion.

No run IDs, repository arguments, tags, or temporary directories need copying.
The helper reuses `target/check`; no Proofstorm payload is built locally. The
authenticated user needs permission to dispatch repository Actions workflows;
write access to Releases belongs to the existing workflow's draft job.

- `just release --preview`: select and show the candidate without dispatching or
  downloading it. This is a selection preview, not full payload verification.
- `just release --yes`: explicitly authorize draft preparation without a prompt,
  using the same authentication and checks. It still never publishes.

These shortcuts require a clean checkout. `just release` additionally requires
local `main` to match GitHub `main`; it does not pull, switch branches, select an
older green commit, or overwrite an existing version. If main changes while you
confirm, it stops so you can review the new selection. A failed/uncertain dispatch
asks you to inspect Actions before retrying; it does not retry automatically.

The full loop is **just release-prepare VERSION → review/merge → green CI →
just release → review/publish**. Controller publication is automatic; publishing
the downloadable alpha remains your explicit approval.

## Automatic controller builds

Only pushes or manual Checks runs on the canonical repository's `main` publish
images. PRs and manual branch runs have read-only code checks. The main Linux job
has `packages: write`; it uses the job's GitHub token, then logs out of GHCR before
building host binaries. No personal token needs to be saved in repository secrets.

The existing `proofstorm/proofstormd` package must be Public and allow this
repository's Actions to write. If the first CI push reports a permission error,
open the package's **Settings → Manage Actions access**, add the `proofstorm`
repository if absent, and grant **Write**. See GitHub's
[package access documentation](https://docs.github.com/en/packages/learn-github-packages/configuring-a-packages-access-control-and-visibility).
Anonymous verification uses no Docker or GitHub credentials and fails if the
image is private or any referenced layer is unavailable.

CI tags use `ci-COMMIT-UNIQUE_SUFFIX`; bundles pin the immutable registry digest,
not that tag. Registry manifest/config hashes must match the locally tested image.
Source fingerprints and versions must agree across controller, host binaries,
and bundle. A changed source snapshot, failed startup check, push, or anonymous
read stops the job before a usable release artifact is uploaded. Rerunning Checks
uses a fresh tag. Images already pushed remain available for diagnosis; this
workflow does not delete images or move version tags.

For explicit local diagnosis, `just release-controller-build --work-dir NEW_DIR`
builds and tests without publishing. Publication is a separate
`just release-controller-publish --work-dir DIRECTORY --confirm-namespace
ghcr.io/orangeshyguy21/proofstorm` command requiring Docker authentication.
Both use Bash/Rust and Docker; registry verification also needs curl.
The resulting `controller.json` can be passed as `--controller-receipt FILE` to
the Linux bundle command, but only with exactly the same clean source snapshot.

This automation covers **Linux AMD64**, not macOS/ARM64 or workload image rebuilds.
Legacy checked-in controller records remain for other build paths. Runtime/lab
acceptance is still separate from image startup and anonymous availability.

## In GitHub

1. Merge the release tooling into `main`. Prepare a new source version with
   `just release-prepare VERSION`, review, and merge it. CI builds its matching
   controller automatically. Do not relabel an old bundle with a new tag.
2. Wait for a **Checks** run on `main` to finish green, including **Linux bundle
   and installer**. Copy the numeric run ID from its URL (`actions/runs/ID`).
   Only optimized, clean alpha bundles qualify. PR, branch, failed, skipped,
   debug, and development builds do not.
3. Open **Actions → Prepare alpha release → Run workflow**, select `main`, and
   enter that run ID and its exact tag, such as `v0.1.0-alpha.2`. The tag must
   match the bundle and installer, and must not already exist. Leave **Create a
   draft prerelease** unchecked for a preview with no release changes.
4. Review the workflow summary. Run again with the same inputs and **Create a
   draft prerelease** checked. It independently repeats verification, creates a
   draft, uploads the six tested files, downloads them again, and checks every
   byte against the candidate.
5. Open **Releases**, inspect the draft and alpha limitations, then use GitHub's
   **Publish release** button when ready to make the candidate available for
   public-download testing. Keep it a prerelease; do not mark it latest/stable.

This lane contains **Linux AMD64 only**, not matching macOS assets. The current
installer default is `0.1.0-alpha.1`; if that version already exists, promotion
will refuse to overwrite it. A new version needs a new coherent build, not just
a different workflow input. Artifacts expire after 14 days; if the selected
artifact has expired or was deleted, run Checks again and use the new run ID.
The latest selected run attempt must contain all three successful jobs and its
matching artifact; when needed, rerun all jobs rather than only failed jobs.

## What gets checked

- The selected run belongs to this repository's Checks workflow, ran on `main`
  from a push or manual dispatch, and completed successfully. Its source commit
  must still be an ancestor of current `main`.
- All three required jobs passed in the selected attempt. Exactly one unexpired
  Linux artifact matches its source SHA and attempt. Run and artifact identity
  are rechecked immediately before creating a draft.
- The six-file inventory, archive checksum, safe extraction, payload checksums,
  Linux target, optimized profile, alpha channel, clean source revision, and
  build/relocation/offline-install reports agree.
- The embedded controller record proves matching clean source/version, an
  immutable GHCR digest, startup checks, and anonymous registry identity and
  availability checks. Older CI artifacts without this evidence cannot be promoted.
- The installer matches the file from the selected source commit and defaults
  to the exact release version. Existing tags and releases (including drafts)
  are refused; API/authentication failures are never treated as absence.
- Uploaded assets remain on the expected draft and match the candidate byte
  for byte. There is no overwrite, automatic cleanup, or publish operation.

These are artifact-promotion checks, **not full alpha acceptance**. The reports
retain `release_ready: false`. CI verifies anonymous controller availability;
before announcing the alpha, verify workload image downloads and test the public installer on a fresh
Linux VM: install, setup, agent attachment, create/read/delete a lab, and cleanup.
Do not treat an offline install/reinstall pass as proof of GitHub download or
live runtime success.

## Local equivalent

Prerequisites: Bash, Rust/Cargo, just, and an authenticated GitHub CLI. Preview
needs Actions/Contents read access; creating a draft also needs Contents write.
GitHub-hosted workflow jobs provide these permissions automatically, with write
access confined to the draft job on `main`.

```bash
just release-promote-linux \
  --repo orangeshyguy21/proofstorm \
  --run-id YOUR_SUCCESSFUL_MAIN_RUN_ID \
  --tag v0.1.0-alpha.2 \
  --work-dir /tmp/proofstorm-alpha-preview
```

Choose a new work directory outside the checkout. Add `--draft` only when you
want GitHub changes, using another new work directory. The local command uses
the verifier in your current checkout; use a reviewed, up-to-date checkout.
The work directory retains downloaded evidence and generated notes for review.
Only the temporary verifier build is removed automatically.

If upload or verification fails after creation, the draft is **retained and
unpublished**. Inspect it in Releases; rerunning intentionally refuses the
existing draft. Decide explicitly whether to repair it manually or delete the
incomplete draft and any associated tag before retrying. Never publish a
partial or unverified draft. An uncertain API response also requires inspection
before retrying, since the draft may have been created.

GitHub references: [artifact downloads](https://cli.github.com/manual/gh_run_download),
[release uploads](https://cli.github.com/manual/gh_release_upload), and
[attempt-specific jobs](https://docs.github.com/en/rest/actions/workflow-jobs#list-jobs-for-a-workflow-run-attempt).
