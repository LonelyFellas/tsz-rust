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

sys.path.insert(0, str(Path(__file__).resolve().parent))
from backup_alert import replica_is_current
from backup_archive_oss import connect, digest_stream, identity, read_config, validate_config
from postgresql_backup_job import inspect_status


def replica_has_archive(config, receipt):
    try:
        replica = config['replica']
        storage = read_config(Path(replica['storage_config']))
        target = validate_config({**storage, 'endpoint': replica['endpoint'],
                                  'region': replica['region'], 'bucket': replica['bucket']})
        if target['bucket'] == storage['bucket'] or target['region'] == storage['region']:
            return False
        key = identity(receipt['backup_id'], receipt['sha256'])
        response = connect(target).get_object(key)
        if (response.content_length != receipt['bytes']
                or response.headers.get('x-oss-server-side-encryption') != 'AES256'):
            response.close()
            return False
        return digest_stream(response, maximum=receipt['bytes']) == (receipt['sha256'], receipt['bytes'])
    except Exception:
        return False


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
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                return None
        status = inspect_status(root, now=now)
        if status['status'] != 'passed' or not replica_is_current(config):
            raise ValueError('current_verified_replica_required')
        removed = 0
        unverified = False
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
            if not replica_has_archive(config, receipt):
                unverified = True
                continue
            archive.unlink()
            fd = os.open(folder, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
            removed += 1
        if unverified:
            raise ValueError('expired_replica_unverified')
        return removed


if __name__ == '__main__':
    try:
        with open(sys.argv[1]) as source:
            count = prune(json.load(source))
        print(json.dumps({'status': 'skipped' if count is None else 'passed',
                          'removed_local_archives': count}))
    except (OSError, ValueError, KeyError, IndexError):
        print(json.dumps({'status': 'failed', 'stage': 'local_backup_retention'}), file=sys.stderr)
        raise SystemExit(1)
