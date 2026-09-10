#!/usr/bin/env python3
"""Build real Linux alpha/development bundles without checkout or host Docker socket mounts.

The build container has its own writable filesystem, capped resources, and no
privileges. This is host packaging, not the privileged Docker-in-Docker
runtime test. It never publishes, creates a cluster, or configures an agent.
"""
import argparse
from pathlib import Path
import subprocess


def build(source, work, debug=False, development=False):
    """Compatibility entrypoint; Bash/Rust own Linux container builds."""
    command = ["bash", str(Path(__file__).resolve().with_name("linux-build.sh")),
               "--source", str(source), "--work-dir", str(work)]
    if debug:
        command.append("--debug")
    if development:
        command.append("--development")
    subprocess.run(command, check=True)


def worker():
    """Compatibility entrypoint; the container itself starts Bash directly."""
    subprocess.run(["bash", str(Path(__file__).resolve().with_name("linux-build-worker.sh"))],
                   check=True)


def install_smoke(archive, installer, work, development=False):
    """Compatibility entrypoint; Bash/Rust own the source-free installer lane."""
    script = Path(__file__).resolve().with_name("linux-install-smoke.sh")
    command = ["bash", str(script), "--archive", str(archive), "--installer",
               str(installer), "--work-dir", str(work)]
    if development:
        command.append("--development")
    subprocess.run(command, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    builder = sub.add_parser("build")
    builder.add_argument("--work-dir", type=Path, required=True)
    builder.add_argument("--debug", action="store_true", help="Faster alpha/development host build")
    builder.add_argument("--development", action="store_true", help="Build an unpublished local bundle instead of the version's normal channel")
    smoker = sub.add_parser("smoke", help="Offline, source-free install/reinstall test; no runtime setup")
    smoker.add_argument("--archive", type=Path, required=True)
    smoker.add_argument("--installer", type=Path, required=True)
    smoker.add_argument("--work-dir", type=Path, required=True)
    smoker.add_argument("--development", action="store_true", help="Explicitly test an unpublished development bundle")
    sub.add_parser("worker", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.command == "worker":
        worker()
    elif args.command == "smoke":
        install_smoke(args.archive, args.installer, args.work_dir, args.development)
    else:
        build(Path(__file__).resolve().parents[1], args.work_dir, args.debug, args.development)


if __name__ == "__main__":
    main()
