"""Packaging failure-path tests. No Docker, Rust builds, or network access."""
import copy
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import release


class PackagingTests(unittest.TestCase):
    def test_rust_validator_fixture_retains_existing_metadata_and_image_contracts(self):
        fixture = Path(__file__).resolve().parents[1] / "crates/proofstorm-xtask/tests/fixtures/release-info.json"
        info = json.loads(fixture.read_text())
        for target, platform in release.TARGETS.items():
            info["target"] = target
            info["bootstrap_tools"]["target"] = target
            info["controller"]["platform"] = platform
            release.validate_info(info)
            release.validate_alpha(info)
            images = release.image_inventory(info)
            self.assertEqual(images[0]["published_source"], "ghcr.io/orangeshyguy21/proofstorm/custom@sha256:" + "d" * 64)
            self.assertEqual(images[1]["published_source"], "docker.io/library/busybox@sha256:" + "e" * 64)
            self.assertEqual(images[2]["published_source"], images[2]["image"])
            self.assertTrue(all(not image["availability_verified"] and image["verified_platforms"] == [] for image in images))

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        # Optional migration parity run: use Rust for every existing verify call,
        # including failure cases, while keeping the legacy packager as producer.
        verifier = os.environ.get("PROOFSTORM_TEST_RELEASE_VERIFIER")
        if verifier:
            executable = str(Path(verifier).resolve(strict=True))

            def rust_verify(root):
                result = subprocess.run([executable, "release-verify", str(root), "--json"],
                                        capture_output=True, text=True, timeout=15, check=False)
                if result.returncode:
                    raise ValueError(result.stderr)
                self.assertTrue(json.loads(result.stdout)["integrity_verified"])
                return json.loads((root / "manifest.json").read_text())

            replacement = patch.object(release, "verify", side_effect=rust_verify)
            replacement.start()
            self.addCleanup(replacement.stop)
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
        self.info["version"] = "0.1.0"
        (self.source / "charts/proofstorm/Chart.yaml").write_text("version: 0.1.0\nappVersion: 0.1.0\n")
        with self.assertRaisesRegex(ValueError, "release blocked"):
            self.package(development=False)
        self.assertFalse(list((self.root / "output").glob("*.tar.gz")))

    def alpha_inputs(self):
        self.info["runtime_contract_sha256"] = "1" * 64
        self.info["controller"] = {
            "image": "ghcr.io/orangeshyguy21/proofstorm/proofstormd@sha256:" + "2" * 64,
            "platform": release.TARGETS[self.info["target"]], "release_ready": False,
            "metadata": {"version": self.info["version"], "runtime_contract_sha256": "1" * 64}}
        self.info["bootstrap_tools"] = {"target": self.info["target"], "tools": [{"name": "fixture"}]}
        self.info["image_publication"] = json.dumps({"namespace": "ghcr.io/orangeshyguy21/proofstorm"})

    def test_normal_alpha_has_download_name_and_retains_maturity_limitations(self):
        self.alpha_inputs()
        result = self.package(development=False)
        self.assertEqual(Path(result["archive"]).name, f'proofstorm-0.1.0-alpha.1-{self.info["target"]}.tar.gz')
        self.assertFalse(result["release_ready"])
        self.assertTrue(result["release_blockers"])
        with tarfile.open(result["archive"]) as archive:
            archive.extractall(self.root / "alpha", filter="data")
        root = self.root / "alpha/proofstorm"
        self.assertEqual(release.verify(root)["channel"], "alpha")
        path = root / "manifest.json"
        manifest = json.loads(path.read_text())
        manifest["controller"]["image"] = "wrong"
        release.write_json(path, manifest)
        with self.assertRaisesRegex(ValueError, "controller metadata"):
            release.verify(root)

    def test_alpha_still_requires_controller_tools_and_image_sources(self):
        self.alpha_inputs()
        for key, value in [("controller", None), ("bootstrap_tools", None), ("image_publication", "{}")]:
            original = self.info[key]
            self.info[key] = value
            with self.assertRaises(ValueError):
                self.package(output=key, development=False)
            self.info[key] = original

    def test_alpha_channel_is_selected_from_the_workspace_version(self):
        cargo = self.source / "Cargo.toml"
        for version, expected in [("0.1.0-alpha.1", True), ("0.1.0", False),
                                  ("0.1.0-alpha.", False), ("0.1.0-alpha.1-dev", False)]:
            cargo.write_text('[workspace.package]\nversion = "' + version + '"\n')
            self.assertEqual(release.alpha_source(self.source), expected)

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

    def test_snapshot_compiler_delegates_to_bash_with_provenance_and_profile(self):
        tools = self.source / "tools/versions.env"
        tools.write_text("TRUNK_VERSION=0.21.14\n")
        trunk = self.root / "trunk"
        trunk.write_text("fixture")
        work = self.root / "build"
        work.mkdir()
        target = work / "target"
        for debug in [False, True]:
            with patch.object(release, "run", return_value=json.dumps({"release_ready": False})) as runner:
                release.compile_snapshot(self.source, self.provenance, work=work, output=work / "out",
                                         target=target, trunk=trunk, development=True, debug=debug,
                                         expected_target=release.host_target())
            runner.assert_called_once()
            command = runner.call_args.args[0]
            self.assertEqual(command[:2], ["bash", Path(release.__file__).with_name("release-build.sh")])
            self.assertEqual(command[command.index("--source") + 1], self.source)
            self.assertEqual(command[command.index("--work-dir") + 1], work / "release-build")
            self.assertEqual(command[command.index("--target-dir") + 1], target)
            self.assertEqual(command[command.index("--trunk") + 1], trunk)
            self.assertEqual(command[command.index("--provenance") + 1], work / "package-source.json")
            self.assertEqual("--debug" in command, debug)
            self.assertIn("--development", command)
            self.assertIn("--json", command)
            self.assertEqual(json.loads((work / "package-source.json").read_text()), self.provenance)
            self.assertEqual(json.loads((work / "result.json").read_text()), {"release_ready": False})

    def test_normal_alpha_source_build_delegates_without_development_override(self):
        (self.source / "Cargo.toml").write_text('[workspace.package]\nversion = "0.1.0-alpha.1"\n')
        (self.source / "tools/versions.env").write_text("TRUNK_VERSION=0.21.14\n")
        trunk = self.root / "trunk"
        trunk.write_text("fixture")
        work = self.root / "build"
        work.mkdir()
        target = work / "target"

        with patch.object(release, "run", return_value=json.dumps({"release_ready": False})) as runner:
            release.compile_snapshot(self.source, self.provenance, work=work, output=work / "out",
                                     target=target, trunk=trunk, development=False, debug=True,
                                     expected_target=release.host_target())
        runner.assert_called_once()
        command = runner.call_args.args[0]
        self.assertNotIn("--development", command)
        self.assertIn("--debug", command)
        self.assertIn("--json", command)

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

    def test_smoke_delegates_to_rust_with_literal_paths_and_isolated_target(self):
        archive = self.root / "quoted 'archive'.tar.gz"
        destination = self.root / "relocated directory"
        with patch.object(release, "run") as runner:
            release.smoke(archive, destination, [])
        command = runner.call_args.args[0]
        self.assertEqual(command[:3], ["cargo", "run", "--locked"])
        self.assertEqual(command[-4:], ["release-smoke", archive, destination, "--json"])
        cache = Path(runner.call_args.kwargs["env"]["CARGO_TARGET_DIR"])
        self.assertEqual(cache.name, "target")
        self.assertFalse(cache.parent.exists(), "temporary helper cache must be cleaned")

    def test_smoke_forwards_denied_sources_and_propagates_helper_failure(self):
        from subprocess import CalledProcessError
        with patch.object(release.platform, "system", return_value="Darwin"), \
             patch.object(release, "run", side_effect=CalledProcessError(1, "cargo")) as runner:
            with self.assertRaises(CalledProcessError):
                release.smoke(self.root / "archive", self.root / "relocated", [self.source])
        self.assertEqual(runner.call_args.args[0][-2:], ["--deny-source", self.source])


if __name__ == "__main__":
    unittest.main()
