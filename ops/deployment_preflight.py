#!/usr/bin/env python3

import json
import os
import re
import subprocess
import sys
from collections.abc import Callable, Sequence
from typing import Any


SCALAR_QUERIES = (
    ("entries", "SELECT count(*) FROM lexicon.entries"),
    (
        "v3_entries",
        "SELECT count(*) FROM lexicon.entries WHERE content_schema_version = 3",
    ),
    (
        "successful_migrations",
        "SELECT count(*) FROM _sqlx_migrations WHERE success IS TRUE",
    ),
    (
        "latest_migration",
        "SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations WHERE success IS TRUE",
    ),
)


class PreflightError(RuntimeError):
    pass


def _parse_scalar(name: str, completed: subprocess.CompletedProcess[str]) -> int:
    if completed.returncode != 0:
        raise PreflightError(f"{name} query failed with exit {completed.returncode}")
    lines = [line.strip() for line in completed.stdout.splitlines() if line.strip()]
    if len(lines) != 1 or not re.fullmatch(r"0|[1-9][0-9]*", lines[0]):
        raise PreflightError(f"{name} query did not return exactly one non-negative integer")
    return int(lines[0])


def collect_snapshot(
    *,
    database_url: str,
    runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
) -> dict[str, Any]:
    if not database_url:
        raise PreflightError("DATABASE_URL is empty")
    snapshot: dict[str, Any] = {"schema_version": 1}
    psql_environment = {
        "PATH": os.environ.get("PATH", os.defpath),
        "LC_ALL": "C",
        "PGCONNECT_TIMEOUT": "8",
    }
    for name, query in SCALAR_QUERIES:
        try:
            completed = runner(
                [
                    "psql",
                    "--dbname",
                    database_url,
                    "-X",
                    "-A",
                    "-t",
                    "-q",
                    "-v",
                    "ON_ERROR_STOP=1",
                    "-c",
                    query,
                ],
                check=False,
                capture_output=True,
                text=True,
                env=psql_environment,
            )
        except OSError:
            raise PreflightError(f"{name} query could not start") from None
        snapshot[name] = _parse_scalar(name, completed)
    return snapshot


def main(argv: Sequence[str] | None = None) -> int:
    arguments = tuple(sys.argv[1:] if argv is None else argv)
    if arguments:
        print("deployment preflight: arguments are not accepted", file=sys.stderr)
        return 2
    try:
        snapshot = collect_snapshot(database_url=os.environ.get("DATABASE_URL", ""))
    except PreflightError as error:
        print(f"deployment preflight: {error}", file=sys.stderr)
        return 1
    print(json.dumps(snapshot, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
