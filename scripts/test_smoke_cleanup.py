"""Regression guards for the live-test teardown, without Docker."""
import json
import os
from pathlib import Path
from subprocess import CompletedProcess
import tempfile
import unittest
from unittest.mock import patch

import test_installed_setup as smoke


class CleanupTests(unittest.TestCase):
    def test_only_interrupted_helper_download_is_retryable(self):
        truncated = "setup stage tools failed\nCaused by:\n curl failed (exit status: 18)"
        self.assertTrue(smoke.interrupted_helper_download(CompletedProcess([], 1, stderr=truncated)))
        for message in ["download checksum mismatch", "setup stage deployment failed: curl failed (exit status: 18)",
                        "setup stage tools failed: curl failed (exit status: 22)"]:
            self.assertFalse(smoke.interrupted_helper_download(CompletedProcess([], 1, stderr=message)))
        self.assertFalse(smoke.interrupted_helper_download(CompletedProcess([], 0, stderr=truncated)))

    def test_cleanup_uses_only_private_kubeconfig(self):
        with tempfile.TemporaryDirectory() as temporary:
            prefix = Path(temporary)
            home = prefix / "lib/proofstorm/state"
            home.mkdir(parents=True)
            identity = "a" * 32
            cluster = "pst-" + identity[:28]
            names = {"k3d-" + cluster + suffix: str(index) for index, suffix in enumerate(
                ["-server-0", "-agent-0", "-serverlb", "-registry"])}
            (home / "installation.json").write_text(json.dumps({"id":identity, "home":str(home)}))
            (home / "runtime-owner.json").write_text(json.dumps({"containers":names, "network_id":"network"}))
            current = prefix / "lib/proofstorm/current"
            current.mkdir()
            (current / "release-info.json").write_text(json.dumps({"bootstrap_tools":{"tools":[{"name":"k3d", "executable_sha256":"fixture"}]}}))
            def inspect(name, _env):
                return {"Id":names[name], "Config":{"Labels":{"proofstorm.dev/installation":identity}}}
            def run(args, env):
                if args[0] == "docker":
                    return CompletedProcess(args, 0, stdout="network\n")
                self.assertEqual(env["KUBECONFIG"], str(home / "kubeconfig"))
                self.assertNotIn("--all", args)
                return CompletedProcess(args, 0, stdout="")
            with patch.object(smoke.isolation, "inspect", side_effect=inspect), patch.object(smoke.isolation, "run", side_effect=run) as runner:
                smoke.cleanup(prefix, dict(os.environ, KUBECONFIG="/foreign/config"), set())
                self.assertEqual(runner.call_count, 3)
            with patch.object(smoke.isolation, "inspect", side_effect=inspect), patch.object(smoke.isolation, "run") as runner:
                with self.assertRaises(AssertionError):
                    smoke.cleanup(prefix, {}, {"0"})
                runner.assert_not_called()


if __name__ == "__main__":
    unittest.main()
