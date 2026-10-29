#!/usr/bin/env python3
"""Back up the Agent database using the running API's connection identity."""

import json
import signal
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from postgresql_backup_job import BackupError, BackupInterrupted, backup, command, database_environment


def agent_environment():
    inspect = ["docker", "inspect", "--format", "{{.State.Pid}}", "tsz-agent-api-1"]
    pid = command(inspect).decode().strip()
    if not pid.isdigit() or int(pid) <= 0:
        raise BackupError("agent_service_not_running")
    values = Path("/proc", pid, "environ").read_bytes().split(b"\0")
    urls = [value.split(b"=", 1)[1].decode() for value in values
            if value.startswith(b"TSZ_AGENT_DATABASE_URL=")]
    if len(urls) != 1 or command(inspect).decode().strip() != pid:
        raise BackupError("agent_database_identity_missing")
    environment = database_environment(urls[0].replace("postgresql+psycopg://", "postgresql://", 1))
    if (environment["PGHOST"] != "127.0.0.1" or environment["PGPORT"] != "5433"
            or environment["PGDATABASE"] != "tsz_agent"):
        raise BackupError("agent_database_identity_mismatch")
    return environment


def main():
    def interrupted(signum, frame):
        raise BackupInterrupted("backup_interrupted")
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    try:
        if len(sys.argv) != 2:
            raise BackupError("invalid_arguments")
        config = json.loads(Path(sys.argv[1]).read_text())
        if config.get("verification_kind") != "agent" or config.get("health_metric") != "tsz_agent_backup_health":
            raise BackupError("invalid_agent_backup_config")
        result = backup(config, environment=agent_environment)
        print(json.dumps({key: result[key] for key in ("status", "backup_id", "bytes")}))
        return 0
    except (BackupError, OSError, ValueError, KeyError) as error:
        print(json.dumps({"status": "failed", "stage": str(error) if isinstance(error, BackupError) else type(error).__name__}))
        return 1


if __name__ == "__main__":
    sys.exit(main())
