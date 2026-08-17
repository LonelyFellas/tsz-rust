#!/usr/bin/env python3
import argparse
import datetime
import hashlib
import json
import os
import pathlib
import re
import shutil
import sys
import tempfile
import urllib.parse
from typing import Any


SHA_RE = re.compile(r"^[0-9a-f]{40}$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
REPOSITORY_RE = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
ACCEPTED_AT_RE = re.compile(
    r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,6})?Z$"
)
EXPECTED_REPOSITORY = "LonelyFellas/tsz-rust"
EXPECTED_ARTIFACT_PATH = "/opt/tsz-rust/target/release/tsz-rust"


class ManifestError(ValueError):
    pass


def _exact_keys(value: Any, expected: set[str], label: str) -> None:
    if not isinstance(value, dict):
        raise ManifestError(f"{label} must be an object")
    actual = set(value)
    missing = sorted(expected - actual)
    unexpected = sorted(actual - expected)
    if missing:
        raise ManifestError(f"{label} missing {', '.join(missing)}")
    if unexpected:
        raise ManifestError(f"{label} has unexpected {', '.join(unexpected)}")


def _validate_run_url(repository: str, run_id: int, run_url: str) -> None:
    try:
        parsed = urllib.parse.urlsplit(run_url)
    except ValueError as error:
        raise ManifestError("ci.run_url is invalid") from error
    if (
        parsed.scheme != "https"
        or parsed.netloc != "github.com"
        or parsed.path != f"/{repository}/actions/runs/{run_id}"
        or parsed.query
        or parsed.fragment
    ):
        raise ManifestError("ci.run_url must match repository and run_id on github.com")


def validate_manifest(manifest: Any) -> dict[str, Any]:
    _exact_keys(
        manifest,
        {"schema_version", "component", "source", "ci", "artifact", "accepted_at"},
        "manifest",
    )
    if isinstance(manifest["schema_version"], bool) or manifest["schema_version"] != 1:
        raise ManifestError("schema_version must be 1")
    if manifest["component"] != "api":
        raise ManifestError("component must be api")

    source = manifest["source"]
    _exact_keys(source, {"repository", "git_sha", "git_tree", "remote_ref"}, "source")
    if not isinstance(source["repository"], str) or not REPOSITORY_RE.fullmatch(
        source["repository"]
    ):
        raise ManifestError("source.repository is invalid")
    if source["repository"] != EXPECTED_REPOSITORY:
        raise ManifestError("source.repository does not match api")
    if not isinstance(source["git_sha"], str) or not SHA_RE.fullmatch(source["git_sha"]):
        raise ManifestError("source.git_sha is invalid")
    if not isinstance(source["git_tree"], str) or not SHA_RE.fullmatch(source["git_tree"]):
        raise ManifestError("source.git_tree is invalid")
    if source["remote_ref"] != "refs/heads/main":
        raise ManifestError("source.remote_ref must be refs/heads/main")

    ci = manifest["ci"]
    _exact_keys(ci, {"workflow", "run_id", "run_url", "conclusion"}, "ci")
    if ci["workflow"] != "CI":
        raise ManifestError("ci.workflow must be CI")
    if (
        isinstance(ci["run_id"], bool)
        or not isinstance(ci["run_id"], int)
        or ci["run_id"] <= 0
        or ci["run_id"] > 999_999_999_999_999
    ):
        raise ManifestError("ci.run_id must be a positive integer of at most 15 digits")
    if ci["conclusion"] != "success":
        raise ManifestError("ci.conclusion must be success")
    if not isinstance(ci["run_url"], str):
        raise ManifestError("ci.run_url must be a string")
    _validate_run_url(source["repository"], ci["run_id"], ci["run_url"])

    artifact = manifest["artifact"]
    _exact_keys(
        artifact,
        {"kind", "path", "sha256", "file_count", "excluded_paths"},
        "artifact",
    )
    if artifact["kind"] != "file":
        raise ManifestError("artifact.kind must be file")
    if artifact["path"] != EXPECTED_ARTIFACT_PATH:
        raise ManifestError("artifact.path does not match api")
    if not isinstance(artifact["sha256"], str) or not SHA256_RE.fullmatch(
        artifact["sha256"]
    ):
        raise ManifestError("artifact.sha256 is invalid")
    if isinstance(artifact["file_count"], bool) or artifact["file_count"] != 1:
        raise ManifestError("artifact.file_count must be 1")
    if artifact["excluded_paths"] != []:
        raise ManifestError("artifact.excluded_paths must be empty for api")

    accepted_at = manifest["accepted_at"]
    if not isinstance(accepted_at, str) or not ACCEPTED_AT_RE.fullmatch(accepted_at):
        raise ManifestError("accepted_at must be an RFC3339 UTC timestamp")
    try:
        parsed_time = datetime.datetime.fromisoformat(accepted_at.removesuffix("Z") + "+00:00")
    except ValueError as error:
        raise ManifestError("accepted_at must be an RFC3339 UTC timestamp") from error
    if parsed_time.tzinfo != datetime.timezone.utc:
        raise ManifestError("accepted_at must be UTC")
    return manifest


def _sha256_file(path: pathlib.Path) -> str:
    if not path.is_file():
        raise ManifestError(f"artifact is not a regular file: {path}")
    digest = hashlib.sha256()
    with path.open("rb") as artifact_file:
        for chunk in iter(lambda: artifact_file.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _atomic_write(output: pathlib.Path, manifest: dict[str, Any]) -> None:
    output.parent.mkdir(parents=True, mode=0o755, exist_ok=True)
    temporary_path: pathlib.Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            dir=output.parent,
            prefix=f".{output.name}.",
            suffix=".partial",
            delete=False,
        ) as temporary:
            temporary_path = pathlib.Path(temporary.name)
            json.dump(manifest, temporary, indent=2, sort_keys=False)
            temporary.write("\n")
            temporary.flush()
            os.fsync(temporary.fileno())
        temporary_path.chmod(0o644)
        os.replace(temporary_path, output)
        temporary_path = None
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)


def _atomic_replace_file(source: pathlib.Path, output: pathlib.Path) -> None:
    if not source.is_file():
        raise ManifestError(f"restore source is not a regular file: {source}")
    output.parent.mkdir(parents=True, mode=0o755, exist_ok=True)
    temporary_path: pathlib.Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="wb",
            dir=output.parent,
            prefix=f".{output.name}.",
            suffix=".restore.partial",
            delete=False,
        ) as temporary:
            temporary_path = pathlib.Path(temporary.name)
            with source.open("rb") as source_file:
                shutil.copyfileobj(source_file, temporary)
            temporary.flush()
            os.fsync(temporary.fileno())
        temporary_path.chmod(source.stat().st_mode & 0o777)
        os.replace(temporary_path, output)
        temporary_path = None
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)


def create_manifest(
    *,
    component: str,
    repository: str,
    git_sha: str,
    git_tree: str,
    ci_run_id: int,
    ci_run_url: str,
    artifact: pathlib.Path,
    artifact_path: str,
    output: pathlib.Path,
) -> dict[str, Any]:
    digest = _sha256_file(artifact)
    manifest = {
        "schema_version": 1,
        "component": component,
        "source": {
            "repository": repository,
            "git_sha": git_sha,
            "git_tree": git_tree,
            "remote_ref": "refs/heads/main",
        },
        "ci": {
            "workflow": "CI",
            "run_id": ci_run_id,
            "run_url": ci_run_url,
            "conclusion": "success",
        },
        "artifact": {
            "kind": "file",
            "path": artifact_path,
            "sha256": digest,
            "file_count": 1,
            "excluded_paths": [],
        },
        "accepted_at": datetime.datetime.now(datetime.timezone.utc).isoformat().replace(
            "+00:00", "Z"
        ),
    }
    validate_manifest(manifest)
    _atomic_write(output, manifest)
    return manifest


def verify_manifest(manifest_path: pathlib.Path, artifact: pathlib.Path) -> dict[str, Any]:
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ManifestError(f"cannot read manifest: {error}") from error
    validate_manifest(manifest)
    digest = _sha256_file(artifact)
    if digest != manifest["artifact"]["sha256"]:
        raise ManifestError("artifact digest does not match manifest")
    return manifest


def validate_backup(backup_dir: pathlib.Path) -> tuple[pathlib.Path, dict[str, Any] | None]:
    backup_artifact = backup_dir / "tsz-rust"
    backup_manifest = backup_dir / "api.json"
    absent_marker = backup_dir / "manifest.absent"
    if not backup_artifact.is_file():
        raise ManifestError("backup artifact is missing")
    if backup_manifest.is_file() == absent_marker.is_file():
        raise ManifestError("backup must contain exactly one manifest state")

    manifest = (
        verify_manifest(backup_manifest, backup_artifact)
        if backup_manifest.is_file()
        else None
    )
    return backup_artifact, manifest


def restore_deployment(
    *,
    backup_dir: pathlib.Path,
    artifact: pathlib.Path,
    manifest_path: pathlib.Path,
) -> dict[str, Any] | None:
    backup_artifact, manifest = validate_backup(backup_dir)
    manifest_path.unlink(missing_ok=True)
    _atomic_replace_file(backup_artifact, artifact)
    if manifest is not None:
        _atomic_write(manifest_path, manifest)
    return manifest


def _summary(manifest: dict[str, Any]) -> str:
    return json.dumps(
        {
            "component": manifest["component"],
            "git_sha": manifest["source"]["git_sha"],
            "ci_run_id": manifest["ci"]["run_id"],
            "artifact_sha256": manifest["artifact"]["sha256"],
            "file_count": manifest["artifact"]["file_count"],
            "accepted_at": manifest["accepted_at"],
        },
        separators=(",", ":"),
    )


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Create or verify a tsz deployment manifest")
    subparsers = parser.add_subparsers(dest="command", required=True)
    create = subparsers.add_parser("create")
    create.add_argument("--component", required=True)
    create.add_argument("--repository", required=True)
    create.add_argument("--git-sha", required=True)
    create.add_argument("--git-tree", required=True)
    create.add_argument("--ci-run-id", required=True, type=int)
    create.add_argument("--ci-run-url", required=True)
    create.add_argument("--artifact", required=True, type=pathlib.Path)
    create.add_argument("--artifact-path", required=True)
    create.add_argument("--output", required=True, type=pathlib.Path)
    verify = subparsers.add_parser("verify")
    verify.add_argument("--manifest", required=True, type=pathlib.Path)
    verify.add_argument("--artifact", required=True, type=pathlib.Path)
    restore = subparsers.add_parser("restore")
    restore.add_argument("--backup-dir", required=True, type=pathlib.Path)
    restore.add_argument("--artifact", required=True, type=pathlib.Path)
    restore.add_argument("--manifest", required=True, type=pathlib.Path)
    validate = subparsers.add_parser("validate-backup")
    validate.add_argument("--backup-dir", required=True, type=pathlib.Path)
    return parser


def main() -> int:
    arguments = _parser().parse_args()
    try:
        if arguments.command == "create":
            manifest = create_manifest(
                component=arguments.component,
                repository=arguments.repository,
                git_sha=arguments.git_sha,
                git_tree=arguments.git_tree,
                ci_run_id=arguments.ci_run_id,
                ci_run_url=arguments.ci_run_url,
                artifact=arguments.artifact,
                artifact_path=arguments.artifact_path,
                output=arguments.output,
            )
        elif arguments.command == "verify":
            manifest = verify_manifest(arguments.manifest, arguments.artifact)
        elif arguments.command == "restore":
            manifest = restore_deployment(
                backup_dir=arguments.backup_dir,
                artifact=arguments.artifact,
                manifest_path=arguments.manifest,
            )
        else:
            _, manifest = validate_backup(arguments.backup_dir)
    except ManifestError as error:
        print(f"deployment manifest: {error}", file=sys.stderr)
        return 1
    print(
        _summary(manifest)
        if manifest is not None
        else json.dumps({"component": "api", "restored_manifest": "absent"})
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
