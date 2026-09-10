#!/usr/bin/env python3
"""Build a development controller from a recorded, external source snapshot.

Never overwrites developer tags or deploys anything. Publishing is separate and
requires an exact namespace confirmation. This is not a production release gate.
"""
import argparse
import json
import re
from pathlib import Path
import subprocess
import tarfile
import uuid

import release
import publish_images

NAMESPACE = "ghcr.io/orangeshyguy21/proofstorm"


def bundle_metadata(archive, platform):
    receipt = Path(str(archive) + ".sha256").read_text().split()
    release.require(receipt == [release.digest(archive), archive.name], "host bundle checksum mismatch")
    with tarfile.open(archive) as bundle:
        matches = [member for member in bundle.getmembers() if member.name == "proofstorm/release-info.json"]
        release.require(len(matches) == 1 and matches[0].isfile() and matches[0].size <= 4 * 1024 * 1024,
                        "invalid host bundle metadata")
        info = json.load(bundle.extractfile(matches[0]))
    release.require(release.TARGETS.get(info["target"]) == platform, "host bundle platform mismatch")
    return info


def verify_local(tag, platform, provenance, host_info=None):
    result = subprocess.run(["docker", "image", "inspect", tag], check=True, capture_output=True, text=True)
    image = json.loads(result.stdout)[0]
    image_id = image["Id"]
    release.require(re.fullmatch(r"sha256:[0-9a-f]{64}", image_id), "invalid local image identity")
    release.require(image["Os"] + "/" + image["Architecture"] == platform, "controller image platform mismatch")
    release.require(image["Config"]["User"] == "65532:65532", "controller must run as its non-root user")
    release.require(image["Config"]["Labels"]["dev.proofstorm.source-sha256"] == provenance["sha256"],
                    "controller image source label mismatch")
    command = ["docker", "run", "--rm", "--platform", platform, "--network", "none", "--read-only",
               "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
               "--memory", "128m", "--cpus", "1", "--pids-limit", "128"]
    result = subprocess.run([*command, image_id, "--release-info"],
                            check=True, capture_output=True, text=True, timeout=60)
    info = json.loads(result.stdout)
    release.require(info["format_version"] == 1 and info["source_sha256"] == provenance["sha256"],
                    "controller provenance mismatch")
    if host_info is not None:
        release.require(all(info[key] == host_info[key] for key in ["version", "runtime_contract_sha256"]),
                        "controller does not match the tested host bundle")
    # No arguments is a deliberate, side-effect-free error-path probe. A loader
    # or architecture error must not be mistaken for the helper's own response.
    helper = subprocess.run([*command, "--entrypoint", "/usr/local/lib/proofstorm-exec", image_id],
                            check=False, capture_output=True, text=True, timeout=60)
    release.require(helper.returncode == 1 and helper.stdout == "" and
                    helper.stderr.strip() == '{"runner_error":"native_runner_failed"}',
                    "bundled execution helper failed its startup probe")
    return {"metadata": info, "local_image_id": image_id,
            "verification": {"offline_metadata": True, "non_root": True,
                             "helper_startup": True, "host_contract_match": host_info is not None,
                             "cluster_reconciliation": False}}


def build(source, work, platform=None, host_bundle=None):
    platform = platform or release.TARGETS[release.host_target()]
    release.require(platform in {"linux/arm64", "linux/amd64"}, "unsupported controller platform")
    release.require(not work.exists(), "work directory must be new")
    release.require(not work.is_relative_to(source), "build outside the checkout")
    host_info = bundle_metadata(host_bundle, platform) if host_bundle else None
    work.mkdir(parents=True)
    provenance = release.snapshot(source, work / "source", True)
    tag = NAMESPACE + "/proofstormd:development-" + uuid.uuid4().hex
    subprocess.run(["docker", "buildx", "build", "--platform", platform, "--load",
                    "--build-arg", "CARGO_BUILD_JOBS=2", "--build-arg",
                    "PROOFSTORM_CONTROLLER_SOURCE_SHA256=" + provenance["sha256"],
                    "--file", str(work / "source/Dockerfile.proofstormd"), "--tag", tag,
                    str(work / "source")], check=True)
    checked = verify_local(tag, platform, provenance, host_info)
    receipt = {"format_version": 1, "release_ready": False, "platform": platform,
               "source": provenance, "tag": tag, **checked}
    if host_bundle:
        receipt["tested_host_bundle_sha256"] = release.digest(host_bundle)
    release.write_json(work / "controller-build.json", receipt)
    print(json.dumps(receipt, indent=2))


def publish(receipt_path, namespace):
    release.require(namespace == NAMESPACE, "namespace confirmation required")
    receipt = json.loads(receipt_path.read_text())
    tag = receipt["tag"]
    release.require(re.fullmatch(re.escape(NAMESPACE) + r"/proofstormd:development-[0-9a-f]{32}", tag)
                    and receipt["release_ready"] is False and receipt["platform"] in {"linux/arm64", "linux/amd64"}
                    and receipt["metadata"]["source_sha256"] == receipt["source"]["sha256"],
                    "not a development controller receipt")
    checked = verify_local(tag, receipt["platform"], receipt["source"])
    release.require(checked["local_image_id"] == receipt["local_image_id"] and
                    checked["metadata"] == receipt["metadata"],
                    "controller tag no longer matches the verified build receipt")
    subprocess.run(["docker", "push", tag], check=True)
    result = subprocess.run(["docker", "buildx", "imagetools", "inspect", tag,
                             "--format", "{{json .Manifest}}"], check=True, capture_output=True, text=True)
    digest = json.loads(result.stdout)["digest"]
    release.require(re.fullmatch(r"sha256:[0-9a-f]{64}", digest), "invalid published digest")
    receipt["image"] = NAMESPACE + "/proofstormd@" + digest
    receipt["anonymous_verified"] = False
    release.write_json(receipt_path, receipt)
    registry = publish_images.Registry((NAMESPACE + "/proofstormd").removeprefix("ghcr.io/"))
    platforms = registry.inspect(digest)
    release.require(platforms == {receipt["platform"]}, "published controller platform mismatch")
    release.require(publish_images.contains_identity(registry, digest, receipt["local_image_id"]),
                    "published controller differs from the verified local image")
    receipt["anonymous_verified"] = True
    release.write_json(receipt_path, receipt)
    print(json.dumps(receipt, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    builder = sub.add_parser("build")
    builder.add_argument("--work-dir", type=Path, required=True)
    builder.add_argument("--platform", choices=["linux/arm64", "linux/amd64"], help="Maintainer override; defaults to the supported build host architecture")
    builder.add_argument("--host-bundle", type=Path, help="Verify compatibility with this checksum-verified CLI/MCP archive")
    publisher = sub.add_parser("publish")
    publisher.add_argument("--receipt", type=Path, required=True)
    publisher.add_argument("--confirm-namespace", required=True)
    args = parser.parse_args()
    if args.command == "build":
        build(Path(__file__).resolve().parents[1], args.work_dir.resolve(), args.platform, args.host_bundle)
    else:
        publish(args.receipt, args.confirm_namespace)
