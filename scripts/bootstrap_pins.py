#!/usr/bin/env python3
"""Maintainer-only: resolve publisher checksums and verify downloaded host tools."""
import hashlib
import io
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile


def fetch(url, directory, name):
    path = directory / name
    subprocess.run(["curl", "--fail", "--location", "--proto", "=https", "--proto-redir", "=https",
                    "--retry", "3", "--max-time", "180", "--silent", "--show-error", url,
                    "--output", str(path)], check=True)
    return path.read_bytes()


def resolve():
    root = Path(__file__).resolve().parents[1]
    pins = dict(line.split("=", 1) for line in (root / "tools/versions.env").read_text().splitlines()
                if line and not line.startswith("#"))
    k3d, kubectl, helm = (pins[key] for key in ["K3D_VERSION", "KUBECTL_VERSION", "HELM_VERSION"])
    descriptions = [
        ("k3d", k3d, f"https://github.com/k3d-io/k3d/releases/download/{k3d}/k3d-darwin-arm64",
         f"https://github.com/k3d-io/k3d/releases/download/{k3d}/checksums.txt", "_dist/k3d-darwin-arm64", None),
        ("kubectl", kubectl, f"https://dl.k8s.io/release/{kubectl}/bin/darwin/arm64/kubectl",
         f"https://dl.k8s.io/release/{kubectl}/bin/darwin/arm64/kubectl.sha256", None, None),
        ("helm", helm, f"https://get.helm.sh/helm-{helm}-darwin-arm64.tar.gz",
         f"https://get.helm.sh/helm-{helm}-darwin-arm64.tar.gz.sha256sum", None, "darwin-arm64/helm")]
    tools = []
    with tempfile.TemporaryDirectory(prefix="proofstorm-tool-pins-") as temporary:
        directory = Path(temporary)
        for name, version, url, checksum_url, checksum_name, member in descriptions:
            receipt = fetch(checksum_url, directory, name + ".sha256").decode()
            rows = [line.split() for line in receipt.splitlines() if line.strip()]
            if checksum_name:
                rows = [row for row in rows if len(row) == 2 and row[1] == checksum_name]
            assert len(rows) == 1, "ambiguous checksum receipt"
            expected = rows[0][0]
            payload = fetch(url, directory, name)
            assert hashlib.sha256(payload).hexdigest() == expected, "publisher checksum mismatch"
            executable = payload
            if member:
                with tarfile.open(fileobj=io.BytesIO(payload), mode="r:gz") as archive:
                    entry = archive.getmember(member)
                    assert entry.isfile()
                    executable = archive.extractfile(entry).read()
            tools.append(dict(name=name, version=version, url=url, sha256=expected,
                              executable_sha256=hashlib.sha256(executable).hexdigest(), archive_member=member))
    return {"format_version": 1, "target": "aarch64-apple-darwin", "tools": tools}


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = resolve()
    with args.output.open("x") as output:
        output.write(json.dumps(result, indent=2) + "\n")
