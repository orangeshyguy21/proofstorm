"""Controller publication guards, without Docker or network access."""
import json
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import controller_release as controller


class ControllerPublicationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / "receipt.json"
        self.receipt = {"tag": controller.NAMESPACE + "/proofstormd:development-" + "a" * 32,
                        "platform": "linux/arm64", "release_ready": False,
                        "local_image_id": "sha256:" + "c" * 64,
                        "source": {"sha256": "b" * 64}, "metadata": {"source_sha256": "b" * 64}}

    def write(self):
        self.path.write_text(json.dumps(self.receipt))

    def test_requires_namespace_confirmation_before_push(self):
        self.write()
        with patch.object(controller.subprocess, "run") as run:
            with self.assertRaises(ValueError):
                controller.publish(self.path, "another/account")
            run.assert_not_called()

    def test_invalid_build_platform_is_rejected_before_creating_work(self):
        work = self.path.parent / "build"
        with patch.object(controller.subprocess, "run") as run:
            with self.assertRaisesRegex(ValueError, "platform"):
                controller.build(self.path.parent, work, "linux/386")
            run.assert_not_called()
        self.assertFalse(work.exists())

    def test_refuses_mutable_alias_and_mismatched_provenance(self):
        for key, value in [("tag", controller.NAMESPACE + "/proofstormd:latest"),
                           ("metadata", {"source_sha256": "wrong"})]:
            original = self.receipt[key]
            self.receipt[key] = value
            self.write()
            with patch.object(controller.subprocess, "run") as run:
                with self.assertRaises(ValueError):
                    controller.publish(self.path, controller.NAMESPACE)
                run.assert_not_called()
            self.receipt[key] = original

    def test_published_receipt_uses_digest_and_stays_development(self):
        self.write()
        from subprocess import CompletedProcess
        with patch.object(controller, "verify_local", return_value=self.receipt), \
             patch.object(controller.publish_images, "Registry") as registry, \
             patch.object(controller.subprocess, "run", side_effect=[CompletedProcess([], 0),
                 CompletedProcess([], 0, stdout=json.dumps({"digest": "sha256:" + "c" * 64}))]):
            registry.return_value.inspect.return_value = {"linux/arm64"}
            controller.publish(self.path, controller.NAMESPACE)
        result = json.loads(self.path.read_text())
        self.assertFalse(result["release_ready"])
        self.assertEqual(result["image"], controller.NAMESPACE + "/proofstormd@sha256:" + "c" * 64)
        self.assertTrue(result["anonymous_verified"])

    def test_changed_tag_or_metadata_is_rejected_before_push(self):
        self.write()
        for key in ["local_image_id", "metadata"]:
            checked = {**self.receipt, key: "changed"}
            with patch.object(controller, "verify_local", return_value=checked), \
                 patch.object(controller.subprocess, "run") as run:
                with self.assertRaisesRegex(ValueError, "no longer matches"):
                    controller.publish(self.path, controller.NAMESPACE)
                run.assert_not_called()

    def test_remote_verification_failure_retains_unverified_upload_receipt(self):
        from subprocess import CompletedProcess
        for wrong_platform in [True, False]:
            self.write()
            with patch.object(controller, "verify_local", return_value=self.receipt), \
                 patch.object(controller.publish_images, "Registry") as registry, \
                 patch.object(controller.publish_images, "contains_identity", return_value=False), \
                 patch.object(controller.subprocess, "run", side_effect=[CompletedProcess([], 0),
                     CompletedProcess([], 0, stdout=json.dumps({"digest": "sha256:" + "c" * 64}))]):
                registry.return_value.inspect.return_value = {"linux/amd64" if wrong_platform else "linux/arm64"}
                with self.assertRaises(ValueError):
                    controller.publish(self.path, controller.NAMESPACE)
            result = json.loads(self.path.read_text())
            self.assertIn("image", result)
            self.assertFalse(result["anonymous_verified"])


class ControllerBuildVerificationTests(unittest.TestCase):
    def setUp(self):
        self.provenance = {"sha256": "a" * 64}
        self.info = {"format_version": 1, "source_sha256": "a" * 64,
                     "version": "0.1.0-alpha.1", "runtime_contract_sha256": "b" * 64}
        self.host = {**self.info, "target": "x86_64-unknown-linux-gnu"}
        self.image = {"Id": "sha256:" + "c" * 64, "Os": "linux", "Architecture": "amd64",
                      "Config": {"User": "65532:65532", "Labels": {"dev.proofstorm.source-sha256": "a" * 64}}}

    def responses(self, helper_error=None):
        from subprocess import CompletedProcess
        return [CompletedProcess([], 0, stdout=json.dumps([self.image])),
                CompletedProcess([], 0, stdout=json.dumps(self.info)),
                CompletedProcess([], 1, stdout="", stderr=helper_error or '{"runner_error":"native_runner_failed"}\n')]

    def test_both_executables_are_probed_offline_by_immutable_image_identity(self):
        with patch.object(controller.subprocess, "run", side_effect=self.responses()) as run:
            result = controller.verify_local("test-tag", "linux/amd64", self.provenance, self.host)
        self.assertTrue(result["verification"]["host_contract_match"])
        self.assertTrue(result["verification"]["helper_startup"])
        self.assertFalse(result["verification"]["cluster_reconciliation"])
        for call in run.call_args_list[1:]:
            command = call.args[0]
            self.assertIn(self.image["Id"], command)
            self.assertNotIn("test-tag", command)
            self.assertIn("--read-only", command)
            self.assertEqual(command[command.index("--network") + 1], "none")
            self.assertEqual(command[command.index("--cap-drop") + 1], "ALL")
            self.assertNotIn("--privileged", command)

    def test_wrong_platform_or_root_image_fails_before_execution(self):
        for key, value in [("Architecture", "arm64"), ("Config", {"User": "0"})]:
            original = self.image[key]
            self.image[key] = value
            with patch.object(controller.subprocess, "run", side_effect=self.responses()) as run:
                with self.assertRaises(ValueError):
                    controller.verify_local("test-tag", "linux/amd64", self.provenance, self.host)
            self.assertEqual(run.call_count, 1)
            self.image[key] = original

    def test_foreign_source_or_runtime_contract_is_rejected(self):
        for key in ["source_sha256", "runtime_contract_sha256", "version"]:
            original = self.info[key]
            self.info[key] = "wrong"
            with patch.object(controller.subprocess, "run", side_effect=self.responses()):
                with self.assertRaises(ValueError):
                    controller.verify_local("test-tag", "linux/amd64", self.provenance, self.host)
            self.info[key] = original

    def test_loader_failure_is_not_a_successful_helper_probe(self):
        with patch.object(controller.subprocess, "run", side_effect=self.responses("exec format error")):
            with self.assertRaisesRegex(ValueError, "helper failed"):
                controller.verify_local("test-tag", "linux/amd64", self.provenance, self.host)

    def test_host_bundle_checksum_and_platform_are_checked(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "host.tar.gz"
            data = json.dumps(self.host).encode()
            with tarfile.open(archive, "w:gz") as bundle:
                member = tarfile.TarInfo("proofstorm/release-info.json")
                member.size = len(data)
                bundle.addfile(member, io.BytesIO(data))
            checksum = Path(str(archive) + ".sha256")
            checksum.write_text(controller.release.digest(archive) + "  " + archive.name + "\n")
            self.assertEqual(controller.bundle_metadata(archive, "linux/amd64"), self.host)
            with self.assertRaisesRegex(ValueError, "platform mismatch"):
                controller.bundle_metadata(archive, "linux/arm64")
            checksum.write_text("d" * 64 + "  " + archive.name + "\n")
            with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                controller.bundle_metadata(archive, "linux/amd64")


if __name__ == "__main__":
    unittest.main()
