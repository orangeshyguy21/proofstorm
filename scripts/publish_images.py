#!/usr/bin/env python3
"""Copy reviewed custom images to GHCR without rebuilding or changing digests.

Plan/preflight/verify are read-only. Publish requires an exact namespace
confirmation and uses the user's existing Docker credential store. No credentials
are read by this script. Anonymous registry verification never uses that store.
"""
import argparse
import hashlib
import json
import re
from pathlib import Path
import subprocess
import urllib.error
import urllib.parse
import urllib.request
import uuid

NAMESPACE = "ghcr.io/orangeshyguy21/proofstorm"
CANONICAL = "proofstorm-registry.localhost:5000/"
ACCEPT = ",".join(["application/vnd.oci.image.index.v1+json", "application/vnd.oci.image.manifest.v1+json",
                   "application/vnd.docker.distribution.manifest.list.v2+json", "application/vnd.docker.distribution.manifest.v2+json"])


def require(condition, message):
    if not condition:
        raise ValueError(message)


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


class SafeRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        require(urllib.parse.urlparse(newurl).scheme == "https", "registry redirect must use HTTPS")
        redirected = super().redirect_request(request, fp, code, msg, headers, newurl)
        if redirected and urllib.parse.urlparse(newurl).netloc != urllib.parse.urlparse(request.full_url).netloc:
            redirected.remove_header("Authorization")
        return redirected


class Registry:
    def __init__(self, repository, local=False):
        require(re.fullmatch(r"[a-z0-9][a-z0-9._/-]*", repository) and ".." not in repository.split("/"), "unsafe repository")
        self.repository = repository
        self.base = "http://127.0.0.1:5111" if local else "https://ghcr.io"
        self.opener = urllib.request.build_opener(SafeRedirect())
        self.headers = {"Accept": ACCEPT}
        if not local:
            # Request only an anonymous pull token. Never use gh or Docker credentials here.
            query = urllib.parse.urlencode({"service": "ghcr.io", "scope": f"repository:{repository}:pull"})
            with self.opener.open("https://ghcr.io/token?" + query, timeout=30) as response:
                token = json.loads(response.read(1024 * 1024))["token"]
            self.headers["Authorization"] = "Bearer " + token

    def data(self, kind, digest):
        require(re.fullmatch(r"sha256:[0-9a-f]{64}", digest), "invalid registry digest")
        request = urllib.request.Request(f"{self.base}/v2/{self.repository}/{kind}/{digest}", headers=self.headers)
        with self.opener.open(request, timeout=30) as response:
            body = response.read(4 * 1024 * 1024 + 1)
        require(len(body) <= 4 * 1024 * 1024, "registry metadata too large")
        require("sha256:" + hashlib.sha256(body).hexdigest() == digest, "registry digest mismatch")
        return json.loads(body)

    def blob_available(self, digest):
        require(re.fullmatch(r"sha256:[0-9a-f]{64}", digest), "invalid blob digest")
        request = urllib.request.Request(f"{self.base}/v2/{self.repository}/blobs/{digest}", method="HEAD", headers=self.headers)
        with self.opener.open(request, timeout=30) as response:
            require(response.status == 200, "blob is unavailable")

    def inspect(self, digest, depth=0):
        require(depth <= 3, "registry index nesting exceeds limit")
        manifest = self.data("manifests", digest)
        if "manifests" in manifest:
            require(0 < len(manifest["manifests"]) <= 100, "invalid image index")
            platforms = set()
            for descriptor in manifest["manifests"]:
                if descriptor.get("platform", {}).get("os") == "unknown":
                    continue  # Attestations are not runnable image platforms.
                platforms.update(self.inspect(descriptor["digest"], depth + 1))
            return platforms
        config = self.data("blobs", manifest["config"]["digest"])
        platform = config["os"] + "/" + config["architecture"]
        if platform == "linux/arm64":
            for layer in manifest["layers"]:
                self.blob_available(layer["digest"])
        return {platform}


def plan(info):
    images = []
    for image in info["workload_images"]:
        if not image.startswith(CANONICAL) or image.startswith(CANONICAL + "upstream/"):
            continue
        repository, digest = image.removeprefix(CANONICAL).split("@")
        require(re.fullmatch(r"[a-z0-9][a-z0-9-]*", repository), "unsafe custom image name")
        require(re.fullmatch(r"sha256:[0-9a-f]{64}", digest), "unpinned custom image")
        images.append({"canonical_image": image, "repository": repository, "digest": digest,
                       "source": "127.0.0.1:5111/" + repository + "@" + digest,
                       "destination": NAMESPACE + "/" + repository + "@" + digest})
    require(images, "no custom catalog images found")
    return {"format_version": 1, "namespace": NAMESPACE, "visibility": "public",
            "publication_id": uuid.uuid4().hex, "images": images}


def validate_plan(value):
    require(value["format_version"] == 1 and value["namespace"] == NAMESPACE and value["visibility"] == "public", "publication destination is not confirmed")
    require(re.fullmatch(r"[0-9a-f]{32}", value["publication_id"]), "invalid publication identity")
    expected = plan({"workload_images": [entry["canonical_image"] for entry in value["images"]]})
    require(expected["images"] == value["images"], "publication plan contains altered sources/destinations")


def preflight(value):
    validate_plan(value)
    for entry in value["images"]:
        platforms = Registry(entry["repository"], local=True).inspect(entry["digest"])
        require("linux/arm64" in platforms, f"{entry['repository']} does not support Linux arm64")
        print(f"Source verified: {entry['repository']} ({', '.join(sorted(platforms))})", flush=True)


def verify(value):
    validate_plan(value)
    report = {"namespace": NAMESPACE, "anonymous_verified": [], "needs_public_visibility": []}
    for entry in value["images"]:
        try:
            repository = entry["destination"].removeprefix("ghcr.io/").split("@")[0]
            platforms = Registry(repository).inspect(entry["digest"])
            require("linux/arm64" in platforms, "published image lacks Linux arm64")
            report["anonymous_verified"].append({"image": entry["destination"], "platforms": sorted(platforms), "arm64_blobs_accessible": True})
            print(f"Anonymous download verified: {entry['repository']}", flush=True)
        except (urllib.error.HTTPError, urllib.error.URLError, ValueError) as error:
            # Do not print token-bearing URLs or request headers.
            report["needs_public_visibility"].append({"image": entry["destination"],
                "reason": f"anonymous verification failed ({type(error).__name__}); check package visibility, digest, and availability"})
    return report


def publish(value, confirm_namespace, output):
    require(confirm_namespace == NAMESPACE, "publishing requires the exact confirmed namespace")
    preflight(value)
    receipt = {"publication_id": value["publication_id"], "copied": []}
    for entry in value["images"]:
        # Unique staging tag avoids replacing any existing version or mutable alias.
        tag = entry["destination"].split("@")[0] + ":upload-" + value["publication_id"]
        print(f"Publishing pinned image: {entry['repository']}", flush=True)
        subprocess.run(["docker", "buildx", "imagetools", "create", "--prefer-index=false", "--tag", tag, entry["source"]], check=True)
        result = subprocess.run(["docker", "buildx", "imagetools", "inspect", entry["destination"], "--format", "{{json .Manifest}}"], check=True, capture_output=True, text=True)
        require(json.loads(result.stdout)["digest"] == entry["digest"], "published digest did not match source")
        receipt["copied"].append({"image": entry["destination"], "staging_tag": tag})
        write_json(output, receipt)
    receipt["verification"] = verify(value)
    write_json(output, receipt)
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    planner = sub.add_parser("plan")
    planner.add_argument("--release-info", type=Path, required=True)
    planner.add_argument("--output", type=Path, required=True)
    for name in ["preflight", "verify", "publish"]:
        command = sub.add_parser(name)
        command.add_argument("--plan", type=Path, required=True)
        if name != "preflight":
            command.add_argument("--output", type=Path, required=True)
        if name == "publish":
            command.add_argument("--confirm-namespace", required=True)
    args = parser.parse_args()
    if args.command == "plan":
        write_json(args.output, plan(json.loads(args.release_info.read_text())))
    else:
        value = json.loads(args.plan.read_text())
        if args.command == "preflight":
            preflight(value)
        elif args.command == "publish":
            publish(value, args.confirm_namespace, args.output)
        else:
            result = verify(value)
            write_json(args.output, result)
            require(not result["needs_public_visibility"], "some images are not anonymously downloadable; see report")


if __name__ == "__main__":
    main()
