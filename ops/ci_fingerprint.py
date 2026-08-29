#!/usr/bin/env python3

import argparse
import hashlib
import json
import os
import pathlib
import platform
import re
import subprocess
from collections.abc import Sequence
from typing import Any


SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
SAFE_VALUE_RE = re.compile(r"^[A-Za-z0-9._+-]+$")
BUILD_CONFIG_PATHS = (
    pathlib.Path("Cargo.toml"),
    pathlib.Path("rust-toolchain.toml"),
    pathlib.Path(".cargo/config.toml"),
    pathlib.Path("build.rs"),
)


class FingerprintError(ValueError):
    pass


def _exact_keys(value: Any, expected: set[str], label: str) -> None:
    if not isinstance(value, dict) or set(value) != expected:
        raise FingerprintError(f"{label} must contain exactly {sorted(expected)}")


def _hash_files(repository_root: pathlib.Path, paths: Sequence[pathlib.Path]) -> str:
    if not paths:
        raise FingerprintError("fingerprint input file set is empty")
    digest = hashlib.sha256()
    for relative_path in sorted(paths, key=lambda path: path.as_posix()):
        path = repository_root / relative_path
        if not path.is_file():
            raise FingerprintError(f"fingerprint input is missing: {relative_path}")
        content = path.read_bytes()
        encoded_path = relative_path.as_posix().encode()
        digest.update(len(encoded_path).to_bytes(4, "big"))
        digest.update(encoded_path)
        digest.update(len(content).to_bytes(8, "big"))
        digest.update(content)
    return digest.hexdigest()


def _rust_value(rustc_verbose: str, label: str) -> str:
    prefix = f"{label}: "
    values = [line.removeprefix(prefix) for line in rustc_verbose.splitlines() if line.startswith(prefix)]
    if len(values) != 1 or not values[0]:
        raise FingerprintError(f"rustc -vV does not contain one {label}")
    return values[0]


def _safe(value: str, label: str) -> str:
    if not SAFE_VALUE_RE.fullmatch(value):
        raise FingerprintError(f"{label} contains unsafe characters")
    return value


def _cache_key(
    *,
    runner_os: str,
    runner_arch: str,
    rust_release: str,
    rust_host: str,
    rust_commit_hash: str,
    profile: str,
    features: str,
    sqlx_offline: str,
    cargo_lock_sha256: str,
    build_config_sha256: str,
    sqlx_sha256: str,
) -> str:
    return "-".join(
        (
            "v3",
            runner_os,
            runner_arch,
            "rustc",
            rust_release,
            rust_commit_hash,
            "host",
            rust_host,
            "profile",
            profile,
            "features",
            features,
            "sqlx",
            sqlx_offline,
            "lock",
            cargo_lock_sha256,
            "config",
            build_config_sha256,
            "sqlxmeta",
            sqlx_sha256,
        )
    )


def validate_fingerprint(value: Any) -> dict[str, Any]:
    _exact_keys(
        value,
        {"schema_version", "runner", "rust", "build", "inputs", "cache_key"},
        "fingerprint",
    )
    if isinstance(value["schema_version"], bool) or value["schema_version"] != 1:
        raise FingerprintError("schema_version must be 1")
    _exact_keys(value["runner"], {"os", "arch"}, "runner")
    _exact_keys(
        value["rust"], {"release", "host", "commit_hash", "cargo_version"}, "rust"
    )
    _exact_keys(value["build"], {"profile", "features", "sqlx_offline"}, "build")
    _exact_keys(
        value["inputs"],
        {"cargo_lock_sha256", "build_config_sha256", "sqlx_sha256"},
        "inputs",
    )
    for label, item in (
        ("runner.os", value["runner"]["os"]),
        ("runner.arch", value["runner"]["arch"]),
        ("rust.release", value["rust"]["release"]),
        ("rust.host", value["rust"]["host"]),
        ("build.profile", value["build"]["profile"]),
        ("build.features", value["build"]["features"]),
    ):
        if not isinstance(item, str):
            raise FingerprintError(f"{label} must be a string")
        _safe(item, label)
    if value["build"]["sqlx_offline"] not in {"true", "false"}:
        raise FingerprintError("build.sqlx_offline must be true or false")
    commit_hash = value["rust"]["commit_hash"]
    if not isinstance(commit_hash, str) or not re.fullmatch(r"[0-9a-f]{40}", commit_hash):
        raise FingerprintError("rust.commit_hash is invalid")
    if not isinstance(value["rust"]["cargo_version"], str) or not value["rust"][
        "cargo_version"
    ].startswith("cargo "):
        raise FingerprintError("rust.cargo_version is invalid")
    for label, digest in value["inputs"].items():
        if not isinstance(digest, str) or not SHA256_RE.fullmatch(digest):
            raise FingerprintError(f"inputs.{label} is invalid")
    cache_key = value["cache_key"]
    if (
        not isinstance(cache_key, str)
        or len(cache_key) > 480
        or not re.fullmatch(r"[A-Za-z0-9._+-]+", cache_key)
    ):
        raise FingerprintError("cache_key is invalid")
    expected_cache_key = _cache_key(
        runner_os=value["runner"]["os"],
        runner_arch=value["runner"]["arch"],
        rust_release=value["rust"]["release"],
        rust_host=value["rust"]["host"],
        rust_commit_hash=value["rust"]["commit_hash"],
        profile=value["build"]["profile"],
        features=value["build"]["features"],
        sqlx_offline=value["build"]["sqlx_offline"],
        cargo_lock_sha256=value["inputs"]["cargo_lock_sha256"],
        build_config_sha256=value["inputs"]["build_config_sha256"],
        sqlx_sha256=value["inputs"]["sqlx_sha256"],
    )
    if cache_key != expected_cache_key:
        raise FingerprintError("cache_key does not match fingerprint inputs")
    return value


def build_fingerprint(
    *,
    repository_root: pathlib.Path,
    runner_os: str,
    runner_arch: str,
    rustc_verbose: str,
    cargo_version: str,
    profile: str,
    features: str,
    sqlx_offline: str,
) -> dict[str, Any]:
    runner_os = _safe(runner_os, "runner_os")
    runner_arch = _safe(runner_arch, "runner_arch")
    profile = _safe(profile, "profile")
    features = _safe(features, "features")
    if sqlx_offline not in {"true", "false"}:
        raise FingerprintError("sqlx_offline must be true or false")

    rust_release = _safe(_rust_value(rustc_verbose, "release"), "rust release")
    rust_host = _safe(_rust_value(rustc_verbose, "host"), "rust host")
    rust_commit_hash = _rust_value(rustc_verbose, "commit-hash")
    cargo_lock_sha256 = _hash_files(repository_root, (pathlib.Path("Cargo.lock"),))
    build_config_sha256 = _hash_files(repository_root, BUILD_CONFIG_PATHS)
    sqlx_paths = tuple(
        path.relative_to(repository_root)
        for path in (repository_root / ".sqlx").glob("**/*.json")
        if path.is_file()
    )
    sqlx_sha256 = _hash_files(repository_root, sqlx_paths)

    cache_key = _cache_key(
        runner_os=runner_os,
        runner_arch=runner_arch,
        rust_release=rust_release,
        rust_host=rust_host,
        rust_commit_hash=rust_commit_hash,
        profile=profile,
        features=features,
        sqlx_offline=sqlx_offline,
        cargo_lock_sha256=cargo_lock_sha256,
        build_config_sha256=build_config_sha256,
        sqlx_sha256=sqlx_sha256,
    )
    fingerprint = {
        "schema_version": 1,
        "runner": {"os": runner_os, "arch": runner_arch},
        "rust": {
            "release": rust_release,
            "host": rust_host,
            "commit_hash": rust_commit_hash,
            "cargo_version": cargo_version.strip(),
        },
        "build": {
            "profile": profile,
            "features": features,
            "sqlx_offline": sqlx_offline,
        },
        "inputs": {
            "cargo_lock_sha256": cargo_lock_sha256,
            "build_config_sha256": build_config_sha256,
            "sqlx_sha256": sqlx_sha256,
        },
        "cache_key": cache_key,
    }
    return validate_fingerprint(fingerprint)


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Build an auditable Rust CI cache fingerprint")
    parser.add_argument(
        "--root",
        type=pathlib.Path,
        default=pathlib.Path(__file__).resolve().parent.parent,
    )
    parser.add_argument("--profile", required=True)
    parser.add_argument("--features", required=True)
    parser.add_argument("--sqlx-offline", choices=("true", "false"), required=True)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--github-output", type=pathlib.Path)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    arguments = _parser().parse_args(argv)
    fingerprint = build_fingerprint(
        repository_root=arguments.root,
        runner_os=os.environ.get("RUNNER_OS", platform.system()),
        runner_arch=os.environ.get("RUNNER_ARCH", platform.machine()),
        rustc_verbose=subprocess.run(
            ["rustc", "-vV"], check=True, capture_output=True, text=True
        ).stdout,
        cargo_version=subprocess.run(
            ["cargo", "--version"], check=True, capture_output=True, text=True
        ).stdout,
        profile=arguments.profile,
        features=arguments.features,
        sqlx_offline=arguments.sqlx_offline,
    )
    rendered = json.dumps(fingerprint, sort_keys=True, separators=(",", ":"))
    if arguments.output is not None:
        arguments.output.parent.mkdir(parents=True, exist_ok=True)
        arguments.output.write_text(rendered + "\n", encoding="utf-8")
    if arguments.github_output is not None:
        with arguments.github_output.open("a", encoding="utf-8") as output:
            output.write(f"cache_key={fingerprint['cache_key']}\n")
    print(rendered)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
