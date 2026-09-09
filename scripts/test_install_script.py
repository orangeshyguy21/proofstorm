"""Shell download/extraction guards; Rust tests cover installation activation."""
import hashlib
import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "install.sh"


class InstallScriptTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.archive = self.root / "proofstorm-test.tar.gz"

    def archive_with(self, name="proofstorm/bin/proofstorm", link=False):
        with tarfile.open(self.archive, "w:gz") as tar:
            info = tarfile.TarInfo(name)
            if link:
                info.type = tarfile.SYMTYPE
                info.linkname = "/outside"
                tar.addfile(info)
            else:
                body = b'#!/bin/sh\n[ "$(stat -f %Lp "$0")" = 755 ] || exit 19\nexit 0\n'
                info.mode = 0o755
                info.size = len(body)
                tar.addfile(info, io.BytesIO(body))
        Path(str(self.archive) + ".sha256").write_text(hashlib.sha256(self.archive.read_bytes()).hexdigest() + "  " + self.archive.name + "\n")

    def run_installer(self, *extra):
        return subprocess.run(["sh", SCRIPT, "--artifact-dir", self.root, "--archive", self.archive.name,
                               "--prefix", self.root / "new prefix", *extra], capture_output=True, text=True)

    def test_local_prebuilt_path_requires_no_compiler_or_python(self):
        self.archive_with()
        result = self.run_installer("--allow-development")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("No cluster", result.stdout)

    def test_bad_checksum_never_creates_install_prefix(self):
        self.archive_with()
        self.archive.write_bytes(b"corrupt")
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("checksum mismatch", result.stderr)
        self.assertFalse((self.root / "new prefix").exists())

    def test_traversal_and_links_are_refused(self):
        for name, link in [("proofstorm/../../escaped", False), ("proofstorm/bin/proofstorm", True)]:
            self.archive_with(name, link)
            result = self.run_installer()
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((self.root / "new prefix").exists())

    def test_development_download_and_unsafe_names_fail_before_network(self):
        result = subprocess.run(["sh", SCRIPT, "--allow-development"], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("restricted", result.stderr)
        result = subprocess.run(["sh", SCRIPT, "--archive", "../escape.tar.gz"], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
