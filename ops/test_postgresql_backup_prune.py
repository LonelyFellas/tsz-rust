import datetime
import fcntl
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import postgresql_backup_prune as pruning


class PruneTests(unittest.TestCase):
    def test_only_expired_owned_successful_archives_are_removed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            now = datetime.datetime.now(datetime.timezone.utc)
            names = [f'scheduled-20261001T000000Z-{n:032x}' for n in range(1, 5)]
            for index, name in enumerate(names):
                folder = root / name
                folder.mkdir(mode=0o700)
                (folder / 'backup.dump').write_bytes(b'archive')
                receipt = {'backup_id': name, 'environment': 'tshb-test',
                           'status': 'failed' if index == 2 else 'passed',
                           'snapshot_started_at': (now - datetime.timedelta(days=20 if index != 1 else 2)).isoformat(),
                           'offsite_reference': 'oss:bucket:key', 'bytes': 7}
                (folder / 'receipt.json').write_text(json.dumps(receipt))
            config = {'environment': 'tshb-test', 'directory': directory, 'replica': {}, 'local_retention_days': 14}
            with patch.object(pruning, 'inspect_status', return_value={'status': 'passed', 'backup_id': names[3]}), \
                 patch.object(pruning, 'replica_is_current', return_value=True):
                self.assertEqual(pruning.prune(config, now=now), 1)
            self.assertFalse((root / names[0] / 'backup.dump').exists())
            self.assertTrue((root / names[0] / 'receipt.json').exists())
            for name in names[1:]:
                self.assertTrue((root / name / 'backup.dump').exists())

    def test_unverified_replica_stops_before_deleting(self):
        with tempfile.TemporaryDirectory() as directory:
            config = {'environment': 'tshb-test', 'directory': directory, 'replica': {}, 'local_retention_days': 14}
            with patch.object(pruning, 'inspect_status', return_value={'status': 'passed', 'backup_id': 'current'}), \
                 patch.object(pruning, 'replica_is_current', return_value=False):
                with self.assertRaisesRegex(ValueError, 'current_verified_replica_required'):
                    pruning.prune(config)

    def test_unsafe_configuration_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            config = {'environment': 'tshb-test', 'directory': directory, 'replica': {}, 'local_retention_days': 13}
            with self.assertRaisesRegex(ValueError, 'invalid_local_retention'):
                pruning.prune(config)

    def test_busy_backup_lock_prevents_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            config = {'environment': 'tshb-test', 'directory': directory, 'replica': {}, 'local_retention_days': 14}
            with (Path(directory) / 'job.lock').open('a') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                with self.assertRaises(BlockingIOError):
                    pruning.prune(config)


if __name__ == '__main__':
    unittest.main()
