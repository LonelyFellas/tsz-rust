#!/usr/bin/env python3
"""Run complete integration shards and audit the actual libtest case results."""
import argparse
from collections import Counter
import json
import os
from pathlib import Path
import re
import subprocess
import sys

from ci_test_modules import ClassificationError, cargo_command, classify_targets, discover_targets

ROOT = Path(__file__).resolve().parents[1]
PAGINATION = "component_target_search_pages_past_the_old_200_entry_cap_and_targets_a_late_entry"
SHARD_MODULES = {
    "lexicon-pagination": "lexicon",
    "lexicon-handler": "lexicon",
    "lexicon-other": "lexicon",
    "admin": "admin",
    "identity": "identity",
    "platform": "platform",
}
ANSI = re.compile(r"\x1b\[[0-9;]*m")
RESULT = re.compile(r"^test (\S+) \.\.\. (ok|FAILED|ignored)(?:,.*)?$")


def targets_for(shard, grouped):
    if shard not in SHARD_MODULES:
        raise ClassificationError(f"unknown shard: {shard}")
    targets = grouped[SHARD_MODULES[shard]]
    if shard in ("lexicon-pagination", "lexicon-handler"):
        return ("lexicon_handler",)
    if shard == "lexicon-other":
        return tuple(target for target in targets if target != "lexicon_handler")
    return targets


def selected_cases(shard, target, names):
    if len(names) != len(set(names)):
        raise ClassificationError(f"duplicate discovered cases in {target}")
    if target == "lexicon_handler":
        if names.count(PAGINATION) != 1:
            raise ClassificationError("pagination case must exist exactly once")
        # libtest --skip matches substrings; reject ambiguous future case names.
        if any(PAGINATION in name and name != PAGINATION for name in names):
            raise ClassificationError("ambiguous pagination skip filter")
        if shard == "lexicon-pagination":
            return [PAGINATION]
        if shard == "lexicon-handler":
            return [name for name in names if name != PAGINATION]
    return names


def filters_for(shard):
    if shard == "lexicon-pagination":
        return [PAGINATION, "--exact"]
    if shard == "lexicon-handler":
        return ["--skip", PAGINATION]
    return []


def list_cases(executable, ignored=False):
    command = [executable, "--list"]
    if ignored:
        command.append("--ignored")
    output = subprocess.run(command, check=True, capture_output=True, text=True).stdout
    names = [line.removesuffix(": test") for line in output.splitlines() if line.endswith(": test")]
    count = re.search(r"(?m)^(\d+) tests?, \d+ benchmarks?$", output)
    if not count or int(count[1]) != len(names) or len(names) != len(set(names)):
        raise ClassificationError("could not verify complete libtest inventory")
    return names


def validate_results(target, selected, ignored, results):
    if Counter(item["name"] for item in results) != Counter(selected):
        raise ClassificationError(f"missing, unexpected or duplicate execution in {target}")
    if not set(ignored) <= set(selected):
        raise ClassificationError(f"invalid ignored inventory in {target}")
    for item in results:
        expected = "ignored" if item["name"] in ignored else "ok"
        if item["status"] != expected:
            raise ClassificationError(f"unexpected result for {target}::{item['name']}")


def build(shard, grouped, directory):
    targets = targets_for(shard, grouped)
    command = cargo_command(SHARD_MODULES[shard], {SHARD_MODULES[shard]: targets})
    command += ["--no-run", "--message-format=json"]
    output = subprocess.run(command, check=True, stdout=subprocess.PIPE, text=True).stdout
    executables = {}
    for line in output.splitlines():
        item = json.loads(line)
        if item.get("reason") == "compiler-artifact" and item.get("executable") and item["target"]["name"] in targets:
            executables[item["target"]["name"]] = item["executable"]
    if set(executables) != set(targets):
        raise ClassificationError("compiled target inventory differs from shard")
    directory.mkdir(parents=True, exist_ok=True)
    (directory / f"{shard}-executables.json").write_text(json.dumps(executables))


def run(shard, grouped, directory, report):
    executables = json.loads((directory / f"{shard}-executables.json").read_text())
    if set(executables) != set(targets_for(shard, grouped)):
        raise ClassificationError("stale executable inventory")
    inventory, executed, timings, template_uses = {}, [], [], []
    batch_timings, batch_fixture_phases = [], []
    for target, executable in sorted(executables.items()):
        names = list_cases(executable)
        ignored = list_cases(executable, ignored=True)
        if not set(ignored) <= set(names):
            raise ClassificationError("ignored cases not in complete inventory")
        inventory[target] = {"names": names, "ignored": ignored}
        selected = selected_cases(shard, target, names)
        results = []
        print(f"Running {target}: shard={shard}, selected={len(selected)}, discovered={len(names)}", flush=True)
        with subprocess.Popen([executable, *filters_for(shard), "--show-output"], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True) as process:
            for line in process.stdout:
                print(line, end="", flush=True)
                cleaned = ANSI.sub("", line).strip()
                if cleaned.startswith("SQLX_CI_PHASE "):
                    timings.append(json.loads(cleaned.removeprefix("SQLX_CI_PHASE ")))
                if cleaned.startswith("SQLX_CI_TEMPLATE_USE "):
                    template_uses.append(json.loads(cleaned.removeprefix("SQLX_CI_TEMPLATE_USE ")))
                if cleaned.startswith("SQLX_CI_BATCH_PHASE "):
                    batch_timings.append(json.loads(cleaned.removeprefix("SQLX_CI_BATCH_PHASE ")))
                if cleaned.startswith('{"ci_test":'):
                    row = json.loads(cleaned)
                    if row["ci_test"] == "maximum-batch":
                        batch_fixture_phases.append(row)
                match = RESULT.fullmatch(cleaned)
                if match:
                    results.append({"target": target, "name": match[1], "status": match[2]})
            if process.wait() != 0:
                raise ClassificationError(f"test binary failed: {target}")
        validate_results(target, selected, [name for name in ignored if name in selected], results)
        executed.extend(results)
    report.parent.mkdir(parents=True, exist_ok=True)
    report.write_text(json.dumps({"sha": os.environ.get("GITHUB_SHA") or subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(), "shard": shard, "inventory": inventory, "executed": executed, **({"timings": timings} if timings else {}), **({"template_uses": template_uses} if template_uses else {}), **({"batch_timings": batch_timings} if batch_timings else {}), **({"batch_fixture_phases": batch_fixture_phases} if batch_fixture_phases else {})}, indent=2) + "\n")


def verify_manifests(manifests, grouped, sha):
    if Counter(item["shard"] for item in manifests) != Counter(SHARD_MODULES.keys()):
        raise ClassificationError("missing, unexpected or duplicate shard report")
    inventory, executed = {}, []
    for item in manifests:
        shard = item["shard"]
        if item["sha"] != sha:
            raise ClassificationError("report SHA differs from workflow SHA")
        if set(item["inventory"]) != set(targets_for(shard, grouped)):
            raise ClassificationError("report target inventory differs from shard")
        for target, cases in item["inventory"].items():
            if target in inventory and inventory[target] != cases:
                raise ClassificationError("shared binary inventories disagree")
            inventory[target] = cases
            selected = selected_cases(shard, target, cases["names"])
            results = [row for row in item["executed"] if row["target"] == target]
            validate_results(target, selected, [name for name in cases["ignored"] if name in selected], results)
        if any(row["target"] not in item["inventory"] for row in item["executed"]):
            raise ClassificationError("executed an unassigned target")
        executed.extend((row["target"], row["name"]) for row in item["executed"])
    expected_targets = {target for targets in grouped.values() for target in targets}
    if set(inventory) != expected_targets:
        raise ClassificationError("complete repository target inventory is not covered")
    expected = [(target, name) for target, cases in inventory.items() for name in cases["names"]]
    if Counter(executed) != Counter(expected) or any(count != 1 for count in Counter(executed).values()):
        raise ClassificationError("repository tests were omitted or executed more than once")
    return {"targets": len(inventory), "cases": len(executed), "shards": len(manifests)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["build", "run", "verify"])
    parser.add_argument("shard_or_directory")
    parser.add_argument("--executables", type=Path, default=ROOT / "target/ci-shards")
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    grouped = classify_targets(discover_targets(ROOT))
    if args.action == "build":
        build(args.shard_or_directory, grouped, args.executables)
    elif args.action == "run":
        run(args.shard_or_directory, grouped, args.executables, args.report or ROOT / f"target/ci-shards/{args.shard_or_directory}.json")
    else:
        manifests = [json.loads(path.read_text()) for path in Path(args.shard_or_directory).rglob("*-report.json")]
        result = verify_manifests(manifests, grouped, os.environ["GITHUB_SHA"])
        print(json.dumps(result), flush=True)


if __name__ == "__main__":
    try:
        main()
    except ClassificationError as error:
        raise SystemExit(str(error)) from error
