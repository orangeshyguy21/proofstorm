"""Hermetic tests for checkout artifact preparation (no Docker/builds)."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import develop


class DevelopmentTests(unittest.TestCase):
    def test_normal_shell_exit_does_not_relay_last_command_failure(self):
        for ending in ["exit\n", ""]:  # Bare exit and end-of-input.
            for status in [0, 1, 130]:
                with self.subTest(ending=ending, status=status):
                    result = subprocess.run(
                        [sys.executable, "-c", "import os, develop; develop.development_shell(os.environ.copy())"],
                        cwd=Path(develop.__file__).parent,
                        input=f'/bin/sh -c "exit {status}"\n{ending}',
                        capture_output=True, text=True, timeout=10,
                    )
                    self.assertEqual(result.returncode, 0, result.stderr)

    def test_shell_launch_and_signal_failures_remain_visible(self):
        with patch.object(develop.subprocess, "run", side_effect=OSError("cannot launch shell")):
            with self.assertRaisesRegex(OSError, "cannot launch"):
                develop.development_shell({})
        with patch.object(develop.subprocess, "run", return_value=subprocess.CompletedProcess([], -15)):
            with self.assertRaises(SystemExit) as error:
                develop.development_shell({})
            self.assertEqual(error.exception.code, 143)

    def test_build_failure_still_propagates(self):
        with patch.object(develop.subprocess, "run", side_effect=subprocess.CalledProcessError(1, ["cargo"])) as run:
            with self.assertRaises(subprocess.CalledProcessError):
                develop.run(["cargo", "build"], {})
            self.assertTrue(run.call_args.kwargs["check"])

    def test_controller_snapshot_excludes_local_state_and_web_source(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            source = root / "source"
            names = ["Cargo.toml", "Cargo.lock", "Dockerfile.proofstormd", "crates/proofstormd/Cargo.toml",
                     "crates/proofstormd/src/main.rs", "crates/proofstorm-web/Cargo.toml",
                     "crates/proofstorm-web/src/lib.rs", ".env", ".proofstorm-dev/state/private.json", ".cargo/config.toml"]
            for name in names:
                path = source / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("fixture")
            first = develop.controller_snapshot(source, root / "first", names)
            self.assertFalse((root / "first/.env").exists())
            self.assertFalse((root / "first/.proofstorm-dev").exists())
            self.assertFalse((root / "first/.cargo").exists())
            self.assertIn("Unbuilt", (root / "first/crates/proofstorm-web/src/lib.rs").read_text())
            (source / "crates/proofstorm-web/src/lib.rs").write_text("changed UI")
            self.assertEqual(first, develop.controller_snapshot(source, root / "second", names))
            (source / "crates/proofstormd/src/main.rs").write_text("changed controller")
            self.assertNotEqual(first, develop.controller_snapshot(source, root / "third", names))

    def test_controller_snapshot_refuses_linked_inputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            source = root / "source"
            source.mkdir()
            (source / "Cargo.toml").symlink_to(root / "secret")
            with self.assertRaisesRegex(ValueError, "linked"):
                develop.controller_snapshot(source, root / "snapshot", ["Cargo.toml"])

    def test_build_environment_drops_ambient_runtime_and_build_selection(self):
        with patch.dict(os.environ, {"PROOFSTORM_HOME": "/foreign", "PROOFSTORM_DB": "foreign",
                                     "CARGO_TARGET_DIR": "foreign", "CARGO_BUILD_TARGET": "foreign",
                                     "TRUNK_BUILD_DIST": "foreign", "PATH": "/tools"}, clear=True):
            self.assertEqual(develop.environment(), {"PATH": "/tools"})

    def test_launcher_quotes_paths_preserves_args_and_pins_checkout_home(self):
        with tempfile.TemporaryDirectory(prefix="proofstorm's checkout ") as temporary:
            root = Path(temporary)
            binary = root / "fake binary"
            develop.write_owned(binary, '#!/bin/sh\nprintf "%s\\n" "$PROOFSTORM_HOME" "$@"\n', 0o700)
            launcher = root / "proofstorm"
            home = root / "state with spaces"
            develop.write_owned(launcher, develop.launcher(binary, home), 0o700)
            result = subprocess.run([launcher, "open", "a directory's name"], check=True,
                                    env={"PROOFSTORM_HOME": "/foreign-production"}, text=True, capture_output=True)
            self.assertEqual(result.stdout.splitlines(), [str(home), "open", "a directory's name"])

    def test_owned_outputs_and_resource_inventory_refuse_links(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "output"
            develop.write_owned(output, "original")
            link = root / "link"
            link.symlink_to(output)
            with self.assertRaises(ValueError):
                develop.write_owned(link, "replacement")
            with self.assertRaises(ValueError):
                develop.inventory(root)
            self.assertEqual(output.read_text(), "original")
            self.assertEqual(output.stat().st_mode & 0o777, 0o600)

    def test_modified_resource_inventory_changes_content_address(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            develop.write_owned(root / "Chart.yaml", "first")
            before = develop.inventory(root)
            develop.write_owned(root / "Chart.yaml", "second")
            self.assertNotEqual(before, develop.inventory(root))

    def test_rejects_unowned_work_directory_without_running_builds(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / ".proofstorm-dev").mkdir()
            with patch.object(develop, "ROOT", root), patch("sys.argv", ["develop.py"]), patch.object(develop, "run") as run:
                with self.assertRaisesRegex(ValueError, "unowned"):
                    develop.main()
                run.assert_not_called()

    def test_rejects_persisted_legacy_target(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            work = root / ".proofstorm-dev"
            work.mkdir()
            develop.write_owned(work / "owner.json", json.dumps({"source": str(root)}))
            develop.write_owned(work / "build.json", json.dumps({"target": str(root / "target")}))
            with patch.object(develop, "ROOT", root), patch("sys.argv", ["develop.py"]), patch.object(develop, "run") as run:
                with self.assertRaisesRegex(ValueError, "legacy checkout target"):
                    develop.main()
                run.assert_not_called()

    def test_web_only_uses_managed_assets_without_building_hosts_or_registering(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            (root / ".tools/bin").mkdir(parents=True)
            develop.write_owned(root / ".tools/bin/trunk", "fixture")
            with patch.object(develop, "ROOT", root), patch("sys.argv", ["develop.py", "--web-only"]), patch.object(develop, "run") as run:
                develop.main()
                run.assert_called_once()
                arguments, env = run.call_args.args
                self.assertEqual(arguments[1], "build")
                self.assertEqual(arguments[-1], root / ".proofstorm-dev/web")
                self.assertEqual(env["PROOFSTORM_WEB_DIST"], str(root / ".proofstorm-dev/web"))
                self.assertFalse((root / ".proofstorm-dev/state").exists())
                self.assertFalse((root / ".proofstorm-dev/resources").exists())


if __name__ == "__main__":
    unittest.main()
