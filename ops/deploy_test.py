#!/usr/bin/env python3
"""Explicit tshb-test release transaction. No workflow or automatic deployment trigger."""

import argparse
import contextlib
import datetime
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import tempfile
import uuid

from artifact_test_runtime import VerificationError, digest
from deployment_manifest import ManifestError, validate_manifest
from release_artifact_manifest import verify_manifest
from release_artifact_smoke import verify_artifact_record


class DeploymentError(Exception):
    pass


STEPS = ("backup", "upload", "stop", "install", "candidate_start", "candidate_verify",
         "formal_start", "formal_verify", "business_verify", "accept", "unlock")
TERMINAL = {"accepted", "recovered", "blocked"}
TOOL_FILES = ("deploy_test.py", "deployment_transport.py", "deployment_host.py", "deployment_business_smoke.py",
              "deployment_manifest.py", "deployment_auth_smoke.py", "deployed_baseline.py",
              "release_artifact_manifest.py", "release_artifact_smoke.py", "migration_compatibility.py",
              "artifact_test_runtime.py", "ci_fingerprint.py")


def tools_identity():
    return {name: digest(Path(__file__).parent / name) for name in TOOL_FILES}


def atomic_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
    name = None
    try:
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent,
                                         prefix="." + path.name, delete=False) as target:
            name = Path(target.name)
            os.chmod(name, 0o600)
            json.dump(value, target, indent=2, sort_keys=True)
            target.write("\n")
            target.flush()
            os.fsync(target.fileno())
        os.replace(name, path)
        name = None
        descriptor = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    finally:
        if name is not None:
            name.unlink(missing_ok=True)


def identity_hash(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def check_plan(plan):
    if plan.get("schema_version") != 1 or plan.get("environment") != "tshb-test":
        raise DeploymentError("invalid_plan_environment")
    try:
        if str(uuid.UUID(plan["deployment_id"])) != plan["deployment_id"]:
            raise ValueError()
    except (ValueError, KeyError, TypeError):
        raise DeploymentError("invalid_deployment_id") from None
    if plan.get("tools") != tools_identity():
        raise DeploymentError("deployment_tools_changed")
    manifest = verify_manifest(Path(plan["manifest_path"]), Path(plan["artifact_path"]),
                               expected_git_sha=plan["source"]["git_sha"],
                               expected_git_tree=plan["source"]["git_tree"],
                               expected_run_id=plan["ci"]["run_id"],
                               expected_run_attempt=plan["ci"]["run_attempt"])
    if plan["source"] != manifest["source"] or any(plan["ci"].get(key) != value for key, value in manifest["ci"].items()):
        raise DeploymentError("plan_differs_from_artifact_identity")
    metadata = json.loads(Path(plan["artifact_metadata_path"]).read_text())
    verify_artifact_record(metadata, manifest, plan["ci"]["artifact_id"])
    validate_manifest(plan["baseline"]["deployment_manifest"])
    baseline = plan["baseline"]
    if baseline["deployment_manifest"]["source"]["git_sha"] == plan["source"]["git_sha"]:
        raise DeploymentError("already_deployed_verify_existing")
    old_digest = baseline["deployment_manifest"]["artifact"]["sha256"]
    if (baseline["service"]["file_sha256"] != old_digest or baseline["service"]["process_sha256"] != old_digest
            or baseline.get("pid_stable") is not True or baseline.get("deployment_lock_present") is not False):
        raise DeploymentError("baseline_not_stable")
    receipt_bytes = Path(plan["verification_receipt_path"]).read_bytes()
    receipt = json.loads(receipt_bytes)
    if (receipt.get("status") != "passed" or receipt.get("cleanup", {}).get("complete") is not True
            or receipt.get("source") != manifest["source"] or receipt.get("ci") != manifest["ci"]
            or receipt.get("artifact_id") != plan["ci"]["artifact_id"]
            or receipt.get("artifact_sha256") != manifest["artifact"]["sha256"]
            or receipt.get("verification_attempt") != plan["ci"]["verification_attempt"]
            or hashlib.sha256(receipt_bytes).hexdigest() != plan["ci"]["verification_receipt_sha256"]):
        raise DeploymentError("release_verification_mismatch")
    required = {"startup", "health", "readiness", "embedded_migrations", "auth", "persistence",
                "redis_unavailable_rejected", "migration_failure_rejected"}
    if any(receipt.get("checks", {}).get(key) != "passed" for key in required):
        raise DeploymentError("incomplete_release_verification")
    if plan["ci"]["verification_attempt"] != plan["ci"]["latest_attempt"]:
        raise DeploymentError("verification_is_not_latest_attempt")
    if plan["artifact_sha256"] != manifest["artifact"]["sha256"]:
        raise DeploymentError("candidate_digest_mismatch")
    ledger = baseline["postgresql"]["migration_ledger"]
    if baseline["postgresql"].get("transaction_read_only") != "on":
        raise DeploymentError("baseline_capture_not_read_only")
    if not ledger or any(row.get("success") is not True for row in ledger):
        raise DeploymentError("invalid_baseline_ledger")
    # Nonempty migration intervals require a separately approved real-data recovery policy.
    # Stage B synthetic success alone must never set rollback_schema_safe.
    if plan["migration"]["baseline_version"] != max(row["version"] for row in ledger):
        raise DeploymentError("baseline_version_mismatch")
    if plan["migration"]["candidate_version"] < plan["migration"]["baseline_version"]:
        raise DeploymentError("candidate_schema_older_than_baseline")
    if plan["migration"]["candidate_version"] != plan["migration"]["baseline_version"]:
        evidence = json.loads(Path(plan["migration_receipt_path"]).read_text())
        policy = plan.get("recovery_policy") or {}
        verified_down = (evidence.get("status") == "passed"
                         and evidence.get("synthetic_recovery_verified") is True
                         and evidence.get("checks", {}).get("old_binary_after_recovery") == "passed"
                         and evidence.get("checks", {}).get("bounded_down") == "passed")
        manual_refusal = (evidence.get("status") == "blocked"
                          and evidence.get("failed_stage") == "recovery_refused_requires_manual_plan"
                          and evidence.get("checks", {}).get("recovery_refusal_atomic") == "passed")
        if (evidence.get("cleanup", {}).get("complete") is not True
                or evidence.get("candidate_source") != manifest["source"] or evidence.get("candidate_ci") != manifest["ci"]
                or evidence.get("candidate_artifact_id") != plan["ci"]["artifact_id"]
                or evidence.get("candidate_artifact_sha256") != plan["artifact_sha256"]
                or evidence.get("baseline_ledger") != ledger or evidence.get("interval") != plan["migration"]
                or evidence.get("baseline_source", {}).get("git_sha") != baseline["deployment_manifest"]["source"]["git_sha"]
                or evidence.get("baseline_source", {}).get("git_tree") != baseline["deployment_manifest"]["source"]["git_tree"]
                or evidence.get("baseline_artifact_sha256") != old_digest
                or evidence.get("checks", {}).get("upgrade_and_persistence") != "passed"
                or not (verified_down or (policy.get("kind") == "manual" and manual_refusal))):
            raise DeploymentError("migration_rehearsal_mismatch")
        if (policy.get("kind") not in {"verified_down", "manual"}
                or policy.get("baseline_sha") != baseline["deployment_manifest"]["source"]["git_sha"]
                or policy.get("candidate_sha") != manifest["source"]["git_sha"]
                or policy.get("baseline_version") != plan["migration"]["baseline_version"]
                or policy.get("candidate_version") != plan["migration"]["candidate_version"]
                or not re.fullmatch(r"[0-9a-f]{64}", policy.get("approval_evidence_sha256", ""))):
            raise DeploymentError("migration_recovery_policy_required")
    return manifest


def build_plan(bundle, gate, host, *, recovery_policy=None):
    """Bundle contains downloaded files only. Gate and host perform fresh read-only queries."""
    folder = Path(bundle).resolve()
    manifest_path = folder / "tsz-rust.manifest.json"
    manifest = json.loads(manifest_path.read_text())
    metadata = json.loads((folder / "artifact.json").read_text())
    receipt_path = folder / "release-smoke-receipt.json"
    receipt_bytes = receipt_path.read_bytes()
    receipt = json.loads(receipt_bytes)
    baseline = host.baseline()
    info = json.loads((folder / "deploy-info.json").read_text())
    from migration_compatibility import migration_interval
    interval = migration_interval(info, baseline["postgresql"]["migration_ledger"])
    plan = {
        "schema_version": 1, "environment": "tshb-test", "deployment_id": str(uuid.uuid4()),
        "tools": tools_identity(),
        "source": manifest["source"], "artifact_sha256": manifest["artifact"]["sha256"],
        "artifact_path": str(folder / "tsz-rust"), "manifest_path": str(manifest_path),
        "artifact_metadata_path": str(folder / "artifact.json"),
        "verification_receipt_path": str(receipt_path),
        "ci": {**manifest["ci"], "artifact_id": metadata["id"],
               "verification_attempt": receipt["verification_attempt"],
               "latest_attempt": receipt["verification_attempt"],
               "verification_receipt_sha256": hashlib.sha256(receipt_bytes).hexdigest()},
        "baseline": baseline, "migration": interval,
        "migration_receipt_path": str(folder / "migration-receipt.json"),
        "recovery_policy": recovery_policy,
    }
    check_plan(plan)
    gate.check(plan)
    host.preflight(plan)
    return plan


class Controller:
    def __init__(self, directory, gate, host):
        self.directory = Path(directory).resolve()
        self.gate = gate
        self.host = host

    def path(self, deployment_id):
        if str(uuid.UUID(deployment_id)) != deployment_id:
            raise DeploymentError("invalid_deployment_id")
        return self.directory / deployment_id / "state.json"

    def save(self, state, event):
        state["events"].append({"at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                                "event": event, "phase": state["phase"]})
        atomic_json(self.path(state["deployment_id"]), state)

    @contextlib.contextmanager
    def writer(self, deployment_id):
        self.path(deployment_id).parent.mkdir(parents=True, mode=0o700, exist_ok=True)
        with (self.path(deployment_id).parent / "writer.lock").open("a") as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise DeploymentError("local_writer_busy") from None
            yield

    def active(self, deployment_id, acquire):
        path = self.directory / "active"
        if acquire:
            try:
                path.mkdir(mode=0o700)
            except FileExistsError:
                raise DeploymentError("local_deployment_busy") from None
            atomic_json(path / "owner.json", {"owner": deployment_id})
        else:
            released = self.path(deployment_id).parent / "released-active"
            if not path.exists() and released.exists():
                if json.loads((released / "owner.json").read_text()) == {"owner": deployment_id}:
                    return
            if json.loads((path / "owner.json").read_text()) != {"owner": deployment_id}:
                raise DeploymentError("local_owner_mismatch")
            path.rename(released)

    def apply(self, plan):
        check_plan(plan)
        deployment_id = plan["deployment_id"]
        with self.writer(deployment_id):
            if self.path(deployment_id).exists():
                raise DeploymentError("deployment_exists_use_resume")
            self.active(deployment_id, True)
            state = {"schema_version": 1, "deployment_id": deployment_id, "plan": plan,
                     "plan_sha256": identity_hash(plan), "phase": "preflight", "status": "running",
                     "pending": None, "completed": [], "mutation_started": False, "events": []}
            self.save(state, "created")
            return self.forward(state)

    def validate_state(self, state):
        if identity_hash(state["plan"]) != state["plan_sha256"] or state["deployment_id"] != state["plan"]["deployment_id"]:
            raise DeploymentError("state_plan_mismatch")
        if state["plan"].get("tools") != tools_identity():
            raise DeploymentError("recovery_tools_changed")
        if state["phase"] not in {*STEPS, "preflight", "locked", "recovering", "recovered", "accepted", "manual_required", "blocked"}:
            raise DeploymentError("unknown_state_phase")

    def forward(self, state):
        plan = state["plan"]
        try:
            self.gate.check(plan)
            self.host.preflight(plan)
            state["pending"] = "acquire"
            self.save(state, "intent")
            self.host.acquire(plan)
            state.update(phase="locked", pending=None)
            self.save(state, "locked")
            for step in STEPS:
                if step == "stop":
                    # Last moving-main/CI check. After this point the transaction stays pinned.
                    self.gate.check(plan)
                    self.host.recheck_baseline(plan)
                    state["mutation_started"] = True
                state["pending"] = step
                self.save(state, "intent")
                self.host.action(step, plan)
                state["completed"].append(step)
                state.update(phase=step, pending=None)
                self.save(state, "done")
            state.update(phase="accepted", status="accepted")
            self.save(state, "accepted")
            self.active(state["deployment_id"], False)
        except (DeploymentError, ManifestError, VerificationError, OSError, ValueError, KeyError) as error:
            state["failed_stage"] = str(error) if isinstance(error, (DeploymentError, VerificationError)) else type(error).__name__
            # A transport error may mean a remote command is still running. Inspect/recover
            # acquires the host-side operation lock, never runs beside that command.
            self.save(state, "failure")
            return self.recover(state)
        return state

    def recover(self, state):
        plan = state["plan"]
        try:
            observed = self.host.observe(plan)
            if observed.get("owner") not in (None, state["deployment_id"]):
                raise DeploymentError("remote_owner_mismatch")
            if observed.get("recovered"):
                if not observed.get("recovery_matches"):
                    raise DeploymentError("recovered_deployment_identity_mismatch")
                self.host.finish_unlock(plan)
                state.update(status="recovered", phase="recovered", pending=None)
            elif observed.get("accepted"):
                if not observed.get("acceptance_matches"):
                    raise DeploymentError("accepted_deployment_identity_mismatch")
                self.host.finish_unlock(plan)
                state.update(status="accepted", phase="accepted", pending=None)
            elif observed.get("mutation_started") is False:
                self.host.abort_unmutated(plan)
                state.update(status="blocked", phase="blocked", pending=None)
            else:
                state.update(phase="recovering", status="recovering")
                self.save(state, "recovery_intent")
                self.host.recover(plan)
                state.update(status="recovered", phase="recovered", pending=None)
            self.save(state, state["status"])
            self.active(state["deployment_id"], False)
        except (DeploymentError, OSError, ValueError, KeyError) as error:
            state.update(status="manual_required", phase="manual_required")
            state["recovery_error"] = str(error) if isinstance(error, (DeploymentError, VerificationError)) else type(error).__name__
            self.save(state, "manual_required")
        return state

    def resume(self, deployment_id):
        with self.writer(deployment_id):
            state = json.loads(self.path(deployment_id).read_text())
            self.validate_state(state)
            if state["status"] in TERMINAL:
                if (self.directory / "active").exists():
                    observed = self.host.observe(state["plan"])
                    if state["status"] == "accepted" and not observed.get("acceptance_matches"):
                        raise DeploymentError("accepted_deployment_identity_mismatch")
                    if state["status"] == "recovered" and not observed.get("recovery_matches"):
                        raise DeploymentError("recovered_deployment_identity_mismatch")
                    self.active(deployment_id, False)
                return state
            owner = json.loads((self.directory / "active" / "owner.json").read_text())
            if owner != {"owner": deployment_id}:
                raise DeploymentError("local_owner_mismatch")
            # Recovery is deliberately monotonic: never re-run an uncertain forward step.
            return self.recover(state)

    def status(self, deployment_id):
        state = json.loads(self.path(deployment_id).read_text())
        self.validate_state(state)
        return state


def main():
    import signal
    def interrupted(signum, frame):
        raise DeploymentError("interrupted")
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    plan = commands.add_parser("plan")
    plan.add_argument("--bundle", type=Path, required=True)
    plan.add_argument("--output", type=Path, required=True)
    plan.add_argument("--recovery-policy", type=Path)
    apply = commands.add_parser("apply")
    apply.add_argument("--plan", type=Path, required=True)
    for name in ("status", "resume"):
        command = commands.add_parser(name)
        command.add_argument("--deployment-id", required=True)
    parser.add_argument("--state-dir", type=Path, default=Path.home() / ".config/tsz-rust/controller")
    args = parser.parse_args()
    try:
        from deployment_transport import GithubGate, SshHost
        controller = Controller(args.state_dir, GithubGate(), SshHost())
        if args.command == "plan":
            if args.output.exists():
                raise DeploymentError("plan_already_exists")
            result = build_plan(args.bundle, controller.gate, controller.host,
                                recovery_policy=json.loads(args.recovery_policy.read_text()) if args.recovery_policy else None)
            atomic_json(args.output, result)
            print(json.dumps({"status": "planned", "deployment_id": result["deployment_id"], "remote_written": False}))
        elif args.command == "status":
            result = controller.status(args.deployment_id)
            print(json.dumps({key: result[key] for key in ("deployment_id", "phase", "status", "pending")}))
        else:
            # Smoke creates and cleans task-owned synthetic users, never shared credentials.
            result = controller.apply(json.loads(args.plan.read_text())) if args.command == "apply" else controller.resume(args.deployment_id)
            print(json.dumps({key: result[key] for key in ("deployment_id", "phase", "status", "pending")}))
            return 0 if result["status"] == "accepted" else 1
    except (DeploymentError, ManifestError, VerificationError, OSError, ValueError, KeyError) as error:
        print(json.dumps({"status": "blocked", "stage": str(error) if isinstance(error, (DeploymentError, VerificationError)) else type(error).__name__}))
        return 1
    return 0


if __name__ == "__main__":
    # The adapter imports DeploymentError from deploy_test. Keep the direct CLI and
    # imported controller on the same module so transport failures enter recovery.
    sys.modules["deploy_test"] = sys.modules[__name__]
    sys.exit(main())
