#!/usr/bin/env python3
"""Opt-in installed-bundle smoke test with development-environment preservation.

Default: download helpers only. --start-runtime also creates, tests and removes
one owned cluster. Build/test records remain in the explicit external work dir.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import selectors
import time
import urllib.request
import sqlite3
import tomllib
import test_managed_gui
import test_agent_attachments

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("isolation", ROOT / "scripts/test-installation-isolation.py")
isolation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(isolation)


def interrupted_helper_download(result):
    return (result.returncode != 0 and "setup stage tools failed" in result.stderr
            and "curl failed (exit status: 18)" in result.stderr)


def mcp_up(prefix, env, work, cell, entry=None, tool="cell_up"):
    """A real installed stdio client, not a harness-discovery assertion."""
    with (work / "mcp.stderr.log").open("w") as errors:
        command = [entry["command"], *entry["args"]] if entry else [prefix / "bin/proofstorm-mcp"]
        process = subprocess.Popen(command, cwd=entry["cwd"] if entry else work,
                                   env=env if entry else dict(env, PROOFSTORM_PRINCIPAL="developer", PROOFSTORM_TOOLSET="developer"),
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=errors, bufsize=0)
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ)
        pending = b""

        def send(message):
            payload = memoryview((json.dumps(dict(jsonrpc="2.0", **message)) + "\n").encode())
            while payload:
                written = process.stdin.write(payload)
                assert written
                payload = payload[written:]

        def request(identifier, method, params):
            nonlocal pending
            send(dict(id=identifier, method=method, params=params))
            deadline = time.monotonic() + 1800
            while time.monotonic() < deadline:
                if b"\n" in pending:
                    line, pending = pending.split(b"\n", 1)
                    message = json.loads(line)
                    if message.get("id") == identifier:
                        assert "error" not in message, message
                        return message["result"]
                elif selector.select(timeout=1):
                    chunk = os.read(process.stdout.fileno(), 65536)
                    assert chunk, "MCP exited before replying; see mcp.stderr.log"
                    pending += chunk
                    assert len(pending) <= 8 * 1024 * 1024
                else:
                    assert process.poll() is None, "MCP exited; see mcp.stderr.log"
            raise TimeoutError("installed MCP request timed out")

        try:
            initialized = request(1, "initialize", {"protocolVersion":"2024-11-05", "capabilities":{},
                                                     "clientInfo":{"name":"installed-image-smoke", "version":"1"}})
            assert initialized["serverInfo"]["name"] == "proofstorm-mcp"
            send(dict(method="notifications/initialized"))
            listed = request(2, "tools/list", {})
            assert "cell_up" in {tool["name"] for tool in listed["tools"]}
            result = request(3, "tools/call", {"name":tool, "arguments":{"name":cell["name"], "cell":cell} if tool == "cell_up" else {}})
            assert not result.get("isError", False), result
            return result
        finally:
            selector.close()
            process.stdin.close()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=10)
            process.stdout.close()


def cleanup(prefix, env, original):
    home = prefix / "lib/proofstorm/state"
    receipt = json.loads((home / "runtime-owner.json").read_text())
    installation = json.loads((home / "installation.json").read_text())
    assert Path(installation["home"]).resolve() == home.resolve()
    cluster = "pst-" + installation["id"][:28]
    registry = "k3d-" + cluster + "-registry"
    assert set(receipt["containers"]) == {"k3d-" + cluster + suffix for suffix in ["-server-0", "-agent-0", "-serverlb", "-registry"]}
    for name, expected_id in receipt["containers"].items():
        current = isolation.inspect(name, env)
        assert current and current["Id"] == expected_id and expected_id not in original
        if name != registry:
            assert current["Config"]["Labels"]["proofstorm.dev/installation"] == installation["id"]
    network = isolation.run(["docker", "network", "inspect", "--format", "{{.Id}}", "k3d-" + cluster], env).stdout.strip()
    assert network == receipt["network_id"]
    pins = json.loads((prefix / "lib/proofstorm/current/release-info.json").read_text())["bootstrap_tools"]["tools"]
    k3d = home / "tools" / next(p["name"] + "-" + p["executable_sha256"] for p in pins if p["name"] == "k3d")
    private_env = dict(env, KUBECONFIG=str(home / "kubeconfig"))
    isolation.run([k3d, "cluster", "delete", cluster], private_env)
    current = isolation.inspect(registry, env)
    if current:
        assert current["Id"] == receipt["containers"][registry]
        isolation.run([k3d, "registry", "delete", registry], private_env)
    print("Removed verified disposable runtime:", cluster, flush=True)


def native_handoff(prefix, env, work):
    """Launch a clean project; wait for externally observed native-task evidence."""
    project = work / "native codex project"
    project.mkdir(mode=0o700)
    result = isolation.run(
        [prefix / "bin/proofstorm", "--json", "open", "codex", "--allow-development"],
        env, cwd=project, timeout=240,
    )
    attached = json.loads(result.stdout)
    assert attached["server_verified"]["environment_read"] and not attached["harness_loaded"]
    ready = {"project":str(project.resolve()), "actor":attached["actor"],
             "gui_launch_command_succeeded":True, "harness_loaded":False,
             "evidence_path":str(work / "native-result.json")}
    (work / "native-ready.json").write_text(json.dumps(ready, indent=2) + "\n")
    print("NATIVE HANDOFF READY:", project, flush=True)
    deadline = time.monotonic() + 1800
    while time.monotonic() < deadline:
        evidence = work / "native-result.json"
        if evidence.exists():
            observed = json.loads(evidence.read_text())
            assert observed.get("harness_tool_call_observed") is True, observed
            assert observed.get("project") == str(project.resolve()), observed
            assert observed.get("thread_id") and observed.get("tool"), observed
            return dict(ready, harness_loaded=True, evidence=observed)
        time.sleep(1)
    raise TimeoutError("Native Codex task evidence not supplied within 30 minutes; cleaning up disposable runtime")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--work-dir", type=Path, required=True)
    parser.add_argument("--start-runtime", action="store_true")
    parser.add_argument("--test-codex-attachment", action="store_true")
    parser.add_argument("--test-gui", action="store_true")
    parser.add_argument("--test-agents", action="store_true", help="Test installed OpenCode and Claude Code in private agent homes")
    parser.add_argument("--gui-browser", action="store_true")
    parser.add_argument("--gui-chrome", action="store_true", help="Also open the browser checkpoint in Chrome without changing the default browser")
    parser.add_argument("--native-handoff", action="store_true",
                        help="Open a clean native Codex project and wait up to 30 minutes for verified task evidence")
    args = parser.parse_args()
    assert not args.test_codex_attachment or args.start_runtime
    assert not args.native_handoff or args.test_codex_attachment
    assert not args.test_gui or args.start_runtime
    assert not args.test_agents or args.start_runtime
    assert not args.gui_browser or args.test_gui
    assert not args.gui_chrome or args.gui_browser
    work = args.work_dir.resolve()
    assert not work.exists() and not work.is_relative_to(ROOT)
    work.mkdir(parents=True, mode=0o700)
    prefix = work / "installed prefix"
    home = prefix / "lib/proofstorm/state"
    env = {k: v for k, v in os.environ.items() if not k.startswith(("PROOFSTORM_", "K3D_", "HELM_"))}
    original = isolation.docker_inventory(env)
    resources = isolation.docker_resources(env)
    states = isolation.container_state(original, env)
    config = Path.home() / ".kube/config"
    config_sha = isolation.file_digest(config)
    development = isolation.development_snapshot(ROOT / ".tools/bin/kubectl", "k3d-proofstorm", config, env)
    (work / "baseline.json").write_text(json.dumps({
        "containers":sorted(original), "resources":resources, "container_states":states,
        "user_kubeconfig_sha256":config_sha, "development":development,
    }, indent=2) + "\n")
    report = {"full_runtime_requested": args.start_runtime, "checks": [], "cleanup_errors": [], "test_completed":False}

    def cli(*arguments, expect=0):
        if "--json" not in arguments:
            arguments = ("--json", *arguments)
        result = isolation.run([prefix / "bin/proofstorm", *arguments], env, check=False, timeout=1800)
        assert result.returncode == expect, result.stderr[-2000:]
        return result

    def setup(prepare):
        arguments = [prefix / "bin/proofstorm", "setup", "--json", "--allow-development"]
        if prepare:
            arguments.append("--prepare-only")
        for attempt in range(3):
            with (work / "last-setup.json").open("w") as output:
                result = subprocess.run(arguments, env=env, stdout=output, stderr=subprocess.PIPE,
                                        text=True, check=False, timeout=3600)
            print(result.stderr, end="", file=sys.stderr, flush=True)
            if result.returncode == 0:
                return
            if attempt == 2 or not interrupted_helper_download(result):
                result.check_returncode()
            report["interrupted_helper_download_retries"] = report.get("interrupted_helper_download_retries", 0) + 1
            print("Retrying the same setup after an interrupted helper download; checksum checks remain required.", flush=True)

    try:
        subprocess.run(["sh", ROOT / "install.sh", "--artifact-dir", args.archive.resolve().parent,
                        "--archive", args.archive.name, "--prefix", prefix, "--allow-development"], env=env, check=True)
        cli("setup", expect=1)
        assert not home.exists(), "normal installer adopted a development runtime"
        cli("doctor", "--json", expect=1)
        assert not home.exists(), "doctor initialized private state"
        setup(True)
        tools = {str(p): (isolation.file_digest(p), p.stat().st_mtime_ns) for p in (home / "tools").iterdir()}
        setup(True)
        assert tools == {str(p): (isolation.file_digest(p), p.stat().st_mtime_ns) for p in (home / "tools").iterdir()}
        assert not (home / "runtime-owner.json").exists()
        assert not (home / "proofstorm.sqlite3").exists()
        report["checks"].extend(["development_guard", "doctor_no_initialization", "verified_tool_downloads", "prepare_idempotency"])
        if args.start_runtime:
            setup(False)
            doctor = json.loads(cli("doctor", "--json").stdout)
            assert doctor["ok"]
            first_owner = (home / "runtime-owner.json").read_bytes()
            first_database = isolation.file_digest(home / "proofstorm.sqlite3")
            installation = json.loads((home / "installation.json").read_text())
            def repositories():
                with urllib.request.urlopen(f'http://127.0.0.1:{installation["registry_port"]}/v2/_catalog', timeout=10) as response:
                    return set(json.load(response)["repositories"])
            assert not repositories(), "default setup fetched cell images before any cell was selected"
            assert json.loads((work / "last-setup.json").read_text())["image_policy"] == "on_demand"
            report["checks"].append("setup_skips_catalog_downloads")
            pins = json.loads((prefix / "lib/proofstorm/current/release-info.json").read_text())["bootstrap_tools"]["tools"]
            kubectl = home / "tools" / next(p["name"] + "-" + p["executable_sha256"] for p in pins if p["name"] == "kubectl")
            context = "k3d-pst-" + installation["id"][:28]
            first_runtime = isolation.development_snapshot(kubectl, context, home / "kubeconfig", env)
            setup(False)
            assert first_owner == (home / "runtime-owner.json").read_bytes()
            assert first_database == isolation.file_digest(home / "proofstorm.sqlite3")
            assert first_runtime == isolation.development_snapshot(kubectl, context, home / "kubeconfig", env)
            report["checks"].extend(["runtime_setup", "doctor_ready", "setup_retry_preserves_controller_and_permissions"])
            example = json.loads((ROOT / "examples/developer-cell.json").read_text())
            bitcoin = dict(example, name="bitcoin-only", components=example["components"][:1], links=[])
            bitcoin_path = work / "bitcoin-only.json"
            bitcoin_path.write_text(json.dumps(bitcoin))
            first_cell = json.loads(cli("up", bitcoin_path, "--wait", "120").stdout)
            assert first_cell["runtime"]["phase"] == "ready"
            assert repositories() == {"bitcoin-core", "upstream/docker.io/library/busybox"}
            report["checks"].append("cli_fetches_only_selected_images")
            report["mcp_result"] = mcp_up(prefix, env, work, example)
            assert len(repositories()) == 3, repositories()
            report["checks"].append("mcp_fetches_only_new_selected_images")
            result = json.loads(cli("up", ROOT / "examples/developer-cell.json", "--wait", "120").stdout)
            assert result["runtime"]["phase"] == "ready"
            report["cell_result"] = result
            report["checks"].append("example_cell_up_returned_success")
            if args.test_codex_attachment:
                project = work / "app project with spaces"
                project.mkdir()
                (project / ".codex").mkdir()
                project_config = project / ".codex/config.toml"
                original_config = '# Keep my preferences\nmodel = "fixture-model" # untouched\n\n[mcp_servers.other]\ncommand = "unrelated-server"\n'
                project_config.write_text(original_config)
                isolated_codex = work / "isolated-codex-home"
                isolated_codex.mkdir()
                attach_env = dict(env, CODEX_HOME=str(isolated_codex))
                actor_db = home / "proofstorm.sqlite3"

                def attach(*arguments, expect=0):
                    if "--json" not in arguments:
                        arguments = ("--json", *arguments)
                    result = isolation.run([prefix / "bin/proofstorm", *arguments], attach_env, check=False, timeout=240, cwd=project)
                    assert result.returncode == expect, result.stderr[-2000:]
                    return result

                database_before = isolation.file_digest(actor_db)
                dry = json.loads(attach("attach", "codex", "--allow-development", "--dry-run").stdout)
                assert dry["attachment"]["project"] == str(project.resolve())
                assert not dry["changes_applied"] and project_config.read_text() == original_config
                assert isolation.file_digest(actor_db) == database_before
                first = json.loads(attach("attach", "codex", "--allow-development").stdout)
                assert first["server_verified"]["environment_read"] and not first["harness_loaded"]
                assert first["actor_initialized"] and first["actor"] != "developer"
                configured = project_config.read_bytes()
                assert configured.decode().startswith(original_config)
                assert Path(first["backup"]).read_text() == original_config
                second = json.loads(attach("attach", "codex", project, "--allow-development").stdout)
                assert not second["configuration_changed"] and not second["actor_initialized"]
                assert project_config.read_bytes() == configured
                entry = tomllib.loads(configured.decode())["mcp_servers"]["proofstorm"]
                poison = dict(attach_env, PROOFSTORM_MODE="memory", PROOFSTORM_DB=str(work / "must-not-create.sqlite3"),
                              PROOFSTORM_CONTEXT="k3d-proofstorm", PROOFSTORM_KUBECONFIG=str(work / "foreign-kubeconfig"),
                              PROOFSTORM_WORKSPACE="foreign", PROOFSTORM_CAPABILITIES="catalog.read", PROOFSTORM_TOOLSET="design")
                verified = mcp_up(prefix, poison, work, example, entry, "environment_read")
                assert not verified.get("isError", False) and not (work / "must-not-create.sqlite3").exists()
                with sqlite3.connect(actor_db) as connection:
                    connection.execute("DELETE FROM grants WHERE principal_id=? AND capability='cell.materialize'", (first["actor"],))
                attach("attach", "codex", project, "--allow-development", expect=1)
                with sqlite3.connect(actor_db) as connection:
                    assert connection.execute("SELECT COUNT(*) FROM grants WHERE principal_id=? AND capability='cell.materialize'", (first["actor"],)).fetchone()[0] == 0
                assert project_config.read_bytes() == configured
                launch = json.loads(attach("open", "codex", "--allow-development", "--dry-run").stdout)
                assert launch["launch"]["interface"] == "desktop" and launch["launch"]["arguments"] == ["app", str(project.resolve())]
                report["attachment"] = {"config_preserved":True, "backup_verified":True, "repeat_idempotent":True,
                    "managed_mcp_ignores_ambient_overrides":True, "revoked_grants_preserved":True,
                    "server_verified":first["server_verified"], "harness_loaded":False, "current_directory_default":True,
                    "launch_plan":launch["launch"], "gui_launched":False}
                report["checks"].append("codex_project_attachment_and_native_launch_plan")
                if args.native_handoff:
                    report["native"] = native_handoff(prefix, env, work)
                    report["checks"].append("native_codex_tool_call_observed")
            if args.test_agents:
                report["agents"] = test_agent_attachments.run(prefix, env, work)
                report["checks"].append("opencode_and_claude_project_discovery_and_gui_api")
            if args.test_gui:
                before_gui = isolation.development_snapshot(kubectl, context, home / "kubeconfig", env)
                report["gui"] = test_managed_gui.run(prefix, env, work, args.gui_browser, args.gui_chrome)
                assert before_gui == isolation.development_snapshot(kubectl, context, home / "kubeconfig", env)
                report["gui"]["gui_stop_preserved_running_cells"] = True
                report["checks"].append("managed_gui_project_attachment_and_lifecycle")
        report["test_completed"] = True
    finally:
        if args.start_runtime and (home / "runtime-owner.json").exists():
            try:
                cleanup(prefix, env, original)
            except Exception as error:
                report["cleanup_errors"].append(str(error))
        report["docker_inventory_restored"] = original == isolation.docker_inventory(env)
        report["docker_networks_and_volumes_restored"] = resources == isolation.docker_resources(env)
        report["preexisting_containers_unchanged"] = states == isolation.container_state(original, env)
        report["user_kubeconfig_unchanged"] = config_sha == isolation.file_digest(config)
        report["development_controller_and_cells_unchanged"] = development == isolation.development_snapshot(
            ROOT / ".tools/bin/kubectl", "k3d-proofstorm", config, env)
        (work / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print("Smoke report:", work / "report.json", flush=True)
    assert not report["cleanup_errors"] and all(report[key] for key in report if key.endswith(("_unchanged", "_restored")))


if __name__ == "__main__":
    main()
