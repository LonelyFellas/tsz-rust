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
import time

from ci_test_modules import ClassificationError, cargo_command, classify_targets, discover_targets

ROOT = Path(__file__).resolve().parents[1]
PAGINATION = "component_target_search_pages_past_the_old_200_entry_cap_and_targets_a_late_entry"
PAGINATION_COMPANIONS = {"wordlists_handler", "question_bank_handler", "shared_sentences"}
HANDLER_SHARD, HANDLER_TAIL_SHARD = "lexicon-handler", "lexicon-handler-b"
# 词形共享与批量用例都压在同一个可执行文件里，单片已占满 runner 的全部核心。
# 这里按名称交替二分，让两片各拿约一半用例时间；新增用例自动落入其中一片，
# 完整性仍由 verify 审计保证。batch5 固定留在 profiling 片：identity-profile
# 的 maximum-batch 审计要求该用例与被测量的同一次执行。
HANDLER_ANCHOR_MODULES = ("batch5",)
SHARD_MODULES = {
    "lexicon-pagination": "lexicon",
    "lexicon-learning": "lexicon",
    HANDLER_SHARD: "lexicon",
    HANDLER_TAIL_SHARD: "lexicon",
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
    if shard == "lexicon-pagination":
        return ("lexicon_handler",) + tuple(target for target in targets if target in PAGINATION_COMPANIONS)
    if shard == "lexicon-learning":
        return tuple(target for target in targets if target.startswith("learning_"))
    if shard in (HANDLER_SHARD, HANDLER_TAIL_SHARD):
        return ("lexicon_handler",)
    if shard == "lexicon-other":
        return tuple(target for target in targets if target != "lexicon_handler" and not target.startswith("learning_") and target not in PAGINATION_COMPANIONS)
    return targets


def handler_parts(names):
    if names.count(PAGINATION) != 1:
        raise ClassificationError("pagination case must exist exactly once")
    # libtest --skip matches substrings; reject ambiguous future case names.
    if any(PAGINATION in name and name != PAGINATION for name in names):
        raise ClassificationError("ambiguous pagination skip filter")
    anchors = sorted(name for name in names if name.partition("::")[0] in HANDLER_ANCHOR_MODULES)
    rest = sorted(name for name in names if name != PAGINATION and name.partition("::")[0] not in HANDLER_ANCHOR_MODULES)
    return anchors + rest[0::2], rest[1::2]


def selected_cases(shard, target, names):
    if len(names) != len(set(names)):
        raise ClassificationError(f"duplicate discovered cases in {target}")
    if target == "lexicon_handler":
        if shard == "lexicon-pagination":
            handler_parts(names)
            return [PAGINATION]
        if shard in (HANDLER_SHARD, HANDLER_TAIL_SHARD):
            return handler_parts(names)[0 if shard == HANDLER_SHARD else 1]
    return names


def filters_for(shard, target, names):
    if target != "lexicon_handler":
        return []
    if shard == "lexicon-pagination":
        return [PAGINATION, "--exact"]
    if shard in (HANDLER_SHARD, HANDLER_TAIL_SHARD):
        selected = set(selected_cases(shard, target, names))
        skipped = [name for name in names if name not in selected]
        # --skip 是子串匹配：任何本片要跑的用例名都不能包含被跳过的名字。
        ambiguous = next((name for name in selected for other in skipped if other in name), None)
        if ambiguous:
            raise ClassificationError(f"ambiguous handler skip filter: {ambiguous}")
        return [argument for name in skipped for argument in ("--skip", name)]
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
    router_pilots, pagination_phases, target_timings = [], [], []
    report.parent.mkdir(parents=True, exist_ok=True)
    report.unlink(missing_ok=True)
    diagnostic = report.with_name(f"{shard}-diagnostic.json")
    diagnostic.unlink(missing_ok=True)
    current_results = []
    try:
        for target, executable in sorted(executables.items()):
            names = list_cases(executable)
            ignored = list_cases(executable, ignored=True)
            if not set(ignored) <= set(names):
                raise ClassificationError("ignored cases not in complete inventory")
            inventory[target] = {"names": names, "ignored": ignored}
            selected = selected_cases(shard, target, names)
            results = []
            current_results = results
            print(f"Running {target}: shard={shard}, selected={len(selected)}, discovered={len(names)}", flush=True)
            started = time.monotonic_ns()
            with subprocess.Popen([executable, *filters_for(shard, target, names), "--show-output"], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True) as process:
                for line in process.stdout:
                    print(line, end="", flush=True)
                    cleaned = ANSI.sub("", line).strip()
                    if cleaned.startswith("SQLX_CI_PHASE "):
                        timings.append(json.loads(cleaned.removeprefix("SQLX_CI_PHASE ")))
                    if cleaned.startswith("SQLX_CI_TEMPLATE_USE "):
                        template_uses.append(json.loads(cleaned.removeprefix("SQLX_CI_TEMPLATE_USE ")))
                    if cleaned.startswith("SQLX_CI_BATCH_PHASE "):
                        batch_timings.append(json.loads(cleaned.removeprefix("SQLX_CI_BATCH_PHASE ")))
                    if cleaned.startswith("SQLX_CI_ROUTER_PILOT "):
                        router_pilots.append(json.loads(cleaned.removeprefix("SQLX_CI_ROUTER_PILOT ")))
                    if cleaned.startswith('{"ci_test":'):
                        row = json.loads(cleaned)
                        if row["ci_test"] == "maximum-batch":
                            batch_fixture_phases.append(row)
                        elif row["ci_test"] == "component-pagination":
                            pagination_phases.append(row)
                    match = RESULT.fullmatch(cleaned)
                    if match:
                        results.append({"target": target, "name": match[1], "status": match[2]})
                exit_code = process.wait()
                target_timings.append({"target": target, "elapsed_ms": (time.monotonic_ns() - started) // 1_000_000, "exit_code": exit_code})
                if exit_code != 0:
                    raise ClassificationError(f"test binary failed: {target}")
            validate_results(target, selected, [name for name in ignored if name in selected], results)
            executed.extend(results)
            current_results = []
    finally:
        diagnostic.write_text(json.dumps({"complete": False, "shard": shard, "sha": os.environ.get("GITHUB_SHA") or subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(), "inventory": inventory, "executed": executed + current_results, "timings": timings, "batch_fixture_phases": batch_fixture_phases, "pagination_phases": pagination_phases, "target_timings": target_timings}, indent=2) + "\n")
    report.parent.mkdir(parents=True, exist_ok=True)
    report.write_text(json.dumps({"sha": os.environ.get("GITHUB_SHA") or subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(), "shard": shard, "inventory": inventory, "executed": executed, **({"timings": timings} if timings else {}), **({"template_uses": template_uses} if template_uses else {}), **({"batch_timings": batch_timings} if batch_timings else {}), **({"batch_fixture_phases": batch_fixture_phases} if batch_fixture_phases else {}), **({"router_pilots": router_pilots} if router_pilots else {}), **({"pagination_phases": pagination_phases} if pagination_phases else {}), "target_timings": target_timings}, indent=2) + "\n")


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
