#!/usr/bin/env python3
"""Validate a downloaded Linux release artifact using owned, disposable dependencies."""

import argparse
import json
from pathlib import Path
import re
import sys
import time

from artifact_test_runtime import IsolatedRuntime, VerificationError, digest, install_signals, write_receipt
from release_artifact_manifest import ArtifactManifestError, verify_manifest


def verify_artifact_record(record, manifest, artifact_id):
    expected_name = "tsz-rust-x86_64-linux-gnu-" + manifest["source"]["git_sha"] + "-attempt-" + str(manifest["ci"]["run_attempt"])
    if (record.get("id") != artifact_id or record.get("expired") is not False
            or record.get("name") != expected_name
            or record.get("workflow_run", {}).get("id") != manifest["ci"]["run_id"]
            or record.get("workflow_run", {}).get("head_sha") != manifest["source"]["git_sha"]
            or record.get("workflow_run", {}).get("head_branch") != "main"):
        raise VerificationError("artifact_metadata_mismatch")


def validate_identity(args):
    if any(isinstance(value, bool) or value <= 0 for value in (args.artifact_id, args.run_id, args.run_attempt)):
        raise VerificationError("positive_ci_identity_required")
    if not all(re.fullmatch(r"[0-9a-f]{40}", value) for value in (args.expected_sha, args.expected_tree)):
        raise VerificationError("exact_source_identity_required")
    if args.output.exists():
        raise VerificationError("receipt_already_exists")
    manifest = verify_manifest(
        args.manifest, args.artifact,
        expected_git_sha=args.expected_sha, expected_git_tree=args.expected_tree,
        expected_run_id=args.run_id, expected_run_attempt=args.run_attempt,
    )
    verify_artifact_record(json.loads(args.artifact_metadata.read_text()), manifest, args.artifact_id)
    args.artifact.chmod(0o700)
    return manifest


def verify_ledger(info, ledger):
    if info.get("schema_version") != 1 or not isinstance(info.get("migrations"), list):
        raise VerificationError("invalid_embedded_migration_info")
    expected = {row["version"]: row["checksum"] for row in info["migrations"] if row["direction"] == "up"}
    actual = {row["version"]: row["checksum"] for row in ledger if row["success"]}
    if not expected or len(expected) != len(ledger) or any(not row["success"] for row in ledger) or expected != actual:
        raise VerificationError("embedded_migration_ledger_mismatch")


def verify(args, runtime_factory=IsolatedRuntime):
    manifest = validate_identity(args)
    runtime = runtime_factory()
    receipt = {
        "schema_version": 1, "kind": "release-artifact-smoke", "status": "failed",
        "source": manifest["source"], "ci": manifest["ci"], "artifact_id": args.artifact_id,
        "artifact_sha256": manifest["artifact"]["sha256"], "started_at": time.time(),
        "checks": {}, "cleanup": {"complete": False}, "resource_owner": runtime.owner if hasattr(runtime, "owner") else None,
    }
    error = None
    try:
        receipt["environment"] = runtime.start()
        if int(receipt["environment"]["postgresql"].split(".")[0]) != 18 or not receipt["environment"]["redis"].startswith("6.0."):
            raise VerificationError("dependency_versions_differ_from_deployment")
        info = json.loads(runtime.binary(args.artifact, "deploy-info"))
        runtime.start_app(args.artifact)
        verify_ledger(info, runtime.ledger())
        receipt["checks"]["embedded_migrations"] = "passed"
        runtime.seed_user()
        runtime.client().roundtrip()
        receipt["checks"].update(startup="passed", health="passed", readiness="passed", auth="passed", persistence="passed")
        runtime.docker("stop_test_redis", "stop", runtime.redis_id)
        unavailable = runtime.client().request("GET", "/readyz", expected=503)
        if unavailable.get("status") != "unavailable":
            raise VerificationError("redis_failure_was_not_rejected")
        receipt["checks"]["redis_unavailable_rejected"] = "passed"
        runtime.stop_app()
        runtime.docker("restart_test_redis", "start", runtime.redis_id)
        runtime.sql("UPDATE _sqlx_migrations SET checksum=decode(repeat('00',48),'hex') WHERE version=(SELECT min(version) FROM _sqlx_migrations)")
        try:
            runtime.start_app(args.artifact)
        except VerificationError as failure:
            if str(failure) != "application_exited":
                raise
            receipt["checks"]["migration_failure_rejected"] = "passed"
        else:
            raise VerificationError("migration_failure_was_not_rejected")
        if digest(args.artifact) != manifest["artifact"]["sha256"]:
            raise VerificationError("artifact_changed_during_verification")
        receipt["status"] = "passed"
    except (VerificationError, ArtifactManifestError, OSError, ValueError, KeyError, StopIteration) as failure:
        error = failure
        receipt["failed_stage"] = str(failure) if isinstance(failure, VerificationError) else type(failure).__name__
    finally:
        try:
            runtime.close()
        except VerificationError as failure:
            error = failure
            receipt["status"] = "failed"
            receipt["failed_stage"] = "cleanup_failed"
        receipt["cleanup"] = runtime.cleanup
        if not runtime.cleanup["complete"]:
            receipt["status"] = "failed"
        receipt["finished_at"] = time.time()
        write_receipt(args.output, receipt)
    if error is not None or receipt["status"] != "passed":
        raise VerificationError(receipt.get("failed_stage", "verification_failed"))
    return receipt


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--manifest", type=Path, required=True)
    result.add_argument("--artifact", type=Path, required=True)
    result.add_argument("--artifact-id", type=int, required=True)
    result.add_argument("--artifact-metadata", type=Path, required=True)
    result.add_argument("--expected-sha", required=True)
    result.add_argument("--expected-tree", required=True)
    result.add_argument("--run-id", type=int, required=True)
    result.add_argument("--run-attempt", type=int, required=True)
    result.add_argument("--output", type=Path, required=True)
    return result


def main(argv=None):
    install_signals()
    args = parser().parse_args(argv)
    try:
        result = verify(args)
    except (VerificationError, ArtifactManifestError, OSError, ValueError) as error:
        print(json.dumps({"status": "failed", "stage": str(error) if isinstance(error, VerificationError) else type(error).__name__}))
        return 1
    print(json.dumps({"status": result["status"], "artifact_id": result["artifact_id"], "cleanup_complete": result["cleanup"]["complete"]}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
