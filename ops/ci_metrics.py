#!/usr/bin/env python3

import argparse
import json
import os
import pathlib
import platform
import subprocess
import time
from collections.abc import Sequence
from typing import Any


def _append_summary(summary_path: pathlib.Path | None, metric: dict[str, Any]) -> None:
    if summary_path is None:
        return
    summary_path.parent.mkdir(parents=True, exist_ok=True)
    needs_header = not summary_path.exists() or summary_path.stat().st_size == 0
    with summary_path.open("a", encoding="utf-8") as summary:
        if needs_header:
            summary.write("| CI metric | value | unit | exit_code | elapsed_ms |\n")
            summary.write("| --- | ---: | --- | ---: | ---: |\n")
        summary.write(
            f"| {metric['name']} | {metric['value']} | {metric['unit']} | "
            f"{metric.get('exit_code', '')} | {metric.get('elapsed_ms', '')} |\n"
        )


def _append_metrics(metric: dict[str, Any]) -> None:
    path = os.environ.get("CI_METRICS_PATH")
    if path:
        destination = pathlib.Path(path)
        destination.parent.mkdir(parents=True, exist_ok=True)
        with destination.open("a", encoding="utf-8") as output:
            output.write(json.dumps(metric, separators=(",", ":")) + "\n")


def _cpu_model() -> str:
    cpuinfo = pathlib.Path("/proc/cpuinfo")
    if cpuinfo.exists():
        for line in cpuinfo.read_text().splitlines():
            if line.startswith("model name"):
                return line.partition(":")[2].strip()
    return platform.processor()


def record_context() -> None:
    # Explicit allowlist: never serialize the environment or command arguments.
    context = {
        "name": "context",
        "sha": os.environ.get("GITHUB_SHA", ""),
        "tree": subprocess.check_output(["git", "rev-parse", "HEAD^{tree}"], text=True).strip(),
        "run_id": os.environ.get("GITHUB_RUN_ID", ""),
        "attempt": os.environ.get("GITHUB_RUN_ATTEMPT", ""),
        "event": os.environ.get("GITHUB_EVENT_NAME", ""),
        "runner_image": os.environ.get("ImageVersion", ""),
        "runner_os": platform.system(),
        "architecture": platform.machine(),
        "cpu_count": os.cpu_count(),
        "cpu_model": _cpu_model(),
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
    }
    _append_metrics(context)
    print(json.dumps(context, separators=(",", ":")), flush=True)


def record_metric(
    name: str,
    value: str | int,
    unit: str,
    *,
    summary_path: pathlib.Path | None = None,
) -> dict[str, Any]:
    metric = {"name": name, "value": value, "unit": unit}
    _append_summary(summary_path, metric)
    _append_metrics(metric)
    print(json.dumps(metric, separators=(",", ":")), flush=True)
    return metric


def run_timed(
    name: str,
    command: Sequence[str],
    *,
    summary_path: pathlib.Path | None = None,
) -> int:
    started = time.monotonic_ns()
    completed = subprocess.run(command, check=False)
    elapsed_ms = (time.monotonic_ns() - started) // 1_000_000
    metric = {
        "name": name,
        "value": elapsed_ms,
        "unit": "milliseconds",
        "exit_code": completed.returncode,
        "elapsed_ms": elapsed_ms,
    }
    _append_summary(summary_path, metric)
    _append_metrics(metric)
    print(json.dumps(metric, separators=(",", ":")), flush=True)
    return completed.returncode


def _summary_path(value: str | None) -> pathlib.Path | None:
    path = value or os.environ.get("GITHUB_STEP_SUMMARY")
    return pathlib.Path(path) if path else None


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Record simple CI and deploy timings")
    subparsers = parser.add_subparsers(dest="action", required=True)
    subparsers.add_parser("context")
    run = subparsers.add_parser("run")
    run.add_argument("--name", required=True)
    run.add_argument("--summary-path")
    run.add_argument("command", nargs=argparse.REMAINDER)
    record = subparsers.add_parser("record")
    record.add_argument("--name", required=True)
    record.add_argument("--value", required=True)
    record.add_argument("--unit", required=True)
    record.add_argument("--summary-path")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    arguments = _parser().parse_args(argv)
    if arguments.action == "context":
        record_context()
        return 0
    summary_path = _summary_path(arguments.summary_path)
    if arguments.action == "record":
        record_metric(arguments.name, arguments.value, arguments.unit, summary_path=summary_path)
        return 0
    command = arguments.command
    if command and command[0] == "--":
        command = command[1:]
    if not command:
        raise SystemExit("ci_metrics run requires a command after --")
    return run_timed(arguments.name, command, summary_path=summary_path)


if __name__ == "__main__":
    raise SystemExit(main())
