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
import uuid

import release

NAMESPACE = "ghcr.io/orangeshyguy21/proofstorm"


def build(source, work):
    release.require(not work.exists(), "work directory must be new")
    release.require(not work.is_relative_to(source), "build outside the checkout")
    work.mkdir(parents=True)
    provenance = release.snapshot(source, work / "source", True)
    tag = NAMESPACE + "/proofstormd:development-" + uuid.uuid4().hex
    subprocess.run(["docker", "buildx", "build", "--platform", "linux/arm64", "--load",
                    "--build-arg", "CARGO_BUILD_JOBS=2", "--build-arg",
                    "PROOFSTORM_CONTROLLER_SOURCE_SHA256=" + provenance["sha256"],
                    "--file", str(work / "source/Dockerfile.proofstormd"), "--tag", tag,
                    str(work / "source")], check=True)
    result = subprocess.run(["docker", "run", "--rm", "--network", "none", "--read-only",
                             "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
                             "--memory", "128m", "--cpus", "1", tag, "--release-info"],
                            check=True, capture_output=True, text=True)
    info = json.loads(result.stdout)
    release.require(info["source_sha256"] == provenance["sha256"], "controller provenance mismatch")
    receipt = {"format_version": 1, "release_ready": False, "platform": "linux/arm64",
               "source": provenance, "tag": tag, "metadata": info}
    release.write_json(work / "controller-build.json", receipt)
    print(json.dumps(receipt, indent=2))


def publish(receipt_path, namespace):
    release.require(namespace == NAMESPACE, "namespace confirmation required")
    receipt = json.loads(receipt_path.read_text())
    tag = receipt["tag"]
    release.require(re.fullmatch(re.escape(NAMESPACE) + r"/proofstormd:development-[0-9a-f]{32}", tag)
                    and receipt["release_ready"] is False and receipt["platform"] == "linux/arm64"
                    and receipt["metadata"]["source_sha256"] == receipt["source"]["sha256"],
                    "not a development controller receipt")
    subprocess.run(["docker", "push", tag], check=True)
    result = subprocess.run(["docker", "buildx", "imagetools", "inspect", tag,
                             "--format", "{{json .Manifest}}"], check=True, capture_output=True, text=True)
    digest = json.loads(result.stdout)["digest"]
    release.require(re.fullmatch(r"sha256:[0-9a-f]{64}", digest), "invalid published digest")
    receipt["image"] = NAMESPACE + "/proofstormd@" + digest
    release.write_json(receipt_path, receipt)
    print(json.dumps(receipt, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    builder = sub.add_parser("build")
    builder.add_argument("--work-dir", type=Path, required=True)
    publisher = sub.add_parser("publish")
    publisher.add_argument("--receipt", type=Path, required=True)
    publisher.add_argument("--confirm-namespace", required=True)
    args = parser.parse_args()
    if args.command == "build":
        build(Path(__file__).resolve().parents[1], args.work_dir.resolve())
    else:
        publish(args.receipt, args.confirm_namespace)
