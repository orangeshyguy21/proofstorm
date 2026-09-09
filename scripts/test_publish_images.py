import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import publish_images as publish


class PublicationTests(unittest.TestCase):
    def setUp(self):
        self.image = publish.CANONICAL + "bitcoin-core@sha256:" + "a" * 64
        self.plan = publish.plan({"workload_images": [self.image]})

    def test_only_custom_images_are_copied_and_digest_is_preserved(self):
        plan = publish.plan({"workload_images": [self.image, publish.CANONICAL + "upstream/docker.io/library/busybox@sha256:" + "b" * 64]})
        self.assertEqual(len(plan["images"]), 1)
        self.assertEqual(plan["images"][0]["destination"], publish.NAMESPACE + "/bitcoin-core@sha256:" + "a" * 64)
        publish.validate_plan(plan)

    def test_foreign_sources_destinations_and_namespace_are_refused(self):
        for key, value in [("source", "foreign/image@sha256:" + "a" * 64), ("destination", "ghcr.io/foreign/image@sha256:" + "a" * 64)]:
            plan = copy.deepcopy(self.plan)
            plan["images"][0][key] = value
            with self.assertRaisesRegex(ValueError, "altered"):
                publish.validate_plan(plan)
        with self.assertRaisesRegex(ValueError, "exact confirmed namespace"):
            publish.publish(self.plan, "ghcr.io/foreign", Path("unused"))

    def test_missing_arm64_stops_before_publishing(self):
        with patch.object(publish.Registry, "inspect", return_value={"linux/amd64"}), patch.object(publish.subprocess, "run") as run:
            with self.assertRaisesRegex(ValueError, "arm64"):
                publish.publish(self.plan, publish.NAMESPACE, Path("unused"))
            run.assert_not_called()

    def test_publish_uses_unique_staging_tag_and_checks_returned_digest(self):
        with tempfile.TemporaryDirectory() as directory:
            result = type("Result", (), {"stdout": json.dumps({"digest": "sha256:" + "a" * 64})})()
            with patch.object(publish.Registry, "inspect", return_value={"linux/arm64"}), patch.object(publish, "verify", return_value={"needs_public_visibility": []}), patch.object(publish.subprocess, "run", return_value=result) as run:
                publish.publish(self.plan, publish.NAMESPACE, Path(directory) / "receipt.json")
                command = run.call_args_list[0].args[0]
                self.assertIn("--prefer-index=false", command)
                self.assertIn(publish.NAMESPACE + "/bitcoin-core:upload-" + self.plan["publication_id"], command)
                self.assertNotIn("latest", " ".join(command))

    def test_anonymous_failure_does_not_claim_public_availability(self):
        with patch.object(publish, "Registry", side_effect=ValueError("private")):
            result = publish.verify(self.plan)
        self.assertEqual(result["anonymous_verified"], [])
        self.assertEqual(len(result["needs_public_visibility"]), 1)


if __name__ == "__main__":
    unittest.main()
