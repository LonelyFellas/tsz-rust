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

    def test_restore_fetches_exact_bytes_and_cleans_after_restore_failure(self):
        for fail in (False, True):
            with self.subTest(fail=fail), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                archive = root / 'original.dump'
                archive.write_bytes(b'archive')
                backup = {'environment': 'tshb-test', 'status': 'passed', 'backup_id': 'scheduled-example',
                          'sha256': job.sha256(archive), 'bytes': 7, 'postgresql_major': 18}
                (root / 'latest.json').write_text(json.dumps(backup))
                owner = [None]
                calls = []
                def command(args, **kwargs):
                    calls.append(args)
                    if args[0] == '/fetch':
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
                config = {**self.config(root), 'restore_image': 'sha256:' + 'a' * 64, 'fetch_command': ['/fetch']}
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
