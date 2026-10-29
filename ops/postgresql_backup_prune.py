#!/usr/bin/env python3
"""Remove expired local scheduled dumps only after the current replica is verified."""

import datetime
import fcntl
import json
import os
from pathlib import Path
import re
import stat
import sys

from backup_alert import replica_is_current
from postgresql_backup_job import inspect_status


def prune(config, *, now=None):
    if config.get('environment') != 'tshb-test' or not isinstance(config.get('replica'), dict):
        raise ValueError('replica_configuration_required')
    days = config.get('local_retention_days')
    if isinstance(days, bool) or not isinstance(days, int) or days < 14:
        raise ValueError('invalid_local_retention')
    root = Path(config['directory'])
    info = root.lstat()
    if not root.is_absolute() or not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o077:
        raise ValueError('insecure_backup_directory')
    now = now or datetime.datetime.now(datetime.timezone.utc)
    with (root / 'job.lock').open('a') as backup_lock, (root / 'restore.lock').open('a') as restore_lock:
        for lock in (backup_lock, restore_lock):
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        status = inspect_status(root, now=now)
        if status['status'] != 'passed' or not replica_is_current(config):
            raise ValueError('current_verified_replica_required')
        removed = 0
        for folder in root.iterdir():
            if not re.fullmatch(r'scheduled-\d{8}T\d{6}Z-[0-9a-f]{32}', folder.name):
                continue
            folder_info = folder.lstat()
            if (not stat.S_ISDIR(folder_info.st_mode) or folder_info.st_uid != os.geteuid()
                    or folder_info.st_mode & 0o077 or folder.name == status['backup_id']):
                continue
            try:
                receipt = json.loads((folder / 'receipt.json').read_text())
                started = datetime.datetime.fromisoformat(receipt['snapshot_started_at'])
                archive = folder / 'backup.dump'
                archive_info = archive.lstat()
                if (receipt.get('backup_id') != folder.name or receipt.get('environment') != 'tshb-test'
                        or receipt.get('status') != 'passed' or not receipt.get('offsite_reference')
                        or (now - started).total_seconds() < days * 86400
                        or not stat.S_ISREG(archive_info.st_mode) or archive_info.st_uid != os.geteuid()
                        or archive_info.st_nlink != 1 or archive_info.st_size != receipt.get('bytes')):
                    continue
            except (OSError, ValueError, KeyError, TypeError):
                continue
            archive.unlink()
            fd = os.open(folder, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
            removed += 1
        return removed


if __name__ == '__main__':
    try:
        with open(sys.argv[1]) as source:
            count = prune(json.load(source))
        print(json.dumps({'status': 'passed', 'removed_local_archives': count}))
    except (OSError, ValueError, KeyError, IndexError):
        print(json.dumps({'status': 'failed', 'stage': 'local_backup_retention'}), file=sys.stderr)
        raise SystemExit(1)
