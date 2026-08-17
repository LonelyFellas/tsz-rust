import copy
import json
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))

from deployment_manifest import (  # noqa: E402
    ManifestError,
    create_manifest,
    restore_deployment,
    validate_backup,
    validate_manifest,
    verify_manifest,
)


SHA = "a" * 40
TREE = "b" * 40


def create_args(root: pathlib.Path) -> dict:
    return {
        "component": "api",
        "repository": "LonelyFellas/tsz-rust",
        "git_sha": SHA,
        "git_tree": TREE,
        "ci_run_id": 123456789,
        "ci_run_url": "https://github.com/LonelyFellas/tsz-rust/actions/runs/123456789",
        "artifact": root / "tsz-rust",
        "artifact_path": "/opt/tsz-rust/target/release/tsz-rust",
        "output": root / "api.json",
    }


class DeploymentManifestTests(unittest.TestCase):
    def test_p01_create_and_verify_api_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            args = create_args(root)
            args["artifact"].write_bytes(b"release-binary")

            manifest = create_manifest(**args)
            verified = verify_manifest(args["output"], args["artifact"])

            self.assertEqual(manifest, verified)
            self.assertEqual(manifest["component"], "api")
            self.assertEqual(manifest["artifact"]["file_count"], 1)
            self.assertEqual(len(manifest["artifact"]["sha256"]), 64)

    def test_p02_binary_tampering_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            args = create_args(root)
            args["artifact"].write_bytes(b"release-binary")
            create_manifest(**args)

            args["artifact"].write_bytes(b"tampered")
            with self.assertRaisesRegex(ManifestError, "digest"):
                verify_manifest(args["output"], args["artifact"])

    def test_p03_schema_and_inputs_are_strict(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            args = create_args(root)
            args["artifact"].write_bytes(b"release-binary")
            valid = create_manifest(**args)

            invalid_manifests = []
            extra = copy.deepcopy(valid)
            extra["extra"] = True
            invalid_manifests.append(extra)
            bad_component = copy.deepcopy(valid)
            bad_component["component"] = "worker"
            invalid_manifests.append(bad_component)
            bad_sha = copy.deepcopy(valid)
            bad_sha["source"]["git_sha"] = "short"
            invalid_manifests.append(bad_sha)
            bad_url = copy.deepcopy(valid)
            bad_url["ci"]["run_url"] = "https://example.com/1"
            invalid_manifests.append(bad_url)
            bad_run_id = copy.deepcopy(valid)
            bad_run_id["ci"]["run_id"] = 9_999_999_999_999_999
            invalid_manifests.append(bad_run_id)
            boolean_schema = copy.deepcopy(valid)
            boolean_schema["schema_version"] = True
            invalid_manifests.append(boolean_schema)
            boolean_file_count = copy.deepcopy(valid)
            boolean_file_count["artifact"]["file_count"] = True
            invalid_manifests.append(boolean_file_count)
            excluded_api_path = copy.deepcopy(valid)
            excluded_api_path["artifact"]["excluded_paths"] = ["cache"]
            invalid_manifests.append(excluded_api_path)
            missing = copy.deepcopy(valid)
            del missing["accepted_at"]
            invalid_manifests.append(missing)

            for manifest in invalid_manifests:
                with self.subTest(manifest=manifest):
                    with self.assertRaises(ManifestError):
                        validate_manifest(manifest)

            invalid_args = create_args(root)
            invalid_args["git_sha"] = "short"
            with self.assertRaises(ManifestError):
                create_manifest(**invalid_args)

    def test_p04_failed_create_does_not_replace_existing_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            args = create_args(root)
            args["artifact"].write_bytes(b"release-binary")
            create_manifest(**args)
            original = args["output"].read_bytes()

            invalid = create_args(root)
            invalid["ci_run_url"] = "https://example.com/1"
            with self.assertRaises(ManifestError):
                create_manifest(**invalid)

            self.assertEqual(args["output"].read_bytes(), original)
            self.assertEqual(json.loads(original)["component"], "api")

    def test_p05_restore_atomically_replaces_binary_and_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            live = root / "live"
            backup = root / "backup"
            live.mkdir()
            backup.mkdir()
            live_artifact = live / "tsz-rust"
            live_manifest = live / "api.json"
            live_artifact.write_bytes(b"new-binary")
            live_manifest.write_text("stale", encoding="utf-8")

            backup_args = create_args(backup)
            backup_args["artifact"].write_bytes(b"old-binary")
            backup_args["artifact"].chmod(0o755)
            create_manifest(**backup_args)

            restored = restore_deployment(
                backup_dir=backup,
                artifact=live_artifact,
                manifest_path=live_manifest,
            )

            self.assertIsNotNone(restored)
            self.assertEqual(live_artifact.read_bytes(), b"old-binary")
            self.assertTrue(live_artifact.stat().st_mode & 0o100)
            verify_manifest(live_manifest, live_artifact)

    def test_p06_invalid_restore_preserves_the_current_deployment(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            live_artifact = root / "tsz-rust"
            live_manifest = root / "api.json"
            live_artifact.write_bytes(b"current-binary")
            live_manifest.write_bytes(b"current-manifest")
            backup = root / "backup"
            backup.mkdir()
            (backup / "tsz-rust").write_bytes(b"backup-binary")
            (backup / "manifest.absent").touch()
            (backup / "api.json").write_text("{}", encoding="utf-8")

            with self.assertRaises(ManifestError):
                restore_deployment(
                    backup_dir=backup,
                    artifact=live_artifact,
                    manifest_path=live_manifest,
                )

            self.assertEqual(live_artifact.read_bytes(), b"current-binary")
            self.assertEqual(live_manifest.read_bytes(), b"current-manifest")

    def test_p07_restore_preserves_an_absent_manifest_state(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            live_artifact = root / "live-tsz-rust"
            live_manifest = root / "api.json"
            live_artifact.write_bytes(b"current-binary")
            live_manifest.write_bytes(b"stale-manifest")
            backup = root / "backup"
            backup.mkdir()
            (backup / "tsz-rust").write_bytes(b"old-binary")
            (backup / "manifest.absent").touch()

            restored = restore_deployment(
                backup_dir=backup,
                artifact=live_artifact,
                manifest_path=live_manifest,
            )

            self.assertIsNone(restored)
            self.assertEqual(live_artifact.read_bytes(), b"old-binary")
            self.assertFalse(live_manifest.exists())

    def test_p08_backup_validation_is_read_only_and_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            backup = pathlib.Path(directory)
            artifact = backup / "tsz-rust"
            artifact.write_bytes(b"backup-binary")
            args = create_args(backup)
            create_manifest(**args)
            before = {path.name: path.read_bytes() for path in backup.iterdir()}

            _, manifest = validate_backup(backup)
            self.assertIsNotNone(manifest)
            self.assertEqual(
                {path.name: path.read_bytes() for path in backup.iterdir()}, before
            )

            artifact.write_bytes(b"tampered-backup")
            with self.assertRaisesRegex(ManifestError, "digest"):
                validate_backup(backup)


if __name__ == "__main__":
    unittest.main()
