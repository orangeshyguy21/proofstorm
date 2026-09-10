# Preparing a Linux alpha release

The **Prepare alpha release** workflow promotes an existing, tested Linux CI
artifact into a **draft prerelease**. It never rebuilds Proofstorm, executes the
downloaded binaries, replaces a version, or publishes automatically. A small
Rust verifier is built from the workflow checkout; Bash handles GitHub calls.
No Python, Docker, or Proofstorm runtime is needed for promotion.

## In GitHub

1. Merge the release tooling into `main`. Before preparing a new version, update
   the product version and installer default together, including any versioned
   chart/controller metadata required by the existing release checks. Do not
   relabel an old bundle with a new tag.
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
- The installer matches the file from the selected source commit and defaults
  to the exact release version. Existing tags and releases (including drafts)
  are refused; API/authentication failures are never treated as absence.
- Uploaded assets remain on the expected draft and match the candidate byte
  for byte. There is no overwrite, automatic cleanup, or publish operation.

These are artifact-promotion checks, **not full alpha acceptance**. The reports
retain `release_ready: false`. Before announcing the alpha, verify anonymous
controller/workload image downloads and test the public installer on a fresh
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
