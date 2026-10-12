#!/usr/bin/env python3
"""CI-only SQLx lifecycle timing using a disposable, version-locked source copy."""
import argparse
from collections import defaultdict
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tomllib

from ci_test_modules import classify_targets, discover_targets

ROOT = Path(__file__).resolve().parents[1]
PACKAGES = {"sqlx-core", "sqlx-postgres"}
PREFIX = "SQLX_CI_PHASE "

TIMER = r'''
/// Timing only: installed in a disposable CI source copy, never the registry.
#[doc(hidden)]
pub struct CiPhaseTimer {
    test_path: &'static str,
    phase: &'static str,
    started: std::time::Instant,
}
impl CiPhaseTimer {
    pub fn new(test_path: &'static str, phase: &'static str) -> Self {
        Self { test_path, phase, started: std::time::Instant::now() }
    }
}
impl Drop for CiPhaseTimer {
    fn drop(&mut self) {
        // Rust test paths are identifiers separated by ::; Debug quotes them as
        // JSON strings, without ever logging SQL, credentials or fixture data.
        eprintln!("SQLX_CI_PHASE {{\"test\":{:?},\"phase\":{:?},\"elapsed_us\":{},\"complete\":{}}}",
            self.test_path, self.phase, self.started.elapsed().as_micros(),
            !std::thread::panicking());
    }
}
'''


def replace_once(source, old, new):
    if source.count(old) != 1:
        raise ValueError("SQLx source anchor changed; instrumentation requires review")
    return source.replace(old, new, 1)


def instrument_core(source):
    source = replace_once(source, "pub struct TestFixture {", TIMER + "\npub struct TestFixture {")
    source = replace_once(source, '''        let pool = pool_opts
            .connect_with(connect_opts)
            .await
            .expect("failed to connect test pool");

        let res = test_fn(pool.clone()).await;

        let close_timed_out = crate::rt::timeout(Duration::from_secs(10), pool.close())
            .await
            .is_err();''', '''        let pool = {
            let _ci = CiPhaseTimer::new(test_path, "pool_connect");
            pool_opts.connect_with(connect_opts).await.expect("failed to connect test pool")
        };
        let res = {
            let _ci = CiPhaseTimer::new(test_path, "body");
            test_fn(pool.clone()).await
        };
        let close_timed_out = {
            let _ci = CiPhaseTimer::new(test_path, "pool_close");
            crate::rt::timeout(Duration::from_secs(10), pool.close()).await.is_err()
        };''')
    source = replace_once(source, '''        let test_context = DB::test_context(&args)
            .await
            .expect("failed to connect to setup test database");''', '''        let _ci_total = CiPhaseTimer::new(args.test_path, "total");
        let test_context = {
            let _ci = CiPhaseTimer::new(args.test_path, "db_setup");
            DB::test_context(&args).await.expect("failed to connect to setup test database")
        };''')
    source = replace_once(source, '''        let res = test_fn(test_context.pool_opts, test_context.connect_opts).await;

        if res.is_success() {''', '''        let res = {
            let _ci = CiPhaseTimer::new(args.test_path, "test_callback");
            test_fn(test_context.pool_opts, test_context.connect_opts).await
        };

        if res.is_success() {
            let _ci = CiPhaseTimer::new(args.test_path, "cleanup");''')
    source = replace_once(source, '''    if let Some(migrator) = args.migrator {
        migrator''', '''    if let Some(migrator) = args.migrator {
        let _ci = CiPhaseTimer::new(args.test_path, "migration");
        migrator''')
    return source


def instrument_postgres(source):
    return replace_once(source, '''    conn.execute(AssertSqlSafe(create_command)).await?;''', '''    {
        let _ci = sqlx_core::testing::CiPhaseTimer::new(args.test_path, "db_create");
        conn.execute(AssertSqlSafe(create_command)).await?;
    }''')


def locked_packages(path):
    lock = tomllib.loads(path.read_text())
    packages = {(item["name"], item["version"]): item for item in lock["package"]}
    if len(packages) != len(lock["package"]):
        raise ValueError("ambiguous package versions in lockfile")
    return packages


def validate_profile_lock(original, candidate):
    if set(original) != set(candidate):
        raise ValueError("profiling changed the locked package set")
    for key, package in original.items():
        expected = dict(package)
        if key[0] in PACKAGES:
            if key[1] != "0.9.0":
                raise ValueError("profiling supports only SQLx 0.9.0")
            expected.pop("source")
            expected.pop("checksum")
        if expected != candidate[key]:
            raise ValueError(f"profiling changed a locked package: {key}")


def profile_lock(text):
    blocks = text.split("\n[[package]]\n")
    for index, block in enumerate(blocks):
        if any(block.startswith(f'name = "{name}"\nversion = "0.9.0"\n') for name in PACKAGES):
            blocks[index] = re.sub(r'(?m)^(?:source|checksum) = .*\n', "", block)
    return "\n[[package]]\n".join(blocks)


def instrument_units(snapshot):
    units = []
    pattern = re.compile(r"(?m)^#\[(?:test|tokio::test(?:\([^\n]*\))?)\]\s*(?:async\s+)?fn\s+(\w+)\s*\(\s*\)\s*\{")
    for target in classify_targets(discover_targets(snapshot))["identity"]:
        path = snapshot / "tests" / f"{target}.rs"
        source = path.read_text()
        matches = list(pattern.finditer(source))
        for match in reversed(matches):
            name = match[1]
            timer = f'\n    let _ci_body = sqlx::testing::CiPhaseTimer::new(concat!(module_path!(), "::", stringify!({name})), "body");'
            source = source[:match.end()] + timer + source[match.end():]
            units.append({"target": target, "name": name})
        path.write_text(source)
    return units


def prepare(root, state):
    snapshot = state / "workspace"
    if snapshot.exists():
        raise ValueError("profiling workspace already exists; use a fresh state directory")
    snapshot.mkdir(parents=True)
    paths = subprocess.check_output(["git", "ls-files", "-z"], cwd=root).decode().split("\0")
    for name in filter(None, paths):
        destination = snapshot / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(root / name, destination, follow_symlinks=False)
    original = locked_packages(root / "Cargo.lock")
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--offline", "--all-features", "--format-version=1"], cwd=root))
    patches = []
    evidence = {}
    for name in sorted(PACKAGES):
        item = next(p for p in metadata["packages"] if p["name"] == name and p["version"] == "0.9.0")
        origin = Path(item["manifest_path"]).parent
        locked = original[(name, "0.9.0")]
        if item["source"] != locked["source"]:
            raise ValueError("SQLx metadata source differs from lockfile")
        archive = origin.parent.parent.parent / "cache" / origin.parent.name / f"{name}-0.9.0.crate"
        if hashlib.sha256(archive.read_bytes()).hexdigest() != locked["checksum"]:
            raise ValueError("SQLx crate archive checksum differs from lockfile")
        relative = "src/testing/mod.rs"
        with tarfile.open(archive) as package:
            prefix = f"{name}-0.9.0/"
            if any(not member.name.startswith(prefix) for member in package.getmembers()):
                raise ValueError("unexpected archive path")
            upstream = package.extractfile(prefix + relative).read()
            package.extractall(state, filter="data")
        copy = state / f"{name}-0.9.0"
        patched = instrument_core(upstream.decode()) if name == "sqlx-core" else instrument_postgres(upstream.decode())
        (copy / relative).write_text(patched)
        evidence[name] = {"version": "0.9.0", "upstream_sha256": hashlib.sha256(upstream).hexdigest(), "timed_sha256": hashlib.sha256(patched.encode()).hexdigest()}
        patches.append(f'{name} = {{ path = {json.dumps(str(copy))} }}')
    manifest = snapshot / "Cargo.toml"
    if "[patch.crates-io]" in manifest.read_text():
        raise ValueError("existing crate patches require explicit profiling review")
    manifest.write_text(manifest.read_text() + "\n[patch.crates-io]\n" + "\n".join(patches) + "\n")
    # Preserve every pinned transitive version; only the two reviewed path sources change.
    (snapshot / "Cargo.lock").write_text(profile_lock((root / "Cargo.lock").read_text()))
    subprocess.run(["cargo", "metadata", "--locked", "--offline", "--all-features", "--format-version=1"], cwd=snapshot, check=True, stdout=subprocess.DEVNULL)
    validate_profile_lock(original, locked_packages(snapshot / "Cargo.lock"))
    evidence["unit_cases"] = instrument_units(snapshot)
    (state / "source-evidence.json").write_text(json.dumps(evidence, indent=2) + "\n")
    return snapshot


def summarize(report, unit_cases=()):
    results = {(row["target"], row["name"]): row["status"] for row in report["executed"]}
    cases = defaultdict(dict)
    allowed = {"total", "db_setup", "db_create", "migration", "pool_connect", "body", "pool_close", "cleanup", "test_callback"}
    for row in report.get("timings", []):
        target, sep, name = row["test"].partition("::")
        if not sep or (target, name) not in results or row["phase"] not in allowed:
            raise ValueError("timing does not belong to an executed identity case")
        if not row["complete"] or type(row["elapsed_us"]) is not int or row["elapsed_us"] < 0:
            raise ValueError("incomplete or invalid timing")
        case = cases[(target, name)]
        if row["phase"] in case:
            raise ValueError("duplicate phase timing")
        case[row["phase"]] = row["elapsed_us"]
    if not cases:
        raise ValueError("SQLx timing instrumentation produced no events")
    units = {(row["target"], row["name"]) for row in unit_cases}
    if set(cases) != set(results) or not units <= set(results):
        raise ValueError("an executed identity case lacks timing or unit inventory differs")
    for key, phases in cases.items():
        if key in units:
            if set(phases) != {"body"}:
                raise ValueError("invalid non-database test phases")
            continue
        required = {"total", "db_setup", "db_create", "body", "pool_connect", "pool_close", "cleanup", "test_callback"}
        if not required <= phases.keys():
            raise ValueError("missing SQLx lifecycle phase")
        if phases["db_create"] > phases["db_setup"] or phases["body"] > phases["test_callback"]:
            raise ValueError("invalid nested phase durations")
        if phases["db_setup"] + phases.get("migration", 0) + phases["test_callback"] + phases["cleanup"] > phases["total"]:
            raise ValueError("lifecycle durations exceed the case total")
    totals = {phase: round(sum(c.get(phase, 0) for key, c in cases.items() if key not in units) / 1000, 3) for phase in sorted(allowed)}
    slowest = [{"target": target, "name": name, "phases_ms": {p: round(v / 1000, 3) for p, v in phases.items()}} for (target, name), phases in sorted(cases.items(), key=lambda x: x[1].get("total", x[1]["body"]), reverse=True)[:20]]
    return {"sha": report["sha"], "executed_cases": len(results), "profiled_sqlx_cases": len(cases) - len(units), "profiled_unit_cases": len(units), "unit_body_ms": round(sum(cases[key]["body"] for key in units) / 1000, 3), "without_sqlx_phases": [{"target": t, "name": n} for t, n in results.keys() - cases.keys()], "cumulative_phase_ms": totals, "slowest_cases": slowest}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["build", "run"])
    parser.add_argument("shard")
    parser.add_argument("--report", type=Path)
    parser.add_argument("--state", type=Path, default=Path(os.environ.get("RUNNER_TEMP", str(ROOT / "target"))) / "identity-phase-profile")
    args = parser.parse_args()
    if args.shard != "identity":
        command = [sys.executable, str(ROOT / "ops/ci_test_shards.py"), args.action, args.shard]
        if args.report:
            command += ["--report", str(args.report)]
        subprocess.run(command, check=True)
        return
    snapshot = prepare(ROOT, args.state) if args.action == "build" else args.state / "workspace"
    env = {**os.environ, "CARGO_TARGET_DIR": str(args.state / "target"), "GITHUB_SHA": os.environ.get("GITHUB_SHA") or subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()}
    command = [sys.executable, str(snapshot / "ops/ci_test_shards.py"), args.action, "identity"]
    if args.action == "run":
        if not args.report:
            parser.error("identity run requires --report")
        command += ["--report", str(args.report.resolve())]
    subprocess.run(command, cwd=snapshot, env=env, check=True)
    if args.action == "run":
        report = json.loads(args.report.read_text())
        evidence = json.loads((args.state / "source-evidence.json").read_text())
        summary = summarize(report, evidence["unit_cases"])
        summary["source_evidence"] = evidence
        output = args.report.parent / "identity-phases.json"
        output.write_text(json.dumps(summary, indent=2) + "\n")
        print(json.dumps({"profiled_sqlx_cases": summary["profiled_sqlx_cases"], "cumulative_phase_ms": summary["cumulative_phase_ms"]}), flush=True)


if __name__ == "__main__":
    main()
