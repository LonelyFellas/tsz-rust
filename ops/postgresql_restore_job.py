#!/usr/bin/env python3
"""Restore one exact offsite backup in a task-owned, network-isolated Docker database."""

import argparse
import datetime
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import sys
import tempfile
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
from postgresql_backup_job import BackupError, BackupInterrupted, command, sha256, write_status


def check_capacity(root, docker_root, archive_bytes):
    if isinstance(archive_bytes, bool) or not isinstance(archive_bytes, int) or archive_bytes <= 0:
        raise BackupError("invalid_backup_size")
    volumes = Path(docker_root) / "volumes"
    if not Path(docker_root).is_absolute() or not volumes.is_dir():
        raise BackupError("docker_volume_path_unknown")
    # Reserve disk for both the compressed copy and the larger restored database.
    download_budget = archive_bytes + 5 * 1024 ** 3
    restore_budget = 20 * archive_bytes + 2 * 1024 ** 3
    if os.stat(root).st_dev == os.stat(volumes).st_dev:
        if shutil.disk_usage(root).free < download_budget + restore_budget:
            raise BackupError("restore_capacity_insufficient")
    elif (shutil.disk_usage(root).free < download_budget
          or shutil.disk_usage(volumes).free < restore_budget):
        raise BackupError("restore_capacity_insufficient")


def cleanup_containers(owner, identifier):
    selector = "label=tsz.restore-job=" + owner
    identifiers = set(command(["docker", "ps", "-aq", "--no-trunc", "--filter", selector]).decode().split())
    if identifier:
        identifiers.add(identifier)
    for value in identifiers:
        inspected = json.loads(command(["docker", "inspect", value]))[0]
        if inspected["Config"]["Labels"].get("tsz.restore-job") != owner:
            raise BackupError("restore_cleanup_owner_mismatch")
        command(["docker", "rm", "-f", "-v", value])
    if command(["docker", "ps", "-aq", "--no-trunc", "--filter", selector]).strip():
        raise BackupError("restore_cleanup_incomplete")


def restore(config):
    if config.get("environment") != "tshb-test":
        raise BackupError("invalid_restore_environment")
    root = Path(config["directory"])
    root.mkdir(parents=True, mode=0o700, exist_ok=True)
    with (root / "restore.lock").open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise BackupError("restore_job_busy") from None
        image = config["restore_image"]
        if not re.fullmatch(r"(?:[A-Za-z0-9./:_-]+@)?sha256:[0-9a-f]{64}", image):
            raise BackupError("immutable_restore_image_required")
        command(["docker", "image", "inspect", image])
        backup = json.loads((root / "latest.json").read_text())
        if backup.get("environment") != "tshb-test" or backup.get("status") != "passed":
            raise BackupError("verified_backup_required")
        docker_root = command(["docker", "info", "--format", "{{.DockerRootDir}}"]).decode().strip()
        check_capacity(root, docker_root, backup.get("bytes"))
        identifier = None
        create_attempted = False
        verified = False
        owner = uuid.uuid4().hex
        temporary = Path(tempfile.mkdtemp(prefix="restore-", dir=root))
        result = {"schema_version": 1, "backup_id": backup["backup_id"], "status": "running",
                  "owner": owner, "temporary_directory": str(temporary),
                  "image": image, "started_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                  "cleanup_complete": False}
        try:
            write_status(root / "restore-latest.json", result)
            archive = temporary / "backup.dump"
            fetch = config["fetch_command"]
            if not isinstance(fetch, list) or not fetch or not all(isinstance(arg, str) for arg in fetch):
                raise BackupError("fetch_command_required")
            fetch_args = [*fetch, backup["backup_id"], backup["sha256"]]
            if config.get("fetch_expected_bytes") is True:
                fetch_args.append(str(backup["bytes"]))
            command([*fetch_args, str(archive)],
                    environment={"PATH": os.environ["PATH"], "LANG": "C.UTF-8"})
            if (not archive.is_file() or archive.stat().st_size != backup["bytes"]
                    or sha256(archive) != backup["sha256"]):
                raise BackupError("retrieved_backup_mismatch")
            check_capacity(root, docker_root, backup["bytes"])
            create_attempted = True
            identifier = command(["docker", "create", "--label", "tsz.restore-job=" + owner,
                                  "--network", "none", "--cpus", "1", "--memory", "2g",
                                  "-e", "POSTGRES_HOST_AUTH_METHOD=trust", "-e", "POSTGRES_DB=restore_verify",
                                  "-e", "PGDATA=/var/lib/postgresql/verification",
                                  "--mount", "type=volume,destination=/var/lib/postgresql",
                                  "--mount", "type=bind,source=" + str(archive) + ",destination=/backup.dump,readonly",
                                  image]).decode().strip()
            command(["docker", "start", identifier])
            for attempt in range(60):
                try:
                    ready = command(["docker", "exec", identifier, "psql", "-X", "-At", "-v", "ON_ERROR_STOP=1",
                                     "-h", "127.0.0.1", "-U", "postgres", "-d", "restore_verify", "-c", "SELECT 1"], timeout=5)
                    if ready.strip() != b"1":
                        raise BackupError("restore_database_not_ready")
                    break
                except BackupInterrupted:
                    raise
                except BackupError:
                    if attempt == 59:
                        raise BackupError("restore_database_not_ready") from None
                    time.sleep(0.5)
            version = command(["docker", "exec", identifier, "pg_restore", "--version"]).decode()
            if not re.search(r"\b" + str(backup["postgresql_major"]) + r"\.", version):
                raise BackupError("restore_tool_version_mismatch")
            command(["docker", "exec", identifier, "pg_restore", "--exit-on-error", "--single-transaction",
                     "--no-owner", "--no-privileges", "-U", "postgres", "-d", "restore_verify", "/backup.dump"])
            query = "SELECT count(*) FROM information_schema.tables WHERE table_schema='public';"
            tables = int(command(["docker", "exec", identifier, "psql", "-X", "-At", "-v", "ON_ERROR_STOP=1",
                                  "-U", "postgres", "-d", "restore_verify", "-c", query]).decode().strip())
            ledger = command(["docker", "exec", identifier, "psql", "-X", "-At", "-v", "ON_ERROR_STOP=1",
                              "-U", "postgres", "-d", "restore_verify", "-c",
                              "SELECT count(*),count(*) FILTER (WHERE NOT success) FROM _sqlx_migrations;"]).decode().strip()
            count, failures = map(int, ledger.split("|"))
            if tables == 0 or count == 0 or failures != 0:
                raise BackupError("restored_schema_or_ledger_invalid")
            result.update(archive_sha256=backup["sha256"], table_count=tables,
                          migration_count=count, restore_exit_code=0)
            verified = True
        finally:
            cleanup_error = None
            try:
                if create_attempted:
                    cleanup_containers(owner, identifier)
            except (BackupError, OSError, ValueError, KeyError) as error:
                cleanup_error = error
            try:
                shutil.rmtree(temporary)
            except OSError as error:
                cleanup_error = cleanup_error or error
            result["cleanup_complete"] = cleanup_error is None
            result["status"] = "passed" if verified and cleanup_error is None else "failed"
            result["completed_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
            write_status(root / "restore-latest.json", result)
            if cleanup_error is not None:
                raise cleanup_error
        return result


def main():
    import signal
    def interrupted(signum, frame):
        raise BackupInterrupted("restore_interrupted")
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True)
    args = parser.parse_args()
    try:
        result = restore(json.loads(args.config.read_text()))
        print(json.dumps({key: result[key] for key in ("status", "backup_id", "cleanup_complete", "table_count", "migration_count")}))
        return 0
    except (BackupError, OSError, ValueError, KeyError) as error:
        print(json.dumps({"status": "failed", "stage": str(error) if isinstance(error, BackupError) else type(error).__name__}))
        return 1


if __name__ == "__main__":
    sys.exit(main())
