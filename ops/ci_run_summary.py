#!/usr/bin/env python3
"""Summarize one Actions attempt; diagnostics never replace the CI gates."""
import argparse
from datetime import datetime
import json
import os
from pathlib import Path
import subprocess


SUMMARY_JOB = "CI timing summary"
# PR/main 关键路径的目标值。只用于报告偏差，不改变任何门禁：这个 job 在任何
# depends_on 之外，且 continue-on-error，保持无授权改变质量检查结果。
WALL_CLOCK_BUDGET_SECONDS = 900


def seconds(start, end):
    if not start or not end:
        return None
    value = (datetime.fromisoformat(end.replace("Z", "+00:00")) -
             datetime.fromisoformat(start.replace("Z", "+00:00"))).total_seconds()
    return round(value, 3) if value >= 0 else None


def phase(name):
    if name.startswith("Post "):
        return "post"
    if "cache" in name.lower():
        return "cache"
    if name.startswith("Upload "):
        return "upload"
    if name.startswith(("Compile ", "Build release binary", "Run Clippy")):
        return "compile_lint"
    if name.startswith(("Run ", "Test ")):
        return "test"
    return "setup_other"


def summarize(run, jobs):
    rows, reused = [], []
    seen = set()
    for job in jobs:
        if job["id"] in seen:
            raise ValueError("duplicate job ID")
        seen.add(job["id"])
        if job["run_id"] != run["id"] or job["run_attempt"] != run["run_attempt"]:
            raise ValueError("job belongs to another run/attempt")
        if job["name"] == SUMMARY_JOB:
            continue
        skipped = job["conclusion"] == "skipped"
        # GitHub reissues successful previous-attempt jobs with the NEW attempt
        # number but preserves their execution timestamps. Do not double count.
        if not skipped and job["started_at"] and seconds(run["run_started_at"], job["started_at"]) is None:
            reused.append({"id": job["id"], "name": job["name"], "conclusion": job["conclusion"],
                           "started_at": job["started_at"], "completed_at": job["completed_at"]})
            continue
        wall = 0 if skipped else seconds(job["started_at"], job["completed_at"])
        phases = {}
        for step in job.get("steps", []):
            if step["conclusion"] == "skipped":
                continue
            duration = seconds(step["started_at"], step["completed_at"])
            if duration is not None:
                group = phase(step["name"])
                phases[group] = round(phases.get(group, 0) + duration, 3)
        rows.append({"id": job["id"], "name": job["name"], "conclusion": job["conclusion"],
                     "status": job["status"], "started_at": job["started_at"],
                     "completed_at": job["completed_at"], "wall_seconds": wall,
                     "created_to_start_seconds": None if skipped else seconds(job.get("created_at"), job["started_at"]),
                     "phases_seconds": phases})
    if not rows:
        raise ValueError("no measured jobs")
    finished = [r for r in rows if r["conclusion"] != "skipped" and r["wall_seconds"] is not None]
    last = max(finished, key=lambda r: r["completed_at"]) if finished else None
    longest = max(finished, key=lambda r: r["wall_seconds"]) if finished else None
    return {"schema_version": 1, "run_id": run["id"], "attempt": run["run_attempt"],
            "head_sha": run["head_sha"], "event": run["event"], "jobs": rows, "reused_jobs": reused,
            "all_measured_jobs_completed": all(r["status"] == "completed" for r in rows),
            "measured_latency_seconds": seconds(run["run_started_at"], last["completed_at"]) if last else None,
            "last_completed_job": last["name"] if last else None,
            "longest_job": longest["name"] if longest else None,
            "completed_runner_minutes": round(sum(r["wall_seconds"] for r in finished) / 60, 3),
            "scope": "current attempt only; excludes summary job and reused jobs from previous attempts; runner minutes are not billed minutes"}


def budget(report):
    measured = report["measured_latency_seconds"]
    if measured is None:
        return None
    return {"budget_seconds": WALL_CLOCK_BUDGET_SECONDS, "measured_seconds": measured,
            "over_budget": measured > WALL_CLOCK_BUDGET_SECONDS}


def markdown(report):
    lines = ["## CI attempt timings", "", report["scope"], "",
             f"Reused previous-attempt jobs (excluded from this attempt cost): {len(report['reused_jobs'])}.",
             f"Run {report['run_id']} / attempt {report['attempt']} / source head {report['head_sha']}",
             f"Evaluated workflow SHA: {report.get('workflow_sha') or 'unknown (historical API does not identify the PR merge checkout)'}",
             f"Measured latency: {report['measured_latency_seconds']} s; completed runner time: {report['completed_runner_minutes']} min.",
             f"Last completed: {report['last_completed_job']}; longest job: {report['longest_job']}."]
    limit = budget(report)
    if limit:
        verdict = "over" if limit["over_budget"] else "within"
        lines.append(f"Wall-clock budget: {limit['budget_seconds']} s ({verdict} the measured latency). "
                     "A breach is reported for review and never changes a gate.")
    lines += [
             "Created-to-start may include dependency and scheduling delay; missing timestamps remain unknown. This is not a reconstructed DAG critical path.", "",
             "| Job | Result | Wall s | Created-to-start s | Compile/lint s | Test s | Cache s | Upload s |",
             "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |"]
    for row in report["jobs"]:
        name = row["name"].replace("|", "/").replace("\n", " ")
        values = [name, row["conclusion"], row["wall_seconds"], row["created_to_start_seconds"],
                  *(row["phases_seconds"].get(p, 0) for p in ("compile_lint", "test", "cache", "upload"))]
        lines.append("| " + " | ".join("unknown" if v is None else str(v) for v in values) + " |")
    return "\n".join(lines) + "\n"


def api(path, paginate=False):
    command = ["gh", "api", path]
    if paginate:
        command += ["--paginate", "--slurp"]
    return json.loads(subprocess.check_output(command, text=True))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY"))
    parser.add_argument("--run-id", type=int, default=os.environ.get("GITHUB_RUN_ID"))
    parser.add_argument("--attempt", type=int, default=os.environ.get("GITHUB_RUN_ATTEMPT"))
    parser.add_argument("--workflow-sha")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not args.repo or not args.run_id or not args.attempt:
        parser.error("repo, run-id and attempt are required")
    base = f"repos/{args.repo}/actions/runs/{args.run_id}/attempts/{args.attempt}"
    run = api(base)
    pages = api(base + "/jobs?per_page=100", paginate=True)
    report = summarize(run, [job for page in pages for job in page["jobs"]])
    report["workflow_sha"] = args.workflow_sha
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    rendered = markdown(report)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as output:
            output.write(rendered)
    print(rendered)
    limit = budget(report)
    if limit and limit["over_budget"]:
        print(f"::warning::CI attempt wall clock {limit['measured_seconds']} s exceeds the "
              f"{limit['budget_seconds']} s budget; inspect the shard and phase columns above")


if __name__ == "__main__":
    main()
