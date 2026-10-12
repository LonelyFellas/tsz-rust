#!/usr/bin/env python3
"""CI-only SQLx lifecycle timing using a disposable, version-locked source copy."""
import argparse
from collections import Counter, defaultdict
import hashlib
import json
import math
import os
import re
from pathlib import Path
import shutil
import statistics
import subprocess
import sys
import tarfile
import time
import tomllib
import uuid

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


def template_cases(root):
    cases = json.loads((root / "ops/ci_template_cases.json").read_text())
    targets = {"auth_me", "session_repository", "session_service", "session_reuse_detection", "user_repository", "user_service"}
    if not cases or len(cases) != len(set(cases)) or any(not re.fullmatch(r"[a-zA-Z_][a-zA-Z_0-9]*::[a-zA-Z_][a-zA-Z_0-9]*", c) or c.partition("::")[0] not in targets for c in cases):
        raise ValueError("invalid or duplicate business template whitelist")
    return cases


def instrument_postgres(source, cases=(), fingerprint=""):
    if cases:
        helper = fingerprint.replace("sqlx::migrate::", "sqlx_core::migrate::")
        helper += '\nconst CI_TEMPLATE_CASES: &[&str] = &[\n' + ",\n".join(json.dumps(c) for c in cases) + '\n];\n'
        source = replace_once(source, "static MASTER_POOL:", helper + "\nstatic MASTER_POOL:")
        source = replace_once(source, '    let create_command = format!("create database {db_name:?}");', '''    let create_command = if CI_TEMPLATE_CASES.contains(&args.test_path) {
        let template = std::env::var("SQLX_CI_TEMPLATE").expect("missing job-owned template");
        let suffix = template.strip_prefix("_tsz_ci_tpl_").expect("invalid template prefix");
        assert!(suffix.len() == 32 && suffix.bytes().all(|b| b.is_ascii_hexdigit()));
        let expected = std::env::var("SQLX_CI_TEMPLATE_FINGERPRINT").expect("missing template fingerprint");
        assert_eq!(migration_fingerprint(args.migrator.expect("whitelisted test needs migrations")), expected,
            "template belongs to different migrations");
        eprintln!("SQLX_CI_TEMPLATE_USE {{\\"test\\":{:?},\\"template\\":{:?}}}", args.test_path, template);
        format!("create database {db_name:?} template {template:?} strategy WAL_LOG")
    } else {
        format!("create database {db_name:?}")
    };''')
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


BATCH_TEST = "lexicon_handler::batch5::batch5_list_references_and_maximum_batch_measurement"
BATCH_COUNTS = {"forms-impact-save": 50, "initial-meanings-save": 50, "relation-save": 50, "list-read": 20, "references-read": 20}
BATCH_FIXTURES = {"empty-fixture", "content-fixture", "relation-fixture", "publish-assertions", "list-reference-assertions"}


def instrument_maximum_batch(snapshot):
    path = snapshot / "tests/lexicon_handler/batch5.rs"
    original = path.read_text()
    source = original
    edits = []

    def edit(old, new):
        nonlocal source
        source = replace_once(source, old, new)
        edits.append((old, new))

    def timer(name, phase):
        return f'let {name} = CiBatchTimer::new("{BATCH_TEST}", "{phase}");'

    edit('''                let (_, forms) = save_v3_forms_after_impact(''', '                ' + timer("_ci_forms", "forms-impact-save") + '\n                let (_, forms) = save_v3_forms_after_impact(')
    edit('''                let meanings = complete_v3_meanings_fixture(''', '''                drop(_ci_forms);
                let meanings = complete_v3_meanings_fixture(''')
    edit('''                save_v3_meanings(state, bearer, &forms, meanings).await''', '                ' + timer("_ci_meanings", "initial-meanings-save") + '''
                let saved = save_v3_meanings(state, bearer, &forms, meanings).await;
                drop(_ci_meanings);
                saved''')
    edit('''        words[index] = save_v3_meanings(&state, &bearer, &words[index], meanings).await;''', '        ' + timer("_ci_relation", "relation-save") + '''
        words[index] = save_v3_meanings(&state, &bearer, &words[index], meanings).await;
        drop(_ci_relation);''')
    edit('''        let (status, list) = call(''', '        ' + timer("_ci_list", "list-read") + '\n        let (status, list) = call(')
    edit('''        assert_eq!(status, StatusCode::OK, "{list}");''', '''        drop(_ci_list);
        assert_eq!(status, StatusCode::OK, "{list}");''')
    edit('''        let refs = inbound_references_of(&state, &bearer, word["id"].as_str().unwrap()).await;''', '        ' + timer("_ci_refs", "references-read") + '''
        let refs = inbound_references_of(&state, &bearer, word["id"].as_str().unwrap()).await;
        drop(_ci_refs);''')
    helper = TIMER.replace("CiPhaseTimer", "CiBatchTimer").replace("SQLX_CI_PHASE ", "SQLX_CI_BATCH_PHASE ")
    source += helper
    # Reversing only the timer edits must reproduce every original byte: no
    # assertion, loop bound, concurrency, request, fixture or await is removed.
    restored = source.removesuffix(helper)
    for old, new in reversed(edits):
        restored = replace_once(restored, new, old)
    if restored != original:
        raise ValueError("batch instrumentation changed test behavior")
    path.write_text(source)
    return {"original_sha256": hashlib.sha256(original.encode()).hexdigest(), "timed_sha256": hashlib.sha256(source.encode()).hexdigest(), "reversible_timer_edits": True}


def summarize_maximum_batch(report):
    if not any(f"{r['target']}::{r['name']}" == BATCH_TEST and r["status"] == "ok" for r in report["executed"]):
        raise ValueError("maximum batch case was not executed successfully")
    samples = defaultdict(list)
    for row in report.get("batch_timings", []):
        if row["test"] != BATCH_TEST or row["phase"] not in BATCH_COUNTS or row["complete"] is not True or type(row["elapsed_us"]) is not int or row["elapsed_us"] < 0:
            raise ValueError("invalid maximum batch timing")
        samples[row["phase"]].append(row["elapsed_us"])
    if {p: len(v) for p, v in samples.items()} != BATCH_COUNTS:
        raise ValueError("missing, duplicate or unexpected maximum batch samples")
    fixtures = report.get("batch_fixture_phases", [])
    if Counter(r["phase"] for r in fixtures) != Counter(BATCH_FIXTURES) or any(r["ci_test"] != "maximum-batch" or type(r["elapsed_ms"]) is not int or r["elapsed_ms"] < 0 for r in fixtures):
        raise ValueError("invalid maximum batch fixture phases")
    walls = {r["phase"]: r["elapsed_ms"] for r in fixtures}
    body = next(t["elapsed_us"] for t in report["timings"] if t["test"] == BATCH_TEST and t["phase"] == "body")
    if sum(walls.values()) * 1000 > body:
        raise ValueError("batch fixture walls exceed test body")
    if sum(samples["forms-impact-save"] + samples["initial-meanings-save"]) > 4 * (walls["content-fixture"] + 1) * 1000 or sum(samples["relation-save"]) > (walls["relation-fixture"] + 1) * 1000 or sum(samples["list-read"] + samples["references-read"]) > (walls["list-reference-assertions"] + 1) * 1000:
        raise ValueError("batch request timings exceed their parent spans")
    phases = {t["phase"]: round(t["elapsed_us"] / 1000, 3) for t in report["timings"] if t["test"] == BATCH_TEST}
    requests = {}
    for phase, values in samples.items():
        values.sort()
        requests[phase] = {"count": len(values), "sum_ms": round(sum(values) / 1000, 3), "p50_ms": round(statistics.median(values) / 1000, 3), "p95_ms": round(values[math.ceil(len(values) * .95) - 1] / 1000, 3), "max_ms": round(max(values) / 1000, 3)}
    return {"test": BATCH_TEST, "lifecycle_ms": phases, "fixture_wall_ms": walls, "request_samples": requests, "content_concurrency": 4}


def prepare(root, state, shard="identity"):
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
        fingerprint = ""
        if shard == "identity":
            helper_source = (root / "ops/ci_identity_template.rs").read_text()
            fingerprint = helper_source.split("// BEGIN MIGRATION FINGERPRINT\n")[1].split("// END MIGRATION FINGERPRINT")[0]
        patched = instrument_core(upstream.decode()) if name == "sqlx-core" else instrument_postgres(upstream.decode(), template_cases(root) if shard == "identity" else (), fingerprint)
        (copy / relative).write_text(patched)
        evidence[name] = {"version": "0.9.0", "upstream_sha256": hashlib.sha256(upstream).hexdigest(), "timed_sha256": hashlib.sha256(patched.encode()).hexdigest()}
        patches.append(f'{name} = {{ path = {json.dumps(str(copy))} }}')
    manifest = snapshot / "Cargo.toml"
    if "[patch.crates-io]" in manifest.read_text():
        raise ValueError("existing crate patches require explicit profiling review")
    helper_bin = '\n[[bin]]\nname = "ci-identity-template"\npath = "ops/ci_identity_template.rs"\n' if shard == "identity" else ""
    manifest.write_text(manifest.read_text() + helper_bin + '\n[patch.crates-io]\n' + "\n".join(patches) + "\n")
    # Preserve every pinned transitive version; only the two reviewed path sources change.
    (snapshot / "Cargo.lock").write_text(profile_lock((root / "Cargo.lock").read_text()))
    subprocess.run(["cargo", "metadata", "--locked", "--offline", "--all-features", "--format-version=1"], cwd=snapshot, check=True, stdout=subprocess.DEVNULL)
    validate_profile_lock(original, locked_packages(snapshot / "Cargo.lock"))
    evidence["unit_cases"] = instrument_units(snapshot) if shard == "identity" else []
    if shard == "lexicon-handler":
        evidence["maximum_batch"] = instrument_maximum_batch(snapshot)
    (state / "source-evidence.json").write_text(json.dumps(evidence, indent=2) + "\n")
    return snapshot


def audit_template_uses(report, cases, template):
    executed = {f"{row['target']}::{row['name']}": row["status"] for row in report["executed"]}
    uses = report.get("template_uses", [])
    if any(executed.get(case) != "ok" for case in cases):
        raise ValueError("template whitelist contains an absent, renamed or failed case")
    if Counter(row["test"] for row in uses) != Counter(cases) or any(row["template"] != template for row in uses):
        raise ValueError("missing, duplicate or non-whitelisted template use")
    return {"cloned_cases": len(uses), "fresh_sqlx_cases": len({row["test"] for row in report["timings"] if row["phase"] == "db_create"}) - len(uses)}


def summarize(report, unit_cases=()):
    results = {(row["target"], row["name"]): row["status"] for row in report["executed"]}
    cases = defaultdict(dict)
    allowed = {"total", "db_setup", "db_create", "migration", "pool_connect", "body", "pool_close", "cleanup", "test_callback"}
    for row in report.get("timings", []):
        target, sep, name = row["test"].partition("::")
        if not sep or (target, name) not in results or row["phase"] not in allowed:
            raise ValueError("timing does not belong to an executed case")
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
        raise ValueError("an executed case lacks timing or unit inventory differs")
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
    parser.add_argument("--state", type=Path)
    args = parser.parse_args()
    args.state = (args.state or Path(os.environ.get("RUNNER_TEMP", str(ROOT / "target"))) / f"{args.shard}-phase-profile").resolve()
    if args.shard not in ("identity", "lexicon-handler"):
        command = [sys.executable, str(ROOT / "ops/ci_test_shards.py"), args.action, args.shard]
        if args.report:
            command += ["--report", str(args.report)]
        subprocess.run(command, check=True)
        return
    snapshot = prepare(ROOT, args.state, args.shard) if args.action == "build" else args.state / "workspace"
    env = {**os.environ, "CARGO_TARGET_DIR": str(args.state / "target"), "GITHUB_SHA": os.environ.get("GITHUB_SHA") or subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()}
    command = [sys.executable, str(snapshot / "ops/ci_test_shards.py"), args.action, args.shard]
    if args.action == "run":
        if not args.report:
            parser.error("profiled run requires --report")
        command += ["--report", str(args.report.resolve())]
    if args.shard == "lexicon-handler":
        subprocess.run(command, cwd=snapshot, env=env, check=True)
        if args.action == "run":
            report = json.loads(args.report.read_text())
            summary = summarize(report)
            summary["source_evidence"] = json.loads((args.state / "source-evidence.json").read_text())
            summary["maximum_batch"] = summarize_maximum_batch(report)
            if report.get("template_uses") or any("migration" not in {t["phase"] for t in report["timings"] if t["test"] == f"{r['target']}::{r['name']}"} for r in report["executed"]):
                raise ValueError("lexicon profiling changed the fresh migration path")
            (args.report.parent / "lexicon-handler-phases.json").write_text(json.dumps(summary, indent=2) + "\n")
            print(json.dumps({"profiled_sqlx_cases": summary["profiled_sqlx_cases"], "cumulative_phase_ms": summary["cumulative_phase_ms"], "maximum_batch": summary["maximum_batch"]}), flush=True)
        return
    helper = args.state / "template-executable.json"
    template_report = args.state / "template-checks.json"
    template = "_tsz_ci_tpl_" + uuid.uuid4().hex
    if args.action == "build":
        build = subprocess.run(["cargo", "build", "--locked", "--all-features", "--bin", "ci-identity-template", "--message-format=json"], cwd=snapshot, env=env, text=True, stdout=subprocess.PIPE)
        items = [json.loads(line) for line in build.stdout.splitlines()]
        for item in items:
            if item.get("reason") == "compiler-message":
                print(item["message"]["rendered"], file=sys.stderr, end="")
        build.check_returncode()
        paths = [item["executable"] for item in items if item.get("reason") == "compiler-artifact" and item.get("executable") and item["target"]["name"] == "ci-identity-template"]
        if len(paths) != 1:
            raise ValueError("template helper executable inventory differs")
        helper.write_text(json.dumps(paths[0]))
        subprocess.run(command, cwd=snapshot, env=env, check=True)
        return
    executable = json.loads(helper.read_text())
    if template_report.exists():
        raise ValueError("template report already exists; use a fresh run state")
    try:
        subprocess.run([executable, "prepare", template, str(template_report)], cwd=snapshot, env=env, check=True)
        checks = json.loads(template_report.read_text())
        env.update(SQLX_CI_TEMPLATE=template, SQLX_CI_TEMPLATE_FINGERPRINT=checks["migration_fingerprint"])
        subprocess.run(command, cwd=snapshot, env=env, check=True)
    finally:
        if template_report.exists():
            cleanup_started = time.monotonic()
            subprocess.run([executable, "cleanup", template, str(template_report)], cwd=snapshot, env=env, check=True)
            checks["cleanup_ms"] = round((time.monotonic() - cleanup_started) * 1000, 3)
    if args.action == "run":
        report = json.loads(args.report.read_text())
        evidence = json.loads((args.state / "source-evidence.json").read_text())
        summary = summarize(report, evidence["unit_cases"])
        summary["source_evidence"] = evidence
        summary["template_pilot"] = {**checks, **audit_template_uses(report, template_cases(ROOT), template)}
        output = args.report.parent / "identity-phases.json"
        output.write_text(json.dumps(summary, indent=2) + "\n")
        print(json.dumps({"profiled_sqlx_cases": summary["profiled_sqlx_cases"], "cumulative_phase_ms": summary["cumulative_phase_ms"]}), flush=True)


if __name__ == "__main__":
    main()
