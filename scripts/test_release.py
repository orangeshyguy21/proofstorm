"""Packaging failure-path tests. No Docker, Rust builds, or network access."""
import copy
import json
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import release


class PackagingTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "source"
        self.binaries = self.root / "binaries"
        self.binaries.mkdir()
        for name in ["proofstorm", "proofstorm-mcp"]:
            (self.binaries / name).write_bytes(b"test executable")
        for name in release.REQUIRED - {"bin/proofstorm", "bin/proofstorm-mcp", "catalog.json", "release-info.json"}:
            source_name = name.replace("chart/", "charts/proofstorm/", 1)
            path = self.source / source_name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("fixture\n")
        (self.source / "charts/proofstorm/Chart.yaml").write_text("version: 0.1.0-alpha.1\nappVersion: 0.1.0-alpha.1\n")
        self.provenance = {"revision": "a" * 40, "sha256": "b" * 64, "dirty": True}
        self.info = {"format_version": 1, "version": "0.1.0-alpha.1", "target": release.host_target(), "build_profile": "debug",
                     "source_revision": self.provenance["revision"], "source_sha256": self.provenance["sha256"],
                     "web_assets": [{"path": name, "sha256": "c" * 64, "size": 1}
                                    for name in ["index.html", "app.js", "app.wasm", "style.css"]],
                     "catalog": {"entries": []}, "tools": "fixture\n",
                     "workload_images": [release.LOCAL_REGISTRY + "custom@sha256:" + "d" * 64,
                                         release.LOCAL_REGISTRY + "upstream/docker.io/library/busybox@sha256:" + "e" * 64]}

    def package(self, output="output", development=True):
        with patch.object(release, "run", return_value=json.dumps(self.info)):
            return release.package(self.source, self.binaries, self.root / output,
                                   self.provenance, development)

    def unpack(self):
        result = self.package()
        with tarfile.open(result["archive"]) as archive:
            archive.extractall(self.root / "unpacked", filter="data")
        return self.root / "unpacked/proofstorm"

    def test_development_archive_is_deterministic_checked_and_honest(self):
        first = self.package("first")
        second = self.package("second")
        self.assertEqual(first["sha256"], second["sha256"])
        self.assertFalse(first["release_ready"])
        self.assertTrue(any("Missing published image" in item for item in first["release_blockers"]))
        root = self.unpack()
        manifest = release.verify(root)
        self.assertEqual(manifest["channel"], "development")
        self.assertEqual(release.digest(Path(first["archive"])), first["sha256"])
        self.assertTrue((Path(first["archive"] + ".sha256")).is_file())
        self.assertEqual(manifest["workload_images"][1]["published_source"],
                         "docker.io/library/busybox@sha256:" + "e" * 64)

    def test_release_mode_refuses_unpublished_and_unverified_runtime(self):
        self.provenance["dirty"] = False
        with self.assertRaisesRegex(ValueError, "release blocked"):
            self.package(development=False)
        self.assertFalse(list((self.root / "output").glob("*.tar.gz")))

    def test_missing_frontend_or_crd_fails_without_publishing(self):
        self.info["web_assets"] = []
        with self.assertRaisesRegex(ValueError, "index.html"):
            self.package()
        self.info["web_assets"] = [{"path": name, "sha256": "c" * 64, "size": 1}
                                   for name in ["index.html", "app.js", "app.wasm", "style.css"]]
        (self.source / "charts/proofstorm/crds/proofstorm.dev_proofstormlabs.yaml").unlink()
        with self.assertRaisesRegex(ValueError, "missing payload"):
            self.package()
        self.assertFalse(list((self.root / "output").glob("*.tar.gz")))

    def test_mismatched_binaries_or_source_fail_closed(self):
        other = copy.deepcopy(self.info)
        other["version"] = "different"
        with patch.object(release, "run", side_effect=[json.dumps(self.info), json.dumps(other)]):
            with self.assertRaisesRegex(ValueError, "same release inputs"):
                release.package(self.source, self.binaries, self.root / "output", self.provenance, True)
        self.provenance["sha256"] = "f" * 64
        with self.assertRaisesRegex(ValueError, "provenance mismatch"):
            self.package()

    def test_existing_output_is_not_replaced(self):
        result = self.package()
        before = Path(result["archive"]).read_bytes()
        with self.assertRaisesRegex(ValueError, "already exists"):
            self.package()
        self.assertEqual(before, Path(result["archive"]).read_bytes())

    def test_tampered_missing_and_unlisted_files_fail_verification(self):
        root = self.unpack()
        path = root / "LICENSE"
        original = path.read_bytes()
        path.write_bytes(b"tampered")
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            release.verify(root)
        path.unlink()
        with self.assertRaisesRegex(ValueError, "missing or unlisted"):
            release.verify(root)
        path.write_bytes(original)
        (root / "unexpected").write_text("extra")
        with self.assertRaisesRegex(ValueError, "missing or unlisted"):
            release.verify(root)

    def test_development_manifest_cannot_claim_release_readiness(self):
        root = self.unpack()
        path = root / "manifest.json"
        manifest = json.loads(path.read_text())
        manifest["release_ready"] = True
        release.write_json(path, manifest)
        with self.assertRaisesRegex(ValueError, "inconsistent release"):
            release.verify(root)
        manifest["release_blockers"] = []
        release.write_json(path, manifest)
        with self.assertRaisesRegex(ValueError, "lacks required evidence"):
            release.verify(root)

    def test_symlink_and_executable_mode_changes_fail_verification(self):
        root = self.unpack()
        executable = root / "bin/proofstorm"
        executable.chmod(0o644)
        with self.assertRaisesRegex(ValueError, "mode mismatch"):
            release.verify(root)
        executable.chmod(0o755)
        (root / "LICENSE").unlink()
        (root / "LICENSE").symlink_to(self.source / "LICENSE")
        with self.assertRaisesRegex(ValueError, "symlink"):
            release.verify(root)

    def test_unsupported_target_or_mutable_image_is_rejected(self):
        self.info["target"] = "x86_64-apple-darwin"
        with self.assertRaisesRegex(ValueError, "unsupported bundle target"):
            self.package()
        self.info["target"] = release.host_target()
        self.info["workload_images"] = ["example.org/app:latest"]
        with self.assertRaisesRegex(ValueError, "not pinned"):
            self.package()

    def test_confirmed_ghcr_mapping_preserves_digest_without_claiming_verification(self):
        self.info["image_publication"] = json.dumps({"namespace": "ghcr.io/orangeshyguy21/proofstorm"})
        inventory = release.image_inventory(self.info)
        self.assertEqual(inventory[0]["published_source"], "ghcr.io/orangeshyguy21/proofstorm/custom@sha256:" + "d" * 64)
        self.assertFalse(inventory[0]["availability_verified"])

    def test_both_targets_package_and_verify_with_platform_specific_blockers(self):
        for target in release.TARGETS:
            self.info["target"] = target
            result = self.package(target)
            self.assertTrue(result["archive"].endswith(target + ".tar.gz"))
            blockers = "\n".join(result["release_blockers"])
            self.assertIn(release.TARGETS[target], blockers)
            self.assertEqual("macOS signing" in blockers, target == "aarch64-apple-darwin")
            with tarfile.open(result["archive"]) as archive:
                archive.extractall(self.root / target, filter="data")
            self.assertEqual(release.verify(self.root / target / "proofstorm")["target"], target)

    def test_manifest_cannot_relabel_payload_architecture(self):
        root = self.unpack()
        path = root / "manifest.json"
        manifest = json.loads(path.read_text())
        manifest["target"] = next(target for target in release.TARGETS if target != self.info["target"])
        release.write_json(path, manifest)
        with self.assertRaisesRegex(ValueError, "target mismatch"):
            release.verify(root)

    def test_foreign_helper_pins_are_rejected(self):
        self.info["bootstrap_tools"] = {"target": "wrong-target", "tools": []}
        with self.assertRaisesRegex(ValueError, "tool target mismatch"):
            self.package()

    def test_host_detection_and_source_denial_fail_closed(self):
        for system, machine, target in [("Darwin", "arm64", "aarch64-apple-darwin"),
                                        ("Linux", "x86_64", "x86_64-unknown-linux-gnu")]:
            with patch.object(release.platform, "system", return_value=system), patch.object(release.platform, "machine", return_value=machine):
                self.assertEqual(release.host_target(), target)
        with patch.object(release.platform, "system", return_value="Linux"):
            with self.assertRaisesRegex(ValueError, "requires macOS"):
                release.smoke(self.root / "absent.tar.gz", self.root / "absent", [self.source])
        with patch.object(release.platform, "system", return_value="Windows"):
            with self.assertRaisesRegex(ValueError, "build on"):
                release.host_target()

    def test_stale_controller_contract_is_refused_even_for_development_bundles(self):
        self.info["runtime_contract_sha256"] = "1" * 64
        self.info["controller"] = {"platform": release.TARGETS[self.info["target"]], "release_ready": False,
                                   "metadata": {"version": self.info["version"], "runtime_contract_sha256": "2" * 64}}
        with self.assertRaisesRegex(ValueError, "runtime contract"):
            self.package()
        self.info["controller"]["metadata"]["runtime_contract_sha256"] = "1" * 64
        self.assertFalse(self.package("matching")["release_ready"])

    def test_snapshot_compiler_preserves_metadata_and_uses_matching_profile(self):
        tools = self.source / "tools/versions.env"
        tools.write_text("TRUNK_VERSION=0.21.14\n")
        trunk = self.root / "trunk"
        trunk.write_text("fixture")
        work = self.root / "build"
        work.mkdir()
        target = work / "target"
        for debug in [False, True]:
            def run(args, **kwargs):
                if args[-1] == "--version":
                    return "trunk 0.21.14\n"
                if args[-1] == "release-info":
                    return json.dumps({"target": release.host_target()})
                return ""
            with patch.object(release, "run", side_effect=run) as runner, \
                 patch.object(release, "package", return_value={"release_ready": False}) as package:
                release.compile_snapshot(self.source, self.provenance, work=work, output=work / "out",
                                         target=target, trunk=trunk, development=True, debug=debug,
                                         expected_target=release.host_target())
            calls = runner.call_args_list
            web = next(call for call in calls if call.args[0][0] == trunk and "build" in call.args[0])
            host = next(call for call in calls if call.args[0][:2] == ["cargo", "build"])
            self.assertEqual("--release" in host.args[0], not debug)
            self.assertEqual(web.kwargs["env"]["PROOFSTORM_BUILD_SOURCE_SHA256"], self.provenance["sha256"])
            self.assertEqual(host.kwargs["env"]["PROOFSTORM_BUILD_REVISION"], self.provenance["revision"])
            self.assertEqual(package.call_args.args[1], target / ("debug" if debug else "release"))

    def test_snapshot_is_explicit_and_excludes_unlisted_private_files(self):
        (self.source / "public.txt").write_text("public")
        (self.source / ".env").write_text("private")
        with patch.object(release, "run", side_effect=[" M public.txt", "a" * 40, "public.txt\0", "a" * 40]):
            provenance = release.snapshot(self.source, self.root / "snapshot", True)
        self.assertTrue(provenance["dirty"])
        self.assertFalse((self.root / "snapshot/.env").exists())
        self.assertEqual((self.root / "snapshot/public.txt").read_text(), "public")
        with patch.object(release, "run", return_value=" M public.txt"):
            with self.assertRaisesRegex(ValueError, "clean committed"):
                release.snapshot(self.source, self.root / "release-snapshot", False)

    def test_smoke_checks_archive_before_extracting_and_runs_relocated_binaries(self):
        result = self.package()
        archive = Path(result["archive"])
        with patch.object(release, "run", return_value=json.dumps(self.info)) as runner:
            release.smoke(archive, self.root / "smoke", [])
            self.assertEqual(runner.call_count, 6)
            self.assertTrue(all(str(self.root / "smoke/proofstorm/bin") in str(call.args[0][0])
                                for call in runner.call_args_list))
        archive.write_bytes(b"corrupt")
        with self.assertRaisesRegex(ValueError, "archive checksum"):
            release.smoke(archive, self.root / "corrupt-smoke", [])
        self.assertFalse((self.root / "corrupt-smoke").exists())

    def test_smoke_refuses_archive_traversal_even_with_matching_checksum(self):
        archive = self.root / "malicious.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            member = tarfile.TarInfo("proofstorm/../../escaped")
            member.size = 3
            tar.addfile(member, io.BytesIO(b"bad"))
        Path(str(archive) + ".sha256").write_text(release.digest(archive) + "  " + archive.name + "\n")
        with self.assertRaisesRegex(ValueError, "unsafe"):
            release.smoke(archive, self.root / "bad-smoke", [])
        self.assertFalse((self.root / "escaped").exists())


if __name__ == "__main__":
    unittest.main()
