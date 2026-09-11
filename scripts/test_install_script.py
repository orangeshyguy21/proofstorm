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
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ["PATH"])

    def platform(self, system, machine):
        path = self.bin / "uname"
        path.write_text(f'#!/bin/sh\ncase "$1" in -s) echo {system};; -m) echo {machine};; esac\n')
        path.chmod(0o755)

    def archive_with(self, name="proofstorm/bin/proofstorm", link=False, reject_development=False):
        with tarfile.open(self.archive, "w:gz") as tar:
            info = tarfile.TarInfo(name)
            if link:
                info.type = tarfile.SYMTYPE
                info.linkname = "/outside"
                tar.addfile(info)
            else:
                body = b'#!/bin/sh\n[ -x "$0" ] || exit 19\nexit 0\n'
                if reject_development:
                    body = b'#!/bin/sh\ncase "$*" in *--allow-development*) exit 23;; esac\nexit 0\n'
                info.mode = 0o755
                info.size = len(body)
                tar.addfile(info, io.BytesIO(body))
        Path(str(self.archive) + ".sha256").write_text(hashlib.sha256(self.archive.read_bytes()).hexdigest() + "  " + self.archive.name + "\n")

    def run_installer(self, *extra):
        return subprocess.run(["sh", SCRIPT, "--artifact-dir", self.root, "--archive", self.archive.name,
                               "--prefix", self.root / "new prefix", *extra], env=self.env, capture_output=True, text=True)

    def test_supported_platforms_select_the_correct_archive(self):
        for system, machine, target in [("Darwin", "arm64", "macos-arm64"),
                                        ("Linux", "x86_64", "linux-amd64")]:
            self.platform(system, machine)
            self.archive = self.root / f"proofstorm-0.1.0-alpha.1-{target}.tar.gz"
            self.archive_with()
            result = subprocess.run(["sh", SCRIPT, "--artifact-dir", self.root,
                                     "--prefix", self.root / "prefix", "--version", "0.1.0-alpha.1", "--allow-development"],
                                    env=self.env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_github_alpha_download_uses_normal_command_without_override(self):
        self.platform("Linux", "x86_64")
        self.archive = self.root / "proofstorm-0.1.0-alpha.1-linux-amd64.tar.gz"
        self.archive_with(reject_development=True)
        curl = self.bin / "curl"
        curl.write_text('''#!/bin/sh
printf '%s\\n' "$@" >> "$DOWNLOAD_LOG"
source="$DOWNLOAD_FIXTURE"
for arg do
  case "$arg" in https://*.sha256) source="$DOWNLOAD_FIXTURE.sha256";; esac
done
while [ "$#" -gt 0 ]; do
  if [ "$1" = --output ]; then cp "$source" "$2"; printf 200; exit; fi
  shift
done
exit 24
''')
        curl.chmod(0o755)
        log = self.root / "downloads"
        result = subprocess.run(["sh", SCRIPT, "--prefix", self.root / "prefix", "--version", "0.1.0-alpha.1"],
                                env=dict(self.env, DOWNLOAD_FIXTURE=str(self.archive), DOWNLOAD_LOG=str(log)),
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        args = log.read_text().splitlines()
        url = "https://github.com/orangeshyguy21/proofstorm/releases/download/v0.1.0-alpha.1/" + self.archive.name
        self.assertIn(url, args)
        self.assertIn(url + ".sha256", args)
        self.assertEqual(args.count("--proto-redir"), 2)
        self.assertNotIn("--allow-development", args)

    def test_unsupported_hosts_fail_before_installation(self):
        for system, machine in [("Linux", "aarch64"), ("Darwin", "x86_64"), ("Windows", "x86_64")]:
            self.platform(system, machine)
            result = self.run_installer()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("supports macOS Apple Silicon and Linux x86-64", result.stderr)
            self.assertFalse((self.root / "new prefix").exists())

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
