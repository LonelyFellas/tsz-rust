#!/usr/bin/env python3
"""Independent tshb-test backup job. Installation and scheduling require deployment authorization."""

import argparse
import datetime
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import signal
import time
import sys
import uuid
from urllib.parse import parse_qsl, unquote, urlsplit


class BackupError(Exception):
    pass


class BackupInterrupted(BackupError):
    pass


def database_environment(uri):
    url = urlsplit(uri)
    if url.scheme not in {"postgres", "postgresql"} or not url.hostname or not url.path.lstrip("/"):
        raise BackupError("invalid_database_identity")
    values = {"PATH": os.environ["PATH"], "LANG": "C.UTF-8", "PGHOST": url.hostname,
              "PGPORT": str(url.port or 5432), "PGUSER": unquote(url.username or ""),
              "PGPASSWORD": unquote(url.password or ""), "PGDATABASE": unquote(url.path.lstrip("/")),
              "PGCONNECT_TIMEOUT": "10"}
    allowed = {"sslmode": "PGSSLMODE", "connect_timeout": "PGCONNECT_TIMEOUT",
               "target_session_attrs": "PGTARGETSESSIONATTRS", "channel_binding": "PGCHANNELBINDING",
               "gssencmode": "PGGSSENCMODE"}
    for key, value in parse_qsl(url.query):
        if key not in allowed:
            raise BackupError("unsupported_database_parameter")
        values[allowed[key]] = value
    if not values["PGUSER"] or any("\n" in value or "\r" in value for value in values.values()):
        raise BackupError("invalid_database_parameter")
    return values


def sha256(path):
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1048576), b""):
            value.update(block)
    return value.hexdigest()


def stop_group(process):
    def send(sig):
        try:
            os.killpg(process.pid, sig)
            return True
        except ProcessLookupError:
            return False
    if send(signal.SIGTERM):
        deadline = time.monotonic() + 1
        while time.monotonic() < deadline:
            process.poll()
            if not send(0):
                break
            time.sleep(0.05)
        send(signal.SIGKILL)
    process.wait()


def command(arguments, *, environment=None, output=None, timeout=1800):
    process = None
    try:
        process = subprocess.Popen(arguments, env=environment, stdout=output or subprocess.PIPE,
                                   stderr=subprocess.PIPE, start_new_session=True)
        stdout, _ = process.communicate(timeout=timeout)
        if process.returncode:
            # libpq diagnostics may contain connection details. Never echo them.
            raise BackupError("backup_command_failed")
        return stdout
    except (OSError, subprocess.TimeoutExpired):
        raise BackupError("backup_command_unavailable") from None
    finally:
        if process is not None:
            try:
                stop_group(process)
            finally:
                for stream in (process.stdout, process.stderr):
                    if stream is not None:
                        stream.close()


def write_status(path, value):
    temporary = path.with_suffix(".partial")
    with temporary.open("w") as target:
        os.chmod(temporary, 0o600)
        json.dump(value, target)
        target.flush()
        os.fsync(target.fileno())
    temporary.replace(path)
    descriptor = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def backup(config, *, environment=None):
    if config.get("environment") != "tshb-test":
        raise BackupError("invalid_backup_environment")
    root = Path(config["directory"])
    root.mkdir(parents=True, mode=0o700, exist_ok=True)
    with (root / "job.lock").open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise BackupError("backup_job_busy") from None
        now = datetime.datetime.now(datetime.timezone.utc)
        name = "scheduled-" + now.strftime("%Y%m%dT%H%M%SZ") + "-" + uuid.uuid4().hex
        folder = root / name
        folder.mkdir(mode=0o700)
        status = {"schema_version": 1, "environment": "tshb-test", "backup_id": name,
                  "snapshot_started_at": now.isoformat(), "status": "running"}
        write_status(root / "last-attempt.json", status)
        try:
            if environment is None:
                pid = command(["systemctl", "show", "tsz-rust", "-p", "MainPID", "--value"]).decode().strip()
                if not pid.isdigit() or int(pid) <= 0:
                    raise BackupError("service_not_running")
                values = Path("/proc", pid, "environ").read_bytes().split(b"\0")
                urls = [value.split(b"=", 1)[1].decode() for value in values if value.startswith(b"DATABASE_URL=")]
                if len(urls) != 1:
                    raise BackupError("database_identity_missing")
                environment = database_environment(urls[0])
            safe_environment = {"PATH": os.environ["PATH"], "LANG": "C.UTF-8"}
            major = str(config["postgresql_major"])
            for tool in ("pg_dump", "pg_restore", "psql"):
                version = command([tool, "--version"]).decode()
                if not re.search(r"\b" + re.escape(major) + r"\.", version):
                    raise BackupError("postgresql_tool_version_mismatch")
            server = command(["psql", "-X", "-At", "-v", "ON_ERROR_STOP=1", "-c", "SHOW server_version_num"], environment=environment).decode().strip()
            if not server.isdigit() or int(server) // 10000 != int(major):
                raise BackupError("postgresql_server_version_mismatch")
            partial = folder / "backup.dump.partial"
            with partial.open("xb") as output:
                os.chmod(partial, 0o600)
                command(["pg_dump", "--format=custom", "--compress=3", "--no-owner", "--no-privileges"],
                        environment=environment, output=output)
                output.flush()
                os.fsync(output.fileno())
            command(["pg_restore", "--list", str(partial)])
            archive = folder / "backup.dump"
            partial.replace(archive)
            status.update(sha256=sha256(archive), bytes=archive.stat().st_size,
                          postgresql_major=int(major), archive_readable=True)
            # The reviewed transport must upload, read back/verify and return this contract.
            # It receives paths, never the database connection or service environment.
            transport = config["archive_command"]
            if not isinstance(transport, list) or not transport or not all(isinstance(arg, str) for arg in transport):
                raise BackupError("archive_command_required")
            remote = json.loads(command([*transport, str(archive), status["sha256"]], environment=safe_environment))
            if (remote.get("verified") is not True or remote.get("sha256") != status["sha256"]
                    or remote.get("bytes") != status["bytes"] or not isinstance(remote.get("reference"), str)
                    or not remote["reference"]):
                raise BackupError("offsite_backup_not_verified")
            status.update(status="passed", offsite_reference=remote["reference"],
                          completed_at=datetime.datetime.now(datetime.timezone.utc).isoformat())
            write_status(folder / "receipt.json", status)
            write_status(root / "latest.json", status)
            write_status(root / "last-attempt.json", status)
            return status
        except (BackupError, OSError, ValueError, KeyError):
            status["status"] = "failed"
            write_status(folder / "receipt.json", status)
            write_status(root / "last-attempt.json", status)
            raise


def inspect_status(root, *, now=None):
    try:
        value = json.loads((Path(root) / "latest.json").read_text())
        attempt = json.loads((Path(root) / "last-attempt.json").read_text())
        if attempt.get("status") != "passed" or attempt.get("backup_id") != value.get("backup_id"):
            raise ValueError()
        started = datetime.datetime.fromisoformat(value["snapshot_started_at"])
        age = ((now or datetime.datetime.now(datetime.timezone.utc)) - started).total_seconds()
        if (value.get("environment") != "tshb-test" or value.get("status") != "passed"
                or value.get("archive_readable") is not True or not value.get("offsite_reference")
                or not 0 <= age <= 26 * 3600):
            raise ValueError()
        return {"status": "passed", "backup_id": value["backup_id"], "age_seconds": round(age)}
    except (OSError, ValueError, KeyError, TypeError):
        return {"status": "warning", "stage": "scheduled_backup_missing_stale_or_failed"}


def main():
    import signal
    def interrupted(signum, frame):
        raise BackupInterrupted("backup_interrupted")
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("run", "status"))
    parser.add_argument("--config", type=Path, required=True)
    args = parser.parse_args()
    try:
        config = json.loads(args.config.read_text())
        result = backup(config) if args.command == "run" else inspect_status(config["directory"])
        # Storage references and raw database data stay out of the journal.
        print(json.dumps({key: value for key, value in result.items()
                          if key in {"status", "backup_id", "age_seconds", "stage", "bytes"}}))
        return 0 if result["status"] == "passed" else 1
    except (BackupError, OSError, ValueError, KeyError) as error:
        print(json.dumps({"status": "failed", "stage": str(error) if isinstance(error, BackupError) else type(error).__name__}))
        return 1


if __name__ == "__main__":
    sys.exit(main())
