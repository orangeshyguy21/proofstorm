#!/usr/bin/env python3
"""Opt-in terminal UX gate against a selected, already-ready installation.

Reconciles setup, opens/reuses the GUI without a browser, and checks doctor.
Creates no labs or agent connections. Stops the GUI only if this test started it.
"""
import argparse
import errno
import json
import os
from pathlib import Path
import pty
import re
import select
import subprocess
import tempfile
import time


def terminal(command, env):
    master, slave = pty.openpty()
    with tempfile.TemporaryFile() as stdout:
        started = time.monotonic()
        process = subprocess.Popen(command, env=env, stdin=subprocess.DEVNULL,
                                   stdout=stdout, stderr=slave)
        os.close(slave)
        chunks = []
        first = None
        try:
            while True:
                assert time.monotonic() - started < 420, "command exceeded UX gate timeout"
                if not select.select([master], [], [], 0.2)[0]:
                    continue
                try:
                    chunk = os.read(master, 65536)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                    break
                if not chunk:
                    break
                if first is None:
                    first = time.monotonic() - started
                chunks.append(chunk)
            process.wait(timeout=10)
        finally:
            os.close(master)
            if process.poll() is None:
                process.kill()
                process.wait()
        stdout.seek(0)
        result = stdout.read().decode()
    progress = b"".join(chunks).decode()
    assert process.returncode == 0, progress[-3000:]
    assert first is not None and first < 5, "no prompt initial progress"
    frames = re.findall(r"\r[|/\\-] [^\r]+", progress)
    assert len(frames) >= 2, "spinner did not keep moving"
    assert not re.search(r"\(\d+s\)", progress), "elapsed timer should not be displayed"
    assert progress.endswith("\r"), "spinner line was not cleared"
    assert "\x1b" not in progress
    stages = list(dict.fromkeys(frame[3:].strip() for frame in frames))
    return result, {"first_progress_seconds":round(first, 3), "frames":len(frames), "stages":stages,
                    "elapsed_seconds":round(time.monotonic() - started, 1)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--home", type=Path, required=True)
    parser.add_argument("--work-dir", type=Path, required=True)
    args = parser.parse_args()
    work = args.work_dir.resolve()
    work.mkdir(mode=0o700, parents=True, exist_ok=False)
    home = args.home.resolve()
    base = [str(args.cli.resolve()), "--home", str(home)]
    env = {key: value for key, value in os.environ.items() if not key.startswith("PROOFSTORM_")}
    env["TERM"] = "xterm-256color"
    gui_was_running = (home / "gui-process.json").exists()
    report = {}

    def machine(*arguments):
        result = subprocess.run([*base, "--json", *arguments], env=env,
                                capture_output=True, text=True, timeout=420)
        assert result.returncode == 0, result.stderr[-3000:]
        assert "\r" not in result.stderr and "\x1b" not in result.stderr
        assert "Checking installation" not in result.stderr
        return json.loads(result.stdout)

    try:
        print("Checking setup terminal progress", flush=True)
        result, report["setup"] = terminal([*base, "setup"], env)
        assert result == "Proofstorm is ready.\n\nRun proofstorm gui to get started.\n"
        assert machine("setup")["ready"]
        print("Checking GUI terminal progress and reuse", flush=True)
        result, report["gui"] = terminal([*base, "gui", "--no-open"], env)
        assert result.startswith("GUI ready: http://") and "Project:" in result
        assert "reused_server" not in result and "{" not in result
        assert "Checking Proofstorm files" in report["gui"]["stages"]
        assert "Checking existing GUI" in report["gui"]["stages"]
        result, report["gui_reuse"] = terminal([*base, "gui", "--no-open"], env)
        assert result.startswith("GUI ready: http://")
        assert "Reusing running GUI" in report["gui_reuse"]["stages"]
        assert machine("gui", "--no-open")["reused_server"]
        assert machine("doctor")["ok"]
        report["json_results_parse"] = True
        report["passed"] = True
    finally:
        if not gui_was_running:
            machine("stop")
        (work / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
