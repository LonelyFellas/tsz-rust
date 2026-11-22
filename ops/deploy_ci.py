#!/usr/bin/env python3
"""Publish successful main CI artifacts through the existing tshb-test controller."""

import argparse
import json
import os
from pathlib import Path, PurePosixPath
import re
import signal
import stat
import subprocess
import sys
import time
from types import SimpleNamespace
import urllib.error
import urllib.request
import zipfile

from artifact_test_runtime import IsolatedRuntime, NoRedirect, VerificationError, digest
from deploy_test import Controller, DeploymentError, atomic_json, build_plan, measured_phase
from deployment_manifest import validate_manifest as validate_deployment
from deployment_transport import GithubGate, REPOSITORY, SshHost
from migration_compatibility import migration_interval, rehearse
from release_artifact_manifest import ArtifactManifestError, verify_manifest
from release_artifact_smoke import verify_artifact_record

ROOT = Path(__file__).resolve().parent.parent
SHA = re.compile(r"[0-9a-f]{40}")
MAX_ARTIFACT_BYTES = 512 * 1024 * 1024


def require(value, reason):
    if not value:
        raise DeploymentError(reason)


def git(*arguments, optional=False, raw=False):
    result = subprocess.run(["git", "-C", str(ROOT), *arguments], capture_output=True,
                            text=True, timeout=60, check=False)
    if optional and result.returncode in {1, 128}:
        return None
    require(result.returncode == 0, "git_source_unavailable")
    return result.stdout if raw else result.stdout.strip()


def publication_input(path):
    value = PurePosixPath(path)
    if value.is_absolute() or ".." in value.parts:
        return True
    if path in {"AGENTS.md", "README.md"}:
        return False
    if path.startswith(("tests/", ".agents/", ".claude/")):
        return False
    if path.startswith("docs/") and value.suffix.lower() in {".md", ".txt", ".png", ".jpg", ".jpeg", ".svg", ".webp", ".pdf"}:
        return False
    if path.startswith("ops/test_") and value.suffix == ".py":
        return False
    return True


def inputs_changed(baseline_sha, candidate_sha):
    require(SHA.fullmatch(baseline_sha or "") and SHA.fullmatch(candidate_sha or ""), "invalid_source_sha")
    if git("cat-file", "-e", baseline_sha + "^{commit}", optional=True) is None:
        return True
    if git("merge-base", "--is-ancestor", baseline_sha, candidate_sha, optional=True) is None:
        return True
    paths = git("diff", "--name-only", "--no-renames", "-z", baseline_sha, candidate_sha, raw=True)
    return any(publication_input(path) for path in paths.split("\0") if path)


def extract_artifact(archive, destination, expected):
    destination = Path(destination)
    destination.mkdir(mode=0o700, parents=True, exist_ok=False)
    with zipfile.ZipFile(archive) as bundle:
        entries = bundle.infolist()
        require(len(entries) == len(expected) and {item.filename for item in entries} == set(expected),
                "unexpected_artifact_files")
        require(sum(item.file_size for item in entries) <= MAX_ARTIFACT_BYTES, "artifact_too_large")
        for item in entries:
            require(not item.is_dir() and not stat.S_ISLNK(item.external_attr >> 16), "invalid_artifact_file")
            require(item.filename in expected and len(PurePosixPath(item.filename).parts) == 1,
                    "invalid_artifact_path")
            with bundle.open(item) as source, (destination / item.filename).open("xb") as output:
                while block := source.read(1024 * 1024):
                    output.write(block)
            (destination / item.filename).chmod(0o600)


class Github(GithubGate):
    def candidate(self, sha, run_id, attempt):
        require(isinstance(sha, str) and SHA.fullmatch(sha), "invalid_source_sha")
        require(type(run_id) is int and run_id > 0 and type(attempt) is int and attempt > 0,
                "invalid_ci_identity")
        run = self.api(f"actions/runs/{run_id}/attempts/{attempt}")
        require(run.get("id") == run_id and run.get("run_attempt") == attempt
                and run.get("name") == "CI" and run.get("path") == ".github/workflows/ci.yml"
                and run.get("head_sha") == sha and run.get("head_branch") == "main"
                and run.get("event") in {"push", "workflow_dispatch"}
                and run.get("status") == "completed" and run.get("conclusion") == "success"
                and (run.get("repository") or {}).get("full_name") == REPOSITORY
                and (run.get("head_repository") or {}).get("full_name") == REPOSITORY,
                "candidate_ci_not_successful_main")
        if self.api("branches/main")["commit"]["sha"] != sha:
            return {"status": "superseded", "reason": "main_advanced", "candidate_sha": sha,
                    "ci_run_id": run_id, "attempt": attempt, "server_writes": 0}
        runs = self.api("actions/runs?branch=main&head_sha=" + sha + "&per_page=100")["workflow_runs"]
        runs = [item for item in runs if item.get("workflow_id") == run["workflow_id"]]
        require(bool(runs), "exact_ci_missing")
        latest = max(runs, key=lambda item: (item["created_at"], item["id"]))
        if (latest["id"], latest["run_attempt"]) != (run_id, attempt):
            return {"status": "superseded", "reason": "ci_superseded", "candidate_sha": sha,
                    "ci_run_id": run_id, "attempt": attempt, "server_writes": 0}
        require(git("branch", "--show-current") == "main" and git("rev-parse", "HEAD") == sha
                and not git("status", "--porcelain"), "clean_exact_main_checkout_required")
        return {"status": "current", "candidate_sha": sha, "ci_run_id": run_id, "attempt": attempt,
                "git_tree": git("rev-parse", "HEAD^{tree}")}

    def artifacts(self, run_id):
        result = []
        for page in range(1, 101):
            rows = self.api(f"actions/runs/{run_id}/artifacts?per_page=100&page={page}")["artifacts"]
            result.extend(rows)
            if len(rows) < 100:
                return result
        raise DeploymentError("artifact_inventory_too_large")

    def download(self, record, destination, expected):
        require(type(record.get("id")) is int and record["id"] > 0 and record.get("expired") is False,
                "artifact_missing_or_expired")
        archive = Path(str(destination) + ".zip")
        require(not archive.exists() and not Path(destination).exists(), "artifact_destination_exists")
        with measured_phase("github_artifact_download", []):
            with archive.open("xb") as output:
                result = subprocess.run(["gh", "api", f"repos/{REPOSITORY}/actions/artifacts/{record['id']}/zip"],
                                        stdout=output, stderr=subprocess.PIPE, timeout=300, check=False)
            require(result.returncode == 0, "artifact_download_failed")
        require(archive.stat().st_size <= MAX_ARTIFACT_BYTES, "artifact_too_large")
        if record.get("digest"):
            require(record["digest"] == "sha256:" + digest(archive), "artifact_archive_digest_mismatch")
        extract_artifact(archive, destination, expected)
        archive.unlink()

    def artifact_pull(self, plan):
        record = json.loads(Path(plan["artifact_metadata_path"]).read_text())
        artifact_id = plan["ci"]["artifact_id"]
        require(record.get("id") == artifact_id and record.get("expired") is False,
                "artifact_missing_or_expired")
        archive_bytes = record.get("size_in_bytes")
        archive_digest = record.get("digest")
        require(type(archive_bytes) is int and 0 < archive_bytes <= MAX_ARTIFACT_BYTES
                and isinstance(archive_digest, str) and re.fullmatch(r"sha256:[0-9a-f]{64}", archive_digest),
                "artifact_pull_metadata_invalid")
        token = os.environ.get("GH_TOKEN")
        require(token, "artifact_pull_token_missing")
        request = urllib.request.Request(
            f"https://api.github.com/repos/{REPOSITORY}/actions/artifacts/{artifact_id}/zip",
            headers={"Authorization": f"Bearer {token}", "Accept": "application/vnd.github+json"},
        )
        try:
            with urllib.request.build_opener(NoRedirect()).open(request, timeout=10):
                raise DeploymentError("artifact_pull_url_unavailable")
        except urllib.error.HTTPError as error:
            with error:
                if error.code in {429, 500, 502, 503, 504}:
                    raise DeploymentError("artifact_pull_failed") from None
                url = error.headers.get("Location") if error.code == 302 else None
        except OSError:
            raise DeploymentError("artifact_pull_failed") from None
        require(url, "artifact_pull_url_unavailable")
        return {"url": url, "archive_bytes": archive_bytes, "archive_sha256": archive_digest[7:],
                "binary_bytes": Path(plan["artifact_path"]).stat().st_size}


def one_artifact(rows, name):
    matches = [row for row in rows if row.get("name") == name and row.get("expired") is False]
    require(len(matches) == 1, "unique_artifact_required")
    return matches[0]


def prepare_bundle(repo, candidate, directory):
    root = Path(directory)
    require(not root.exists(), "bundle_directory_exists")
    root.mkdir(mode=0o700, parents=True)
    sha, run_id, attempt = (candidate[key] for key in ("candidate_sha", "ci_run_id", "attempt"))
    receipt_record = one_artifact(repo.artifacts(run_id), f"release-smoke-{sha}-attempt-{attempt}")
    receipt_folder = root / "verification"
    repo.download(receipt_record, receipt_folder, {"release-smoke-receipt.json"})
    receipt = json.loads((receipt_folder / "release-smoke-receipt.json").read_bytes())
    require(receipt.get("status") == "passed" and receipt.get("cleanup", {}).get("complete") is True
            and receipt.get("verification_attempt") == attempt and receipt.get("source", {}).get("git_sha") == sha
            and receipt.get("ci", {}).get("run_id") == run_id, "release_verification_mismatch")
    metadata = repo.api("actions/artifacts/" + str(receipt["artifact_id"]))
    bundle = root / "bundle"
    repo.download(metadata, bundle, {"tsz-rust", "tsz-rust.sha256", "tsz-rust.manifest.json"})
    manifest = verify_manifest(bundle / "tsz-rust.manifest.json", bundle / "tsz-rust",
                               expected_git_sha=sha, expected_git_tree=candidate["git_tree"], expected_run_id=run_id)
    verify_artifact_record(metadata, manifest, receipt["artifact_id"])
    require(manifest["source"] == receipt["source"] and manifest["ci"] == receipt["ci"]
            and manifest["artifact"]["sha256"] == receipt["artifact_sha256"]
            and manifest["ci"]["run_attempt"] <= attempt, "release_verification_mismatch")
    atomic_json(bundle / "artifact.json", metadata)
    (bundle / "release-smoke-receipt.json").write_bytes((receipt_folder / "release-smoke-receipt.json").read_bytes())
    binary = bundle / "tsz-rust"
    binary.chmod(0o700)
    info = subprocess.run([str(binary), "deploy-info"], capture_output=True, text=True, timeout=30, check=False)
    require(info.returncode == 0, "embedded_migration_info_failed")
    atomic_json(bundle / "deploy-info.json", json.loads(info.stdout))
    return bundle


def migration_evidence(repo, host, bundle, baseline):
    info = json.loads((bundle / "deploy-info.json").read_text())
    interval = migration_interval(info, baseline["postgresql"]["migration_ledger"], recovery_mode="forward_only")
    if not interval["versions"]:
        return
    previous = bundle.parent / "baseline-artifact"
    ci = baseline["deployment_manifest"]["ci"]
    require(type(ci.get("artifact_id")) is int, "accepted_baseline_artifact_identity_required")
    try:
        metadata = repo.api("actions/artifacts/" + str(ci["artifact_id"]))
    except DeploymentError:
        metadata = {"expired": True}
    deployed = metadata.get("expired") is not False
    if deployed:
        host.download_baseline(baseline, previous)
    else:
        repo.download(metadata, previous, {"tsz-rust", "tsz-rust.sha256", "tsz-rust.manifest.json"})
        atomic_json(previous / "artifact.json", metadata)
    atomic_json(bundle / "baseline.json", baseline)
    manifest = json.loads((bundle / "tsz-rust.manifest.json").read_text())
    args = SimpleNamespace(
        baseline=bundle / "baseline.json", baseline_manifest=previous / "tsz-rust.manifest.json",
        baseline_artifact=previous / "tsz-rust", baseline_artifact_metadata=previous / "artifact.json",
        baseline_artifact_id=ci["artifact_id"], manifest=bundle / "tsz-rust.manifest.json",
        artifact=bundle / "tsz-rust", artifact_metadata=bundle / "artifact.json",
        artifact_id=json.loads((bundle / "artifact.json").read_text())["id"],
        output=bundle / "migration-receipt.json", run_id=manifest["ci"]["run_id"],
        run_attempt=manifest["ci"]["run_attempt"], expected_sha=manifest["source"]["git_sha"],
        expected_tree=manifest["source"]["git_tree"], recovery_mode="forward_only", baseline_deployed=deployed)
    # Each Actions job gets a fresh Docker cache; the CI smoke job's images are
    # unavailable here. Fetch only when an actual migration needs rehearsal.
    prepare_runtime_images()
    rehearse(args)


def prepare_runtime_images():
    for image in IsolatedRuntime().images.values():
        result = subprocess.run(["docker", "pull", image], capture_output=True, timeout=180, check=False)
        require(result.returncode == 0, "migration_runtime_image_unavailable")


def wait_for_baseline(host, *, seconds=15 * 60):
    deadline = time.monotonic() + seconds
    stale_abort_attempted = False
    while True:
        try:
            baseline = host.baseline()
            validate_deployment(baseline["deployment_manifest"])
            verified = host.verify_existing(baseline)
            require(verified.get("verified") is True
                    and verified.get("git_sha") == baseline["deployment_manifest"]["source"]["git_sha"],
                    "existing_deployment_not_verified")
            return baseline
        except DeploymentError as error:
            # Only an identified concurrent deployment is waitable. Unknown IO fails normally.
            if str(error) not in {"remote_deployment_busy", "frontend_deployment_busy", "deployment_in_progress"}:
                raise
            inspection = host.inspect()
            print(json.dumps({"status": "waiting", "reason": str(error),
                              "deployment": inspection}), file=sys.stderr, flush=True)
            # The host has already proved that this transaction is old, idle, and
            # has not crossed the mutation boundary. Abort it immediately instead
            # of waiting for the full queue timeout before the first safe cleanup.
            if inspection.get("safe_abort") is True:
                try:
                    aborted = host.abort_stale()
                except DeploymentError as error:
                    if str(error) != "remote_operation_busy":
                        raise
                    stale_abort_attempted = False
                    time.sleep(10)
                    continue
                if aborted.get("aborted"):
                    deadline = time.monotonic() + seconds
                    continue
            if time.monotonic() >= deadline:
                if not stale_abort_attempted:
                    stale_abort_attempted = True
                    try:
                        aborted = host.abort_stale()
                    except DeploymentError as error:
                        if str(error) != "remote_operation_busy":
                            raise
                        stale_abort_attempted = False
                        deadline = time.monotonic() + seconds
                        time.sleep(10)
                        continue
                    if aborted.get("aborted"):
                        deadline = time.monotonic() + seconds
                        continue
                require(False, "deployment_queue_timeout")
            time.sleep(10)


def deploy(repo, host, sha, run_id, attempt, directory):
    candidate = repo.candidate(sha, run_id, attempt)
    if candidate["status"] != "current":
        return candidate
    baseline = wait_for_baseline(host)
    accepted_sha = baseline["deployment_manifest"]["source"]["git_sha"]
    if not inputs_changed(accepted_sha, sha):
        return {**candidate, "status": "skipped", "reason": "verified_inputs_unchanged",
                "accepted_sha": accepted_sha, "server_writes": 0}
    bundle = prepare_bundle(repo, candidate, Path(directory) / "release")
    migration_evidence(repo, host, bundle, baseline)
    candidate = repo.candidate(sha, run_id, attempt)
    if candidate["status"] != "current":
        return candidate
    plan = build_plan(bundle, repo, host, recovery_policy={"kind": "forward_only"})
    atomic_json(Path(directory) / "plan.json", plan)
    controller = Controller(Path(directory) / "state", repo, host)
    result = controller.apply(plan)
    return {**candidate, "status": result["status"], "deployment_id": plan["deployment_id"],
            "accepted_sha": sha if result["status"] == "accepted" else None,
            "failed_stage": result.get("failed_stage"), "recovery_error": result.get("recovery_error"),
            "server_build": 0}


def write_result(directory, result, started):
    value = {**result, "duration_seconds": round(time.monotonic() - started, 3)}
    try:
        atomic_json(Path(directory) / "result.json", value)
    except OSError:
        print("deployment_result_copy_failed; controller state remains authoritative", file=sys.stderr)
    if value["status"] != "current" and (output := os.environ.get("GITHUB_STEP_SUMMARY")):
        try:
            with open(output, "a") as target:
                target.write("\n### Backend deployment\n\n")
                target.write("```json\n" + json.dumps(value, indent=2, sort_keys=True) + "\n```\n")
        except OSError:
            print("deployment_summary_write_failed", file=sys.stderr)
    print(json.dumps(value, sort_keys=True), flush=True)
    return value


def main(argv=None):
    def interrupted(signum, frame):
        raise DeploymentError("interrupted")
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("check", "deploy"))
    parser.add_argument("--sha", required=True)
    parser.add_argument("--run-id", required=True, type=int)
    parser.add_argument("--attempt", required=True, type=int)
    parser.add_argument("--directory", required=True, type=Path)
    parser.add_argument("--pull-from-github", action="store_true")
    args = parser.parse_args(argv)
    started = time.monotonic()
    try:
        repo = Github()
        if args.command == "check":
            result = repo.candidate(args.sha, args.run_id, args.attempt)
        else:
            host = SshHost(artifact_pull=repo.artifact_pull if args.pull_from_github else None)
            result = deploy(repo, host, args.sha, args.run_id, args.attempt, args.directory)
    except (DeploymentError, VerificationError, ArtifactManifestError, OSError, ValueError, KeyError,
            subprocess.TimeoutExpired, zipfile.BadZipFile) as error:
        result = {"status": "failed", "candidate_sha": args.sha,
                  "reason": str(error) if isinstance(error, (DeploymentError, VerificationError)) else type(error).__name__}
    value = write_result(args.directory, result, started)
    if args.command == "check" and (output := os.environ.get("GITHUB_OUTPUT")):
        with open(output, "a") as target:
            target.write("current=" + str(value["status"] == "current").lower() + "\n")
    return 0 if value["status"] in {"current", "accepted", "skipped", "superseded"} else 1


if __name__ == "__main__":
    sys.exit(main())
