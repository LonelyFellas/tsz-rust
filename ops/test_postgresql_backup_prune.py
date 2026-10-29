import datetime
import fcntl
import io
import json
from pathlib import Path
import subprocess
import sys
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch

import postgresql_backup_prune as pruning


class PruneTests(unittest.TestCase):
    def test_isolated_python_can_start_the_service_script(self):
        script = Path(pruning.__file__)
        result = subprocess.run([sys.executable, '-I', str(script), '/dev/null'], capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn('local_backup_retention', result.stderr)
        self.assertNotIn('ModuleNotFoundError', result.stderr)

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
                 patch.object(pruning, 'replica_is_current', return_value=True), \
                 patch.object(pruning, 'replica_has_archive', return_value=True):
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

    def test_missing_old_replica_preserves_expired_local_archive(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            name = 'scheduled-20261001T000000Z-' + 'a' * 32
            folder = root / name
            folder.mkdir(mode=0o700)
            archive = folder / 'backup.dump'
            archive.write_bytes(b'archive')
            now = datetime.datetime.now(datetime.timezone.utc)
            (folder / 'receipt.json').write_text(json.dumps({
                'backup_id': name, 'environment': 'tshb-test', 'status': 'passed', 'bytes': 7,
                'snapshot_started_at': (now - datetime.timedelta(days=20)).isoformat(),
                'offsite_reference': 'oss:bucket:key',
            }))
            good_name = 'scheduled-20261001T000000Z-' + 'b' * 32
            good_folder = root / good_name
            good_folder.mkdir(mode=0o700)
            (good_folder / 'backup.dump').write_bytes(b'archive')
            (good_folder / 'receipt.json').write_text(json.dumps({
                'backup_id': good_name, 'environment': 'tshb-test', 'status': 'passed', 'bytes': 7,
                'snapshot_started_at': (now - datetime.timedelta(days=20)).isoformat(),
                'offsite_reference': 'oss:bucket:key',
            }))
            config = {'environment': 'tshb-test', 'directory': directory, 'replica': {}, 'local_retention_days': 14}
            with patch.object(pruning, 'inspect_status', return_value={'status': 'passed', 'backup_id': 'current'}), \
                 patch.object(pruning, 'replica_is_current', return_value=True), \
                 patch.object(pruning, 'replica_has_archive', side_effect=lambda _, r: r['backup_id'] == good_name):
                with self.assertRaisesRegex(ValueError, 'expired_replica_unverified'):
                    pruning.prune(config, now=now)
            self.assertTrue(archive.exists())
            self.assertFalse((good_folder / 'backup.dump').exists())

    def test_unsafe_configuration_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            config = {'environment': 'tshb-test', 'directory': directory, 'replica': {}, 'local_retention_days': 13}
            with self.assertRaisesRegex(ValueError, 'invalid_local_retention'):
                pruning.prune(config)

    def test_each_old_replica_must_match_its_own_sha256(self):
        import hashlib
        contents = b'archive'
        receipt = {'backup_id': 'scheduled-test', 'sha256': hashlib.sha256(contents).hexdigest(), 'bytes': len(contents)}
        config = {'replica': {'storage_config': '/root/oss.json', 'endpoint': 'https://oss-cn-shenzhen.aliyuncs.com',
                              'region': 'cn-shenzhen', 'bucket': 'replica-bucket'}}
        storage = {'endpoint': 'https://oss-cn-heyuan-internal.aliyuncs.com', 'region': 'cn-heyuan',
                   'bucket': 'source-bucket', 'access_key_id': 'id', 'access_key_secret': 'secret'}
        def response(value):
            stream = io.BytesIO(value)
            stream.content_length = len(value)
            stream.headers = {'x-oss-server-side-encryption': 'AES256'}
            return stream
        for value, expected in ((contents, True), (b'bad-data', False)):
            with self.subTest(value=value), patch.object(pruning, 'read_config', return_value=storage), \
                 patch.object(pruning, 'connect', return_value=SimpleNamespace(get_object=lambda _: response(value))):
                self.assertEqual(pruning.replica_has_archive(config, receipt), expected)

    def test_busy_backup_lock_prevents_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            config = {'environment': 'tshb-test', 'directory': directory, 'replica': {}, 'local_retention_days': 14}
            with (Path(directory) / 'job.lock').open('a') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                self.assertIsNone(pruning.prune(config))


if __name__ == '__main__':
    unittest.main()
