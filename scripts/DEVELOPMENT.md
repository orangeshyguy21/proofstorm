# Checkout workflow

Run `make dev` from the Proofstorm checkout. It builds matching CLI/MCP binaries,
web assets, and chart/CRD resources, then enters a shell selecting this checkout's
private installation. Docker is not touched by the build. Inside that shell:

```sh
proofstorm setup
proofstorm doctor
proofstorm up examples/developer-lab.json
proofstorm gui
```

For agent attachment, change to the application's directory and run
`proofstorm open codex`, `proofstorm open opencode`, or `proofstorm open claude`.
The connection is project-specific and keeps this installation selected even
after leaving the development shell. No global agent configuration is changed.

`make dev-build` rebuilds without entering a shell. `.proofstorm-dev/bin/proofstorm`
is the same command launcher outside that shell. `make setup`, `make doctor`,
and `make gui` are conveniences for that launcher. No release archive, installer,
global PATH mutation, or legacy lab migration is involved.

## Rebuilding

- Web: run `make web-dev` in another terminal, then refresh the managed GUI after
  each build. Assets use the same authenticated backend/origin; there is no
  separate API proxy. Automatic browser reload is not implemented yet.
  `make web` performs a single asset rebuild through the same path.
- Host code: run `make dev-build`; stop/reopen the GUI and reconnect agent
  sessions afterward. Existing labs, installation identity, and grants survive.
- Chart/CRDs: rebuild, then run `proofstorm setup` to apply the new snapshot.
- Controller/runtime-contract changes: local build/deploy support is still the
  next slice. Setup currently uses the pinned release controller and rejects a
  runtime-contract mismatch; do not use the old `make deploy` against this home.

Build output, immutable resource snapshots, launchers, and private state live
under ignored `.proofstorm-dev/`. Do not delete it casually: it also identifies
the installation that owns Docker resources. Ordinary builds reuse it. The
old checkout `target/`, web `dist/`, and legacy cluster are not adopted.

An explicit `DEV_ARGS='--target-dir /absolute/dedicated/cargo-cache'` can select
a different build cache on first registration. Keep it dedicated: replacing
either binary outside `make dev-build` makes registration stale and commands
fail closed until a coherent build is registered. Switching binary paths after
registration is intentionally refused rather than silently retargeting agents.

## Remaining consolidation

Controller builds/image publication, owned runtime teardown, and older acceptance
gates still need to move behind the installation-aware path. The low-level
legacy Makefile targets are not part of this new workflow. Release packaging and
installer tests remain separate because they test distribution, not a second
product runtime.

## Live verification

After setup, stop the GUI and leave this installation idle while running:

```sh
python3 scripts/test_checkout.py \
  --cli "$PWD/.proofstorm-dev/target/debug/proofstorm" \
  --home "$PWD/.proofstorm-dev/state" \
  --work-dir /absolute/new/test-output-directory --test-agents
```

This runs doctor, GUI reuse/authentication checks, and the same private
OpenCode/Claude Code connection scenario used for installed releases. It starts
no model sessions and stops its GUI afterward. Test attachment receipts and
private test projects remain for inspection. Do not overlap it with lab creation
or another installation write; those operations intentionally serialize.
