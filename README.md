<p align="center">
  <img src="crates/proofstorm-web/assets/proofstorm-logo.svg" width="44" height="51" alt="">
  &nbsp;
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="crates/proofstorm-web/assets/proofstorm-word-mark-on-dark.svg">
    <img src="crates/proofstorm-web/assets/proofstorm-word-mark.svg" width="240" height="44" alt="Proofstorm">
  </picture>
</p>
<p align="center">Local test cells for Bitcoin, Lightning &amp; Cashu.</p>
<p align="center">
  <a href="#quick-start">Quick start</a> ·
  <a href="#supported-environments">Environments</a> ·
  <a href="#components">Components</a> ·
  <a href="#development">Development</a>
</p>

Build a lab, connect your application, and test it through a CLI, a browser, or
your coding agent. Proofstorm runs the services in a private local Kubernetes
runtime and downloads prebuilt images as you need them.

**Alpha:** for local development and disposable test data—not production or real funds.

The CLI is `storm`; `proofstorm` also works. The short command is skipped if it
conflicts with an existing executable.

## Quick start

These command examples require a build with the new CLI.

On **Linux x86-64**, install Docker Engine with Buildx and make sure
`docker info` works as your normal user. Then:

```sh
curl -fsSL https://github.com/orangeshyguy21/proofstorm/releases/download/v0.1.0-alpha.2/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
storm setup
storm doctor
```

No Rust, source checkout, or compilation required. The installer does not change
your shell profile, start a runtime, or configure an agent. `setup` downloads the
tools and controller, then starts the private runtime. Lab images download on
first use. Add the PATH line to your shell profile if you want it to persist.

From your application's directory, launch an installed, authenticated coding agent:

```sh
storm agent open codex
# or: storm agent open opencode
# or: storm agent open claude
```

Ask it: “Use Proofstorm to create a lab named demo with one Bitcoin Core regtest
node. Wait for it to be ready, then read it back.” The MCP connection is named
`proofstorm`. Opening an agent configures its connection; ordinary setup does not.

Prefer a browser? Run `storm gui`. It opens your default browser and offers
launch buttons for detected native apps on macOS. Add `--desktop` to an `agent open`
command to launch a native app instead of its CLI.

## Supported environments

| Host | Public installer | Status |
| --- | --- | --- |
| Linux x86-64 / AMD64 | `0.1.0-alpha.2` | Fresh Ubuntu VM smoke test passed with Docker Engine + Buildx |
| macOS Apple Silicon / ARM64 | In progress | Checkout workflow available with Docker Desktop; packaged clean-Mac test pending |
| Linux ARM64 | Not yet | No published host bundle |
| macOS Intel | Not supported | No host bundle |
| Windows / WSL | Not supported yet | No validated installation flow |

The Linux smoke test covered installation, setup, one Bitcoin lab through Codex
and OpenCode, headless GUI startup, reinstall, and cleanup. It did **not** cover
every component, transactions, or visual GUI behavior. See the
[acceptance summary](release/alpha-2-linux-smoke.md).

On a headless host, use `storm gui start` and forward its loopback port
over SSH; do not expose the GUI publicly. Native app launch is macOS-only.
OpenCode's current desktop launch may still require selecting the project folder
inside the app; its CLI opens in the requested directory.

## Components

These are the versions and integrations in the built-in catalog—not a claim that
every combination has passed the fresh-VM test. A connected agent can read the
catalog for full configuration and compatibility details.

| Component | Catalog ID | Version | Integration |
| --- | --- | --- | --- |
| Bitcoin Core | `bitcoin-core` | 31.1 | Regtest chain, RPC, persistent state |
| LND | `lnd` | 0.21.3-beta; 0.20.4-beta | Lightning, BOLT11 |
| Core Lightning | `cln` | 26.06.7 | Lightning, BOLT11 |
| CDK mint | `cdk` | 0.18.0 | LND / CLN; SQLite / PostgreSQL |
| CDK + LDK mint | `cdk-ldk` | 0.18.0 | Embedded Lightning; BOLT11 / BOLT12 |
| CDK + BDK mint | `cdk-bdk` | 0.18.0 | On-chain payments; Bitcoin regtest |
| Nutshell mint | `nutshell` | 0.20.3 | LND / CLN; optional NUT-21 / NUT-22 auth |
| Nutshell wallet | `nutshell-wallet` | 0.20.3 | Persistent Cashu wallet |
| CDK CLI wallet | `cdk-cli-wallet` | 0.18.0 | Cashu wallet CLI |
| Coco daemon | `cocod-wallet` | 0.0.17-dev.44e5101c | Experimental Cashu wallet |
| PostgreSQL | `postgresql` | 17.11 | Persistent database |
| Redis | `redis` | 8.10.1 | Ephemeral cache |
| Keycloak | `keycloak` | 25.0.6 | Test OIDC provider |
| Attacker workspace | `attacker-workspace` | 0.1.0-alpha.1 | Disposable client shell |

## CLI in a minute

Download the example lab: one Bitcoin node and a CDK mint with an on-chain backend.

```sh
curl -fsSL https://raw.githubusercontent.com/orangeshyguy21/proofstorm/v0.1.0-alpha.2/examples/developer-lab.json -o lab.json
storm up lab.json --name demo
storm status demo
storm ls
```

Connect your app to the mint in another terminal:

```sh
storm connect demo mint http --config connection.json
```

Keep that command running. `connection.json` contains the local URL your app can
use. It is private to your user and removed on normal disconnect; an existing
file is never overwritten. Use `chain rpc` instead of `mint http` for Bitcoin RPC.

When you're finished:

```sh
storm rm demo          # Deletes the lab, its data, and history
storm gui stop         # Stops the GUI service; labs keep running
storm gui status       # Shows GUI service status
```

Commands show progress and readable results. Add `--json` for scripts, or
`--help` to any command for options. Labs are not automatically funded.

## Development

Contributors need Rust, just, and Docker. Installed users do not.

```sh
just check-quick        # Formatting, shell checks, and command-dispatch tests
just check             # Also runs Rust lints and hermetic tests
just dev               # Builds the checkout and enters its private dev shell
storm setup
storm gui
```

Development uses the same `storm` commands as a release. The difference is
where its binaries and controller come from: your checkout instead of a download.
State stays under `.proofstorm-dev/`, separate from an installed release.

Use `just dev-build` to rebuild, `just web-dev` to watch GUI assets, and `exit` to
leave the dev shell. See [development](scripts/DEVELOPMENT.md),
[check prerequisites](scripts/CHECKS.md), [releases](scripts/RELEASING.md), and
[macOS release work](release/macos.md).

## License

[MIT](LICENSE).
