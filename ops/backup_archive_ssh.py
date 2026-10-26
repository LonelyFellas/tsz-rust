#!/usr/bin/env python3
"""Upload a fixed backup to reviewed SSH storage and verify its remote bytes."""

import json
from pathlib import Path
import re
import subprocess
import sys


def archive(config_path, file_path, expected):
    config = json.loads(Path(config_path).read_text())
    host, directory = config["host"], config["directory"]
    file = Path(file_path)
    if (not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", host)
            or not re.fullmatch(r"/[A-Za-z0-9/_-]+", directory)
            or not re.fullmatch(r"[0-9a-f]{64}", expected)
            or file.name != "backup.dump" or not file.parent.name.startswith("scheduled-")):
        raise ValueError("invalid_archive_destination")
    remote = directory.rstrip("/") + "/" + file.parent.name
    if not re.fullmatch(r"/[A-Za-z0-9/_-]+", remote):
        raise ValueError("invalid_backup_identity")
    ssh = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "StrictHostKeyChecking=yes", host]
    def run(arguments, timeout=1800):
        result = subprocess.run(arguments, capture_output=True, text=True, timeout=timeout, check=False)
        if result.returncode:
            raise ValueError("archive_transport_failed")
        return result.stdout.strip()
    run([*ssh, "umask 077; mkdir -p '" + remote + "'"])
    run(["rsync", "--partial", "--timeout=180", "--chmod=F600,D700", "-e",
         "ssh -o BatchMode=yes -o ConnectTimeout=10 -o StrictHostKeyChecking=yes",
         str(file), host + ":" + remote + "/backup.dump.partial"])
    raw = run([*ssh, "sha256sum '" + remote + "/backup.dump.partial'; wc -c < '" + remote + "/backup.dump.partial'"])
    lines = raw.splitlines()
    if len(lines) != 2 or lines[0].split()[0] != expected or int(lines[1]) != file.stat().st_size:
        raise ValueError("archive_digest_mismatch")
    run([*ssh, "mv '" + remote + "/backup.dump.partial' '" + remote + "/backup.dump'"])
    return {"verified": True, "sha256": expected, "bytes": file.stat().st_size,
            "reference": "ssh:" + host + ":" + remote + "/backup.dump"}


def fetch(config_path, backup_id, expected, destination):
    config = json.loads(Path(config_path).read_text())
    host, directory = config['host'], config['directory']
    if (not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]*', host)
            or not re.fullmatch(r'/[A-Za-z0-9/_-]+', directory)
            or not re.fullmatch(r'scheduled-[A-Za-z0-9_-]+', backup_id)
            or not re.fullmatch(r'[0-9a-f]{64}', expected) or Path(destination).exists()):
        raise ValueError('invalid_archive_fetch')
    remote = directory.rstrip('/') + '/' + backup_id + '/backup.dump'
    result = subprocess.run(['rsync', '--timeout=180', '--chmod=F600', '-e',
                             'ssh -o BatchMode=yes -o ConnectTimeout=10 -o StrictHostKeyChecking=yes',
                             host + ':' + remote, str(destination)],
                            capture_output=True, timeout=1800, check=False)
    if result.returncode:
        raise ValueError('archive_fetch_failed')
    # The restore job independently checks bytes and SHA256 before using this file.
    return {'retrieved': True}


if __name__ == "__main__":
    try:
        if sys.argv[1:2] == ['fetch']:
            print(json.dumps(fetch(*sys.argv[2:])))
        else:
            print(json.dumps(archive(*sys.argv[1:])))
    except (OSError, ValueError, KeyError, subprocess.TimeoutExpired):
        print('{"verified":false}', file=sys.stderr)
        sys.exit(1)
