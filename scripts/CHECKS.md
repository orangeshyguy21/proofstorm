# Code checks

Run `just check` before opening a pull request. GitHub Actions runs the same
checks on pull requests targeting `main`, pushes to `main`, and manual dispatch.
Quick checks finish before Rust compilation starts, so formatting mistakes do
not spend a full build. There are no path filters that leave required checks
pending on documentation-only changes.

## Prerequisites

- Rust through rustup; `rust-toolchain.toml` pins Rust, rustfmt, and Clippy.
- Git, Bash, just, and ShellCheck (`brew install just shellcheck` on macOS;
  see the [just installation instructions](https://just.systems/man/en/packages.html)
  for Linux, and `sudo apt-get install shellcheck` on Debian/Ubuntu).
- A native C toolchain for Rust dependencies, including bundled SQLite.

The check script never installs tools. GitHub installs just 1.42.4 and ShellCheck
on its disposable Ubuntu runner. Rust dependencies may need downloading on the first run.
Docker, Kubernetes, Helm, Python, Node, and Trunk are not needed by these checks.

## Commands

| Command | Checks |
| --- | --- |
| `just check` | Everything below, quick checks first |
| `just check-quick` | Just parsing/dispatch tests, Rust formatting, shell syntax, scoped ShellCheck |
| `just check-rust` | Strict workspace Clippy, then workspace tests |
| `just test` | Workspace unit and integration tests only |
| `just lint` | Formatting, shell checks, and strict Clippy |
| `just lint-helm` | Separate chart validation using the pinned Helm tool |

Rust tests and Clippy use `--locked`. They include MCP response compatibility,
CLI/installer behavior, and controller/rendering contracts without a live cluster.
Some tests use temporary directories, child processes, and loopback servers;
an execution sandbox that forbids localhost listeners cannot run the entire suite.

Checks build under `target/check`, separate from registered development binaries.
An explicit `CARGO_TARGET_DIR` is respected for scratch builds. The wrapper ignores
development GUI asset selection and runtime home/kubeconfig overrides. It does
not launch apps, update agent configurations, or change running labs.

Shell syntax is checked for tracked and non-ignored new `.sh` files. Strict
ShellCheck initially covers `install.sh`, `tools/install-trunk.sh`,
`tools/install-host-tools.sh`, `scripts/check.sh`, and `scripts/test-just.sh`;
legacy scenario/lab scripts are syntax-only until formalized.
No existing Python helper tests are replaced or removed by this first slice.

Just dispatch tests use fake commands in a temporary checkout. They verify
argument quoting, dependency order, aliases, runtime-selection isolation, and
failure propagation without rebuilding anything or touching a live runtime.

## Boundaries and next slices

These are host-code checks, not release acceptance. They do not build the
Wasm-only GUI, exercise a browser, validate container availability, or prove that
an installed bundle starts successfully. Existing Python packaging/helper tests,
Helm checks, and live acceptance gates remain separate for now.

The workflow uses read-only repository permissions, commit-pinned actions,
cancellation of superseded runs, and Rust caching. Only pushes to `main` save
caches; PRs may restore them. Initial builds are slower than warm runs; use the
first hosted runs to establish timings before adding more jobs.

After the workflow has run successfully, maintainers can require both
`Formatting and shell` and `Rust lints and tests` in the GitHub ruleset for `main`.
Adding the workflow does not configure branch protection automatically.

Next: migrate thin development orchestration to Bash and structured release
validation to a Rust maintainer command, preserving the existing tests. Then add
Linux image/bundle builds on `main` and explicit alpha publication of tested
artifacts. This workflow never publishes or changes the public installer.
