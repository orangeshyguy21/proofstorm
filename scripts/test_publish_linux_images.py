"""AMD64 publication boundaries and partial receipts; no Docker/network access."""
import copy
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

import publish_linux_images as publisher


class LinuxPublicationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.controller = {"platform": "linux/amd64", "release_ready": False,
                           "local_image_id": "sha256:" + "a" * 64,
                           "source": {"sha256": "d" * 64}, "metadata": {"source_sha256": "d" * 64},
                           "verification": {key: True for key in
                              ["offline_metadata", "non_root", "helper_startup", "host_contract_match"]}}
        self.wallets = {"local_amd64_wallet_builds": [
            {"tag": "proofstorm-linux-check/cdk-cli-wallet:test", "local_image_id": "sha256:" + "b" * 64},
            {"tag": "proofstorm-linux-check/cocod-wallet:test", "build_manifest_digest": "sha256:" + "c" * 64},
        ]}
        self.controller_path, self.wallet_path = self.root / "controller.json", self.root / "wallets.json"
        self.controller_path.write_text(json.dumps(self.controller))
        self.wallet_path.write_text(json.dumps(self.wallets))
        self.work = self.root / "publication"

    def publish(self):
        publisher.publish(self.controller_path, self.wallet_path, self.work, publisher.NAMESPACE)

    def docker_result(self, args, **kwargs):
        digest = "sha256:" + ("a" if "/proofstormd:" in " ".join(args) else
                              "b" if "/cdk-cli-wallet:" in " ".join(args) else "c") * 64
        return subprocess.CompletedProcess(args, 0, stdout=json.dumps({"digest": digest}))

    def test_plan_has_only_approved_repositories_and_new_tags(self):
        first = publisher.plan(self.controller, self.wallets)
        second = publisher.plan(self.controller, self.wallets)
        self.assertNotEqual(first["publication_id"], second["publication_id"])
        self.assertFalse(first["release_ready"])
        self.assertEqual([entry["repository"] for entry in first["images"]],
                         ["proofstormd", "cdk-cli-wallet", "cocod-wallet"])
        for entry in first["images"]:
            self.assertTrue(entry["tag"].startswith(publisher.NAMESPACE + "/"))
            self.assertIn(":development-amd64-", entry["tag"])
            self.assertFalse(entry["uploaded"])

    def test_unverified_or_wrong_platform_controller_refused(self):
        for change in [{"platform": "linux/arm64"}, {"release_ready": True}, {"verification": {}}]:
            with self.assertRaises(ValueError):
                publisher.plan({**self.controller, **change}, self.wallets)

    def test_missing_duplicate_or_mutable_wallet_identity_refused(self):
        variants = [{"local_amd64_wallet_builds": []}, copy.deepcopy(self.wallets), copy.deepcopy(self.wallets)]
        variants[1]["local_amd64_wallet_builds"].append(variants[1]["local_amd64_wallet_builds"][0])
        variants[2]["local_amd64_wallet_builds"][0]["local_image_id"] = "latest"
        for wallets in variants:
            with self.assertRaises(ValueError):
                publisher.plan(self.controller, wallets)

    def test_namespace_confirmation_precedes_filesystem_or_docker_mutation(self):
        with patch.object(publisher.subprocess, "run") as run:
            with self.assertRaisesRegex(ValueError, "namespace"):
                publisher.publish(self.controller_path, self.wallet_path, self.work, "ghcr.io/elsewhere")
            run.assert_not_called()
        self.assertFalse(self.work.exists())

    def test_preflight_failure_stops_all_pushes(self):
        with patch.object(publisher, "preflight", side_effect=ValueError("version mismatch")), \
             patch.object(publisher.subprocess, "run") as run:
            with self.assertRaisesRegex(ValueError, "version"):
                self.publish()
            run.assert_not_called()
        receipt = json.loads((self.work / "publication.json").read_text())
        self.assertFalse(any(entry["uploaded"] for entry in receipt["images"]))

    def test_all_uploads_use_verified_ids_and_require_anonymous_verification(self):
        with patch.object(publisher, "preflight"), \
             patch.object(publisher.subprocess, "run", side_effect=self.docker_result) as run, \
             patch.object(publisher.publish_images, "Registry") as registry:
            registry.return_value.inspect.return_value = {"linux/amd64"}
            self.publish()
        receipt = json.loads((self.work / "publication.json").read_text())
        self.assertFalse(receipt["release_ready"])
        for entry in receipt["images"]:
            self.assertTrue(entry["uploaded"] and entry["anonymous_verified"])
        tags = [call.args[0] for call in run.call_args_list if call.args[0][:2] == ["docker", "tag"]]
        self.assertEqual([command[2] for command in tags], ["sha256:" + char * 64 for char in "abc"])

    def test_anonymous_failure_records_upload_without_claiming_verification(self):
        with patch.object(publisher, "preflight"), \
             patch.object(publisher.subprocess, "run", side_effect=self.docker_result), \
             patch.object(publisher.publish_images, "Registry", side_effect=ValueError("not public")):
            with self.assertRaisesRegex(ValueError, "not public"):
                self.publish()
        receipt = json.loads((self.work / "publication.json").read_text())
        self.assertTrue(receipt["images"][0]["uploaded"])
        self.assertFalse(receipt["images"][0]["anonymous_verified"])
        self.assertFalse(receipt["images"][1]["uploaded"])

    def test_config_identity_and_manifest_identity_are_supported(self):
        registry = Mock()
        image_id = "sha256:" + "a" * 64
        registry.data.return_value = {"config": {"digest": image_id}}
        self.assertTrue(publisher.contains_identity(registry, "sha256:" + "b" * 64, image_id))
        self.assertFalse(publisher.contains_identity(registry, "sha256:" + "b" * 64, "sha256:" + "c" * 64))
        self.assertTrue(publisher.contains_identity(registry, image_id, image_id))

    def test_wallet_preflight_requires_nonroot_amd64_and_expected_version(self):
        value = publisher.plan(self.controller, self.wallets)
        for architecture, version in [("arm64", "cdk-cli 0.18.0"), ("amd64", "unexpected version")]:
            image = {"Id": "sha256:" + "b" * 64, "Os": "linux", "Architecture": architecture,
                     "Config": {"User": "1000:1000"}}
            with patch.object(publisher.controller_release, "verify_local", return_value={
                "local_image_id": self.controller["local_image_id"], "verification": {"offline_metadata": True},
            }), patch.object(publisher.subprocess, "run", side_effect=[
                subprocess.CompletedProcess([], 0, stdout=json.dumps([image])),
                subprocess.CompletedProcess([], 0, stdout=version),
            ]):
                with self.assertRaises(ValueError):
                    publisher.preflight(value)


if __name__ == "__main__":
    unittest.main()
