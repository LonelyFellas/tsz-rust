import copy
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent))

from artifact_test_runtime import IsolatedRuntime, VerificationError
from release_artifact_manifest import ArtifactManifestError, create_manifest
from release_artifact_smoke import verify, verify_ledger
from test_release_artifact_manifest import FINGERPRINT, SHA, TREE


INFO = {"schema_version": 1, "migrations": [
    {"version": 1, "direction": "up", "checksum": "a" * 96, "transactional": True},
    {"version": 1, "direction": "down", "checksum": "b" * 96, "transactional": True},
]}
LEDGER = [{"version": 1, "checksum": "a" * 96, "success": True}]


def fixture(root):
    artifact = root / "tsz-rust"
    artifact.write_bytes(b"synthetic test artifact")
    manifest = root / "manifest.json"
    create_manifest(artifact=artifact, output=manifest, repository="LonelyFellas/tsz-rust",
                    git_ref="refs/heads/main", git_sha=SHA, git_tree=TREE,
                    run_id=123456789, run_attempt=2,
                    run_url="https://github.com/LonelyFellas/tsz-rust/actions/runs/123456789",
                    fingerprint=FINGERPRINT)
    metadata = root / "artifact-metadata.json"
    metadata.write_text(json.dumps({"id": 101, "expired": False,
                                    "name": "tsz-rust-x86_64-linux-gnu-" + SHA + "-attempt-2",
                                    "workflow_run": {"id": 123456789, "head_sha": SHA, "head_branch": "main"}}))
    return SimpleNamespace(artifact=artifact, manifest=manifest, artifact_id=101, artifact_metadata=metadata,
                           expected_sha=SHA, expected_tree=TREE, run_id=123456789,
                           run_attempt=2, verification_attempt=2, output=root / "receipt.json")


class FakeRuntime:
    def __init__(self):
        self.cleanup = {"complete": False}
        self.redis_id = "owned-redis"
        self.app_starts = 0
        self.closed = False
        self.seeded = False
        self.queries = []

    def start(self):
        return {"postgresql": "18.3", "redis": "6.0.16", "network_internal": True}

    def binary(self, artifact, *arguments):
        return json.dumps(INFO)

    def start_app(self, artifact):
        self.app_starts += 1
        if self.app_starts == 2:
            raise VerificationError("application_exited")

    def ledger(self):
        return copy.deepcopy(LEDGER)

    def seed_user(self):
        self.seeded = True

    def client(self):
        return self

    def roundtrip(self):
        if not self.seeded:
            raise AssertionError("fixture user must be inserted")
        return {"id": "test"}

    def request(self, method, path, *, expected):
        if (method, path, expected) != ("GET", "/readyz", 503):
            raise AssertionError("real unavailability must be checked")
        return {"status": "unavailable"}

    def sql(self, statement):
        self.queries.append(statement)

    def stop_app(self):
        pass

    def docker(self, *arguments):
        return b""

    def close(self):
        self.closed = True
        self.cleanup = {"complete": True, "remaining": []}


class ReleaseSmokeTests(unittest.TestCase):
    def test_tampered_or_wrong_attempt_never_creates_dependencies(self):
        for mutation in ("binary", "attempt", "artifact_id", "source", "artifact_metadata"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as directory:
                args = fixture(Path(directory))
                if mutation == "binary":
                    args.artifact.write_bytes(b"tampered")
                elif mutation == "attempt":
                    args.run_attempt = 1
                elif mutation == "artifact_id":
                    args.artifact_id = 0
                elif mutation == "source":
                    args.expected_sha = "not-a-sha"
                else:
                    metadata = json.loads(args.artifact_metadata.read_text())
                    metadata["id"] = 102
                    args.artifact_metadata.write_text(json.dumps(metadata))
                with patch("release_artifact_smoke.IsolatedRuntime") as factory:
                    with self.assertRaises((ArtifactManifestError, VerificationError)):
                        verify(args, runtime_factory=factory)
                    factory.assert_not_called()
                self.assertFalse(args.output.exists())

    def test_success_receipt_binds_identity_and_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            args = fixture(Path(directory))
            runtime = FakeRuntime()
            result = verify(args, runtime_factory=lambda: runtime)
            self.assertEqual(result["status"], "passed")
            self.assertEqual(result["artifact_id"], 101)
            self.assertEqual(result["ci"]["run_attempt"], 2)
            self.assertTrue(result["cleanup"]["complete"])
            self.assertEqual(result["checks"]["migration_failure_rejected"], "passed")
            self.assertEqual(result["checks"]["redis_unavailable_rejected"], "passed")
            self.assertTrue(runtime.closed)
            self.assertIn("UPDATE _sqlx_migrations", runtime.queries[0])

    def test_failed_job_rerun_keeps_successful_producer_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            args = fixture(Path(directory))
            args.verification_attempt = 3
            result = verify(args, runtime_factory=FakeRuntime)
            self.assertEqual(result["status"], "passed")
            self.assertEqual(result["artifact_id"], 101)
            self.assertEqual(result["ci"]["run_attempt"], 2)
            self.assertEqual(result["verification_attempt"], 3)
            metadata = json.loads(args.artifact_metadata.read_text())
            self.assertTrue(metadata["name"].endswith("attempt-2"))

    def test_verification_cannot_precede_artifact_production(self):
        with tempfile.TemporaryDirectory() as directory:
            args = fixture(Path(directory))
            args.verification_attempt = 1
            with patch("release_artifact_smoke.IsolatedRuntime") as factory:
                with self.assertRaisesRegex(VerificationError, "verification_precedes"):
                    verify(args, runtime_factory=factory)
                factory.assert_not_called()

    def test_startup_or_persistence_failure_still_cleans_and_never_passes(self):
        for stage in ("application_exited", "wordlist_persistence_failed", "interrupted"):
            with self.subTest(stage=stage), tempfile.TemporaryDirectory() as directory:
                args = fixture(Path(directory))
                runtime = FakeRuntime()
                method = "start_app" if stage == "application_exited" else "roundtrip"
                with patch.object(runtime, method, side_effect=VerificationError(stage)):
                    with self.assertRaises(VerificationError):
                        verify(args, runtime_factory=lambda: runtime)
                self.assertTrue(runtime.closed)
                self.assertEqual(json.loads(args.output.read_text())["status"], "failed")

    def test_cleanup_failure_changes_success_to_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            args = fixture(Path(directory))
            runtime = FakeRuntime()
            with patch.object(runtime, "close", side_effect=VerificationError("cleanup_failed")):
                with self.assertRaises(VerificationError):
                    verify(args, runtime_factory=lambda: runtime)
            result = json.loads(args.output.read_text())
            self.assertEqual(result["status"], "failed")
            self.assertEqual(result["failed_stage"], "cleanup_failed")

    def test_ledger_rejects_checksum_missing_extra_or_failed_rows(self):
        invalid = [[], [{**LEDGER[0], "checksum": "c" * 96}], [{**LEDGER[0], "success": False}], LEDGER + [{"version": 2, "checksum": "d" * 96, "success": True}]]
        verify_ledger(INFO, LEDGER)
        for ledger in invalid:
            with self.subTest(ledger=ledger), self.assertRaises(VerificationError):
                verify_ledger(INFO, ledger)

    def test_owner_mismatch_never_removes_an_existing_resource(self):
        runtime = IsolatedRuntime()
        runtime.resources = ["not-owned"]
        with patch.object(runtime, "inspect", return_value={"Config": {"Labels": {"tsz.artifact-test": "someone-else"}}}), patch.object(runtime, "docker") as docker:
            with self.assertRaises(VerificationError):
                runtime.close()
            docker.assert_not_called()
        self.assertFalse(runtime.cleanup["complete"])
        self.assertEqual(runtime.cleanup["remaining"], ["not-owned"])

    def test_internal_http_preserves_request_and_duplicate_cookie_headers(self):
        import base64
        import urllib.request
        runtime = IsolatedRuntime()
        runtime.http_id = "owned-probe"
        runtime.app_host = "172.18.0.5"
        request = urllib.request.Request("http://172.18.0.5:8383/readyz", method="GET")
        wire = {"status": 200, "headers": [["Set-Cookie", "a=1"], ["Set-Cookie", "b=2"]],
                "body": base64.b64encode(b'{"status":"ready"}').decode()}
        with patch.object(runtime, "docker", return_value=json.dumps(wire).encode()) as docker:
            response = runtime.http(request)
            self.assertEqual(response.status, 200)
            self.assertEqual(json.loads(response.read()), {"status": "ready"})
            self.assertEqual(response.headers.get_all("Set-Cookie"), ["a=1", "b=2"])
        payload = json.loads(docker.call_args.kwargs["data"])
        self.assertEqual(payload["method"], "GET")
        self.assertEqual(payload["path"], "/readyz")

    def test_invalid_ready_payload_cannot_be_accepted_as_started(self):
        from unittest.mock import Mock
        runtime = IsolatedRuntime()
        runtime.images["app"] = "cached-image"
        client = Mock()
        client.request.return_value = {"status": "wrong"}
        state = {"State": {"Running": True}, "HostConfig": {"PortBindings": {}},
                 "NetworkSettings": {"Networks": {"owned": {"IPAddress": "172.18.0.5"}}}}
        with patch.object(runtime, "create", return_value="owned-app"), patch.object(runtime, "docker"), patch.object(runtime, "inspect", return_value=state), patch.object(runtime, "client", return_value=client), patch("artifact_test_runtime.time.sleep"):
            with self.assertRaisesRegex(VerificationError, "application_readiness_timeout"):
                runtime.start_app(Path("/synthetic/artifact"))
        self.assertEqual(client.request.call_count, 120)

    def test_cleanup_audit_failure_does_not_claim_success(self):
        runtime = IsolatedRuntime()
        with patch.object(runtime, "docker", side_effect=VerificationError("daemon_unavailable")):
            with self.assertRaisesRegex(VerificationError, "cleanup_failed"):
                runtime.close()
        self.assertFalse(runtime.cleanup["complete"])
        self.assertTrue(runtime.cleanup["remaining"])

    def test_credentials_are_environment_inputs_not_docker_argv(self):
        runtime = IsolatedRuntime()
        with patch("artifact_test_runtime.execute", return_value=b"ok") as execute:
            runtime.docker("create", "create", "-e", "JWT_SECRET=synthetic-secret", "image")
        args = execute.call_args.args[1]
        self.assertNotIn("JWT_SECRET=synthetic-secret", args)
        self.assertIn("JWT_SECRET", args)
        self.assertEqual(execute.call_args.kwargs["environment"]["JWT_SECRET"], "synthetic-secret")


if __name__ == "__main__":
    unittest.main()
