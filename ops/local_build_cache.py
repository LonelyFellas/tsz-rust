#!/usr/bin/env python3
"""Plan first; remove only tsz-rust integration artifacts with proven ownership.

No Cargo configuration parsing, time-based eviction, directory removal or hooks.
The target and selected worktrees must be explicit. Active/protected owners stay intact.
"""
import argparse
from contextlib import ExitStack
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess

REPO = Path(__file__).resolve().parent.parent
PROTECTED_NAME = 'tsz-lexicon-domain-batch-2'
UNIT = re.compile(r'(.+)-([0-9a-f]{16})(?:\..*\.rcgu\.o)?$')


def checked(command):
    result = subprocess.run(command, capture_output=True, text=True, check=True)
    if result.stderr.strip():
        raise ValueError(f'{command[0]} returned diagnostics; inspect before cleanup')
    return result.stdout


def worktrees():
    return {line[9:] for line in checked(
        ['git', '-C', str(REPO), 'worktree', 'list', '--porcelain']
    ).splitlines() if line.startswith('worktree ')}


def signature(path):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_uid != os.getuid():
        raise ValueError(f'not a singly-linked owned regular file: {path}')
    return {'size': info.st_size, 'inode': info.st_ino, 'mtime_ns': info.st_mtime_ns,
            'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}


def native_artifact(path):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_uid != os.getuid():
        return False
    with path.open('rb') as stream:
        header = stream.read(18)
    object_file = path.name.endswith('.rcgu.o')
    if header[:4] == b'\xcf\xfa\xed\xfe':
        return int.from_bytes(header[12:16], 'little') == (1 if object_file else 2)
    if header[:4] == b'\x7fELF' and header[5:6] in (b'\x01', b'\x02'):
        kind = int.from_bytes(header[16:18], 'little' if header[5] == 1 else 'big')
        return kind in ({1} if object_file else {2, 3})
    return False


def usage():
    processes = checked(['ps', '-axo', 'comm=']).splitlines()
    busy = any(Path(p.strip()).name in {'cargo', 'rustc', 'rustdoc', 'clippy-driver'}
               for p in processes)
    opened = set()
    cwd = set()
    kind = ''
    for line in checked(['lsof', '-nP', '-Ffn']).splitlines():
        if line.startswith('f'):
            kind = line[1:]
        elif line.startswith('n'):
            opened.add(line[1:])
            if kind == 'cwd':
                cwd.add(line[1:])
    return busy, opened, cwd


def plan(target, owners):
    target = Path(target).absolute()
    if target.resolve() != target:
        raise ValueError('target must be a canonical path without symlinks')
    debug = target / 'debug'
    deps = debug / 'deps'
    fingerprints = debug / '.fingerprint'
    if any(p.is_symlink() or not p.is_dir() for p in (debug, deps, fingerprints)):
        raise ValueError('expected ordinary debug/deps and debug/.fingerprint directories')
    owners = sorted(set(str(Path(p).absolute()) for p in owners))
    registered = worktrees()
    busy, opened, cwd = usage()
    for owner in owners:
        if PROTECTED_NAME in owner or Path(owner).resolve() != Path(owner):
            raise ValueError(f'owner is protected or noncanonical: {owner}')
        if os.path.lexists(owner) and owner not in registered:
            raise ValueError(f'existing owner is not a worktree of this repository: {owner}')
    files = {}
    for path in deps.iterdir():
        match = UNIT.fullmatch(path.name)
        if match:
            files.setdefault(match.group(1) + '-' + match.group(2), []).append(path)
    inventory = {}
    entries = []
    for fingerprint in sorted(fingerprints.glob('tsz-rust-*/test-integration-test-*.json')):
        if fingerprint.parent.is_symlink() or fingerprint.is_symlink():
            continue
        name = fingerprint.stem.removeprefix('test-integration-test-')
        suffix = fingerprint.parent.name.removeprefix('tsz-rust-')
        unit = f'{name}-{suffix}'
        artifacts = files.get(unit, [])
        dep = deps / f'{unit}.d'
        if not artifacts or not dep.is_file() or dep.is_symlink():
            continue
        dependency_text = dep.read_text()
        roots = set(re.findall(r'(?<!\S)(/[^\s:]+?)/migrations/', dependency_text))
        owner = next(iter(roots)) if len(roots) == 1 else 'unknown'
        source = REPO / 'tests' / f'{name}.rs'
        protected = (busy or PROTECTED_NAME in dependency_text
                     or any(p == owner or p.startswith(owner + '/') for p in opened | cwd)
                     or any(str(p) in opened for p in artifacts))
        state = ('unknown' if owner == 'unknown' or
                 (os.path.lexists(owner) and owner not in registered) else 'protected' if protected
                 else 'eligible' if source.is_file() else 'source-unavailable')
        group = inventory.setdefault(owner, {'files': 0, 'allocated_bytes': 0,
                                             'states': {}, 'registered': owner in registered})
        for artifact in sorted(artifacts):
            artifact_state = state if native_artifact(artifact) else 'unknown'
            size = artifact.lstat().st_blocks * 512
            group['files'] += 1
            group['allocated_bytes'] += size
            group['states'][artifact_state] = group['states'].get(artifact_state, 0) + size
            if owner not in owners:
                continue
            if artifact_state != 'eligible':
                raise ValueError(f'owner/artifact is not eligible: {owner}: {artifact_state}')
            data = json.loads(fingerprint.read_text())
            if not isinstance(data, dict) or not {'rustc', 'target', 'profile', 'deps'} <= data.keys():
                raise ValueError(f'unsupported fingerprint: {fingerprint}')
            entries.append({'path': str(artifact), 'owner': owner,
                            'artifact': signature(artifact),
                            'dep_info': signature(dep), 'fingerprint': signature(fingerprint)})
    return {'version': 1, 'target': str(target), 'worktrees': owners,
            'inventory': inventory, 'entries': entries}


def apply(saved):
    target = Path(saved['target'])
    with ExitStack() as stack:
        # Do not create locks or guess another Cargo version's lock layout.
        for name in ('.cargo-lock', '.cargo-build-lock', '.cargo-artifact-lock'):
            path = target / 'debug' / name
            if path.is_symlink():
                raise ValueError('symlink lock refused')
            lock = stack.enter_context(path.open('r+'))
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        current = plan(target, saved['worktrees'])
        if current != saved:
            raise ValueError('plan changed; no files removed; review a fresh plan')
        busy, opened, cwd = usage()
        if (busy or any(entry['path'] in opened for entry in saved['entries'])
                or any(p == owner or p.startswith(owner + '/')
                       for owner in saved['worktrees'] for p in opened | cwd)):
            raise ValueError('usage changed during validation; no files removed')
        removed = []
        try:
            for entry in saved['entries']:
                path = Path(entry['path'])
                if signature(path) != entry['artifact']:
                    raise ValueError('artifact changed during cleanup')
                path.unlink()
                removed.append(str(path))
        finally:
            # A partial failure must still expose exactly what was removed.
            print(json.dumps({'removed': removed, 'removed_count': len(removed)}))
        if any(Path(path).exists() for path in removed):
            raise ValueError('post-delete verification failed')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='action', required=True)
    inspect = commands.add_parser('plan', help='read-only inventory and exact removal plan to stdout')
    inspect.add_argument('--target-dir', required=True)
    inspect.add_argument('--worktree', action='append', default=[],
                         help='exact owner to include; omission inventories without candidates')
    execute = commands.add_parser('apply', help='revalidate and apply a previously reviewed JSON plan')
    execute.add_argument('plan_file', type=Path)
    args = parser.parse_args()
    try:
        if args.action == 'plan':
            print(json.dumps(plan(args.target_dir, args.worktree), indent=2))
        else:
            apply(json.loads(args.plan_file.read_text()))
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        parser.exit(2, f'cache cleanup refused/failed: {error}\n')


if __name__ == '__main__':
    main()
