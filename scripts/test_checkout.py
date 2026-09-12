#!/usr/bin/env python3
"""Exercise a registered checkout's live product path (setup must already pass).

Uses private agent homes/projects and starts no model sessions. Leaves the selected
runtime/labs alone; stops only the test GUI. Requires an idle/stopped GUI initially.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import urllib.error
import urllib.request

import test_agent_attachments


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--home", type=Path, required=True)
    parser.add_argument("--work-dir", type=Path, required=True)
    parser.add_argument("--test-agents", action="store_true")
    args = parser.parse_args()
    executable, home = args.cli.resolve(), args.home.resolve()
    work = args.work_dir.resolve()
    work.mkdir(mode=0o700, parents=True, exist_ok=False)
    env = {key: value for key, value in os.environ.items() if not key.startswith("PROOFSTORM_")}
    record_path = home / "gui-process.json"
    assert not record_path.exists(), "stop the selected installation's GUI before this test"
    before = json.loads((home / "installation.json").read_text())

    def cli(*arguments):
        if "--json" not in arguments:
            arguments = ("--json", *arguments)
        result = subprocess.run([executable, "--home", home, *arguments], env=env,
                                cwd=work, capture_output=True, text=True, timeout=240)
        assert result.returncode == 0, result.stderr[-3000:]
        return json.loads(result.stdout)

    report = {"source": "checkout", "development_flag_required": False}
    try:
        print("Checkout gate: doctor and managed GUI", file=sys.stderr, flush=True)
        report["doctor"] = cli("doctor", "--json")
        cli("gui", "start")
        first_record = json.loads(record_path.read_text())
        cli("gui", "start")
        second_record = json.loads(record_path.read_text())
        assert first_record["instance"] == second_record["instance"]
        assert first_record["pid"] == second_record["pid"]
        assert first_record["build_sha256"]
        report["gui_reused"] = True
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        base = f'http://127.0.0.1:{first_record["port"]}'
        with opener.open(base + "/", timeout=30) as response:
            assert response.status == 200 and response.headers["Cache-Control"] == "no-store"
            assert b"<html" in response.read().lower()
        report["checkout_web_served"] = True
        try:
            opener.open(base + "/v1/environment", timeout=30)
            raise AssertionError("unauthenticated API unexpectedly accepted")
        except urllib.error.HTTPError as error:
            assert error.code in (401, 403)
        request = urllib.request.Request(base + "/v1/environment", headers={"Authorization": "Bearer " + first_record["token"]})
        with opener.open(request, timeout=30) as response:
            assert response.status == 200
        report["managed_auth_enforced"] = True
        cli("gui", "stop")
        assert not record_path.exists()
        if args.test_agents:
            print("Checkout gate: private agent connections (keep this installation idle)", file=sys.stderr, flush=True)
            report["agents"] = test_agent_attachments.run(None, env, work, executable=executable,
                installation_home=home, allow_development=False)
        assert json.loads((home / "installation.json").read_text()) == before
        report["installation_identity_preserved"] = True
        (work / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps({"passed": True, "report": str(work / "report.json")}, indent=2))
    finally:
        if record_path.exists():
            cli("gui", "stop")


if __name__ == "__main__":
    main()
