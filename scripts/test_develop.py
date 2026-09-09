"""Hermetic tests for checkout artifact preparation (no Docker/builds)."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import develop


class DevelopmentTests(unittest.TestCase):
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
