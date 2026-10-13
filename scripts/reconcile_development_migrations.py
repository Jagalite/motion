#!/usr/bin/env python3
"""Copy a development SQLite DB and reconcile known branch migration numbers.

Never edits the source. Stop Motion before using this offline tool. The new
copy must be started with the matching integrated binary, which applies any
missing migrations normally. Unknown or unsuccessful history fails closed.
"""
import argparse
from contextlib import closing
import hashlib
import json
from pathlib import Path
import sqlite3

ROOT = Path(__file__).resolve().parents[1]


def known_migrations(root=ROOT):
    canonical = {}
    for path in (root / 'migrations').glob('*.sql'):
        version = int(path.name.split('_', 1)[0])
        if version in canonical:
            raise ValueError(f'duplicate canonical migration {version}')
        canonical[version] = hashlib.sha384(path.read_bytes()).digest()
    remapped = {}
    for entry in json.loads((root / 'scripts/development_migration_map.json').read_text()):
        checksum = bytes.fromhex(entry['sha384'])
        if canonical.get(entry['new']) != checksum:
            raise ValueError(f"canonical migration {entry['new']} no longer matches known branch bytes")
        remapped[(entry['old'], checksum)] = entry['new']
    return canonical, remapped


def reconcile(source, destination, root=ROOT):
    source, destination = Path(source).resolve(), Path(destination).absolute()
    if not source.is_file():
        raise ValueError('source must be an existing SQLite database')
    canonical, remapped = known_migrations(root)
    # Exclusive creation: an existing destination, including a symlink, is never touched.
    with destination.open('xb'):
        pass
    try:
        with closing(sqlite3.connect(source.as_uri() + '?mode=ro', uri=True)) as original:
            with closing(sqlite3.connect(destination)) as copied:
                original.backup(copied)
        with closing(sqlite3.connect(destination)) as db, db:
            if db.execute('PRAGMA integrity_check').fetchone() != ('ok',):
                raise ValueError('source integrity check failed')
            rows = db.execute('SELECT version, checksum, success FROM _sqlx_migrations ORDER BY version').fetchall()
            moves, targets = [], set()
            for version, checksum, success in rows:
                if success != 1:
                    raise ValueError(f'unsuccessful migration {version}')
                checksum = bytes(checksum)
                target = version if canonical.get(version) == checksum else remapped.get((version, checksum))
                if target is None:
                    raise ValueError(f'unknown migration/checksum at {version}; no changes accepted')
                if target in targets:
                    raise ValueError(f'multiple migrations map to canonical version {target}')
                targets.add(target)
                if target != version:
                    moves.append((version, target))
            # Two phases avoid collisions when a branch's old number is another's target.
            for old, _ in moves:
                db.execute('UPDATE _sqlx_migrations SET version=? WHERE version=?', (-old, old))
            for old, new in moves:
                db.execute('UPDATE _sqlx_migrations SET version=? WHERE version=?', (new, -old))
        return moves
    except BaseException:
        destination.unlink(missing_ok=True)
        for suffix in ('-wal', '-shm', '-journal'):
            Path(str(destination) + suffix).unlink(missing_ok=True)
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('destination', type=Path, help='new database path; must not exist')
    args = parser.parse_args()
    moves = reconcile(args.source, args.destination)
    print(f'Created {args.destination}; source unchanged. Remapped: {moves}')
    print('Start this copy with the integrated server to apply missing migrations before use.')


if __name__ == '__main__':
    main()
