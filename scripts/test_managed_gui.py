"""Managed GUI gate, invoked only against an explicitly disposable installation."""
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request


def run(prefix, env, work, browser=False, chrome=False):
    project = work / "gui project with spaces"
    other = work / "other gui project"
    project.mkdir(mode=0o700)
    other.mkdir(mode=0o700)
    record_path = prefix / "lib/proofstorm/state/gui-process.json"

    def cli(*args, cwd=project):
        if "--json" not in args:
            args = ("--json", *args)
        result = subprocess.run([prefix / "bin/proofstorm", *args], cwd=cwd, env=env,
                                capture_output=True, text=True, timeout=180, check=False)
        assert result.returncode == 0, result.stderr[-4000:]
        return json.loads(result.stdout)

    def api(record, path, body=None, auth=True, headers=None):
        supplied = {"Authorization":"Bearer " + record["token"]} if auth else {}
        supplied.update(headers or {})
        data = None if body is None else json.dumps(body).encode()
        if data is not None:
            supplied["Content-Type"] = "application/json"
        request = urllib.request.Request(f'http://127.0.0.1:{record["port"]}{path}', data=data, headers=supplied)
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        try:
            with opener.open(request, timeout=240) as response:
                return response.status, json.load(response)
        except urllib.error.HTTPError as error:
            return error.code, json.load(error)

    try:
        first = cli("gui", "start", "--allow-development")
        record = json.loads(record_path.read_text())
        assert record_path.stat().st_mode & 0o077 == 0
        assert record["token"] not in json.dumps(first)
        second = cli("gui", "start", "--allow-development", cwd=other)
        assert second["reused_server"] and second["url"] == first["url"]
        assert json.loads(record_path.read_text()) == record
        assert not (project / ".codex").exists() and not (other / ".codex").exists()
        assert api(record, "/v1/environment", auth=False)[0] == 401
        assert api(record, "/v1/gui/open", {"project":str(project)}, auth=False)[0] == 403
        assert api(record, "/v1/gui/open", {"project":str(project)}, headers={"Origin":"https://evil.invalid"})[0] == 403
        status, context = api(record, "/v1/gui/context")
        assert status == 200 and context["runtime_ready"] and context["codex_available"]
        status, plan = api(record, "/v1/gui/plan", {"project":str(project)})
        assert status == 200 and plan["project"] == str(project.resolve())
        assert not (project / ".codex").exists()
        if browser:
            launched = cli("gui", "--allow-development")
            assert launched["reused_server"] and launched["browser"] in ("opened_default_browser", "existing_tab_focused")
            if chrome:
                # Test-only browser selection; still exercise the CLI's default handler above.
                fragment = urllib.parse.urlencode({"session":record["token"], "project":str(project)})
                subprocess.run(["/usr/bin/open", "-a", "Google Chrome", first["url"] + "/#" + fragment],
                               check=True, capture_output=True, timeout=30)
            print("GUI BROWSER READY:", first["url"], "project:", project, flush=True)
            print("Inspect the project dialog, click Open in Codex, then send Enter to finish this test (10-minute timeout).", flush=True)
            readable, _, _ = select.select([sys.stdin], [], [], 600)
            assert readable and sys.stdin.readline() != "", "GUI inspection timed out or input closed"
            assert (project / ".codex/config.toml").exists(), "GUI attachment was not observed"
        status, attached = api(record, "/v1/gui/open", {"project":str(project)})
        assert status == 200, attached
        assert attached["app_opened"] and attached["server_verified"]["environment_read"]
        assert not attached["harness_loaded"]
        if browser:
            assert not attached["configuration_changed"] and not attached["actor_initialized"]
        configured = (project / ".codex/config.toml").read_bytes()
        status, repeated = api(record, "/v1/gui/open", {"project":str(project)})
        assert status == 200 and not repeated["configuration_changed"] and not repeated["actor_initialized"]
        assert configured == (project / ".codex/config.toml").read_bytes()
        assert not (other / ".codex").exists()
        assert str(project.resolve()) in api(record, "/v1/gui/context")[1]["recent_projects"]
        stopped = cli("gui", "stop")
        assert stopped["stopped"] and not stopped["cells_stopped"] and not record_path.exists()
        assert cli("gui", "stop")["reason"] == "not_running"
        # Simulate a crash's stale owner record. Only this disposable GUI record is restored.
        descriptor = os.open(record_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "w") as output:
            json.dump(record, output)
        restarted = cli("gui", "start", "--allow-development")
        fresh = json.loads(record_path.read_text())
        assert not restarted["reused_server"] and fresh["instance"] != record["instance"] and fresh["token"] != record["token"]
        assert api(fresh, "/v1/environment", headers={"Authorization":"Bearer " + record["token"]})[0] == 401
        assert cli("gui", "stop")["stopped"] and not record_path.exists()
        return {"server_reused":True, "opening_does_not_attach":True,
                "unauthenticated_and_foreign_origin_actions_refused":True,
                "project_preview_read_only":True, "project_attachment_verified":True,
                "repeat_attachment_idempotent":True, "other_project_untouched":True,
                "recent_project_recorded":True, "stop_and_restart_verified":True,
                "stale_record_recovered":True, "old_session_rejected_after_restart":True,
                "browser_flow_confirmed":browser, "browser_checked":"chrome" if chrome else "default" if browser else None,
                "tab_focus":"best_effort", "harness_loaded":False}
    finally:
        if record_path.exists():
            cli("gui", "stop")
