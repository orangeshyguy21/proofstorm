<p align="center">
  <a href="https://proofstorm.com/">
    <img src="crates/proofstorm-web/assets/proofstorm-logo.svg" width="44" height="51" alt="">
    &nbsp;
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="crates/proofstorm-web/assets/proofstorm-word-mark-on-dark.svg">
      <img src="crates/proofstorm-web/assets/proofstorm-word-mark.svg" width="240" height="44" alt="Proofstorm">
    </picture>
  </a>
</p>
<p align="center">Dynamic test environments for Bitcoin, Lightning &amp; Cashu.</p>
<p align="center">
  <a href="#quick-start">Quick start</a> ·
  <a href="#supported-environments">Environments</a> ·
  <a href="#components">Components</a> ·
  <a href="#development">Development</a>
</p>

Proofstorm allows any coding agent to build regtest networks on the fly,
and drive them through native CLIs. Proofstorm runs the services in a private local Kubernetes
runtime with an optional web GUI for viewing your networks.

**Alpha:** for local development and disposable test data—not production or real funds.

The CLI is `storm`; `proofstorm` also works. The short command is skipped if it
conflicts with an existing executable.

## Quick start

Get the current public release from [proofstorm.com](https://proofstorm.com/).

On **Linux x86-64**, install Docker Engine with Buildx and make sure
`docker info` works as your normal user. Then:

```sh
curl -fsSL https://proofstorm.com/install | sh
export PATH="$HOME/.local/bin:$PATH"
storm setup
storm doctor
```

From your desired directory, launch an installed, authenticated coding agent:

```sh
storm agent open codex
storm agent open opencode
storm agent open claude
```

Prompt the agent to build your desired regtest, or use it alongside regular reviews and scans
to build and test proofs of concept.

Prefer a browser? Run `storm gui`. It opens your default browser and offers
launch buttons for detected native apps on macOS. Add `--desktop` to an `agent open`
command to launch a native app instead of its CLI.

## Supported environments

| Host | Public installer | Status |
| --- | --- | --- |
| Linux x86-64 / AMD64 | Available | Docker Engine + Buildx required |
| macOS Apple Silicon / ARM64 | Available | Docker Desktop required |


## Components

These are the versions and integrations in the built-in catalog—not a claim that
every combination will work. A connected agent can read the
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
| Workspace | `attacker-workspace` | 0.1.0-alpha.1 | General-purpose shell for commands and testing cell services |

## Updates

```sh
storm update --check
storm update
```

Follow the reported steps to refresh the runtime and reconnect agents; the command only installs files.

## Development

Contributors also need Rust and just. Docker is required for the runtime in both
development and installed releases.

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
