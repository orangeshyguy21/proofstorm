"""Linux build containment and transported-provenance checks; no Docker needed."""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import linux_container as linux


class LinuxContainerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def test_container_has_no_host_mounts_privileges_or_host_configuration(self):
        command = linux.create_command("proofstorm-linux-build-test", "test-image")
        for forbidden in ["--privileged", "--volume", "-v", "--mount", "--env-file", "--network=host"]:
            self.assertNotIn(forbidden, command)
        self.assertNotIn("docker.sock", " ".join(command))
        for key, value in [("--platform", "linux/amd64"), ("--memory", "3g"), ("--cpus", "2"),
                           ("--cap-drop", "ALL"), ("--security-opt", "no-new-privileges")]:
            self.assertEqual(command[command.index(key) + 1], value)

    def test_transport_verification_uses_snapshot_file_names_modes_and_bytes(self):
        path = self.root / "test.sh"
        path.write_bytes(b"original")
        path.chmod(0o755)
        tree = hashlib.sha256(b"test.sh\0" + str(0o755).encode() + b"\0" + hashlib.sha256(b"original").digest())
        provenance = {"sha256": tree.hexdigest()}
        linux.verify_snapshot(self.root, provenance)
        path.chmod(0o644)
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            linux.verify_snapshot(self.root, provenance)
        path.chmod(0o755)
        path.write_bytes(b"tampered")
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            linux.verify_snapshot(self.root, provenance)

    def test_transport_refuses_added_files_and_symlinks(self):
        provenance = {"sha256": hashlib.sha256().hexdigest()}
        linux.verify_snapshot(self.root, provenance)
        path = self.root / "unexpected"
        path.write_text("extra")
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            linux.verify_snapshot(self.root, provenance)
        path.unlink()
        path.symlink_to("/etc/passwd")
        with self.assertRaisesRegex(ValueError, "symlink"):
            linux.verify_snapshot(self.root, provenance)

    def test_existing_or_checkout_work_directory_refused_before_docker(self):
        with patch.object(linux.subprocess, "run") as run:
            with self.assertRaisesRegex(ValueError, "must be new"):
                linux.build(self.root, self.root)
            with self.assertRaisesRegex(ValueError, "outside the checkout"):
                linux.build(self.root, self.root / "build")
            run.assert_not_called()
        self.assertFalse((self.root / "build").exists())

    def test_installer_check_is_offline_source_free_and_read_only(self):
        command = linux.smoke_command("proofstorm-linux-install-test", "proofstorm-test.tar.gz")
        self.assertEqual(command[command.index("--network") + 1], "none")
        self.assertIn("--read-only", command)
        self.assertEqual(command[command.index("--user") + 1], "1000:1000")
        for forbidden in ["--privileged", "--volume", "-v", "--mount", "--env-file"]:
            self.assertNotIn(forbidden, command)
        self.assertIn("for attempt in first reinstall", linux.INSTALL_CHECK)
        self.assertNotIn("proofstorm setup", linux.INSTALL_CHECK)
        self.assertIn("cargo rustc trunk python3 docker", linux.INSTALL_CHECK)

    def test_installer_rejects_tampered_archive_before_creating_container(self):
        archive = self.root / "proofstorm-test-x86_64-unknown-linux-gnu.tar.gz"
        archive.write_bytes(b"tampered")
        Path(str(archive) + ".sha256").write_text("a" * 64 + "  " + archive.name + "\n")
        installer = self.root / "install.sh"
        installer.write_text("exit 0\n")
        with patch.object(linux.subprocess, "run") as run:
            with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                linux.install_smoke(archive, installer, self.root / "work")
            run.assert_not_called()
        self.assertFalse((self.root / "work").exists())

    def test_failed_install_does_not_write_a_success_report(self):
        archive = self.root / "proofstorm-test-x86_64-unknown-linux-gnu.tar.gz"
        archive.write_bytes(b"fixture")
        Path(str(archive) + ".sha256").write_text(linux.release.digest(archive) + "  " + archive.name + "\n")
        installer = self.root / "install.sh"
        installer.write_text("exit 1\n")
        from subprocess import CompletedProcess
        with patch.object(linux.subprocess, "run", return_value=CompletedProcess([], 0, stdout="1\n")) as run:
            with self.assertRaisesRegex(ValueError, "installer check failed"):
                linux.install_smoke(archive, installer, self.root / "work")
        self.assertFalse((self.root / "work/install-smoke-report.json").exists())
        receipt = json.loads((self.root / "work/run.json").read_text())
        self.assertEqual(run.call_args.args[0], ["docker", "rm", receipt["container"]])

    def test_failed_worker_stops_only_owned_container_and_does_not_export(self):
        source = self.root / "source"
        source.mkdir()
        dockerfile = source / "docker/release/Dockerfile.linux-builder"
        dockerfile.parent.mkdir(parents=True)
        dockerfile.write_text("FROM fixture\n")
        def snapshot(_, destination, development):
            import shutil
            shutil.copytree(source, destination)
            return {"revision": "a" * 40, "sha256": "b" * 64, "dirty": True}
        from subprocess import CompletedProcess
        with patch.object(linux.release, "snapshot", side_effect=snapshot), \
             patch.object(linux.subprocess, "run", return_value=CompletedProcess([], 0, stdout="1\n")) as run:
            with self.assertRaisesRegex(ValueError, "build failed"):
                linux.build(source, self.root / "work")
        receipt = json.loads((self.root / "work/run.json").read_text())
        commands = [call.args[0] for call in run.call_args_list]
        self.assertEqual(commands[-2], ["docker", "stop", "--timeout", "10", receipt["container"]])
        self.assertEqual(commands[-1], ["docker", "rm", receipt["container"]])
        self.assertFalse(any(":/artifacts" in " ".join(command) for command in commands))

    def test_log_timeout_still_stops_and_removes_container(self):
        from subprocess import CompletedProcess, TimeoutExpired
        with patch.object(linux.subprocess, "run", side_effect=[
            TimeoutExpired("docker logs", 30), CompletedProcess([], 0), CompletedProcess([], 0),
        ]) as run:
            linux.cleanup("proofstorm-linux-build-test", self.root / "build.log")
        self.assertEqual(run.call_args_list[-2].args[0],
                         ["docker", "stop", "--timeout", "10", "proofstorm-linux-build-test"])
        self.assertEqual(run.call_args.args[0], ["docker", "rm", "proofstorm-linux-build-test"])


if __name__ == "__main__":
    unittest.main()
