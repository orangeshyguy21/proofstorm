#!/usr/bin/env python3
"""Build a checkout-independent alpha bundle, or verify an unpacked bundle.

Builds use a source snapshot and separate target directory. Development bundles
are explicit and carry release blockers; this command never publishes images,
starts Docker, edits a harness configuration, or installs into the user's PATH.
"""

import argparse
import gzip
import hashlib
import json
import os
import platform
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib

TARGETS = {"aarch64-apple-darwin": "linux/arm64", "x86_64-unknown-linux-gnu": "linux/amd64"}


def alpha_version(version):
    return isinstance(version, str) and re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+-alpha\.[0-9]+", version) is not None


def alpha_source(source):
    return alpha_version(tomllib.loads((source / "Cargo.toml").read_text())["workspace"]["package"]["version"])


def validate_alpha(info):
    """Alpha relaxes maturity gates, never artifact identity or compatibility."""
    require(alpha_version(info["version"]), "alpha channel requires an alpha version")
    controller = info.get("controller") or {}
    require(re.fullmatch(r"ghcr\.io/[^\s@]+@sha256:[0-9a-f]{64}", controller.get("image", "")),
            "alpha requires a published digest-pinned controller")
    require(controller.get("platform") == TARGETS[info["target"]] and
            controller.get("metadata", {}).get("version") == info["version"] and
            re.fullmatch(r"[0-9a-f]{64}", info.get("runtime_contract_sha256", "")) and
            controller.get("metadata", {}).get("runtime_contract_sha256") == info["runtime_contract_sha256"],
            "alpha controller compatibility mismatch")
    require(bool((info.get("bootstrap_tools") or {}).get("tools")), "alpha requires pinned bootstrap tools")
    require(all(image["published_source"] for image in image_inventory(info)),
            "alpha requires published workload image sources")


def host_target():
    host = (platform.system(), platform.machine())
    targets = {("Darwin", "arm64"): "aarch64-apple-darwin",
               ("Linux", "x86_64"): "x86_64-unknown-linux-gnu"}
    require(host in targets, "build on macOS Apple Silicon or Linux x86-64; cross-compilation is not supported")
    return targets[host]


LOCAL_REGISTRY = "proofstorm-registry.localhost:5000/"
REQUIRED = {"bin/proofstorm", "bin/proofstorm-mcp", "LICENSE", "catalog.json",
            "tools/versions.env", "chart/Chart.yaml", "chart/values.yaml",
            "chart/templates/deployment.yaml", "release-info.json",
            "chart/templates/_helpers.tpl", "chart/templates/serviceaccount.yaml",
            "chart/templates/rbac.yaml", "chart/templates/private-pvc.yaml",
            "chart/crds/proofstorm.dev_proofstormlabs.yaml",
            "chart/crds/proofstorm.dev_proofstormlabactions.yaml",
            "chart/crds/proofstorm.dev_proofstormcandidatebuilds.yaml"}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def clean_environment():
    return {key: value for key, value in os.environ.items()
            if not key.startswith(("PROOFSTORM_", "K3D_", "TRUNK_"))
            and key not in {"CARGO_BUILD_TARGET", "CARGO_TARGET_DIR"}}


def run(argv, *, cwd, env=None, capture=False):
    result = subprocess.run([str(arg) for arg in argv], cwd=cwd,
                            env=env if env is not None else clean_environment(),
                            stdout=subprocess.PIPE if capture else None,
                            text=True, check=True)
    return result.stdout if capture else None


def snapshot(source, destination, development):
    status = run(["git", "status", "--porcelain"], cwd=source, capture=True)
    require(development or not status.strip(), "release requires a clean committed source tree")
    revision = run(["git", "rev-parse", "HEAD"], cwd=source, capture=True).strip()
    names = run(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
                cwd=source, capture=True).split("\0")
    tree_hash = hashlib.sha256()
    destination.mkdir()
    for name in sorted(set(filter(None, names))):
        relative = Path(name)
        require(not relative.is_absolute() and ".." not in relative.parts, "unsafe source path")
        original = source / relative
        require(not original.is_symlink(), f"source symlink is not supported: {name}")
        if not original.exists():  # A tracked deletion in an explicit development snapshot.
            continue
        require(original.is_file(), f"source is not a regular file: {name}")
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(original, target)
        mode = 0o755 if original.stat().st_mode & 0o111 else 0o644
        target.chmod(mode)
        tree_hash.update(name.encode() + b"\0" + str(mode).encode() + b"\0")
        tree_hash.update(bytes.fromhex(digest(target)))
    # Do not stamp a clean revision onto files modified while the snapshot was copied.
    require(run(["git", "rev-parse", "HEAD"], cwd=source, capture=True).strip() == revision,
            "source revision changed during snapshot; retry")
    if not development:
        require(not run(["git", "status", "--porcelain"], cwd=source, capture=True).strip(),
                "source changed during snapshot; retry")
    return {"revision": revision, "dirty": bool(status.strip()), "sha256": tree_hash.hexdigest()}


def validate_info(info):
    require(info["format_version"] == 1, "unsupported binary metadata format")
    require(info["target"] in TARGETS, "unsupported bundle target")
    if info.get("bootstrap_tools"):
        require(info["bootstrap_tools"].get("target") == info["target"], "bootstrap tool target mismatch")
    require(info["build_profile"] in {"debug", "release"}, "unsupported build profile")
    require(re.fullmatch(r"[0-9A-Za-z][0-9A-Za-z.+-]*", info["version"]), "unsafe version")
    assets = info["web_assets"]
    require(any(asset["path"] == "index.html" for asset in assets), "missing embedded index.html")
    for suffix in [".js", ".wasm", ".css"]:
        require(any(asset["path"].endswith(suffix) for asset in assets), f"missing embedded {suffix}")
    for asset in assets:
        require(asset["size"] > 0 and re.fullmatch(r"[0-9a-f]{64}", asset["sha256"]),
                "invalid embedded asset receipt")
    for image in info["workload_images"]:
        require(re.fullmatch(r"[^\s@]+@sha256:[0-9a-f]{64}", image), f"image is not pinned: {image}")


def image_inventory(info):
    images = []
    publication = json.loads(info.get("image_publication", "{}"))
    namespace = publication.get("namespace")
    if namespace:
        require(re.fullmatch(r"ghcr\.io/[a-z0-9][a-z0-9._-]*/[a-z0-9][a-z0-9._/-]*", namespace), "invalid publication namespace")
    for image in info["workload_images"]:
        if image.startswith(LOCAL_REGISTRY + "upstream/"):
            source = image.removeprefix(LOCAL_REGISTRY + "upstream/")
        elif image.startswith(LOCAL_REGISTRY):
            source = namespace + "/" + image.removeprefix(LOCAL_REGISTRY) if namespace else None
        else:
            source = image
        images.append({"image": image, "published_source": source,
                       "verified_platforms": [], "availability_verified": False})
    return images


def package(source, binaries, output, provenance, development):
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="proofstorm-package-", dir=output) as temporary:
        root = Path(temporary) / "proofstorm"
        root.mkdir()
        (root / "bin").mkdir()
        for name in ["proofstorm", "proofstorm-mcp"]:
            binary = binaries / name
            require(binary.is_file() and not binary.is_symlink(), f"missing regular binary: {binary}")
            shutil.copyfile(binary, root / "bin" / name)
            (root / "bin" / name).chmod(0o755)
        # Execute the copied payload from its new location, never the source binaries.
        info = json.loads(run([root / "bin/proofstorm", "release-info"], cwd=root, capture=True))
        mcp = json.loads(run([root / "bin/proofstorm-mcp", "--release-info"], cwd=root, capture=True))
        require(info == mcp, "CLI and MCP were not built from the same release inputs")
        validate_info(info)
        alpha = not development and alpha_version(info["version"])
        if alpha:
            validate_alpha(info)
        require(info["source_revision"] == provenance["revision"] and
                info["source_sha256"] == provenance["sha256"], "binary/source provenance mismatch")
        shutil.copytree(source / "charts/proofstorm", root / "chart")
        shutil.copyfile(source / "LICENSE", root / "LICENSE")
        (root / "tools").mkdir()
        shutil.copyfile(source / "tools/versions.env", root / "tools/versions.env")
        require((root / "tools/versions.env").read_text() == info["tools"], "binary/tool pins mismatch")
        chart = dict(line.split(": ", 1) for line in (root / "chart/Chart.yaml").read_text().splitlines() if ": " in line)
        require(chart.get("version") == info["version"] and chart.get("appVersion") == info["version"],
                "chart version does not match the binaries")
        write_json(root / "catalog.json", info["catalog"])
        write_json(root / "release-info.json", info)
        images = image_inventory(info)
        controller = info.get("controller")
        target = info["target"]
        if controller:
            require(controller.get("platform") == TARGETS[target], "controller platform does not match host bundle")
            require(controller.get("metadata", {}).get("version") == info["version"] and
                    controller.get("metadata", {}).get("runtime_contract_sha256") == info.get("runtime_contract_sha256")
                    and bool(info.get("runtime_contract_sha256")),
                    "controller runtime contract does not match host bundle; rebuild the controller after changing image pins")
        blockers = [f"Remote image availability and {TARGETS[target]} platforms are not verified."]
        if target == "aarch64-apple-darwin":
            blockers.append("Downloaded macOS signing/quarantine behavior has not been validated.")
        else:
            blockers.append("Fresh Linux VM installation and runtime have not been validated.")
        if not controller:
            blockers.append("Published digest-pinned controller image is not configured.")
        elif not controller.get("release_ready"):
            blockers.append("Configured controller is a development preview, not a coherent release build.")
        if controller and controller.get("platform") != TARGETS[target]:
            blockers.append(f"Controller image is not pinned for {TARGETS[target]}.")
        if not info.get("bootstrap_tools"):
            blockers.append("Pinned bootstrap-tool downloads/checksums are not yet packaged.")
        blockers.extend(f"Missing published image source: {entry['image']}"
                        for entry in images if entry["published_source"] is None)
        if provenance["dirty"]:
            blockers.append("Source snapshot includes uncommitted development changes.")
        if info["build_profile"] != "release":
            blockers.append("Host executables use a debug build profile.")
        require(development or alpha or not blockers, "release blocked:\n" + "\n".join(blockers))
        files = {}
        for path in sorted(root.rglob("*")):
            require(not path.is_symlink(), f"payload symlink refused: {path}")
            if path.is_file():
                relative = path.relative_to(root).as_posix()
                mode = 0o755 if relative.startswith("bin/") else 0o644
                path.chmod(mode)
                require(path.stat().st_size > 0, f"empty payload: {relative}")
                files[relative] = {"sha256": digest(path), "size": path.stat().st_size, "mode": mode}
        require(REQUIRED <= files.keys(), f"missing payload: {sorted(REQUIRED - files.keys())}")
        manifest = {"format_version": 1, "version": info["version"], "target": target,
                    "build_profile": info["build_profile"],
                    "channel": "development" if development else "alpha" if alpha else "release", "release_ready": not blockers,
                    "release_blockers": blockers, "source": provenance, "files": files,
                    "controller": controller, "workload_images": images}
        write_json(root / "manifest.json", manifest)
        verify(root)
        suffix = "-dev-" + info["build_profile"] + "-" + provenance["sha256"][:12] if development else ""
        name = f"proofstorm-{info['version']}{suffix}-{target}.tar.gz"
        archive = Path(temporary) / name
        # Stable ordering, ownership, modes, and gzip metadata for identical inputs.
        with archive.open("wb") as stream, gzip.GzipFile(filename="", mode="wb", fileobj=stream, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as tar:
                for path in sorted(root.rglob("*")):
                    if not path.is_file():
                        continue
                    entry = tar.gettarinfo(str(path), arcname="proofstorm/" + path.relative_to(root).as_posix())
                    entry.uid = entry.gid = entry.mtime = 0
                    entry.uname = entry.gname = ""
                    with path.open("rb") as payload:
                        tar.addfile(entry, payload)
        checksum = digest(archive)
        final = output / name
        # Publish last, without replacing a prior artifact or following a symlink.
        checksum_path = output / (name + ".sha256")
        require(not final.exists() and not checksum_path.exists(), "bundle output already exists")
        with checksum_path.open("x") as receipt:
            receipt.write(f"{checksum}  {name}\n")
        os.link(archive, final)
        return {"archive": str(final), "sha256": checksum, "release_ready": manifest["release_ready"],
                "release_blockers": blockers}


def verify(root):
    require(root.is_dir() and not root.is_symlink(), "bundle root must be a real directory")
    manifest_path = root / "manifest.json"
    require(not manifest_path.is_symlink(), "manifest symlink refused")
    manifest = json.loads(manifest_path.read_text())
    require(manifest["format_version"] == 1 and manifest["target"] in TARGETS, "unsupported bundle")
    require(manifest["channel"] in {"development", "alpha", "release"}, "unsupported bundle channel")
    require(manifest["release_ready"] == (not manifest["release_blockers"]), "inconsistent release readiness")
    if manifest["release_ready"]:
        require(manifest["channel"] == "release" and manifest["controller"] is not None
                and not manifest["source"]["dirty"] and manifest["build_profile"] == "release"
                and manifest["controller"].get("platform") == TARGETS[manifest["target"]]
                and all(image["availability_verified"] and TARGETS[manifest["target"]] in image["verified_platforms"]
                        and image["published_source"] for image in manifest["workload_images"]),
                "release readiness lacks required evidence")
    files = manifest["files"]
    require(REQUIRED <= files.keys(), "incomplete bundle manifest")
    observed = set()
    for path in root.rglob("*"):
        require(not path.is_symlink(), f"payload symlink refused: {path}")
        if path.is_file():
            observed.add(path.relative_to(root).as_posix())
    require(observed == set(files) | {"manifest.json"}, "bundle has missing or unlisted files")
    for name, receipt in files.items():
        path = Path(name)
        require(not path.is_absolute() and ".." not in path.parts and path.as_posix() == name,
                "unsafe manifest path")
        payload = root / path
        require(payload.is_file() and payload.stat().st_size == receipt["size"] and
                digest(payload) == receipt["sha256"], f"payload checksum mismatch: {name}")
        require(payload.stat().st_mode & 0o7777 == receipt["mode"], f"payload mode mismatch: {name}")
    info = json.loads((root / "release-info.json").read_text())
    validate_info(info)
    if manifest["channel"] == "alpha":
        validate_alpha(info)
        require(manifest["controller"] == info["controller"], "alpha controller metadata mismatch")
    require(info["target"] == manifest["target"], "manifest target mismatch")
    require(info["version"] == manifest["version"], "manifest version mismatch")
    require(info["build_profile"] == manifest["build_profile"], "manifest build profile mismatch")
    require(info["source_revision"] == manifest["source"]["revision"] and
            info["source_sha256"] == manifest["source"]["sha256"], "manifest provenance mismatch")
    require(info["catalog"] == json.loads((root / "catalog.json").read_text()), "catalog mismatch")
    require(info["tools"] == (root / "tools/versions.env").read_text(), "tool pins mismatch")
    require(manifest["workload_images"] == image_inventory(info), "image inventory mismatch")
    return manifest


def build(args):
    """Compatibility entry point; the only build orchestration lives in Bash."""
    command = ["bash", Path(__file__).with_name("release-build.sh"), "--source", args.source,
               "--work-dir", args.work_dir, "--output", args.output]
    if args.target_dir:
        command.extend(["--target-dir", args.target_dir])
    if args.development:
        command.append("--development")
    if args.debug:
        command.append("--debug")
    run(command, cwd=Path.cwd())


def compile_snapshot(snapshot_root, provenance, *, work, output, target, trunk,
                     development, debug, expected_target):
    """Compatibility bridge for the Linux worker; Rust rechecks transported bytes."""
    require(host_target() == expected_target, "build target differs from build host")
    provenance_path = work / "package-source.json"
    write_json(provenance_path, provenance)
    command = ["bash", Path(__file__).with_name("release-build.sh"), "--source", snapshot_root,
               "--provenance", provenance_path, "--work-dir", work / "release-build",
               "--output", output, "--target-dir", target, "--trunk", trunk, "--json"]
    if development:
        command.append("--development")
    if debug:
        command.append("--debug")
    result = json.loads(run(command, cwd=work, capture=True))
    write_json(work / "result.json", result)
    print(json.dumps(result, indent=2))


def smoke(archive, destination, deny_sources):
    require(not deny_sources or platform.system() == "Darwin", "--deny-source requires macOS sandbox-exec; use source-free relocation on Linux")
    require(not destination.exists(), "smoke destination must not already exist")
    receipt = Path(str(archive) + ".sha256").read_text().strip().split()
    require(len(receipt) == 2 and receipt[1] == archive.name and receipt[0] == digest(archive),
            "archive checksum mismatch")
    destination.mkdir(parents=True)
    with tarfile.open(archive) as tar:
        members = tar.getmembers()
        require(len(members) <= 10_000 and sum(member.size for member in members) <= 1024**3,
                "archive exceeds bundle size limits")
        names = set()
        for member in members:
            path = Path(member.name)
            require(member.isfile() and not path.is_absolute() and ".." not in path.parts
                    and len(path.parts) >= 2 and path.parts[0] == "proofstorm"
                    and path.as_posix() == member.name and member.name not in names,
                    "unsafe or duplicate archive member")
            names.add(member.name)
        tar.extractall(destination, members=members, filter="data")
    root = destination / "proofstorm"
    manifest = verify(root)
    require(manifest["target"] == host_target(), "smoke must run on the bundle's target host")
    env = clean_environment()
    env["PROOFSTORM_HOME"] = str(destination / "must-not-be-created")
    env["PROOFSTORM_PRINCIPAL"] = ""
    prefix = []
    if deny_sources:
        # Negative-access smoke check on macOS. No changes to the source tree.
        policy = '(version 1) (allow default)'
        for source in deny_sources:
            policy += ' (deny file-read* (subpath ' + json.dumps(str(source.resolve())) + '))'
        prefix = ["/usr/bin/sandbox-exec", "-p", policy]
    for name, metadata_flag in [("proofstorm", "release-info"), ("proofstorm-mcp", "--release-info")]:
        binary = root / "bin" / name
        for flag in ["--version", "--help"]:
            require(bool(run([*prefix, binary, flag], cwd=destination, env=env, capture=True)),
                    "empty executable help/version output")
        embedded = json.loads(run([*prefix, binary, metadata_flag], cwd=destination, env=env, capture=True))
        require(embedded == json.loads((root / "release-info.json").read_text()), "relocated metadata mismatch")
    require(not (destination / "must-not-be-created").exists() and
            not (destination / ".proofstorm").exists(), "metadata command created runtime state")
    result = {"integrity_verified": True, "relocated_binaries_verified": True,
              "source_read_access_denied": bool(deny_sources), "release_ready": manifest["release_ready"]}
    write_json(destination / "smoke-report.json", result)
    print(json.dumps(result, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    builder = commands.add_parser("build")
    builder.add_argument("--source", type=Path, default=Path(__file__).resolve().parents[1])
    builder.add_argument("--work-dir", type=Path, required=True)
    builder.add_argument("--output", type=Path, required=True)
    builder.add_argument("--target-dir", type=Path, help="Optional external build cache; never the checkout target")
    builder.add_argument("--development", action="store_true", help="Allow explicit non-release bundles with recorded blockers")
    builder.add_argument("--debug", action="store_true", help="Faster host build for alpha/development bundles")
    checker = commands.add_parser("verify")
    checker.add_argument("directory", type=Path)
    smoker = commands.add_parser("smoke")
    smoker.add_argument("archive", type=Path)
    smoker.add_argument("--destination", type=Path, required=True)
    smoker.add_argument("--deny-source", type=Path, action="append", default=[],
                        help="macOS-only: deny child executables read access to this directory")
    args = parser.parse_args()
    if args.command == "build":
        build(args)
    elif args.command == "smoke":
        smoke(args.archive.resolve(), args.destination.resolve(), args.deny_source)
    else:
        manifest = verify(args.directory)
        print(json.dumps({"integrity_verified": True, "release_ready": manifest["release_ready"],
                          "release_blockers": manifest["release_blockers"]}, indent=2))


if __name__ == "__main__":
    main()
