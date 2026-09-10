# Checkout workflow

Install just (`brew install just` on macOS; see [Linux packages](https://just.systems/man/en/packages.html)).
Run `just` or `just --list` to discover commands. Just is a contributor tool, not
a dependency for people installing Proofstorm.

Normal development no longer requires Python. `scripts/develop.sh` orchestrates
Trunk, Cargo, and checkout registration. The small Rust `proofstorm-xtask` helper
owns ownership checks, persisted build settings, resource snapshots, and atomic
launcher writes. It is built separately under `target/maintainer`; it is not
shipped to installed users. The first development run compiles this helper.

Existing `.proofstorm-dev/owner.json`, `build.json`, state, launchers, and selected
binary paths are retained. Resource snapshots may get a new content-addressed
directory; older snapshots are not deleted or overwritten. Controller source
hashing keeps the existing contract and excludes host/web source.

The development shell uses Bash or Zsh from `SHELL`, with startup files disabled;
otherwise it defaults to Zsh on macOS and Bash on Linux. Normal exit/EOF succeeds
even after an interrupted command; shell launch errors and signals still fail.

Run `just dev` from the Proofstorm checkout. It builds matching CLI/MCP binaries,
web assets, chart/CRD resources, and controller source snapshot, then enters a shell selecting this checkout's
private installation. Docker is not touched by the build. Inside that shell:

```sh
proofstorm setup
proofstorm doctor
proofstorm up examples/developer-lab.json
proofstorm gui
```

Leaving the development shell with `exit` or Ctrl-D is a successful session end,
even after an interrupted or failed command. Command failures still appear in
the shell; build/registration failures before it opens still fail `just dev`.

Commands show an ASCII spinner and status text in an interactive terminal,
starting before installation checks. Setup reports its current stage. Ordinary
results are human-readable; use `proofstorm setup --json`, `proofstorm gui --json`,
or the global `--json` flag on another command for the full machine-readable
result, with no spinner. Redirected output uses plain progress lines on stderr,
not terminal animation. `release-info` and internal checkout registration retain
their machine-readable output. This is the same CLI behavior in release bundles.

GUI startup verifies artifacts once in the launcher and independently once in
the new backend. The verified snapshot is reused only within that startup; it is
not a persistent cache. Subsequent requests still detect changed artifacts and
stale GUI builds. Progress reports file checks, server startup, runtime ownership
and health checks, and browser activation. A new server opens the browser directly;
an existing server first attempts to focus its tab.

The checksum dependency is optimized in debug builds via `.cargo/config.toml`;
application debugging and integrity checks are unchanged. This host-only build
setting does not invalidate the controller snapshot or require rebuilding images.

For agent attachment, change to the application's directory and run
`proofstorm open codex`, `proofstorm open opencode`, or `proofstorm open claude`.
The connection is project-specific and keeps this installation selected even
after leaving the development shell. No global agent configuration is changed.

`just dev-build` rebuilds without entering a shell. `.proofstorm-dev/bin/proofstorm`
is the same command launcher outside that shell. `just setup`, `just doctor`,
and `just gui` are conveniences for that launcher. No release archive, installer,
global PATH mutation, or legacy lab migration is involved.

## Rebuilding

- Web: run `just web-dev` in another terminal, then refresh the managed GUI after
  each build. Assets use the same authenticated backend/origin; there is no
  separate API proxy. Automatic browser reload is not implemented yet.
  `just web` performs a single asset rebuild through the same path.
- Host code: run `just dev-build`; stop/reopen the GUI and reconnect agent
  sessions afterward. Existing labs, installation identity, and grants survive.
- Chart/CRDs: rebuild, then run `proofstorm setup` to apply the new snapshot.
- Controller/runtime-contract changes: run `just dev-build`, then `proofstorm
  setup` (or simply `just deploy`). Setup builds the recorded linux/arm64 source,
  verifies source identity, platform, and client compatibility, publishes only
  to this installation's loopback registry, and deploys by immutable digest.
  The first build can take several minutes. Subsequent changed-source builds
  reuse Docker/Cargo caches; unchanged source reuses its verified image and
  healthy deployment. Host/web source edits do not invalidate controller source.
  Runtime contract changes require this setup before reconnecting agents/GUI.
  Failed build diagnostics stay in private `state/controller-build.log`.

The old `make docker-build`, `docker-push`, `install`, and `cluster-schema`
controller workflow has been removed. `just deploy` now uses the same setup
path as the CLI, including chart/CRD updates and ownership checks. No global
kubeconfig, fixed legacy registry, or manually selected deployment context is used.

Build output, immutable resource snapshots, launchers, and private state live
under ignored `.proofstorm-dev/`. Do not delete it casually: it also identifies
the installation that owns Docker resources. Ordinary builds reuse it. The
old checkout `target/`, web `dist/`, and legacy cluster are not adopted.

An explicit `just dev-build --target-dir /absolute/dedicated/cargo-cache` can select
a different build cache on first registration. Keep it dedicated: replacing
either binary outside `just dev-build` makes registration stale and commands
fail closed until a coherent build is registered. Switching binary paths after
registration is intentionally refused rather than silently retargeting agents.

## Remaining consolidation

Owned runtime teardown and older acceptance gates still need to move behind
the installation-aware path. The remaining low-level
legacy recipes (listed under `legacy` by `just --list`) are not part of this new workflow. Release packaging and
installer tests remain separate because they test distribution, not a second
product runtime.

## Moving from Make

The root Makefile has been replaced by `justfile`; use `just dev`, `just check`,
and `just gui`. Arguments are ordinary quoted CLI arguments, not Make assignments:
`just dev-build --target-dir '/absolute/path with spaces'` or `just gui --no-open`.
Legacy gates use `just e2e slice4` instead of `make e2e-slice4`.
The old Compose harness remains available through `just compose <target>` and
is the only recipe that still invokes Make. No runtime is migrated by this change.

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

For a controller update/reuse check, run `scripts/test_checkout_controller.py`
with the same `--cli`, `--home`, and a new `--work-dir`. It verifies setup/doctor,
image identity, unchanged repeated deployment, and preservation of installation,
runtime ownership, and existing database contents. It creates no labs and
publishes nothing outside the installation's private registry.

For terminal-output verification, run `scripts/test_cli_progress.py` with the same
`--cli`, `--home`, and a new `--work-dir`. It checks immediate animated progress,
line cleanup, readable setup/GUI summaries, and explicit JSON results against the
ready installation. It creates no labs or agent connections, opens no browser,
and stops the GUI only if one was not already running when the test began.
