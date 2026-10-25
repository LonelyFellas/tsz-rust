#!/usr/bin/env python3
"""Capture tshb-test deployment metadata over SSH; no login, migration or server writes."""

import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import urllib.parse


class BaselineError(Exception):
    pass


def run(arguments, *, data=None, environment=None):
    result = subprocess.run(arguments, input=data, env=environment, capture_output=True,
                            text=True, timeout=30, check=False)
    if result.returncode:
        raise BaselineError("metadata_command_failed")
    return result.stdout


def file_digest(path):
    value = hashlib.sha256()
    with open(path, "rb") as source:
        for block in iter(lambda: source.read(1048576), b""):
            value.update(block)
    return value.hexdigest()


def remote_snapshot():
    if Path("/opt/tsz-rust/deploy.lock").exists():
        raise BaselineError("deployment_in_progress")
    manifest_path = Path("/opt/tsz-deploy-manifests/api.json")
    manifest_bytes = manifest_path.read_bytes()
    manifest = json.loads(manifest_bytes)
    pid = run(["systemctl", "show", "tsz-rust", "-p", "MainPID", "--value"]).strip()
    if not pid.isdigit() or int(pid) <= 0:
        raise BaselineError("service_not_running")
    process = Path("/proc") / pid
    values = dict(item.split(b"=", 1) for item in (process / "environ").read_bytes().split(b"\0") if b"=" in item)
    uri = urllib.parse.urlsplit(values[b"DATABASE_URL"].decode())
    if uri.scheme not in ("postgresql", "postgres") or not uri.hostname:
        raise BaselineError("unsupported_database_configuration")
    environment = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "LC_ALL": "C",
                   "PGHOST": uri.hostname, "PGPORT": str(uri.port or 5432),
                   "PGDATABASE": urllib.parse.unquote(uri.path.lstrip("/")),
                   "PGUSER": urllib.parse.unquote(uri.username or ""),
                   "PGPASSWORD": urllib.parse.unquote(uri.password or ""),
                   "PGCONNECT_TIMEOUT": "8", "PGAPPNAME": "tsz-artifact-readonly-baseline",
                   "PGOPTIONS": "-c default_transaction_read_only=on -c statement_timeout=10000 -c lock_timeout=2000"}
    options = dict(urllib.parse.parse_qsl(uri.query))
    for name, variable in {"sslmode": "PGSSLMODE", "sslrootcert": "PGSSLROOTCERT",
                           "sslcert": "PGSSLCERT", "sslkey": "PGSSLKEY",
                           "channel_binding": "PGCHANNELBINDING"}.items():
        if name in options:
            environment[variable] = options[name]
    query = """
BEGIN READ ONLY;
SELECT json_build_object(
  'status','ok','server_version',current_setting('server_version'),
  'server_version_num',current_setting('server_version_num'),
  'transaction_read_only',current_setting('transaction_read_only'),
  'migration_ledger',(SELECT json_agg(json_build_object(
    'version',version,'checksum',encode(checksum,'hex'),'success',success
  ) ORDER BY version) FROM _sqlx_migrations)
);
ROLLBACK;
"""
    metadata = json.loads(run(["psql", "-X", "-qAt", "-v", "ON_ERROR_STOP=1"], data=query, environment=environment))
    binary = Path("/opt/tsz-rust/target/release/tsz-rust")
    snapshot = {
        "schema_version": 1, "captured_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "target": "tshb-test", "deployment_manifest": manifest, "postgresql": metadata,
        "service": {"file_sha256": file_digest(binary), "process_sha256": file_digest(process / "exe")},
        "pid_stable": pid == run(["systemctl", "show", "tsz-rust", "-p", "MainPID", "--value"]).strip(),
        "deployment_lock_present": Path("/opt/tsz-rust/deploy.lock").exists(),
    }
    if manifest_bytes != manifest_path.read_bytes() or not snapshot["pid_stable"] or snapshot["deployment_lock_present"]:
        raise BaselineError("deployment_changed_during_capture")
    expected = manifest["artifact"]["sha256"]
    if snapshot["service"]["file_sha256"] != expected or snapshot["service"]["process_sha256"] != expected:
        raise BaselineError("running_artifact_differs_from_manifest")
    return snapshot


def capture(output):
    if output.exists():
        raise BaselineError("baseline_output_already_exists")
    source = Path(__file__).read_text()
    text = run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "StrictHostKeyChecking=yes",
                "tshb-test", "python3 -I - --remote"], data=source)
    value = json.loads(text)
    from deployment_manifest import validate_manifest
    from artifact_test_runtime import write_receipt
    validate_manifest(value["deployment_manifest"])
    if value.get("target") != "tshb-test" or value["postgresql"]["transaction_read_only"] != "on":
        raise BaselineError("invalid_readonly_baseline")
    write_receipt(output, value)
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--remote", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    try:
        if args.remote:
            print(json.dumps(remote_snapshot(), sort_keys=True))
        else:
            if args.output is None:
                parser.error("--output is required")
            value = capture(args.output)
            print(json.dumps({"status": "captured", "git_sha": value["deployment_manifest"]["source"]["git_sha"], "read_only": True}))
    except (BaselineError, OSError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
        print(json.dumps({"status": "failed", "stage": str(error) if isinstance(error, BaselineError) else type(error).__name__}))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
