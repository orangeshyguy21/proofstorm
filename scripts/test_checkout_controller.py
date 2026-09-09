#!/usr/bin/env python3
"""Live checkout controller rollout/reuse gate. Leave this installation idle.

Builds/deploys only into the selected owned installation. Creates no labs,
launches no GUI/agents, and publishes nothing to an external registry.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess


def file_hash(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(65536), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--home", type=Path, required=True)
    parser.add_argument("--work-dir", type=Path, required=True)
    args = parser.parse_args()
    cli, home, work = args.cli.resolve(), args.home.resolve(), args.work_dir.resolve()
    work.mkdir(mode=0o700, parents=True, exist_ok=False)
    env = {key: value for key, value in os.environ.items() if not key.startswith(("PROOFSTORM_", "K3D_", "HELM_"))}
    env["KUBECONFIG"] = str(home / "kubeconfig")
    assert not (home / "gui-process.json").exists(), "stop the selected GUI before running this idle-installation gate"
    installation = (home / "installation.json").read_bytes()
    owner = (home / "runtime-owner.json").read_bytes()
    database = file_hash(home / "proofstorm.sqlite3")
    identity = json.loads(installation)
    context = "k3d-pst-" + identity["id"][:28]
    info = json.loads(subprocess.check_output([cli, "release-info"], env=env, text=True))
    pin = next(pin for pin in info["bootstrap_tools"]["tools"] if pin["name"] == "kubectl")
    kubectl = home / "tools" / ("kubectl-" + pin["executable_sha256"])
    assert file_hash(kubectl) == pin["executable_sha256"]

    def runtime():
        output = subprocess.check_output([kubectl, "--kubeconfig", home / "kubeconfig", "--context", context,
            "get", "deployment/proofstormd", "-n", "proofstorm-system", "-o", "json"], env=env, text=True, timeout=40)
        value = json.loads(output)
        return {"uid": value["metadata"]["uid"], "generation":value["metadata"]["generation"],
                "image":value["spec"]["template"]["spec"]["containers"][0]["image"],
                "ready":value["status"].get("readyReplicas") == 1}

    def command(*arguments):
        if "--json" not in arguments:
            arguments = ("--json", *arguments)
        result = subprocess.run([cli, "--home", home, *arguments], env=env, cwd=work,
                                capture_output=True, text=True, timeout=4200)
        (work / ("-".join(arguments).replace("--", "") + ".log")).write_text(result.stderr)
        assert result.returncode == 0, result.stderr[-3000:]
        return json.loads(result.stdout)

    before = runtime()
    print("Controller gate: setup/build/deploy", flush=True)
    command("setup")
    doctor = command("doctor", "--json")
    assert doctor["ok"]
    after = runtime()
    receipt_path = home / "checkout-controller.json"
    receipt = receipt_path.read_bytes()
    value = json.loads(receipt)
    assert after["ready"] and after["image"] == value["image"]
    assert after["uid"] == before["uid"]
    assert value["image"].startswith("proofstorm-registry.localhost:5000/proofstormd@sha256:")
    print("Controller gate: unchanged setup must reuse image and deployment", flush=True)
    command("setup")
    assert runtime() == after, "unchanged setup changed controller deployment"
    assert receipt_path.read_bytes() == receipt, "unchanged setup rewrote controller receipt"
    assert (home / "installation.json").read_bytes() == installation
    assert (home / "runtime-owner.json").read_bytes() == owner
    assert file_hash(home / "proofstorm.sqlite3") == database, "setup changed existing permissions/state"
    report = {"passed":True, "before":before, "after":after, "controller":value,
              "doctor_ready":True, "repeat_setup_preserves_deployment":True,
              "installation_runtime_state_preserved":True, "external_publication":False}
    (work / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"passed":True, "report":str(work / "report.json")}, indent=2))


if __name__ == "__main__":
    main()
