#!/usr/bin/env python3
"""Build/register a checkout installation; product operations stay in the CLI.

No release archive, global PATH changes, legacy cluster selection, or Docker
mutation. `just dev` enters an isolated-selection shell; `just dev-build` just
refreshes artifacts. Web watch publishes assets to the same managed GUI origin.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
RUNTIME_CRATES = {"proofstorm-core", "proofstorm-kube", "proofstorm-transfer", "proofstorm-exec", "proofstormd"}


def environment():
    return {key: value for key, value in os.environ.items()
            if not key.startswith(("PROOFSTORM_", "TRUNK_"))
            and key not in {"CARGO_BUILD_TARGET", "CARGO_TARGET_DIR"}}


def write_owned(path, text, mode=0o600):
    if path.is_symlink() or (path.exists() and not path.is_file()):
        raise ValueError(f"refusing linked/non-file output: {path}")
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as stream:
        temporary = Path(stream.name)
        stream.write(text.encode())
        stream.flush()
        os.fsync(stream.fileno())
    temporary.chmod(mode)
    temporary.replace(path)


def directory(path):
    if path.is_symlink() or (path.exists() and not path.is_dir()):
        raise ValueError(f"refusing linked/non-directory output: {path}")
    path.mkdir(mode=0o700, parents=True, exist_ok=True)


def run(args, env, capture=False):
    result = subprocess.run([str(a) for a in args], cwd=ROOT, env=env,
                            stdout=subprocess.PIPE if capture else None,
                            text=True, check=True)
    return result.stdout


def inventory(root):
    files = {}
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise ValueError("linked build resource refused")
        if path.is_file():
            digest = hashlib.sha256()
            with path.open("rb") as stream:
                for chunk in iter(lambda: stream.read(65536), b""):
                    digest.update(chunk)
            files[str(path.relative_to(root))] = digest.hexdigest()
    return files


def launcher(binary, home):
    return ("#!/bin/sh\n# Proofstorm checkout launcher v1\n"
            f"export PROOFSTORM_HOME={shlex.quote(str(home))}\n"
            f'exec {shlex.quote(str(binary))} "$@"\n')


def development_shell(env):
    # A completed interactive session is not a build result. Bare exit and EOF
    # inherit the last command's status, including 130 after Ctrl-C.
    # Keep launch failures and abnormal signal termination visible to Make.
    result = subprocess.run(["/bin/zsh", "-f", "-i"], env=env, check=False)
    if result.returncode < 0:
        raise SystemExit(128 - result.returncode)


def controller_snapshot(source, destination, names):
    """Only controller build inputs, never state, credentials, or web build output."""
    destination.mkdir()
    for name in sorted(set(filter(None, names))):
        relative = Path(name)
        if relative.is_absolute() or ".." in relative.parts:
            raise ValueError("unsafe controller source path")
        parts = relative.parts
        selected = name in {"Cargo.toml", "Cargo.lock", "Dockerfile.proofstormd", ".dockerignore"}
        if len(parts) >= 3 and parts[0] == "crates":
            selected |= parts[1] in RUNTIME_CRATES or (len(parts) == 3 and parts[2] == "Cargo.toml")
        selected |= (len(parts) == 3 and parts[0] == "docker" and parts[1] in {"wallet", "mint", "bitcoin"}
                     and parts[2].endswith("-provenance.json"))
        if not selected:
            continue
        original = source / relative
        if original.is_symlink():
            raise ValueError("linked controller source refused")
        if not original.exists():  # tracked development deletion
            continue
        if not original.is_file() or any(parent.is_symlink() for parent in original.parents if parent != source.parent):
            raise ValueError("non-regular controller source refused")
        output = destination / relative
        output.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(original, output)
    # Cargo resolves all workspace members. Unbuilt host/web members need targets,
    # but their source/assets must not invalidate the controller build cache.
    for manifest in (destination / "crates").glob("*/Cargo.toml"):
        if manifest.parent.name not in RUNTIME_CRATES:
            (manifest.parent / "src").mkdir(exist_ok=True)
            (manifest.parent / "src/lib.rs").write_text("// Unbuilt workspace member.\n")
            (manifest.parent / "src/main.rs").write_text("fn main() {}\n")
    files = inventory(destination)
    digest = hashlib.sha256()
    for name, sha in sorted(files.items()):
        digest.update(name.encode() + b"\0" + sha.encode() + b"\n")
    return {"format_version": 1, "sha256": digest.hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--shell", action="store_true")
    mode.add_argument("--watch-web", action="store_true")
    mode.add_argument("--web-only", action="store_true")
    parser.add_argument("--target-dir", type=Path)
    args = parser.parse_args()
    if args.shell and not sys.stdin.isatty():
        parser.error("just dev needs an interactive terminal; use just dev-build in automation")
    work = ROOT / ".proofstorm-dev"
    marker = work / "owner.json"
    if work.exists() and not marker.is_file():
        raise ValueError("unowned .proofstorm-dev directory; select/resolve it explicitly")
    directory(work)
    if marker.exists():
        if marker.is_symlink() or json.loads(marker.read_text()) != {"source":str(ROOT)}:
            raise ValueError("development directory belongs to a different checkout")
    else:
        write_owned(marker, json.dumps({"source":str(ROOT)}))
    target = args.target_dir.resolve() if args.target_dir else work / "target"
    # Persist the chosen cache so watch and later builds use the same toolchain.
    settings = work / "build.json"
    if not args.target_dir and settings.exists():
        if settings.is_symlink():
            raise ValueError("linked build settings refused")
        target = Path(json.loads(settings.read_text())["target"])
    if not target.is_absolute() or target != target.resolve():
        raise ValueError("build target must be an absolute canonical directory")
    if target == ROOT / "target" or target.is_relative_to(ROOT / "target"):
        raise ValueError("use a dedicated dev target, not the legacy checkout target")
    directory(target)
    write_owned(settings, json.dumps({"target":str(target)}))
    env = environment()
    env.update(CARGO_TARGET_DIR=str(target), PROOFSTORM_WEB_DIST=str(work / "web"), NO_COLOR="true")
    trunk = ROOT / ".tools/bin/trunk"
    if not trunk.is_file():
        raise ValueError("run just web-tools to install the pinned web builder")
    web_command = [trunk, "watch" if args.watch_web else "build", "--release", "--locked",
                   "--config", ROOT / "crates/proofstorm-web/Trunk.toml", "--dist", work / "web"]
    if args.watch_web:
        if not (work / "state/checkout-artifacts.json").is_file():
            raise ValueError("run just dev-build first")
        print("Watching web assets for the managed GUI. Refresh its browser tab after a build.", flush=True)
        run(web_command, env)
        return
    print("Building checkout assets (no release archive or runtime changes)", flush=True)
    run(web_command, env)
    if args.web_only:
        print("Web assets rebuilt. Refresh the managed GUI tab.", flush=True)
        return
    env["PROOFSTORM_REQUIRE_WEB_ASSETS"] = "1"
    run(["cargo", "build", "--locked", "-p", "proofstorm-app", "-p", "proofstorm-mcp", "--bins"], env)
    cli, mcp = (target / "debug" / name for name in ("proofstorm", "proofstorm-mcp"))
    info = run([cli, "release-info"], env, capture=True)
    resources = work / "resources"
    directory(resources)
    with tempfile.TemporaryDirectory(prefix=".build-", dir=resources) as scratch:
        stage = Path(scratch) / "payload"
        stage.mkdir()
        shutil.copytree(ROOT / "charts/proofstorm", stage / "chart")
        run(["cargo", "run", "--locked", "-p", "proofstorm-kube", "--example", "export_crds", "--", stage / "chart/crds"], env)
        (stage / "release-info.json").write_text(info)
        names = run(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], env, capture=True).split("\0")
        controller = controller_snapshot(ROOT, stage / "controller-source", names)
        (stage / "controller-source.json").write_text(json.dumps(controller))
        files = inventory(stage)
        name = hashlib.sha256(json.dumps(files, sort_keys=True).encode()).hexdigest()
        destination = resources / name
        if destination.exists():
            if inventory(destination) != files:
                raise ValueError("resource snapshot was modified; refusing overwrite")
        else:
            stage.rename(destination)
    home = work / "state"
    run([cli, "--home", home, "checkout-register", "--source", ROOT,
         "--resources", destination, "--mcp", mcp, "--web-dist", work / "web"], env)
    directory(work / "bin")
    for name in ("proofstorm", "proofstorm-mcp"):
        path = work / "bin" / name
        if path.exists() and not path.read_text().startswith("#!/bin/sh\n# Proofstorm checkout launcher v1\n"):
            raise ValueError(f"refusing foreign launcher: {path}")
        write_owned(path, launcher(target / "debug" / name, home), 0o755)
    print(f"\nCheckout ready: {work / 'bin/proofstorm'}\n"
          "Use the same commands: setup, doctor, gui, up, attach, open.\n"
          "No Docker runtime was started. After a host rebuild, stop/reopen an existing GUI.", flush=True)
    if args.shell:
        shell_env = environment()
        shell_env.update(PROOFSTORM_HOME=str(home), PATH=str(work / "bin") + os.pathsep + env.get("PATH", ""),
                         PROMPT="(proofstorm dev) %~ %# ")
        print("Development shell selected. Run proofstorm setup first. Exit returns to your normal shell.", flush=True)
        development_shell(shell_env)


if __name__ == "__main__":
    main()
