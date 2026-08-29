#!/usr/bin/env python3

import argparse
import hashlib
import json
import os
import pathlib
import re
import sys
import tempfile
import urllib.parse
from collections.abc import Sequence
from typing import Any

from ci_fingerprint import FingerprintError, validate_fingerprint


SHA_RE = re.compile(r"^[0-9a-f]{40}$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
EXPECTED_REPOSITORY = "LonelyFellas/tsz-rust"
EXPECTED_RUNNER = {"os": "Linux", "arch": "X64"}
EXPECTED_TARGET = "x86_64-unknown-linux-gnu"


class ArtifactManifestError(ValueError):
    pass


def _exact_keys(value: Any, expected: set[str], label: str) -> None:
    if not isinstance(value, dict) or set(value) != expected:
        raise ArtifactManifestError(f"{label} must contain exactly {sorted(expected)}")


def _positive_integer(value: Any, label: str) -> None:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise ArtifactManifestError(f"{label} must be a positive integer")


def _validate_run_url(repository: str, run_id: int, run_url: str) -> None:
    try:
        parsed = urllib.parse.urlsplit(run_url)
    except ValueError as error:
        raise ArtifactManifestError("ci.run_url is invalid") from error
    if (
        parsed.scheme != "https"
        or parsed.netloc != "github.com"
        or parsed.path != f"/{repository}/actions/runs/{run_id}"
        or parsed.query
        or parsed.fragment
    ):
        raise ArtifactManifestError("ci.run_url does not match repository and run_id")


def validate_manifest(value: Any) -> dict[str, Any]:
    _exact_keys(value, {"schema_version", "source", "ci", "build", "artifact"}, "manifest")
    if isinstance(value["schema_version"], bool) or value["schema_version"] != 1:
        raise ArtifactManifestError("schema_version must be 1")

    source = value["source"]
    _exact_keys(source, {"repository", "git_ref", "git_sha", "git_tree"}, "source")
    if source["repository"] != EXPECTED_REPOSITORY:
        raise ArtifactManifestError("source.repository is invalid")
    if source["git_ref"] != "refs/heads/main":
        raise ArtifactManifestError("source.git_ref must be refs/heads/main")
    for label in ("git_sha", "git_tree"):
        if not isinstance(source[label], str) or not SHA_RE.fullmatch(source[label]):
            raise ArtifactManifestError(f"source.{label} is invalid")

    ci = value["ci"]
    _exact_keys(ci, {"workflow", "run_id", "run_attempt", "run_url"}, "ci")
    if ci["workflow"] != "CI":
        raise ArtifactManifestError("ci.workflow must be CI")
    _positive_integer(ci["run_id"], "ci.run_id")
    _positive_integer(ci["run_attempt"], "ci.run_attempt")
    if not isinstance(ci["run_url"], str):
        raise ArtifactManifestError("ci.run_url must be a string")
    _validate_run_url(source["repository"], ci["run_id"], ci["run_url"])

    build = value["build"]
    _exact_keys(build, {"fingerprint"}, "build")
    try:
        validate_fingerprint(build["fingerprint"])
    except FingerprintError as error:
        raise ArtifactManifestError(f"build fingerprint is invalid: {error}") from error
    if build["fingerprint"]["build"] != {
        "profile": "release",
        "features": "default",
        "sqlx_offline": "true",
    }:
        raise ArtifactManifestError("build fingerprint does not match deploy profile")
    if build["fingerprint"]["runner"] != EXPECTED_RUNNER:
        raise ArtifactManifestError("build fingerprint does not match Linux X64 runner")
    if build["fingerprint"]["rust"]["host"] != EXPECTED_TARGET:
        raise ArtifactManifestError("build fingerprint does not match deploy target")

    artifact = value["artifact"]
    _exact_keys(artifact, {"name", "target", "path", "size_bytes", "sha256"}, "artifact")
    if artifact["name"] != "tsz-rust" or artifact["path"] != "tsz-rust":
        raise ArtifactManifestError("artifact name/path is invalid")
    if artifact["target"] != EXPECTED_TARGET:
        raise ArtifactManifestError("artifact target does not match deploy target")
    _positive_integer(artifact["size_bytes"], "artifact.size_bytes")
    if not isinstance(artifact["sha256"], str) or not SHA256_RE.fullmatch(
        artifact["sha256"]
    ):
        raise ArtifactManifestError("artifact.sha256 is invalid")
    return value


def _sha256_file(path: pathlib.Path) -> str:
    if not path.is_file():
        raise ArtifactManifestError(f"artifact is not a regular file: {path}")
    digest = hashlib.sha256()
    with path.open("rb") as artifact_file:
        for chunk in iter(lambda: artifact_file.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _atomic_write(output: pathlib.Path, manifest: dict[str, Any]) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
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
        os.replace(temporary_path, output)
        temporary_path = None
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)


def create_manifest(
    *,
    artifact: pathlib.Path,
    output: pathlib.Path,
    repository: str,
    git_ref: str,
    git_sha: str,
    git_tree: str,
    run_id: int,
    run_attempt: int,
    run_url: str,
    fingerprint: dict[str, Any],
) -> dict[str, Any]:
    try:
        validate_fingerprint(fingerprint)
    except FingerprintError as error:
        raise ArtifactManifestError(f"build fingerprint is invalid: {error}") from error
    if not artifact.is_file():
        raise ArtifactManifestError(f"artifact is not a regular file: {artifact}")
    manifest = {
        "schema_version": 1,
        "source": {
            "repository": repository,
            "git_ref": git_ref,
            "git_sha": git_sha,
            "git_tree": git_tree,
        },
        "ci": {
            "workflow": "CI",
            "run_id": run_id,
            "run_attempt": run_attempt,
            "run_url": run_url,
        },
        "build": {"fingerprint": fingerprint},
        "artifact": {
            "name": "tsz-rust",
            "target": fingerprint["rust"]["host"],
            "path": "tsz-rust",
            "size_bytes": artifact.stat().st_size if artifact.is_file() else 0,
            "sha256": _sha256_file(artifact),
        },
    }
    validate_manifest(manifest)
    _atomic_write(output, manifest)
    return manifest


def verify_manifest(
    manifest_path: pathlib.Path,
    artifact: pathlib.Path,
    *,
    expected_git_sha: str | None = None,
    expected_git_tree: str | None = None,
    expected_run_id: int | None = None,
    expected_run_attempt: int | None = None,
) -> dict[str, Any]:
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ArtifactManifestError(f"cannot read manifest: {error}") from error
    validate_manifest(manifest)
    if not artifact.is_file():
        raise ArtifactManifestError(f"artifact is not a regular file: {artifact}")
    if artifact.stat().st_size != manifest["artifact"]["size_bytes"]:
        raise ArtifactManifestError("artifact size does not match manifest")
    if _sha256_file(artifact) != manifest["artifact"]["sha256"]:
        raise ArtifactManifestError("artifact digest does not match manifest")
    if expected_git_sha is not None and manifest["source"]["git_sha"] != expected_git_sha:
        raise ArtifactManifestError("manifest git_sha does not match expected value")
    if expected_git_tree is not None and manifest["source"]["git_tree"] != expected_git_tree:
        raise ArtifactManifestError("manifest git_tree does not match expected value")
    if expected_run_id is not None and manifest["ci"]["run_id"] != expected_run_id:
        raise ArtifactManifestError("manifest run_id does not match expected value")
    if (
        expected_run_attempt is not None
        and manifest["ci"]["run_attempt"] != expected_run_attempt
    ):
        raise ArtifactManifestError("manifest run_attempt does not match expected value")
    return manifest


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Create or verify a release artifact manifest")
    subparsers = parser.add_subparsers(dest="command", required=True)
    create = subparsers.add_parser("create")
    create.add_argument("--artifact", required=True, type=pathlib.Path)
    create.add_argument("--output", required=True, type=pathlib.Path)
    create.add_argument("--repository", required=True)
    create.add_argument("--git-ref", required=True)
    create.add_argument("--git-sha", required=True)
    create.add_argument("--git-tree", required=True)
    create.add_argument("--run-id", required=True, type=int)
    create.add_argument("--run-attempt", required=True, type=int)
    create.add_argument("--run-url", required=True)
    create.add_argument("--fingerprint", required=True, type=pathlib.Path)
    verify = subparsers.add_parser("verify")
    verify.add_argument("--manifest", required=True, type=pathlib.Path)
    verify.add_argument("--artifact", required=True, type=pathlib.Path)
    verify.add_argument("--expected-git-sha")
    verify.add_argument("--expected-git-tree")
    verify.add_argument("--expected-run-id", type=int)
    verify.add_argument("--expected-run-attempt", type=int)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    arguments = _parser().parse_args(argv)
    try:
        if arguments.command == "create":
            fingerprint = json.loads(arguments.fingerprint.read_text(encoding="utf-8"))
            manifest = create_manifest(
                artifact=arguments.artifact,
                output=arguments.output,
                repository=arguments.repository,
                git_ref=arguments.git_ref,
                git_sha=arguments.git_sha,
                git_tree=arguments.git_tree,
                run_id=arguments.run_id,
                run_attempt=arguments.run_attempt,
                run_url=arguments.run_url,
                fingerprint=fingerprint,
            )
        else:
            manifest = verify_manifest(
                arguments.manifest,
                arguments.artifact,
                expected_git_sha=arguments.expected_git_sha,
                expected_git_tree=arguments.expected_git_tree,
                expected_run_id=arguments.expected_run_id,
                expected_run_attempt=arguments.expected_run_attempt,
            )
    except (ArtifactManifestError, FingerprintError, OSError, json.JSONDecodeError) as error:
        print(f"release artifact manifest: {error}", file=sys.stderr)
        return 1
    print(
        json.dumps(
            {
                "git_sha": manifest["source"]["git_sha"],
                "git_tree": manifest["source"]["git_tree"],
                "run_id": manifest["ci"]["run_id"],
                "artifact_size": manifest["artifact"]["size_bytes"],
                "artifact_sha256": manifest["artifact"]["sha256"],
            },
            separators=(",", ":"),
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
