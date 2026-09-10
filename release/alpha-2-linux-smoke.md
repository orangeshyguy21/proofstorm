# Linux alpha.2 smoke test

User-supplied fresh Ubuntu VM report, received 2026-09-10. This is a summary of
that report, not an independent rerun; raw VM logs are not checked into this repo.

| Check | Reported result |
| --- | --- |
| Anonymous public install | Passed in about 3.5 seconds; CLI and MCP version `0.1.0-alpha.2`; no compilation, checkout, or bypass flags |
| Setup and doctor | Passed in 63 seconds as normal user `ubuntu`; anonymous controller download |
| Codex MCP | Codex 0.154.0 / gpt-6-astra created and read `vm-alpha-smoke`; Bitcoin Core 31.1 regtest, 1/1 ready, responding protocol probe |
| OpenCode MCP | OpenCode 1.18.30 / Big Pickle read the existing lab; component data matched CLI |
| GUI | HTTP 200 on loopback; stopped successfully; no visual test |
| Reinstall | Passed in about 3.3 seconds; versions, healthy runtime, and existing lab preserved |
| Cleanup | Passed in 32 seconds; lab, namespace, workloads, and lab storage absent afterward |

## Follow-up

These fixes are post-alpha.2 development work; the published alpha.2 assets have
not been replaced.

- The release README incorrectly called the alpha unpublished. The new README
  starts with the public installer and distinguishes released from planned hosts.
- Setup and cleanup needed more descriptive wait messages.
- The resource projection reported the protocol prober's initial template scale
  (`0`) instead of the controller-managed Deployment scale. The read model now
  identifies its kind and scheduling policy, reads its live scale and status, and
  reports unknown values when an observation is unavailable. Generation fields
  distinguish an up-to-date status from a previous reconciliation.
- Codex needed `TERM=xterm-256color` after a `TERM=dumb` warning on the VM.
- OpenCode called the prober a transient job. That description was not supported
  by Kubernetes evidence: it is a Deployment.

This validates the installation and single-Bitcoin-lab workflow, not the full
catalog, transaction operations, native GUI launches, or macOS installation.
After testing, the healthy private runtime, installed tools, images, configuration,
and evidence remained. The GUI, agent sessions, and test lab were stopped/removed.
