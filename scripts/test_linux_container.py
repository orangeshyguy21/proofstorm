"""Legacy command compatibility only; containment is tested in Bash/Rust."""
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import linux_container as linux


class LinuxContainerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def test_build_delegates_to_bash_with_literal_paths_and_options(self):
        source = self.root / "quoted 'source'"
        work = self.root / "new work"
        for debug, development in [(False, False), (True, False), (True, True)]:
            with patch.object(linux.subprocess, "run") as run:
                linux.build(source, work, debug, development)
            expected = ["bash", str(Path(linux.__file__).resolve().with_name("linux-build.sh")),
                        "--source", str(source), "--work-dir", str(work)]
            if debug:
                expected.append("--debug")
            if development:
                expected.append("--development")
            run.assert_called_once_with(expected, check=True)

    def test_worker_delegates_to_bash(self):
        with patch.object(linux.subprocess, "run") as run:
            linux.worker()
        run.assert_called_once_with(
            ["bash", str(Path(linux.__file__).resolve().with_name("linux-build-worker.sh"))],
            check=True)

    def test_installer_smoke_delegates_without_changing_arguments(self):
        archive = self.root / "bundle with 'quotes'.tar.gz"
        installer = self.root / "install.sh"
        work = self.root / "new work"
        for development in [False, True]:
            with patch.object(linux.subprocess, "run") as run:
                linux.install_smoke(archive, installer, work, development)
            expected = ["bash", str(Path(linux.__file__).resolve().with_name("linux-install-smoke.sh")),
                        "--archive", str(archive), "--installer", str(installer),
                        "--work-dir", str(work)]
            if development:
                expected.append("--development")
            run.assert_called_once_with(expected, check=True)

    def test_bash_failure_propagates(self):
        with patch.object(linux.subprocess, "run", side_effect=subprocess.CalledProcessError(23, "bash")):
            with self.assertRaises(subprocess.CalledProcessError):
                linux.build(self.root, self.root / "work")


if __name__ == "__main__":
    unittest.main()
