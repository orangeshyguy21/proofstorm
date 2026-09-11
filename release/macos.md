# macOS Apple Silicon release

Status: CI build/install coverage and multi-platform draft promotion are wired.
The public alpha.2 still contains only Linux AMD64. Hosted builds and fresh-Mac
acceptance must pass before announcing a Mac download.

## What a Mac release contains

| Piece | Target | Runs where |
| --- | --- | --- |
| CLI, MCP server, embedded browser GUI | `aarch64-apple-darwin` | macOS |
| Controller and execution helper | `linux/arm64` | Docker's Linux runtime |
| Lab components and protocol prober | Pinned images with ARM64 support | The same private runtime |

There is no macOS container image. The host bundle is native macOS; its services
are Linux containers. A matching controller receipt must come from the exact same
clean source snapshot as the host bundle, including its version and source hash.

The controller build/publication commands now accept ARM64, verify local image
architecture, run both startup probes for that architecture, and verify anonymous
registry manifest/config identity and layer access. Native bundle builds now use
an explicitly supplied ARM64 receipt instead of silently retaining the old Mac
controller pin. Crossed AMD64/ARM64 receipts are rejected.

Main CI builds the ARM64 controller on `ubuntu-24.04-arm`, then passes its verified
receipt to a native `macos-15` runner. The Mac runner builds the host bundle without
Docker, verifies relocation with source access denied, and tests install/reinstall
under a checked sandbox. GitHub-hosted runners are enough for these build checks;
no AWS host is needed until fresh-host runtime and desktop acceptance.

The CI Mac deployment target is **macOS 15 or newer, Apple Silicon**. Intel Mac is
not included. These archives are not Developer ID signed/notarized; CI success
does not establish Gatekeeper or first-run desktop behavior.

## Build commands

Run from a clean checkout on an Apple Silicon Mac with Rust, just, Docker/Buildx,
and the pinned Trunk tool/browser target installed (`just web-tools`). Keep the
checkout unchanged across controller and host builds. Work directories must be
new and outside the checkout; the commands do not touch registered dev binaries.

```sh
mac_work=$(mktemp -d /tmp/proofstorm-macos.XXXXXXXX)
just release-controller-build --platform linux/arm64 \
  --work-dir "$mac_work/controller"
```

That builds and probes the controller locally. It does not publish or start a
cluster. Publication is a separate, authenticated action when authorized:

```sh
just release-controller-publish --work-dir "$mac_work/controller" \
  --confirm-namespace ghcr.io/orangeshyguy21/proofstorm
just release-ci-macos --work-dir "$mac_work/bundle" \
  --controller-receipt "$mac_work/controller/controller.json"
```

The output directory `bundle/bundle` contains the archive, checksum, installer,
and three reports. Work outside both source trees, a disposable home, restricted
PATH, and a verified `sandbox-exec` policy isolate the installer from source reads,
compiler execution, networking, and outside writes. Source and build tools still
exist on the host; the report explicitly records that access is restricted.
Unavailable or ineffective sandbox enforcement fails rather than falling back.

`just release` requires matching Linux and Mac artifacts from the same main commit
and run attempt. It prepares one draft with friendly `linux-amd64`/`macos-arm64`
download names, one installer, and the six platform reports inside a single
`verification-reports.tar.gz`, plus a generated `release.json` download inventory.
It still asks for approval. See [release flow](../scripts/RELEASING.md).
A build/startup receipt is not fresh-host acceptance evidence.

## AWS test host

EC2 Mac uses dedicated physical Apple hardware, not a small shared VM. AWS requires
a **24-hour minimum host allocation**, with one Mac instance per host. Choose an
Apple Silicon type, not Intel `mac1.metal`. An M2 `mac2-m2.metal` has 24 GiB RAM;
it is a reasonable candidate, subject to region availability, quota, and an
approved quote. This is a proposed test configuration, not a measured minimum.
[AWS Mac instance documentation](https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/ec2-mac-instances.html).

Before allocating anything:

1. Confirm region, Mac Dedicated Host quota, compatible macOS AMI, and total
   24-hour host + EBS cost. Use encrypted EBS, not the unmanaged internal SSD.
2. Restrict SSH to the tester. Use SSH tunnelling for remote desktop and the GUI;
   keep Kubernetes, the registry, and Proofstorm HTTP ports on loopback.
3. Plan a desktop login for Docker Desktop and native app tests. SSH-only process
   checks do not establish that browser/app launch works.
4. Record the host allocation time and assign cleanup ownership before launch.

On the host, install and start a supported Docker Desktop release for Apple
Silicon. Its OS requirements, licensing, and first-run permissions are independent
of Proofstorm. Verify `docker info` and `docker buildx version`, then run an ARM64
container before attempting Proofstorm. Do not infer Docker readiness from a
successful Rust build. See [Docker's Mac installation guide](https://docs.docker.com/desktop/setup/install/mac-install/).

When finished, stop/terminate the instance **and release its Dedicated Host** once
the minimum allocation period has elapsed. Stopping the instance alone does not
release the host. AWS warns that Apple Silicon host scrubbing can take up to
4.5 hours, during which the host cannot launch an instance; billing is paused in
the scrubbing `pending` state. Check the host and retained EBS resources afterward.
[AWS stop and release procedure](https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/mac-instance-stop.html).

## Remaining acceptance

1. Build the ARM64 controller from reviewed source, publish by immutable digest,
   and build the matching native Mac archive. Verify every catalog/helper image
   needed by the smoke lab has an anonymously accessible ARM64 manifest.
2. Verify the first hosted Mac build and installer run, then prepare a new alpha
   version through the shared release flow. Inspect both platform reports before
   explicitly publishing a candidate for fresh-host testing.
3. Test the public installer on a fresh Mac account/host with no Proofstorm source,
   installed Rust, development flags, or cached Proofstorm state. Docker and the
   chosen agent are prerequisites, not payload build tools. No compilation should
   occur during installation or setup.
4. Repeat Linux's install → setup/doctor → Bitcoin lab → actual MCP read →
   reinstall → cleanup scenario. Compare prober scale and status to Kubernetes.
   Reinstall must preserve the existing lab; cleanup must verify storage absence.
5. In a desktop session, separately test default-browser opening/reuse, folder
   selection, and native agent buttons. Record permission prompts and any signing
   or Gatekeeper friction. Do not prescribe disabling macOS security checks.

Keep build evidence, source-free installer evidence, public-download evidence,
runtime evidence, and visual/native-app evidence separate. A cached developer
machine or an offline archive test alone is not clean-Mac acceptance.
