#!/usr/bin/env python3
"""Publish the three built AMD64 development images under new, unique GHCR tags.

No rebuilding, deployment, catalog edits, or release-readiness promotion. All
local probes pass before the first push. A progressive receipt records partial
uploads if a later push or anonymous verification fails. Credentials are used
only by Docker's own push command, never read or logged here.
"""
import argparse
import json
from pathlib import Path
import re
import subprocess
import uuid

import controller_release
import publish_images
import release

NAMESPACE = controller_release.NAMESPACE


def plan(controller, wallets):
    release.require(controller["platform"] == "linux/amd64" and controller["release_ready"] is False,
                    "expected an AMD64 development controller")
    release.require(controller["metadata"]["source_sha256"] == controller["source"]["sha256"],
                    "controller provenance mismatch")
    release.require(all(controller["verification"].get(key) is True for key in
                        ["offline_metadata", "non_root", "helper_startup", "host_contract_match"]),
                    "controller build verification is incomplete")
    publication_id = uuid.uuid4().hex
    entries = [{"repository": "proofstormd", "local_image_id": controller["local_image_id"]}]
    for repository in ["cdk-cli-wallet", "cocod-wallet"]:
        matches = [entry for entry in wallets["local_amd64_wallet_builds"]
                   if entry["tag"].split(":")[0] == "proofstorm-linux-check/" + repository]
        release.require(len(matches) == 1, "missing or duplicate wallet build receipt")
        entry = matches[0]
        entries.append({"repository": repository,
                        "local_image_id": entry.get("local_image_id", entry.get("build_manifest_digest"))})
    for entry in entries:
        release.require(isinstance(entry["local_image_id"], str) and
                        re.fullmatch(r"sha256:[0-9a-f]{64}", entry["local_image_id"]), "invalid built image identity")
        entry["tag"] = NAMESPACE + "/" + entry["repository"] + ":development-amd64-" + publication_id
        entry.update({"uploaded": False, "anonymous_verified": False})
    return {"format_version": 1, "publication_id": publication_id, "namespace": NAMESPACE,
            "platform": "linux/amd64", "release_ready": False,
            "controller_build": controller, "images": entries}


def preflight(value):
    for entry in value["images"]:
        image_id = entry["local_image_id"]
        if entry["repository"] == "proofstormd":
            built = value["controller_build"]
            checked = controller_release.verify_local(image_id, "linux/amd64", built["source"], built["metadata"])
            release.require(checked["local_image_id"] == image_id, "controller identity changed")
            entry["local_verification"] = checked["verification"]
        else:
            result = subprocess.run(["docker", "image", "inspect", image_id],
                                    check=True, capture_output=True, text=True)
            image = json.loads(result.stdout)[0]
            release.require(image["Id"] == image_id and image["Os"] == "linux" and
                            image["Architecture"] == "amd64" and image["Config"]["User"] == "1000:1000",
                            "wallet image identity, architecture, or user mismatch")
            executable, expected = (("cdk-cli", "cdk-cli 0.18.0") if entry["repository"] == "cdk-cli-wallet"
                                    else ("cocod", "0.0.17"))
            result = subprocess.run(["docker", "run", "--rm", "--platform", "linux/amd64", "--network", "none",
                                     "--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
                                     "--memory", "256m", "--cpus", "1", "--pids-limit", "128",
                                     "--entrypoint", executable, image_id, "--version"],
                                    check=True, capture_output=True, text=True, timeout=60)
            release.require(result.stdout.strip() == expected, "wallet version mismatch")
            entry["local_verification"] = {"offline_version": result.stdout.strip(), "non_root": True}
        print(f"Local AMD64 preflight passed: {entry['repository']}", flush=True)


contains_identity = publish_images.contains_identity


def publish(controller_path, wallets_path, work, confirm_namespace):
    release.require(confirm_namespace == NAMESPACE, "exact namespace confirmation required")
    release.require(not work.exists(), "publication work directory must be new; retain prior receipts")
    value = plan(json.loads(controller_path.read_text()), json.loads(wallets_path.read_text()))
    work.mkdir(parents=True)
    receipt = work / "publication.json"
    release.write_json(receipt, value)
    preflight(value)
    release.write_json(receipt, value)
    for entry in value["images"]:
        print(f"Publishing AMD64 development image: {entry['repository']}", flush=True)
        subprocess.run(["docker", "tag", entry["local_image_id"], entry["tag"]], check=True)
        subprocess.run(["docker", "push", entry["tag"]], check=True)
        entry["uploaded"] = True
        release.write_json(receipt, value)
        result = subprocess.run(["docker", "buildx", "imagetools", "inspect", entry["tag"],
                                 "--format", "{{json .Manifest}}"], check=True, capture_output=True, text=True)
        digest = json.loads(result.stdout)["digest"]
        release.require(re.fullmatch(r"sha256:[0-9a-f]{64}", digest), "invalid published digest")
        repository = (NAMESPACE + "/" + entry["repository"]).removeprefix("ghcr.io/")
        entry["image"] = "ghcr.io/" + repository + "@" + digest
        release.write_json(receipt, value)
        registry = publish_images.Registry(repository)
        platforms = registry.inspect(digest)
        release.require(platforms == {"linux/amd64"}, "unexpected published image architectures")
        release.require(contains_identity(registry, digest, entry["local_image_id"]),
                        "published image differs from the verified local image")
        entry.update({"anonymous_verified": True, "platforms": sorted(platforms)})
        release.write_json(receipt, value)
        print(f"Anonymous AMD64 download verified: {entry['repository']}", flush=True)
    print(f"Published all three development images. Receipt: {receipt}", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--controller-receipt", type=Path, required=True)
    parser.add_argument("--wallet-receipt", type=Path,
                        default=Path(__file__).resolve().parents[1] / "release/linux-container-verification.json")
    parser.add_argument("--work-dir", type=Path, required=True)
    parser.add_argument("--confirm-namespace", required=True)
    args = parser.parse_args()
    publish(args.controller_receipt, args.wallet_receipt, args.work_dir.resolve(), args.confirm_namespace)
