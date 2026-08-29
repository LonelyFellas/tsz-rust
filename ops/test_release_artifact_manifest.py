import copy
import json
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))

from release_artifact_manifest import (  # noqa: E402
    ArtifactManifestError,
    create_manifest,
    main,
    validate_manifest,
    verify_manifest,
)


SHA = "a" * 40
TREE = "b" * 40
FINGERPRINT = {
    "schema_version": 1,
    "runner": {"os": "Linux", "arch": "X64"},
    "rust": {
        "release": "1.90.0",
        "host": "x86_64-unknown-linux-gnu",
        "commit_hash": "c" * 40,
        "cargo_version": "cargo 1.90.0 (840b83a10 2025-07-30)",
    },
    "build": {"profile": "release", "features": "default", "sqlx_offline": "true"},
    "inputs": {
        "cargo_lock_sha256": "d" * 64,
        "build_config_sha256": "e" * 64,
        "sqlx_sha256": "f" * 64,
    },
    "cache_key": (
        "v3-Linux-X64-rustc-1.90.0-"
        + "c" * 40
        + "-host-x86_64-unknown-linux-gnu-profile-release-features-default-"
        + "sqlx-true-lock-"
        + "d" * 64
        + "-config-"
        + "e" * 64
        + "-sqlxmeta-"
        + "f" * 64
    ),
}


class ReleaseArtifactManifestTests(unittest.TestCase):
    def _create(self, root: pathlib.Path) -> tuple[pathlib.Path, pathlib.Path, dict]:
        artifact = root / "tsz-rust"
        output = root / "tsz-rust.manifest.json"
        artifact.write_bytes(b"release-binary")
        manifest = create_manifest(
            artifact=artifact,
            output=output,
            repository="LonelyFellas/tsz-rust",
            git_ref="refs/heads/main",
            git_sha=SHA,
            git_tree=TREE,
            run_id=123456789,
            run_attempt=2,
            run_url="https://github.com/LonelyFellas/tsz-rust/actions/runs/123456789",
            fingerprint=FINGERPRINT,
        )
        return artifact, output, manifest

    def test_art01_create_and_verify_binds_all_provenance(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            artifact, output, manifest = self._create(pathlib.Path(directory))
            verified = verify_manifest(
                output,
                artifact,
                expected_git_sha=SHA,
                expected_git_tree=TREE,
                expected_run_id=123456789,
                expected_run_attempt=2,
            )

            self.assertEqual(verified, manifest)
            self.assertEqual(manifest["source"]["git_tree"], TREE)
            self.assertEqual(manifest["build"]["fingerprint"], FINGERPRINT)
            self.assertEqual(manifest["artifact"]["size_bytes"], len(b"release-binary"))

    def test_art02_binary_tampering_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            artifact, output, _ = self._create(pathlib.Path(directory))
            artifact.write_bytes(b"tampered")
            with self.assertRaises(ArtifactManifestError):
                verify_manifest(output, artifact)

    def test_art03_schema_is_strict(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            _, _, manifest = self._create(pathlib.Path(directory))
            invalid = []
            extra = copy.deepcopy(manifest)
            extra["secret"] = "should-never-exist"
            invalid.append(extra)
            wrong_ref = copy.deepcopy(manifest)
            wrong_ref["source"]["git_ref"] = "refs/pull/1/merge"
            invalid.append(wrong_ref)
            missing_tree = copy.deepcopy(manifest)
            del missing_tree["source"]["git_tree"]
            invalid.append(missing_tree)
            wrong_runner = copy.deepcopy(manifest)
            wrong_runner["build"]["fingerprint"]["runner"] = {
                "os": "macOS",
                "arch": "ARM64",
            }
            invalid.append(wrong_runner)
            wrong_target = copy.deepcopy(manifest)
            wrong_target["build"]["fingerprint"]["rust"]["host"] = (
                "aarch64-unknown-linux-gnu"
            )
            wrong_target["artifact"]["target"] = "aarch64-unknown-linux-gnu"
            invalid.append(wrong_target)

            for value in invalid:
                with self.subTest(value=value):
                    with self.assertRaises(ArtifactManifestError):
                        validate_manifest(value)

    def test_art04_wrong_run_attempt_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            artifact, output, _ = self._create(pathlib.Path(directory))
            with self.assertRaises(ArtifactManifestError):
                verify_manifest(output, artifact, expected_run_attempt=1)

    def test_art05_cli_create_and_verify_round_trip(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            artifact = root / "tsz-rust"
            manifest = root / "tsz-rust.manifest.json"
            fingerprint = root / "fingerprint.json"
            artifact.write_bytes(b"release-binary")
            fingerprint.write_text(json.dumps(FINGERPRINT), encoding="utf-8")
            self.assertEqual(
                main(
                    [
                        "create",
                        "--artifact",
                        str(artifact),
                        "--output",
                        str(manifest),
                        "--repository",
                        "LonelyFellas/tsz-rust",
                        "--git-ref",
                        "refs/heads/main",
                        "--git-sha",
                        SHA,
                        "--git-tree",
                        TREE,
                        "--run-id",
                        "123456789",
                        "--run-attempt",
                        "2",
                        "--run-url",
                        "https://github.com/LonelyFellas/tsz-rust/actions/runs/123456789",
                        "--fingerprint",
                        str(fingerprint),
                    ]
                ),
                0,
            )
            self.assertEqual(
                main(
                    [
                        "verify",
                        "--manifest",
                        str(manifest),
                        "--artifact",
                        str(artifact),
                        "--expected-git-sha",
                        SHA,
                        "--expected-git-tree",
                        TREE,
                        "--expected-run-id",
                        "123456789",
                        "--expected-run-attempt",
                        "2",
                    ]
                ),
                0,
            )


if __name__ == "__main__":
    unittest.main()
