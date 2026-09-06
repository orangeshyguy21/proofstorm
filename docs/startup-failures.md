# Component startup failures

Readiness separates normal startup from observed failures. The controller reads
current-rollout Pod and init-container states. `workload_ready` conditions carry
specific reasons for image pull failures/backoff, invalid image names, container
configuration/start errors, crash loops, unsuccessful exits, and unschedulable
Pods. Old-rollout and terminating Pods do not override the current workload.
Messages are fixed, bounded recovery guidance; raw Kubernetes messages, registry
responses, credentials, and Pod names are not exposed.

`proofstorm_lab_component_status_list` returns these conditions without needing
an experiment, session, logs action, or native execution. HTTP environment
components include the same reason and message. The website marks blocked
components in red and shows the recovery message in their inspector.

Compact MCP status and wait receipts include `blockers` (up to eight; use the
paged component status tool for the complete list). Waiting for `ready` returns
immediately when a startup blocker is observed, for example:

```json
{
  "phase": "pending",
  "target_phase": "ready",
  "reached": false,
  "timed_out": false,
  "blockers": [{
    "component_id": "wallet-cdk",
    "reason": "image_pull_backoff",
    "message": "Image pull is failing and backing off, not building. Operator: run make images and make doctor; verify registry access."
  }]
}
```

This is a recoverable failure, not proof of an active build and not successful
readiness. Agents should report the blocker and follow its recovery guidance.
An MCP-only agent asks the operator to run host setup commands; it must not
invent an experiment or repeatedly wait for a missing image to appear. Closing
waits continue normally; an image failure must not prevent verified teardown.

## Setup and image availability

`make setup` invokes `make images` before deployment. The image list comes from
the installed catalog. For each local-registry image, setup checks its exact
manifest digest and restores it from the Docker cache if missing. A mutable tag
or different rebuild digest is never silently substituted into a published lock.
Repeated runs skip images already present at the required digest.

`make doctor` verifies every distinct catalog image with a real container-runtime
pull on each schedulable local k3d node, covering registry access and platform
compatibility. A missing or inaccessible image fails the check with a recovery
command. These checks download/cache images but do not create labs or run them.

The two wallet artifacts are currently local-only pinned builds. On a cold
machine with neither the registry contents nor an exact cached image, setup
fails explicitly. Import the exact images with `docker load`/`docker pull` before
running `make images`, or deliberately rebuild and update the catalog pins after
validation. The source recipes live in `docker/wallet/`; rebuilding source does
not guarantee the old manifest digest. Publishing these artifacts to a shared
registry remains separate work.
