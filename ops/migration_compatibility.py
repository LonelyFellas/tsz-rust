#!/usr/bin/env python3
"""Rehearse a captured deployed baseline and candidate using synthetic data only."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import sys
import time

from artifact_test_runtime import IsolatedRuntime, VerificationError, digest, install_signals, write_receipt
from release_artifact_manifest import ArtifactManifestError, verify_manifest
from release_artifact_smoke import verify_artifact_record, verify_ledger


def migration_interval(info, ledger):
    if info.get("schema_version") != 1 or not isinstance(info.get("migrations"), list):
        raise VerificationError("invalid_embedded_migration_info")
    up, down = {}, {}
    for row in info["migrations"]:
        if row.get("direction") not in ("up", "down"):
            raise VerificationError("invalid_migration_direction")
        target = up if row["direction"] == "up" else down
        version = row.get("version")
        checksum = row.get("checksum")
        if isinstance(version, bool) or not isinstance(version, int) or version <= 0 or version in target:
            raise VerificationError("invalid_or_duplicate_migration_version")
        if not isinstance(checksum, str) or len(checksum) != 96 or any(c not in "0123456789abcdef" for c in checksum):
            raise VerificationError("invalid_migration_checksum")
        target[version] = row
    versions = [row.get("version") for row in ledger]
    if not versions or len(set(versions)) != len(versions) or any(row.get("success") is not True for row in ledger):
        raise VerificationError("invalid_baseline_ledger")
    highest = max(versions)
    for row in ledger:
        if row["version"] not in up or up[row["version"]]["checksum"] != row.get("checksum"):
            raise VerificationError("baseline_migration_drift")
    if {v for v in up if v <= highest} != set(versions):
        raise VerificationError("baseline_migration_missing")
    interval = sorted(v for v in up if v > highest)
    if any(v not in down or not up[v]["transactional"] or not down[v]["transactional"] for v in interval):
        raise VerificationError("unsupported_recovery_interval")
    return {"classification": "no_schema_change" if not interval else "new_migrations",
            "baseline_version": highest, "candidate_version": max(up),
            "versions": interval, "migrations": [{"up": up[v], "down": down[v]} for v in interval]}


def baseline_identity(baseline, manifest):
    deployment = baseline["deployment_manifest"]
    if deployment["source"]["repository"] != "LonelyFellas/tsz-rust" or deployment["ci"]["conclusion"] != "success":
        raise VerificationError("baseline_not_accepted")
    for field in ("git_sha", "git_tree"):
        if deployment["source"][field] != manifest["source"][field]:
            raise VerificationError("baseline_source_mismatch")
    if deployment["ci"]["run_id"] != manifest["ci"]["run_id"]:
        raise VerificationError("baseline_ci_mismatch")
    expected = manifest["artifact"]["sha256"]
    if any(value != expected for value in [deployment["artifact"]["sha256"], baseline["service"]["file_sha256"], baseline["service"]["process_sha256"]]):
        raise VerificationError("baseline_process_digest_mismatch")
    if baseline["postgresql"]["status"] != "ok" or baseline["postgresql"]["transaction_read_only"] != "on":
        raise VerificationError("baseline_capture_not_read_only")
    if not baseline.get("pid_stable") or baseline.get("deployment_lock_present"):
        raise VerificationError("baseline_not_stable")
    return baseline["postgresql"]["migration_ledger"]


def same_ledger(expected, actual):
    def normalized(rows):
        return sorted((r["version"], r["checksum"], r["success"]) for r in rows)
    if normalized(expected) != normalized(actual):
        raise VerificationError("baseline_ledger_mismatch")


def rehearse(args, runtime_factory=IsolatedRuntime):
    if args.output.exists():
        raise VerificationError("receipt_already_exists")
    if any(isinstance(value, bool) or value <= 0 for value in (args.artifact_id, args.baseline_artifact_id, args.run_id, args.run_attempt)):
        raise VerificationError("positive_ci_identity_required")
    if not all(re.fullmatch(r"[0-9a-f]{40}", value) for value in (args.expected_sha, args.expected_tree)):
        raise VerificationError("exact_source_identity_required")
    baseline_bytes = args.baseline.read_bytes()
    baseline = json.loads(baseline_bytes)
    old = verify_manifest(args.baseline_manifest, args.baseline_artifact)
    candidate = verify_manifest(args.manifest, args.artifact, expected_git_sha=args.expected_sha,
                                expected_git_tree=args.expected_tree, expected_run_id=args.run_id,
                                expected_run_attempt=args.run_attempt)
    verify_artifact_record(json.loads(args.artifact_metadata.read_text()), candidate, args.artifact_id)
    verify_artifact_record(json.loads(args.baseline_artifact_metadata.read_text()), old, args.baseline_artifact_id)
    ledger = baseline_identity(baseline, old)
    for artifact in (args.artifact, args.baseline_artifact):
        artifact.chmod(0o700)
    runtime = runtime_factory()
    receipt = {
        "schema_version": 1, "kind": "migration-compatibility", "status": "failed", "started_at": time.time(),
        "baseline_capture_sha256": hashlib.sha256(baseline_bytes).hexdigest(),
        "baseline_source": old["source"], "baseline_ci": old["ci"],
        "baseline_artifact_id": args.baseline_artifact_id, "baseline_artifact_sha256": old["artifact"]["sha256"],
        "candidate_source": candidate["source"], "candidate_ci": candidate["ci"],
        "candidate_artifact_id": args.artifact_id, "candidate_artifact_sha256": candidate["artifact"]["sha256"],
        "baseline_ledger": ledger, "checks": {}, "rollback_schema_safe": False,
        "resource_owner": runtime.owner if hasattr(runtime, "owner") else None,
        "evidence_scope": "synthetic fixture only; not production data or PITR authorization",
    }
    error = None
    try:
        receipt["environment"] = runtime.start()
        actual_major = int(receipt["environment"]["postgresql"].split(".")[0])
        if actual_major != int(baseline["postgresql"]["server_version_num"]) // 10000:
            raise VerificationError("postgresql_major_differs_from_baseline")
        info = json.loads(runtime.binary(args.artifact, "deploy-info"))
        interval = migration_interval(info, ledger)
        receipt["interval"] = interval
        runtime.start_app(args.baseline_artifact)
        same_ledger(ledger, runtime.ledger())
        runtime.seed_user()
        wordlist = runtime.client().roundtrip()
        runtime.stop_app()
        baseline_snapshot = runtime.snapshot()
        baseline_columns = runtime.table_columns() if interval["versions"] else None
        fixture_before = runtime.sql("SELECT row_to_json(w) FROM wordlists w WHERE id='" + wordlist["id"] + "'")
        runtime.start_app(args.artifact)
        verify_ledger(info, runtime.ledger())
        fixture_after = runtime.sql("SELECT row_to_json(w) FROM wordlists w WHERE id='" + wordlist["id"] + "'")
        if fixture_before != fixture_after:
            raise VerificationError("existing_wordlist_changed_by_upgrade")
        runtime.client().roundtrip(wordlist=wordlist)
        runtime.stop_app()
        receipt["checks"]["upgrade_and_persistence"] = "passed"
        if interval["versions"]:
            before = runtime.snapshot()
            # Capture candidate writes through the schema that the old binary will read.
            # Full snapshots still protect new tables, refusal atomicity and sequences.
            before_recovery_data = runtime.snapshot(projection=baseline_columns)
            try:
                runtime.binary(args.artifact, "deploy-undo-migrations", str(interval["baseline_version"]), str(interval["candidate_version"]))
            except VerificationError as failure:
                if str(failure) != "binary_command_failed":
                    raise
                if before != runtime.snapshot():
                    raise VerificationError("rejected_recovery_changed_data")
                receipt["checks"]["recovery_refusal_atomic"] = "passed"
                raise VerificationError("recovery_refused_requires_manual_plan")
            same_ledger(ledger, runtime.ledger())
            restored = runtime.snapshot()
            if restored.get("schema_sha256") != baseline_snapshot.get("schema_sha256"):
                raise VerificationError("recovered_schema_differs_from_baseline")
            before_sequences = {(row["schemaname"], row["sequencename"]): row for row in before["sequences"]}
            restored_sequences = {(row["schemaname"], row["sequencename"]): row for row in restored["sequences"]}
            for name, sequence in restored_sequences.items():
                if before_sequences.get(name) != sequence:
                    raise VerificationError("recovery_changed_sequence_state")
            for table, summary in before.items():
                if table in ("schema_sha256", "sequences", "public._sqlx_migrations"):
                    continue
                if table not in restored:
                    if summary.get("rows", 0):
                        raise VerificationError("recovery_dropped_synthetic_data")
                elif restored[table] != before_recovery_data[table]:
                    raise VerificationError("recovery_changed_synthetic_data")
            receipt["checks"]["bounded_down"] = "passed"
        else:
            receipt["checks"]["bounded_down"] = "not_needed_no_schema_change"
        runtime.start_app(args.baseline_artifact)
        same_ledger(ledger, runtime.ledger())
        runtime.client().roundtrip(wordlist=wordlist)
        runtime.stop_app()
        receipt["checks"]["old_binary_after_recovery"] = "passed"
        for artifact, expected in [(args.artifact, candidate["artifact"]["sha256"]), (args.baseline_artifact, old["artifact"]["sha256"])]:
            if digest(artifact) != expected:
                raise VerificationError("artifact_changed_during_rehearsal")
        receipt["status"] = "passed"
        receipt["synthetic_recovery_verified"] = True
        receipt["baseline_revalidation_required_before_server_write"] = True
    except (VerificationError, OSError, ValueError, KeyError, StopIteration) as failure:
        error = failure
        receipt["failed_stage"] = str(failure) if isinstance(failure, VerificationError) else type(failure).__name__
        if receipt["failed_stage"] in {"recovery_refused_requires_manual_plan", "unsupported_recovery_interval"}:
            receipt["status"] = "blocked"
    finally:
        try:
            runtime.close()
        except VerificationError as failure:
            error = failure
            receipt.update(status="failed", failed_stage="cleanup_failed")
        receipt["cleanup"] = runtime.cleanup
        if not runtime.cleanup["complete"]:
            receipt["status"] = "failed"
        receipt["finished_at"] = time.time()
        write_receipt(args.output, receipt)
    if error is not None or receipt["status"] != "passed":
        raise VerificationError(receipt.get("failed_stage", "rehearsal_failed"))
    return receipt


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    for name in ("baseline", "baseline-manifest", "baseline-artifact", "baseline-artifact-metadata", "manifest", "artifact", "artifact-metadata", "output"):
        result.add_argument("--" + name, type=Path, required=True)
    for name in ("artifact-id", "baseline-artifact-id", "run-id", "run-attempt"):
        result.add_argument("--" + name, type=int, required=True)
    result.add_argument("--expected-sha", required=True)
    result.add_argument("--expected-tree", required=True)
    return result


def main(argv=None):
    install_signals()
    args = parser().parse_args(argv)
    try:
        receipt = rehearse(args)
    except (VerificationError, ArtifactManifestError, OSError, ValueError, KeyError) as error:
        print(json.dumps({"status": "failed", "stage": str(error) if isinstance(error, VerificationError) else type(error).__name__}))
        return 1
    print(json.dumps({"status": receipt["status"], "synthetic_recovery_verified": True, "cleanup_complete": receipt["cleanup"]["complete"]}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
