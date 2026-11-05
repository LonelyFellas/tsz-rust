import contextlib
import fcntl
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from ops import local_build_cache as cache


class CacheTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.repo = self.root / 'repo'
        (self.repo / 'tests').mkdir(parents=True)
        (self.repo / 'tests/example.rs').write_text('// source')
        self.target = self.root / 'target'
        self.debug = self.target / 'debug'
        self.deps = self.debug / 'deps'
        self.deps.mkdir(parents=True)
        self.fp = self.debug / '.fingerprint/tsz-rust-0123456789abcdef'
        self.fp.mkdir(parents=True)
        self.fingerprint = self.fp / 'test-integration-test-example.json'
        self.fingerprint.write_text(json.dumps({'rustc': 1, 'target': 2, 'profile': 3, 'deps': []}))
        self.owner = self.root / 'existing-owner'
        self.owner.mkdir()
        self.binary = self.deps / 'example-0123456789abcdef'
        self.binary.write_bytes(b'\xcf\xfa\xed\xfe' + b'\x00' * 8 + b'\x02\x00\x00\x00')
        self.obj = self.deps / (self.binary.name + '.example.hash-cgu.0.rcgu.o')
        self.obj.write_bytes(b'\xcf\xfa\xed\xfe' + b'\x00' * 8 + b'\x01\x00\x00\x00')
        self.dep = self.binary.with_suffix('.d')
        self.dep.write_text(f'{self.binary}: tests/example.rs {self.owner}/migrations/001.sql\n')
        self.unrelated = self.deps / 'libexample-0123456789abcdef.rlib'
        self.unrelated.write_bytes(b'preserve')
        for name in ('.cargo-lock', '.cargo-build-lock', '.cargo-artifact-lock'):
            (self.debug / name).touch()
        for obj in [patch.object(cache, 'REPO', self.repo),
                    patch.object(cache, 'worktrees', return_value={str(self.owner)}),
                    patch.object(cache, 'usage', return_value=(False, set(), set()))]:
            obj.start()
            self.addCleanup(obj.stop)

    def plan(self):
        return cache.plan(self.target, [str(self.owner)])

    def test_inventory_defaults_to_no_candidates_and_apply_is_exact(self):
        self.assertEqual(cache.plan(self.target, [])['entries'], [])
        saved = self.plan()
        self.assertEqual({x['path'] for x in saved['entries']}, {str(self.binary), str(self.obj)})
        self.assertTrue(self.binary.exists())
        with contextlib.redirect_stdout(io.StringIO()):
            cache.apply(saved)
        self.assertFalse(self.binary.exists())
        self.assertFalse(self.obj.exists())
        for path in [self.unrelated, self.dep, self.fingerprint, self.repo / 'tests/example.rs']:
            self.assertTrue(path.exists())

    def test_modified_or_forged_plan_never_deletes(self):
        for alteration in ['path', 'file', 'fingerprint', 'dep-info']:
            with self.subTest(alteration=alteration):
                saved = self.plan()
                if alteration == 'path':
                    saved['entries'][0]['path'] = str(self.unrelated)
                elif alteration == 'file':
                    self.binary.write_bytes(self.binary.read_bytes() + b'x')
                elif alteration == 'fingerprint':
                    self.fingerprint.write_text(self.fingerprint.read_text() + ' ')
                else:
                    self.dep.write_text(self.dep.read_text() + '\n')
                with self.assertRaises(ValueError):
                    cache.apply(saved)
                self.assertTrue(self.binary.exists())
                self.assertTrue(self.obj.exists())

    def test_active_compiler_open_artifact_and_active_owner_block_apply(self):
        saved = self.plan()
        for state in [(True, set(), set()), (False, {str(self.binary)}, set()),
                      (False, set(), {str(self.owner)}),
                      (False, {str(self.owner / 'src/lib.rs')}, set())]:
            with self.subTest(state=state), patch.object(cache, 'usage', return_value=state):
                with self.assertRaises(ValueError):
                    cache.apply(saved)
                self.assertTrue(self.binary.exists())

    def test_busy_cargo_lock_blocks_apply(self):
        saved = self.plan()
        with (self.debug / '.cargo-lock').open('r+') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with self.assertRaises(BlockingIOError):
                cache.apply(saved)
        self.assertTrue(self.binary.exists())

    def test_usage_changed_while_hashing_blocks_apply(self):
        saved = self.plan()
        with patch.object(cache, 'usage', side_effect=[
            (False, set(), set()), (False, {str(self.binary)}, set())
        ]):
            with self.assertRaises(ValueError):
                cache.apply(saved)
        self.assertTrue(self.binary.exists())

    def test_lsof_observations_include_cwd_and_open_files(self):
        with patch.object(cache, 'checked', side_effect=[
            '/usr/bin/rustc\n', 'p12\nfcwd\nn/worktree\nf4\nn/artifact\n'
        ]):
            self.assertEqual(self.actual_usage(),
                             (True, {'/worktree', '/artifact'}, {'/worktree'}))

    def test_ambiguous_protected_and_relative_dep_paths(self):
        self.dep.write_text(self.dep.read_text() + ' tests/example/../../migrations/002.sql\n')
        self.assertEqual(len(self.plan()['entries']), 2)
        self.dep.write_text(self.dep.read_text() + f' {self.root}/other/migrations/003.sql\n')
        self.assertEqual(self.plan()['entries'], [])
        self.dep.write_text(f' {self.owner}/migrations/001.sql {cache.PROTECTED_NAME}/src/lib.rs\n')
        with self.assertRaises(ValueError):
            self.plan()
        with self.assertRaises(ValueError):
            cache.plan(self.target, [str(self.root / cache.PROTECTED_NAME)])

    def test_links_and_non_native_files_are_not_removed(self):
        original = self.binary.read_bytes()
        for kind in ['symlink', 'hardlink', 'not-native']:
            with self.subTest(kind=kind):
                self.binary.unlink()
                if kind == 'symlink':
                    self.binary.symlink_to(self.unrelated)
                elif kind == 'hardlink':
                    os.link(self.unrelated, self.binary)
                else:
                    self.binary.write_bytes(b'ordinary user data')
                with self.assertRaises(ValueError):
                    self.plan()
                self.assertEqual(self.unrelated.read_bytes(), b'preserve')
                self.binary.unlink()
                self.binary.write_bytes(original)

    def test_existing_owner_from_another_repository_is_not_eligible(self):
        with patch.object(cache, 'worktrees', return_value=set()):
            with self.assertRaises(ValueError):
                self.plan()

    def test_probe_failure_is_not_assumed_idle(self):
        # Run the actual usage implementation with a failing observation command.
        with patch.object(cache, 'checked', side_effect=OSError('lsof unavailable')):
            self.assertRaises(OSError, CacheTests.actual_usage)

    actual_usage = staticmethod(cache.usage)


if __name__ == '__main__':
    unittest.main()
