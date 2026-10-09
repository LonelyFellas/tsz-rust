#!/usr/bin/env python3
"""Synthetic local test data only; never restores into an existing database/container."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import uuid


class DrillError(Exception):
    pass


def command(stage, args, *, data=None, output=None):
    try:
        result = subprocess.run(args, input=data, stdout=output or subprocess.PIPE,
                                stderr=subprocess.PIPE, timeout=180, check=False)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise DrillError(stage) from error
    if result.returncode:
        # Child diagnostics can contain credentials, object names or business rows.
        raise DrillError(stage)
    return result.stdout


def docker(stage, *args, **kwargs):
    return command(stage, ["docker", *args], **kwargs)


def sql(container, database, statement):
    return docker("sql_check", "exec", "-i", container, "psql", "-X", "-qAt",
                  "-v", "ON_ERROR_STOP=1", "-U", "postgres", "-d", database,
                  data=("SET timezone='UTC';\n" + statement).encode()).decode().strip()


def quoted(identifier):
    return '"' + identifier.replace('"', '""') + '"'


def summary(container, database):
    tables = json.loads(sql(container, database, """
      SELECT coalesce(json_agg(json_build_array(schemaname,tablename) ORDER BY schemaname,tablename),'[]')
      FROM pg_tables WHERE schemaname NOT IN ('pg_catalog','information_schema');
    """))
    result = {}
    for schema, table in tables:
        result[f"{schema}.{table}"] = json.loads(sql(container, database, f"""
          SELECT json_build_object('rows',count(*),'checksum',
            md5(coalesce(string_agg(md5(to_jsonb(t)::text),'' ORDER BY md5(to_jsonb(t)::text)),'')))
          FROM {quoted(schema)}.{quoted(table)} t;
        """))
    required = {"public.coin_wallets", "public.coin_entries", "public.coin_operations", "public._sqlx_migrations"}
    if not required.issubset(result):
        raise DrillError("representative_schema_missing")
    # Keep reconciliation identical to the application's single-snapshot invariant query.
    query = (Path(__file__).resolve().parents[1] / "src/coins/reconciliation.sql").read_text()
    ledger = json.loads(sql(container, database, f"SELECT row_to_json(r) FROM ({query}) r;"))
    if any(ledger[key] != 0 for key in ("wallet_mismatches", "running_balance_mismatches", "operation_mismatches")) or ledger["total_balance"] != ledger["net_issuance"]:
        raise DrillError("ledger_invariant_failed")
    if result["public.coin_entries"]["rows"] == 0:
        raise DrillError("representative_ledger_missing")
    schema = docker("schema_dump", "exec", container, "pg_dump", "-U", "postgres", "-d", database,
                    "--schema-only", "--no-owner", "--no-privileges").decode()
    # PostgreSQL dump restrict keys are random, not schema content.
    schema = '\n'.join(line for line in schema.splitlines() if not line.startswith(('\\restrict ', '\\unrestrict ')))
    migrations = json.loads(sql(container, database, "SELECT json_agg(json_build_object('version',version,'success',success,'checksum',encode(checksum,'hex')) ORDER BY version) FROM _sqlx_migrations;"))
    return {"tables": result, "schema_sha256": hashlib.sha256(schema.encode()).hexdigest(),
            "ledger": ledger, "migrations": migrations}


def validate(args):
    if not args.synthetic_test_source or not re.fullmatch(r"tsz-reliability-[a-z0-9-]+", args.source_container):
        raise DrillError("explicit_synthetic_test_source_required")
    if not re.fullmatch(r"[a-z][a-z0-9_]*_test", args.database):
        raise DrillError("test_database_required")
    if not re.fullmatch(r"sha256:[0-9a-f]{64}|[^\s]+@sha256:[0-9a-f]{64}", args.image):
        raise DrillError("immutable_cached_image_required")
    if not re.fullmatch(r"tsz-restore-drill-[a-z0-9-]+", args.target):
        raise DrillError("isolated_target_name_required")
    if args.target == args.source_container:
        raise DrillError("source_is_not_a_target")


def drill(args):
    validate(args)
    os.umask(0o077)
    args.output.mkdir(mode=0o700, parents=False, exist_ok=False)
    created = None
    started = time.time()
    report = {"status": "failed", "started_at": started, "target": args.target}
    report["code_revision"] = command("code_revision", ["git", "rev-parse", "HEAD"]).decode().strip()
    report["tool_sha256"] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    try:
        source_info = json.loads(docker("inspect_source", "inspect", args.source_container))[0]
        if not source_info["State"]["Running"]:
            raise DrillError("source_not_running")
        image = json.loads(docker("inspect_cached_image", "image", "inspect", args.image))[0]
        if source_info["Image"] != image["Id"]:
            raise DrillError("source_target_image_mismatch")
        report["image"] = image["Id"]
        report["pg_dump_version"] = docker("dump_version", "exec", args.source_container, "pg_dump", "--version").decode().strip()
        report["pg_restore_version"] = docker("restore_version", "exec", args.source_container, "pg_restore", "--version").decode().strip()
        report["postgres_version"] = sql(args.source_container, args.database, "SHOW server_version;")
        existing = docker("check_target_absent", "container", "ls", "-a", "--filter", f"name=^/{args.target}$", "--format", "{{.ID}}")
        if existing.strip():
            raise DrillError("target_already_exists")
        before = summary(args.source_container, args.database)
        backup = args.output / "backup.dump"
        with backup.open("xb") as output:
            if args.backup:
                with args.backup.open("rb") as original:
                    import shutil
                    shutil.copyfileobj(original, output)
            else:
                docker("backup", "exec", args.source_container, "pg_dump", "-U", "postgres", "-d", args.database,
                       "--format=custom", "--no-owner", "--no-privileges", output=output)
        report["backup_sha256"] = hashlib.sha256(backup.read_bytes()).hexdigest()
        created = docker("create_isolated_target", "create", "--name", args.target, "--network", "none",
                         "--tmpfs", "/var/lib/postgresql", "-e", "POSTGRES_HOST_AUTH_METHOD=trust",
                         "-e", "PGDATA=/var/lib/postgresql/drill", "-e", "POSTGRES_DB=restore_test",
                         args.image).decode().strip()
        docker("start_target", "start", created)
        for attempt in range(60):
            try:
                sql(created, "restore_test", "SELECT 1;")
                break
            except DrillError:
                if attempt == 59:
                    raise DrillError("target_readiness_timeout")
                time.sleep(0.5)
        payload = backup.read_bytes()
        docker("validate_archive", "exec", "-i", created, "pg_restore", "--list", data=payload)
        docker("restore", "exec", "-i", created, "pg_restore", "-U", "postgres", "-d", "restore_test",
               "--exit-on-error", "--single-transaction", "--no-owner", "--no-privileges", data=payload)
        restored = summary(created, "restore_test")
        if before != restored:
            raise DrillError("restored_summary_mismatch")
        if before != summary(args.source_container, args.database):
            raise DrillError("source_changed_during_drill")
        report.update(status="passed", evidence=restored)
    except (DrillError, KeyboardInterrupt) as error:
        report["failed_stage"] = str(error) if isinstance(error, DrillError) else "interrupted"
        raise
    finally:
        if created:
            # Use only the ID returned by our create. Never remove an existing named target.
            try:
                docker("cleanup", "rm", "-f", "-v", created)
                report["target_removed"] = True
            except DrillError:
                report.update(status="failed", failed_stage="cleanup", target_removed=False)
        report["completed_at"] = time.time()
        report["elapsed_seconds"] = round(time.time() - started, 3)
        (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    if report["status"] != "passed":
        raise DrillError(report["failed_stage"])
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--synthetic-test-source", action="store_true")
    parser.add_argument("--source-container", required=True)
    parser.add_argument("--database", required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--target", default="tsz-restore-drill-" + uuid.uuid4().hex)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--backup", type=Path, help="Validate a prior test archive, including corruption drills")
    try:
        report = drill(parser.parse_args())
        print(json.dumps({"status": report["status"], "elapsed_seconds": report["elapsed_seconds"]}))
        return 0
    except (DrillError, OSError, ValueError, KeyboardInterrupt) as error:
        print(json.dumps({"status": "failed", "stage": str(error) if isinstance(error, DrillError) else "local_io_or_interruption"}))
        return 1


if __name__ == "__main__":
    sys.exit(main())
