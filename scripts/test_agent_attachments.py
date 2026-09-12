"""Installed OpenCode/Claude Code gate; only called with a disposable runtime.

Agent homes/cache/config are private test directories. No models, prompts, login,
native app launch, or permission bypasses are used. `mcp list` tests discovery and
connection, not an actual model tool call.
"""
import json
from pathlib import Path
import shutil
import subprocess
import urllib.error
import urllib.request


def run(prefix, env, work, *, executable=None, installation_home=None, allow_development=True):
    root = work / "agent attachment tests"
    root.mkdir(mode=0o700)
    user = root / "private user"
    project = root / "project with spaces and 'quote"
    other = root / "unconnected project"
    for path in (user, project, other):
        path.mkdir(mode=0o700)
    clients = {name: shutil.which(name, path=env.get("PATH")) for name in ("opencode", "claude")}
    assert all(clients.values()), "install OpenCode and Claude Code before this opt-in test"
    isolated = {k: v for k, v in env.items() if not k.startswith(("OPENCODE_", "CLAUDE_", "ANTHROPIC_"))}
    isolated.update(HOME=str(user), XDG_CONFIG_HOME=str(user / ".config"),
                    XDG_DATA_HOME=str(user / ".local/share"), XDG_CACHE_HOME=str(user / ".cache"),
                    XDG_STATE_HOME=str(user / ".local/state"), CLAUDE_CONFIG_DIR=str(user / ".claude"),
                    DOCKER_CONFIG=env.get("DOCKER_CONFIG", str(Path(env["HOME"]) / ".docker")),
                    OPENCODE_DISABLE_AUTOUPDATE="true", OPENCODE_DISABLE_MODELS_FETCH="true")
    executable = executable or prefix / "bin/proofstorm"
    installation_home = installation_home or prefix / "lib/proofstorm/state"
    record_path = installation_home / "gui-process.json"

    def cli(*args, expected=0, cwd=project):
        if "--json" not in args:
            args = ("--json", *args)
        if not allow_development:
            args = tuple(arg for arg in args if arg != "--allow-development")
        result = subprocess.run([executable, "--home", installation_home, *args], env=isolated, cwd=cwd,
                                capture_output=True, text=True, timeout=240)
        assert result.returncode == expected, result.stderr[-3000:]
        return json.loads(result.stdout) if expected == 0 else result

    def api(record, route, agent, cwd=project):
        body = json.dumps({"project":str(cwd), "harness":agent}).encode()
        request = urllib.request.Request(f'http://127.0.0.1:{record["port"]}/v1/gui/{route}', data=body,
            headers={"Authorization":"Bearer " + record["token"], "Content-Type":"application/json"})
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open(request, timeout=240) as response:
            return json.load(response)

    actors = []
    report = {}
    try:
        cli("gui", "start", "--allow-development")
        record = json.loads(record_path.read_text())
        for agent, file, key in [("opencode", "opencode.json", "mcp"), ("claude", ".mcp.json", "mcpServers")]:
            path = project / file
            original = '{\n  "' + key + '": {}\n}\n'
            path.write_text(original)
            dry = cli("agent", "open", agent, "--dry-run", "--allow-development")
            assert dry["attachment"]["harness"] == agent and dry["launch"]["interface"] == "cli"
            assert dry["launch"]["project"] == str(project.resolve())
            assert dry["launch"]["arguments"] == [] and not dry["changes_applied"]
            # GUI open now launches a native app. Keep this unattended gate
            # model/window-free: only preview when a native app is available.
            native_preview = False
            try:
                preview = api(record, "plan", agent)
                assert preview["harness"] == agent and preview["interface"] == "desktop"
                native_preview = True
            except urllib.error.HTTPError as error:
                assert error.code == 409
                message = json.load(error)["error"]["message"]
                assert "desktop" in message or "native" in message, message
            assert path.read_text() == original and not (other / file).exists()
            attached = cli("agent", "configure", agent, "--allow-development")
            assert attached["server_verified"]["environment_read"] and not attached["harness_loaded"]
            assert "app_opened" not in attached
            assert Path(attached["backup"]).read_text() == original
            assert attached["actor"].startswith(agent + "-")
            actors.append(attached["actor"])
            before = path.read_bytes()
            repeated = cli("agent", "configure", agent, "--allow-development")
            assert not repeated["configuration_changed"] and not repeated["actor_initialized"]
            assert path.read_bytes() == before and not (other / file).exists()
            # Exercise each actual installed client's native config parser and
            # stdio connection without starting a model session or approving trust.
            result = subprocess.run([clients[agent], "mcp", "list"], cwd=project, env=isolated,
                                    capture_output=True, text=True, timeout=180)
            (root / f"{agent}-mcp-list.log").write_text(result.stdout + result.stderr)
            assert result.returncode == 0, f"{agent}: inspect its private mcp-list log"
            output = result.stdout.lower() + result.stderr.lower()
            assert "proofstorm" in output and "connected" in output and "failed" not in output, f"{agent}: connection not confirmed; inspect its private log"
            unrelated = subprocess.run([clients[agent], "mcp", "list"], cwd=other, env=isolated,
                                       capture_output=True, text=True, timeout=180)
            assert unrelated.returncode == 0 and "proofstorm" not in (unrelated.stdout + unrelated.stderr).lower()
            content = json.loads(before)
            content[key]["proofstorm"]["command"] = "manual" if agent == "claude" else ["manual"]
            path.write_text(json.dumps(content))
            modified = path.read_bytes()
            cli("agent", "configure", agent, "--allow-development", expected=1)
            assert path.read_bytes() == modified
            # Restore only this test-owned fixture, not an operator's project.
            path.write_bytes(before)
            report[agent] = {"version":dry["launch"]["version"], "dry_run_read_only":True,
                "project_config_only":True, "gui_native_preview":native_preview,
                "native_app_opened":False, "explicit_cli_handoff":True, "backup_verified":True,
                "repeat_idempotent":True, "modified_entry_preserved":True,
                "client_mcp_connected":True, "unrelated_project_has_no_proofstorm":True,
                "server_verified":attached["server_verified"], "model_tool_call":False}
        assert len(set(actors)) == 2
        report["per_agent_identity"] = True
        report["private_agent_homes"] = True
        return report
    finally:
        if record_path.exists():
            cli("gui", "stop")
