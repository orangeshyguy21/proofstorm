#!/usr/bin/env python3
"""Build real Linux alpha/development bundles without checkout or host Docker socket mounts.

The build container has its own writable filesystem, capped resources, and no
privileges. This is host packaging, not the privileged Docker-in-Docker
runtime test. It never publishes, creates a cluster, or configures an agent.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import uuid

import release

TARGET = "x86_64-unknown-linux-gnu"
PLATFORM = release.TARGETS[TARGET]
SMOKE_IMAGE = "debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171"

INSTALL_CHECK = r'''
set -eu
for tool in cargo rustc trunk python3 docker; do
    if command -v "$tool" >/dev/null 2>&1; then
        echo "Unexpected prerequisite in clean installer test: $tool" >&2
        exit 1
    fi
done
test ! -e /input/source
mkdir -p /tmp/first-user
export HOME=/tmp/first-user PROOFSTORM_HOME=/tmp/runtime-must-not-exist
if [ "$2" = true ]; then set -- "$1" --allow-development; else set -- "$1"; fi
for attempt in first reinstall; do
    sh /input/install.sh --artifact-dir /input --archive "$1" \
        --prefix /tmp/first-user/.local ${2:+"$2"}
    /tmp/first-user/.local/bin/proofstorm --version
    /tmp/first-user/.local/bin/proofstorm --help >/dev/null
    /tmp/first-user/.local/bin/proofstorm release-info > /tmp/cli-info.json
    /tmp/first-user/.local/bin/proofstorm-mcp --version
    /tmp/first-user/.local/bin/proofstorm-mcp --help >/dev/null
    /tmp/first-user/.local/bin/proofstorm-mcp --release-info > /tmp/mcp-info.json
    cmp /tmp/cli-info.json /tmp/mcp-info.json
    test ! -e /tmp/runtime-must-not-exist
    test ! -e /tmp/first-user/.codex
    test ! -e /tmp/first-user/.config/opencode
done
echo 'Install and reinstall checks passed.'
'''


def verify_snapshot(root, provenance):
    """Check the transported snapshot before installing tools or compiling it."""
    receipt = hashlib.sha256()
    for path in sorted(root.rglob("*"), key=lambda item: item.relative_to(root).as_posix()):
        release.require(not path.is_symlink(), "snapshot symlink refused")
        if path.is_dir():
            continue
        release.require(path.is_file(), "snapshot special file refused")
        name = path.relative_to(root).as_posix()
        mode = 0o755 if path.stat().st_mode & 0o111 else 0o644
        receipt.update(name.encode() + b"\0" + str(mode).encode() + b"\0")
        receipt.update(bytes.fromhex(release.digest(path)))
    release.require(receipt.hexdigest() == provenance["sha256"], "transported source checksum mismatch")


def create_command(name, tag):
    return ["docker", "create", "--name", name, "--platform", PLATFORM,
            "--cpus", "2", "--memory", "3g", "--pids-limit", "512",
            "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
            tag, "python3", "-B", "/input/source/scripts/linux_container.py", "worker"]


def cleanup(name, log_path):
    """Keep log retrieval failures from preventing shutdown of our container."""
    try:
        with log_path.open("w") as log:
            subprocess.run(["docker", "logs", name], stdout=log, stderr=subprocess.STDOUT,
                           check=False, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        print(f"Could not save logs for {name}", flush=True)
    try:
        subprocess.run(["docker", "stop", "--timeout", "10", name], check=True, timeout=30)
        subprocess.run(["docker", "rm", name], check=True, timeout=30)
    except (OSError, subprocess.SubprocessError):
        print(f"Cleanup incomplete. Check this test container: {name}", flush=True)
        print(f"Stop it with: docker stop --timeout 10 {name}", flush=True)


def build(source, work, debug=False, development=False):
    source, work = source.resolve(), work.resolve()
    release.require(not work.exists(), "work directory must be new")
    release.require(not work.is_relative_to(source), "build outside the checkout")
    work.mkdir(parents=True)
    inputs = work / "input"
    inputs.mkdir()
    provenance = release.snapshot(source, inputs / "source", development or release.alpha_source(source))
    release.write_json(inputs / "source.json", provenance)
    release.write_json(inputs / "options.json", {"debug": debug, "development": development})
    # Only the Dockerfile is sent to the toolchain build. Source is copied later.
    context = work / "toolchain"
    context.mkdir()
    dockerfile = inputs / "source/docker/release/Dockerfile.linux-builder"
    shutil.copyfile(dockerfile, context / "Dockerfile")
    tag = "proofstorm-linux-builder:" + release.digest(dockerfile)[:16]
    name = "proofstorm-linux-build-" + uuid.uuid4().hex
    release.write_json(work / "run.json", {"container": name, "toolchain_image": tag,
                       "platform": PLATFORM, "source": provenance, "privileged": False,
                       "host_mounts": [], "cpus": 2, "memory": "3g"})
    print("Preparing the isolated Linux toolchain (first use downloads build tools)", flush=True)
    subprocess.run(["docker", "buildx", "build", "--platform", PLATFORM, "--load",
                    "--tag", tag, str(context)], check=True, timeout=900)
    created = False
    try:
        subprocess.run(create_command(name, tag), check=True)
        created = True
        subprocess.run(["docker", "cp", str(inputs), name + ":/input"], check=True)
        print("Building in Linux storage: 2 CPUs / 3 GiB; no host mounts", flush=True)
        subprocess.run(["docker", "start", "--attach", name], check=True, timeout=3600)
        # docker start --attach does not reliably propagate every worker failure.
        status = subprocess.run(["docker", "inspect", "--format", "{{.State.ExitCode}}", name],
                                check=True, capture_output=True, text=True)
        release.require(status.stdout.strip() == "0", "Linux build failed; see container output")
        subprocess.run(["docker", "cp", name + ":/artifacts", str(work / "artifacts")], check=True)
        print(f"Linux artifacts and verification reports: {work / 'artifacts'}", flush=True)
    finally:
        if created:
            # Exact UUID-owned container only. Never prune Docker or touch other labs.
            cleanup(name, work / "build.log")


def worker():
    release.require(release.host_target() == TARGET, "worker must run on Linux x86-64")
    inputs, work, output = Path("/input"), Path("/build"), Path("/artifacts")
    transported = inputs / "source"
    provenance = json.loads((inputs / "source.json").read_text())
    options = json.loads((inputs / "options.json").read_text())
    verify_snapshot(transported, provenance)
    # Docker Desktop can preserve the Mac UID during docker cp. With all
    # capabilities dropped, root must not rely on bypassing that ownership.
    source = work / "source"
    shutil.copytree(transported, source)
    subprocess.run(["sh", str(source / "tools/install-trunk.sh")], check=True)
    release.compile_snapshot(source, provenance, work=work, output=output,
                             target=work / "target", trunk=source / ".tools/bin/trunk",
                             development=options["development"], debug=options["debug"], expected_target=TARGET)
    shutil.copyfile(work / "result.json", output / "build-report.json")
    result = json.loads((work / "result.json").read_text())
    release.smoke(Path(result["archive"]), work / "relocated", [])
    shutil.copyfile(work / "relocated/smoke-report.json", output / "smoke-report.json")
    shutil.copyfile(source / "install.sh", output / "install.sh")


def smoke_command(name, archive_name, image=SMOKE_IMAGE, development=False):
    return ["docker", "create", "--name", name, "--platform", PLATFORM,
            "--user", "1000:1000",
            "--network", "none", "--read-only", "--tmpfs", "/tmp:rw,exec,nosuid,nodev,size=768m",
            "--cpus", "2", "--memory", "1g", "--pids-limit", "128",
            "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
            image, "sh", "-c", INSTALL_CHECK, "install-check", archive_name, str(development).lower()]


def install_smoke(archive, installer, work, development=False):
    release.require(not archive.is_symlink() and not installer.is_symlink(), "smoke input symlink refused")
    archive, installer, work = archive.resolve(), installer.resolve(), work.resolve()
    release.require(archive.is_file() and installer.is_file(), "smoke inputs must exist")
    release.require(re.fullmatch(r"proofstorm-[A-Za-z0-9._+-]+-x86_64-unknown-linux-gnu\.tar\.gz", archive.name),
                    "expected a Linux x86-64 archive")
    checksum = Path(str(archive) + ".sha256")
    release.require(not checksum.is_symlink(), "checksum symlink refused")
    receipt = checksum.read_text().split()
    release.require(receipt == [release.digest(archive), archive.name], "archive checksum mismatch")
    release.require(not work.exists(), "smoke work directory must be new")
    work.mkdir(parents=True)
    inputs = work / "input"
    inputs.mkdir()
    for path in [archive, checksum]:
        shutil.copyfile(path, inputs / path.name)
    shutil.copyfile(installer, inputs / "install.sh")
    # Read-only inputs must be readable by the ordinary test user even when the
    # maintainer's host umask is private. No credentials are copied here.
    inputs.chmod(0o755)
    for path in inputs.iterdir():
        path.chmod(0o644)
    name = "proofstorm-linux-install-" + uuid.uuid4().hex
    tag = name + ":inputs"
    # Bake in only the three public test inputs before making the root read-only.
    # docker cp cannot populate a container whose root filesystem is read-only.
    (work / "Dockerfile").write_text(f"FROM {SMOKE_IMAGE}\nCOPY --chown=1000:1000 input/ /input/\n")
    release.write_json(work / "run.json", {"container": name, "base_image": SMOKE_IMAGE, "input_image": tag,
                       "archive_sha256": receipt[0], "installer_sha256": release.digest(installer),
                       "network": "none", "host_mounts": [], "privileged": False, "user": "1000:1000"})
    created = False
    try:
        subprocess.run(["docker", "buildx", "build", "--platform", PLATFORM, "--load",
                        "--tag", tag, str(work)], check=True, timeout=180)
        subprocess.run(smoke_command(name, archive.name, tag, development), check=True)
        created = True
        print("Testing install and reinstall in source-free Debian, with networking disabled", flush=True)
        subprocess.run(["docker", "start", "--attach", name], check=True, timeout=180)
        status = subprocess.run(["docker", "inspect", "--format", "{{.State.ExitCode}}", name],
                                check=True, capture_output=True, text=True)
        release.require(status.stdout.strip() == "0", "source-free installer check failed")
        release.write_json(work / "install-smoke-report.json", {
            "local_install": True, "reinstall": True, "cli_mcp_metadata_match": True,
            "source_checkout_present": False, "build_tools_present": False,
            "network_enabled": False, "runtime_tested": False, "github_download_tested": False,
            "archive_sha256": receipt[0], "installer_sha256": release.digest(installer),
            "development_override": development,
        })
        print(f"Source-free installer test passed: {work / 'install-smoke-report.json'}", flush=True)
    finally:
        if created:
            cleanup(name, work / "install.log")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    builder = sub.add_parser("build")
    builder.add_argument("--work-dir", type=Path, required=True)
    builder.add_argument("--debug", action="store_true", help="Faster alpha/development host build")
    builder.add_argument("--development", action="store_true", help="Build an unpublished local bundle instead of the version's normal channel")
    smoker = sub.add_parser("smoke", help="Offline, source-free install/reinstall test; no runtime setup")
    smoker.add_argument("--archive", type=Path, required=True)
    smoker.add_argument("--installer", type=Path, required=True)
    smoker.add_argument("--work-dir", type=Path, required=True)
    smoker.add_argument("--development", action="store_true", help="Explicitly test an unpublished development bundle")
    sub.add_parser("worker", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.command == "worker":
        worker()
    elif args.command == "smoke":
        install_smoke(args.archive, args.installer, args.work_dir, args.development)
    else:
        build(Path(__file__).resolve().parents[1], args.work_dir, args.debug, args.development)


if __name__ == "__main__":
    main()
