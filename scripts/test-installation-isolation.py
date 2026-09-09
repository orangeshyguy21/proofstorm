#!/usr/bin/env python3
"""Opt-in live routing check for two disposable Proofstorm installations.

Uses the already-built CLI and pinned k3d, never deploys a Proofstorm controller.
Each cluster has one memory-limited server and a separate registry. The test
publishes tiny synthetic OCI manifests, verifies own-registry pulls and failed
cross-registry pulls, and checks the preexisting Docker inventory and user
kubeconfig after cleanup. All subprocess arguments are passed without a shell.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import urllib.error
import urllib.parse
import urllib.request


def run(argv, env, *, check=True, timeout=180, cwd=None):
    result = subprocess.run(
        [str(arg) for arg in argv], env=env, capture_output=True, text=True,
        timeout=timeout, check=False, cwd=cwd,
    )
    if check and result.returncode:
        raise RuntimeError(f"{argv[0]} {argv[1:3]} failed: {result.stderr[-4000:]}")
    return result


def docker_inventory(env):
    # Do not capture container environment or labels: k3d labels contain tokens.
    return set(run(["docker", "ps", "-aq", "--no-trunc"], env).stdout.split())


def docker_resources(env):
    return {
        "networks": sorted(run(["docker", "network", "ls", "-q", "--no-trunc"], env).stdout.split()),
        "volumes": sorted(run(["docker", "volume", "ls", "-q"], env).stdout.split()),
    }


def container_state(ids, env):
    if not ids:
        return []
    template = '{{.Id}} {{.State.Status}} {{.State.StartedAt}} {{.RestartCount}}'
    return sorted(run(["docker", "inspect", "--format", template, *sorted(ids)], env).stdout.splitlines())


def file_digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest() if path.exists() else None


def development_snapshot(kubectl, context, kubeconfig, env):
    if not kubectl:
        return None
    command = [kubectl, "--kubeconfig", kubeconfig, "--context", context]
    pods = json.loads(run([*command, "get", "pods", "-n", "proofstorm-system",
                           "-l", "app.kubernetes.io/name=proofstormd", "-o", "json"], env).stdout)
    instances = json.loads(run([*command, "get", "proofstormlabs", "-A", "-o", "json"], env).stdout)
    return {
        "controllers": sorted((pod["metadata"]["uid"],
                               sorted((state["name"], state["restartCount"])
                                      for state in pod.get("status", {}).get("containerStatuses", [])))
                              for pod in pods["items"]),
        "labs": sorted((instance["metadata"]["uid"], json.dumps(instance["spec"], sort_keys=True))
                       for instance in instances["items"]),
    }


def inspect(name, env):
    result = run(["docker", "container", "inspect", name], env, check=False)
    if result.returncode:
        return None
    return json.loads(result.stdout)[0]


def http_request(url, method, data=None, content_type=None):
    headers = {"Content-Type": content_type} if content_type else {}
    request = urllib.request.Request(url, data=data, method=method, headers=headers)
    return urllib.request.urlopen(request, timeout=15)


def publish_probe(port, identity):
    base = f"http://127.0.0.1:{port}/v2/isolation-probe"
    config = json.dumps({
        "architecture": "arm64", "os": "linux",
        "rootfs": {"type": "layers", "diff_ids": []},
        "config": {"Labels": {"proofstorm.test.installation": identity}},
    }, sort_keys=True).encode()
    digest = "sha256:" + hashlib.sha256(config).hexdigest()
    with http_request(base + "/blobs/uploads/", "POST", b"") as response:
        location = urllib.parse.urljoin(base + "/", response.headers["Location"])
    separator = "&" if "?" in location else "?"
    with http_request(location + separator + "digest=" + digest, "PUT", config,
                      "application/octet-stream"):
        pass
    manifest_type = "application/vnd.oci.image.manifest.v1+json"
    manifest = json.dumps({
        "schemaVersion": 2, "mediaType": manifest_type,
        "config": {"mediaType": "application/vnd.oci.image.config.v1+json",
                   "digest": digest, "size": len(config)}, "layers": [],
    }, sort_keys=True).encode()
    with http_request(base + "/manifests/smoke", "PUT", manifest, manifest_type) as response:
        digest = "sha256:" + hashlib.sha256(manifest).hexdigest()
        assert response.headers["Docker-Content-Digest"] == digest
    return "proofstorm-registry.localhost:5000/isolation-probe@" + digest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--k3d", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--kubectl", type=Path,
                        help="Also verify the existing development controller and lab specs")
    parser.add_argument("--development-context", default="k3d-proofstorm")
    args = parser.parse_args()
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("PROOFSTORM_", "K3D_"))}
    original_inventory = docker_inventory(env)
    original_resources = docker_resources(env)
    original_containers = container_state(original_inventory, env)
    original_config = Path.home() / ".kube/config"
    original_digest = file_digest(original_config)
    original_development = development_snapshot(args.kubectl, args.development_context,
                                                original_config, env)
    instances = []
    report = {"checks": [], "instances": [], "cleanup_errors": []}
    with tempfile.TemporaryDirectory(prefix="proofstorm-isolation-") as root:
        try:
            for index in range(2):
                home = Path(root) / f"installation {index}"
                result = run([args.binary, "--home", home, "init"], env)
                installation = json.loads(result.stdout)["installation"]
                config = json.loads((home / "k3d.yaml").read_text())
                cluster = config["metadata"]["name"]
                registry = config["registries"]["create"]["name"]
                private_env = dict(env, KUBECONFIG=str(home / "kubeconfig"))
                for name in [registry, f"k3d-{cluster}-server-0", f"k3d-{cluster}-serverlb"]:
                    assert inspect(name, private_env) is None, f"name already exists: {name}"
                instance = dict(home=home, cluster=cluster, registry=registry,
                                identity=installation["id"], env=private_env,
                                registry_id=None)
                instances.append(instance)
                report["instances"].append({"identity": installation["id"],
                                            "cluster": cluster, "registry": registry})
                print(f"Creating disposable cluster {index + 1}/2", flush=True)
                run([args.k3d, "cluster", "create", "--config", home / "k3d.yaml",
                     "--agents", "0", "--servers-memory", "1g",
                     "--kubeconfig-update-default=false", "--kubeconfig-switch-context=false"],
                    private_env)
                registry_info = inspect(registry, private_env)
                assert registry_info is not None
                instance["registry_id"] = registry_info["Id"]
                assert "k3d-" + cluster in registry_info["NetworkSettings"]["Networks"]
                server = inspect(f"k3d-{cluster}-server-0", private_env)
                assert server["Config"]["Labels"]["proofstorm.dev/installation"] == installation["id"]
                kubeconfig = run([args.k3d, "kubeconfig", "get", cluster], private_env).stdout
                with (home / "kubeconfig").open("x") as stream:
                    os.chmod(stream.name, 0o600)
                    stream.write(kubeconfig)
                instance["image"] = publish_probe(installation["registry_port"], installation["id"])
                assert file_digest(original_config) == original_digest
            for index, instance in enumerate(instances):
                node = "k3d-" + instance["cluster"] + "-server-0"
                own = run(["docker", "exec", node, "crictl", "--timeout=30s", "pull", instance["image"]],
                          instance["env"], timeout=60)
                assert own.returncode == 0
                other = run(["docker", "exec", node, "crictl", "--timeout=15s", "pull",
                             instances[1-index]["image"]], instance["env"], check=False, timeout=45)
                assert other.returncode != 0, "cross-registry image unexpectedly resolved"
                report["checks"].append({"installation": instance["identity"],
                                         "own_registry_pull": True, "cross_registry_pull_refused": True})
                print(f"Registry isolation passed for cluster {index + 1}/2", flush=True)
        finally:
            for instance in reversed(instances):
                try:
                    node = inspect("k3d-" + instance["cluster"] + "-server-0", instance["env"])
                    if node:
                        assert node["Config"]["Labels"].get("proofstorm.dev/installation") == instance["identity"]
                        run([args.k3d, "cluster", "delete", instance["cluster"]], instance["env"])
                    registry = inspect(instance["registry"], instance["env"])
                    if registry:
                        # A failed create can leave a registry before its ID was captured.
                        # Keep it for inspection rather than delete an unrecorded container.
                        assert instance["registry_id"] == registry["Id"], "unrecorded registry retained"
                        run([args.k3d, "registry", "delete", instance["registry"]], instance["env"])
                except Exception as error:
                    report["cleanup_errors"].append(str(error))
            report["user_kubeconfig_unchanged"] = file_digest(original_config) == original_digest
            report["docker_inventory_restored"] = docker_inventory(env) == original_inventory
            report["docker_networks_and_volumes_restored"] = docker_resources(env) == original_resources
            report["preexisting_containers_unchanged"] = container_state(original_inventory, env) == original_containers
            if args.kubectl:
                report["development_controller_and_labs_unchanged"] = development_snapshot(
                    args.kubectl, args.development_context, original_config, env) == original_development
            args.output.write_text(json.dumps(report, indent=2) + "\n")
            print(f"Report: {args.output}", flush=True)
    assert not report["cleanup_errors"], report["cleanup_errors"]
    assert report["user_kubeconfig_unchanged"]
    assert report["docker_inventory_restored"]
    assert report["docker_networks_and_volumes_restored"]
    assert report["preexisting_containers_unchanged"]
    assert report.get("development_controller_and_labs_unchanged", True)


if __name__ == "__main__":
    main()
