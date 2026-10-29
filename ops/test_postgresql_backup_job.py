import datetime
import os
import signal
import subprocess
import time
import contextlib
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent))
import postgresql_backup_job as job
import backup_archive_ssh as archive_ssh
import backup_archive_oss as archive_oss
import backup_alert
import postgresql_restore_job as restore_job
from deploy_test import measured_phase


class BackupJobTests(unittest.TestCase):
    def config(self, root):
        return {"environment": "tshb-test", "directory": str(root), "postgresql_major": 18,
                "archive_command": ["/reviewed/offsite-upload"]}

    def fake_command(self, args, **kwargs):
        if args[0] in ("pg_dump", "pg_restore", "psql") and args[1:] == ["--version"]:
            return (args[0] + " (PostgreSQL) 18.6\n").encode()
        if args[0] == "psql":
            return b"180006\n"
        if args[0] == "pg_dump":
            kwargs["output"].write(b"test-archive")
            return None
        if args[0] == "pg_restore":
            return b"archive catalog"
        archive = Path(args[-2])
        return json.dumps({"verified": True, "sha256": args[-1], "bytes": archive.stat().st_size,
                           "reference": "offsite:test/backup"}).encode()

    def test_database_uri_is_parsed_into_libpq_environment_not_arguments(self):
        value = job.database_environment('postgresql://test:p%40ss@127.0.0.1:5439/example?sslmode=require')
        self.assertEqual(value['PGDATABASE'], 'example')
        self.assertEqual(value['PGHOST'], '127.0.0.1')
        self.assertEqual(value['PGPORT'], '5439')
        self.assertEqual(value['PGPASSWORD'], 'p@ss')
        self.assertEqual(value['PGSSLMODE'], 'require')
        with self.assertRaisesRegex(job.BackupError, 'unsupported_database_parameter'):
            job.database_environment('postgresql://test@localhost/example?options=unsafe')

    def test_job_publishes_status_only_after_verified_offsite_copy(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(job, "command", side_effect=self.fake_command):
            root = Path(directory)
            result = job.backup(self.config(root), environment={"PGDATABASE": "secret"})
            self.assertEqual(result["status"], "passed")
            self.assertEqual(job.inspect_status(root)["status"], "passed")
            self.assertNotIn("secret", (root / "latest.json").read_text())
            archive = next(root.glob("scheduled-*/backup.dump"))
            self.assertEqual(archive.stat().st_mode & 0o777, 0o600)

    def test_offsite_failure_retains_last_good_status_and_reports_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "latest.json").write_text('{"old":true}')
            def command(args, **kwargs):
                if args[0] == "/reviewed/offsite-upload":
                    return b'{"verified":false}'
                return self.fake_command(args, **kwargs)
            with patch.object(job, "command", side_effect=command):
                with self.assertRaisesRegex(job.BackupError, "offsite_backup_not_verified"):
                    job.backup(self.config(root), environment={})
            self.assertEqual(json.loads((root / "latest.json").read_text()), {"old": True})
            self.assertEqual(json.loads(next(root.glob("scheduled-*/receipt.json")).read_text())["status"], "failed")
            self.assertEqual(json.loads((root / 'last-attempt.json').read_text())['status'], 'failed')
            self.assertEqual(job.inspect_status(root)['status'], 'warning')

    def test_missing_stale_and_wrong_environment_warn(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.assertEqual(job.inspect_status(root)["status"], "warning")
            old = datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(days=2)
            stale = {"status": "passed", "environment": "tshb-test", "backup_id": "scheduled-old",
                     "snapshot_started_at": old.isoformat(), "archive_readable": True, "offsite_reference": "offsite"}
            (root / "latest.json").write_text(json.dumps(stale))
            (root / "last-attempt.json").write_text(json.dumps(stale))
            self.assertEqual(job.inspect_status(root)["status"], "warning")
            with self.assertRaisesRegex(job.BackupError, "invalid_backup_environment"):
                job.backup({**self.config(root), "environment": "production"}, environment={})

    def test_ssh_archive_verifies_remote_bytes_before_final_name(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            folder = root / 'scheduled-example'
            folder.mkdir()
            file = folder / 'backup.dump'
            file.write_bytes(b'backup')
            config = root / 'storage.json'
            config.write_text(json.dumps({'host': 'backup-store', 'directory': '/backups/tsz'}))
            digest = job.sha256(file)
            def process(args, **kwargs):
                from types import SimpleNamespace
                output = digest + ' backup.dump.partial\n6\n' if 'sha256sum' in args[-1] else ''
                return SimpleNamespace(returncode=0, stdout=output)
            with patch.object(archive_ssh.subprocess, 'run', side_effect=process) as run:
                result = archive_ssh.archive(config, file, digest)
                self.assertTrue(result['verified'])
                self.assertIn('mv ', run.call_args_list[-1].args[0][-1])
                self.assertIn('StrictHostKeyChecking=yes', str(run.call_args_list))
            with patch.object(archive_ssh.subprocess, 'run', side_effect=process) as run:
                with self.assertRaisesRegex(ValueError, 'archive_digest_mismatch'):
                    archive_ssh.archive(config, file, 'a' * 64)
                self.assertFalse(any('mv ' in str(call) for call in run.call_args_list))

    def test_oss_archive_readback_and_fetch_verify_actual_bytes(self):
        class Remote:
            def __init__(self):
                self.objects = {}
                self.encrypted = True
            def put_object(self, key, source, headers):
                from types import SimpleNamespace
                if key in self.objects:
                    raise archive_oss.ArchiveError('duplicate')
                self.assert_headers = headers
                self.objects[key] = source.read()
                return SimpleNamespace(status=200)
            def get_object(self, key):
                stream = io.BytesIO(self.objects[key])
                stream.content_length = len(self.objects[key])
                stream.headers = {'x-oss-server-side-encryption': 'AES256' if self.encrypted else ''}
                return stream
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'scheduled-example' / 'backup.dump'
            source.parent.mkdir()
            source.write_bytes(b'recoverable archive')
            digest = job.sha256(source)
            remote = Remote()
            result = archive_oss.archive(remote, 'private-bucket', source, digest)
            self.assertEqual((result['sha256'], result['bytes']), (digest, source.stat().st_size))
            self.assertEqual(remote.assert_headers['x-oss-forbid-overwrite'], 'true')
            self.assertEqual(remote.assert_headers['x-oss-object-acl'], 'private')
            recovered = root / 'recovered.dump'
            archive_oss.fetch(remote, source.parent.name, digest, source.stat().st_size, recovered)
            self.assertEqual(recovered.read_bytes(), source.read_bytes())
            with self.assertRaisesRegex(archive_oss.ArchiveError, 'destination_exists'):
                archive_oss.fetch(remote, source.parent.name, digest, source.stat().st_size, recovered)
            self.assertEqual(recovered.read_bytes(), source.read_bytes())
            remote.objects[next(iter(remote.objects))] = b'corrupted'
            with self.assertRaisesRegex(archive_oss.ArchiveError, 'remote_size_mismatch'):
                archive_oss.fetch(remote, source.parent.name, digest, source.stat().st_size, root / 'corrupt.dump')
            self.assertFalse((root / 'corrupt.dump').exists())
            remote.encrypted = False
            with self.assertRaisesRegex(archive_oss.ArchiveError, 'remote_encryption_unverified'):
                archive_oss.fetch(remote, source.parent.name, digest, source.stat().st_size, root / 'unencrypted.dump')
            self.assertFalse((root / 'unencrypted.dump').exists())

            class CorruptReadback(Remote):
                def get_object(self, key):
                    stream = io.BytesIO(b'wrong remote bytes')
                    stream.headers = {'x-oss-server-side-encryption': 'AES256'}
                    return stream
            with self.assertRaisesRegex(archive_oss.ArchiveError, 'remote_backup_mismatch'):
                archive_oss.archive(CorruptReadback(), 'private-bucket', source, digest)

            class OversizeReadback(Remote):
                def get_object(self, key):
                    stream = io.BytesIO(b'x' * (source.stat().st_size + 1))
                    stream.content_length = source.stat().st_size
                    stream.headers = {'x-oss-server-side-encryption': 'AES256'}
                    return stream
            with self.assertRaisesRegex(archive_oss.ArchiveError, 'backup_exceeds_expected_size'):
                archive_oss.fetch(OversizeReadback(), source.parent.name, digest,
                                  source.stat().st_size, root / 'oversize.dump')
            self.assertFalse((root / 'oversize.dump').exists())

    def test_oss_config_requires_owner_only_and_safe_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'storage.json'
            path.write_text(json.dumps({'endpoint': 'https://oss-cn-heyuan.aliyuncs.com',
                                        'region': 'cn-heyuan', 'bucket': 'private-bucket',
                                        'access_key_id': 'id', 'access_key_secret': 'secret'}))
            path.chmod(0o644)
            with self.assertRaisesRegex(archive_oss.ArchiveError, 'insecure_storage_config'):
                archive_oss.read_config(path)
            path.chmod(0o600)
            self.assertEqual(archive_oss.read_config(path)['region'], 'cn-heyuan')
            with self.assertRaisesRegex(archive_oss.ArchiveError, 'invalid_backup_identity'):
                archive_oss.identity('scheduled-../escape', 'a' * 64)

    def test_oss_requires_private_unversioned_bucket(self):
        from types import SimpleNamespace
        config = {'access_key_id': 'id', 'access_key_secret': 'secret',
                  'endpoint': 'https://oss-cn-heyuan.aliyuncs.com',
                  'bucket': 'private-bucket', 'region': 'cn-heyuan'}
        for acl, blocked, versioning, xml, expected in (
            ('private', True, None, b'<OverwriteConfiguration><Rule><Action>forbid</Action><Prefix>database-backups/</Prefix></Rule></OverwriteConfiguration>', None),
            ('public-read', True, None, b'', 'bucket_not_private'),
            ('private', False, None, b'', 'public_access_not_blocked'),
            ('private', True, 'Enabled', b'', 'bucket_versioning_unsupported'),
            ('private', True, 'Suspended', b'', 'bucket_versioning_unsupported'),
            ('private', True, None, b'<OverwriteConfiguration><Rule><Action>forbid</Action><Prefix>database-backups/</Prefix><Principals><Principal>other</Principal></Principals></Rule></OverwriteConfiguration>', 'overwrite_protection_missing'),
        ):
            with self.subTest(acl=acl, blocked=blocked, versioning=versioning, expected=expected):
                response = io.BytesIO(xml)
                response.response = response
                bucket = SimpleNamespace(get_bucket_acl=lambda: SimpleNamespace(acl=acl),
                                         get_bucket_public_access_block=lambda: SimpleNamespace(block_public_access=blocked),
                                         get_bucket_versioning=lambda: SimpleNamespace(status=versioning),
                                         bucket_name='private-bucket', _do=lambda *args, **kwargs: response)
                fake_oss = SimpleNamespace(AuthV4=lambda *args: object(), Bucket=lambda *args, **kwargs: bucket)
                with patch.dict(sys.modules, {'oss2': fake_oss}):
                    if expected:
                        with self.assertRaisesRegex(archive_oss.ArchiveError, expected):
                            archive_oss.connect(config)
                    else:
                        self.assertIs(archive_oss.connect(config), bucket)

    def test_cloud_monitor_alert_reports_only_known_service_without_secrets(self):
        class Request:
            def __init__(self):
                self.params = {}
            def set_domain(self, value): self.domain = value
            def set_version(self, value): self.version = value
            def set_action_name(self, value): self.action = value
            def set_method(self, value): self.method = value
            def set_protocol_type(self, value): self.protocol = value
            def set_accept_format(self, value): self.accept_format = value
            def add_query_param(self, key, value): self.params[key] = value
        class Client:
            def do_action_with_exception(self, request):
                self.request = request
                return b'{"Code":"200"}'
        request, client = Request(), Client()
        result = backup_alert.report({'monitor_group_id': '123'}, 'tsz-backup.service',
                                     lambda: (client, request))
        self.assertEqual(result['status'], 'reported')
        self.assertEqual(request.protocol, 'https')
        self.assertEqual(request.accept_format, 'json')
        self.assertEqual(request.params['EventInfo.1.GroupId'], '123')
        self.assertEqual(request.params['EventInfo.1.EventName'], 'tsz_backup_failure')
        self.assertNotIn('DATABASE_URL', str(request.params))
        with self.assertRaisesRegex(backup_alert.AlertError, 'invalid_alert_unit'):
            backup_alert.report({'monitor_group_id': '123'}, 'unknown.service', lambda: (client, request))
        with self.assertRaisesRegex(backup_alert.AlertError, 'invalid_monitor_group'):
            backup_alert.report({'monitor_group_id': '0'}, 'tsz-backup.service', lambda: (client, request))
        with tempfile.TemporaryDirectory() as directory:
            settings = {'environment': 'tshb-test', 'directory': directory}
            request = Request()
            self.assertFalse(backup_alert.heartbeat({'monitor_group_id': '123'}, settings,
                                                    lambda: (client, request))['backup_healthy'])
            self.assertEqual(json.loads(request.params['MetricList.1.Values']), {'value': 0})
            status = {'environment': 'tshb-test', 'status': 'passed', 'backup_id': 'scheduled-test',
                      'snapshot_started_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
                      'archive_readable': True, 'offsite_reference': 'oss:example'}
            for name in ('latest.json', 'last-attempt.json'):
                (Path(directory) / name).write_text(json.dumps(status))
            request = Request()
            self.assertTrue(backup_alert.heartbeat({'monitor_group_id': '123'}, settings,
                                                   lambda: (client, request))['backup_healthy'])
            self.assertEqual(json.loads(request.params['MetricList.1.Values']), {'value': 1})
            with patch.object(backup_alert.shutil, 'disk_usage') as usage:
                usage.return_value = type('Disk', (), {'free': 1024})()
                self.assertFalse(backup_alert.heartbeat({'monitor_group_id': '123'}, settings,
                                                        lambda: (client, Request()))['backup_healthy'])

    def test_alert_config_is_independent_and_owner_only(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'alert.json'
            path.write_text(json.dumps({'access_key_id': 'alert-id', 'access_key_secret': 'alert-secret',
                                        'monitor_group_id': '123'}))
            path.chmod(0o644)
            with self.assertRaisesRegex(backup_alert.AlertError, 'insecure_alert_config'):
                backup_alert.read_config(path)
            path.chmod(0o600)
            self.assertEqual(backup_alert.read_config(path)['monitor_group_id'], '123')

    def test_heartbeat_replica_check_fails_closed_on_missing_or_corrupt_copy(self):
        from types import SimpleNamespace
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'latest.json').write_text(json.dumps({
                'backup_id': 'scheduled-test', 'sha256': 'a' * 64, 'bytes': 42,
            }))
            config = {'directory': directory, 'replica': {
                'storage_config': '/root/backup-oss.json', 'endpoint': 'https://oss-cn-shenzhen.aliyuncs.com',
                'region': 'cn-shenzhen', 'bucket': 'replica-bucket',
            }}
            source = SimpleNamespace(content_length=42, headers={'x-oss-hash-crc64ecma': '123'})
            target = SimpleNamespace(content_length=42, headers={
                'x-oss-hash-crc64ecma': '123', 'x-oss-server-side-encryption': 'AES256',
            })
            def bucket(value):
                return SimpleNamespace(head_object=lambda key: value)
            with patch.object(archive_oss, 'read_config', return_value={'bucket': 'source-bucket'}), \
                 patch.object(archive_oss, 'connect', side_effect=[bucket(source), bucket(target)]):
                self.assertTrue(backup_alert.replica_is_current(config))
            target.headers['x-oss-hash-crc64ecma'] = '999'
            with patch.object(archive_oss, 'read_config', return_value={'bucket': 'source-bucket'}), \
                 patch.object(archive_oss, 'connect', side_effect=[bucket(source), bucket(target)]):
                self.assertFalse(backup_alert.replica_is_current(config))
            with patch.object(archive_oss, 'read_config', side_effect=OSError('unavailable')):
                self.assertFalse(backup_alert.replica_is_current(config))
            config['replica'] = 'invalid'
            self.assertFalse(backup_alert.replica_is_current(config))

    def test_restore_fetches_exact_bytes_and_cleans_after_restore_failure(self):
        for fail in (False, True):
            with self.subTest(fail=fail), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / 'volumes').mkdir()
                archive = root / 'original.dump'
                archive.write_bytes(b'archive')
                backup = {'environment': 'tshb-test', 'status': 'passed', 'backup_id': 'scheduled-example',
                          'sha256': job.sha256(archive), 'bytes': 7, 'postgresql_major': 18}
                (root / 'latest.json').write_text(json.dumps(backup))
                owner = [None]
                calls = []
                def command(args, **kwargs):
                    calls.append(args)
                    if args[:2] == ['docker', 'info']:
                        return str(root).encode()
                    if args[0] == '/fetch':
                        self.assertEqual(args[-2], '7')
                        Path(args[-1]).write_bytes(b'archive')
                    elif args[:2] == ['docker', 'create']:
                        owner[0] = args[args.index('--label') + 1].split('=', 1)[1]
                        self.assertIn('none', args)
                        return b'owned-container'
                    elif args[:2] == ['docker', 'inspect']:
                        return json.dumps([{'Config': {'Labels': {'tsz.restore-job': owner[0]}}}]).encode()
                    elif args[-1] == 'SELECT 1':
                        self.assertIn('127.0.0.1', args)
                        return b'1'
                    elif 'pg_restore' in args and '--version' in args:
                        return b'pg_restore (PostgreSQL) 18.6'
                    elif 'pg_restore' in args and fail:
                        raise job.BackupError('restore_test_failure')
                    elif args[-1].startswith('SELECT count(*) FROM information_schema'):
                        return b'133'
                    elif '_sqlx_migrations' in args[-1]:
                        return b'4|0'
                    return b''
                config = {**self.config(root), 'restore_image': 'sha256:' + 'a' * 64,
                          'fetch_command': ['/fetch'], 'fetch_expected_bytes': True}
                with patch.object(restore_job, 'command', side_effect=command):
                    if fail:
                        with self.assertRaisesRegex(job.BackupError, 'restore_test_failure'):
                            restore_job.restore(config)
                    else:
                        self.assertEqual(restore_job.restore(config)['status'], 'passed')
                receipt = json.loads((root / 'restore-latest.json').read_text())
                self.assertEqual(receipt['status'], 'failed' if fail else 'passed')
                self.assertTrue(receipt['cleanup_complete'])
                self.assertTrue(any(args[:2] == ['docker', 'rm'] for args in calls))
                self.assertEqual(list(root.glob('restore-*.*')), [root / 'restore-latest.json'])

    def test_status_syncs_directory_after_replace(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / 'receipt.json'
            synced = []
            original = os.fsync
            def sync(descriptor):
                import stat
                synced.append(stat.S_ISDIR(os.fstat(descriptor).st_mode))
                return original(descriptor)
            with patch.object(job.os, 'fsync', side_effect=sync):
                job.write_status(target, {'status': 'failed'})
            self.assertEqual(synced, [False, True])
            self.assertEqual(json.loads(target.read_text()), {'status': 'failed'})

    def test_command_stops_transfer_descendants_before_returning(self):
        with tempfile.TemporaryDirectory() as directory:
            pid_file = Path(directory) / 'pid'
            child = ('import signal,time,os,pathlib; signal.signal(signal.SIGTERM,signal.SIG_IGN); '
                     f'pathlib.Path({str(pid_file)!r}).write_text(str(os.getpid())); time.sleep(60)')
            parent = ('import subprocess,time,sys,pathlib\n'
                      f'subprocess.Popen([sys.executable,"-I","-c",{child!r}],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)\n'
                      f'while not pathlib.Path({str(pid_file)!r}).exists(): time.sleep(0.01)\n'
                      'time.sleep(60)')
            pid = None
            try:
                with self.assertRaisesRegex(job.BackupError, 'backup_command_unavailable'):
                    job.command([sys.executable, '-I', '-c', parent], timeout=0.5)
                pid = int(pid_file.read_text())
                state = subprocess.run(['ps', '-p', str(pid), '-o', 'stat='], capture_output=True, text=True).stdout.strip()
                self.assertTrue(not state or state.startswith('Z'), f'live descendant: {state}')
            finally:
                if pid:
                    with contextlib.suppress(ProcessLookupError):
                        os.kill(pid, signal.SIGKILL)

    def test_restore_cleans_uncertain_creation_and_records_local_cleanup_failure(self):
        for scenario in ('create-lost', 'cancel-ready', 'local-cleanup', 'success'):
            with self.subTest(scenario=scenario), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / 'volumes').mkdir()
                archive = root / 'original.dump'
                archive.write_bytes(b'archive')
                backup = {'environment': 'tshb-test', 'status': 'passed', 'backup_id': 'scheduled-test',
                          'sha256': job.sha256(archive), 'bytes': 7, 'postgresql_major': 18}
                (root / 'latest.json').write_text(json.dumps(backup))
                config = {**self.config(root), 'restore_image': 'sha256:' + 'a' * 64, 'fetch_command': ['/fetch']}
                resources = {}
                calls = []
                def command(args, **kwargs):
                    calls.append(args)
                    if args[:2] == ['docker', 'info']:
                        return str(root).encode()
                    if args[0] == '/fetch':
                        Path(args[-1]).write_bytes(b'archive')
                    elif args[:2] == ['docker', 'create']:
                        owner = args[args.index('--label') + 1].split('=', 1)[1]
                        resources['owned-container'] = owner
                        if scenario == 'create-lost':
                            raise job.BackupError('backup_command_unavailable')
                        return b'owned-container'
                    elif args[:2] == ['docker', 'inspect']:
                        return json.dumps([{'Config': {'Labels': {'tsz.restore-job': resources[args[-1]]}}}]).encode()
                    elif args[:2] == ['docker', 'ps']:
                        self.assertIn('--no-trunc', args)
                        return '\n'.join(resources).encode()
                    elif args[:2] == ['docker', 'rm']:
                        resources.pop(args[-1])
                    elif 'pg_isready' in args or args[-1] == 'SELECT 1':
                        if scenario == 'cancel-ready':
                            raise job.BackupInterrupted('restore_interrupted')
                        self.assertIn('127.0.0.1', args)
                        return b'1'
                    elif 'pg_restore' in args and '--version' in args:
                        return b'pg_restore (PostgreSQL) 18.6'
                    elif args[-1].startswith('SELECT count(*) FROM information_schema'):
                        return b'2'
                    elif '_sqlx_migrations' in args[-1]:
                        return b'1|0'
                    return b''
                original_cleanup = restore_job.shutil.rmtree
                def cleanup(*args, **kwargs):
                    if scenario == 'local-cleanup':
                        raise PermissionError('simulated raw backup cleanup failure')
                    return original_cleanup(*args, **kwargs)
                with patch.object(restore_job, 'command', side_effect=command), \
                     patch.object(restore_job.shutil, 'rmtree', side_effect=cleanup):
                    if scenario == 'success':
                        self.assertEqual(restore_job.restore(config)['status'], 'passed')
                    else:
                        with self.assertRaises((job.BackupError, PermissionError)):
                            restore_job.restore(config)
                self.assertFalse(resources)
                receipt = json.loads((root / 'restore-latest.json').read_text())
                self.assertEqual(receipt['status'], 'passed' if scenario == 'success' else 'failed')
                self.assertEqual(receipt['cleanup_complete'], scenario != 'local-cleanup')
                if scenario == 'cancel-ready':
                    self.assertFalse(any('pg_restore' in args for args in calls))
                self.assertIn('owner', receipt)

    def test_restore_capacity_guard_stops_before_downloading(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'volumes').mkdir()
            (root / 'latest.json').write_text(json.dumps({
                'environment': 'tshb-test', 'status': 'passed', 'backup_id': 'scheduled-test',
                'sha256': 'a' * 64, 'bytes': 1024, 'postgresql_major': 18,
            }))
            (root / 'restore-latest.json').write_text(json.dumps({'status': 'passed', 'backup_id': 'old'}))
            calls = []
            def command(args, **kwargs):
                calls.append(args)
                return str(root).encode() if args[:2] == ['docker', 'info'] else b''
            config = {**self.config(root), 'restore_image': 'sha256:' + 'a' * 64, 'fetch_command': ['/fetch']}
            with patch.object(restore_job, 'command', side_effect=command), \
                 patch.object(restore_job.shutil, 'disk_usage') as usage:
                usage.return_value = type('Disk', (), {'free': 1024})()
                with self.assertRaisesRegex(job.BackupError, 'restore_capacity_insufficient'):
                    restore_job.restore(config)
            self.assertFalse(any(args[0] == '/fetch' or args[:2] == ['docker', 'create'] for args in calls))
            receipt = json.loads((root / 'restore-latest.json').read_text())
            self.assertEqual((receipt['status'], receipt['backup_id'], receipt['cleanup_complete']),
                             ('failed', 'scheduled-test', True))

    def test_phase_reports_failure_and_heartbeat_without_changing_exception(self):
        timings = []
        with patch("sys.stderr", new_callable=io.StringIO) as output:
            with self.assertRaisesRegex(RuntimeError, "original"):
                with measured_phase("upload", timings, heartbeat_seconds=0.001):
                    import time
                    time.sleep(0.004)
                    raise RuntimeError("original")
        self.assertEqual(timings[0]["status"], "failed")
        self.assertGreaterEqual(timings[0]["elapsed_seconds"], 0)
        self.assertIn('"status": "running"', output.getvalue())


if __name__ == "__main__":
    unittest.main()
