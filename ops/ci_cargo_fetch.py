#!/usr/bin/env python3

import json
import os
import pathlib
import subprocess
import sys
import tempfile
from collections.abc import Callable, Mapping, Sequence


def _safe_environment_path(value: str, label: str) -> pathlib.Path:
    if not value or "\n" in value or "\r" in value:
        raise ValueError(f"{label} is invalid")
    return pathlib.Path(value)


def fetch_with_clean_fallback(
    *,
    runner_temp: pathlib.Path,
    github_env: pathlib.Path,
    environment: Mapping[str, str],
    runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
) -> int:
    command = ["cargo", "fetch", "--locked"]
    first = runner(command, check=False, env=dict(environment))
    if first.returncode == 0:
        print(json.dumps({"cargo_fetch": "cache-or-network", "fallback": False}))
        return 0

    clean_cargo_home = pathlib.Path(
        tempfile.mkdtemp(prefix="cargo-home-clean-", dir=runner_temp)
    )
    clean_target = pathlib.Path(
        tempfile.mkdtemp(prefix="cargo-target-clean-", dir=runner_temp)
    )
    clean_environment = dict(environment)
    clean_environment.update(
        {"CARGO_HOME": str(clean_cargo_home), "CARGO_TARGET_DIR": str(clean_target)}
    )
    print("::warning::cached Cargo state failed validation; retrying from clean directories")
    second = runner(command, check=False, env=clean_environment)
    if second.returncode != 0:
        return second.returncode

    with github_env.open("a", encoding="utf-8") as output:
        output.write(f"CARGO_HOME={clean_cargo_home}\n")
        output.write(f"CARGO_TARGET_DIR={clean_target}\n")
    print(json.dumps({"cargo_fetch": "clean-rebuild", "fallback": True}))
    return 0


def main(argv: Sequence[str] | None = None) -> int:
    arguments = tuple(sys.argv[1:] if argv is None else argv)
    if arguments:
        print("ci cargo fetch: arguments are not accepted", file=sys.stderr)
        return 2
    try:
        runner_temp = _safe_environment_path(os.environ.get("RUNNER_TEMP", ""), "RUNNER_TEMP")
        github_env = _safe_environment_path(os.environ.get("GITHUB_ENV", ""), "GITHUB_ENV")
        runner_temp.mkdir(parents=True, exist_ok=True)
        return fetch_with_clean_fallback(
            runner_temp=runner_temp,
            github_env=github_env,
            environment=os.environ,
        )
    except (OSError, ValueError) as error:
        print(f"ci cargo fetch: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
