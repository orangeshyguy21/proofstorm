"""Controller publication guards, without Docker or network access."""
import json
from pathlib import Path
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
        with patch.object(controller.subprocess, "run", side_effect=[CompletedProcess([], 0),
            CompletedProcess([], 0, stdout=json.dumps({"digest": "sha256:" + "c" * 64}))]):
            controller.publish(self.path, controller.NAMESPACE)
        result = json.loads(self.path.read_text())
        self.assertFalse(result["release_ready"])
        self.assertEqual(result["image"], controller.NAMESPACE + "/proofstormd@sha256:" + "c" * 64)


if __name__ == "__main__":
    unittest.main()
