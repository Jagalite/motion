"""Consistent SQLite backups and non-overwriting restore. Media/config/tokens are separate."""
import argparse
import datetime
import hashlib
import json
import os
import pathlib
import shutil
import sqlite3
import time


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def readonly(path, immutable=False):
    return sqlite3.connect(path.resolve().as_uri() + '?mode=ro' + ('&immutable=1' if immutable else ''), uri=True, timeout=10)


def validate(path):
    db = readonly(path, immutable=True)
    try:
        if db.execute('PRAGMA integrity_check').fetchall() != [('ok',)]:
            raise ValueError('SQLite integrity check failed')
        if db.execute('PRAGMA foreign_key_check').fetchall():
            raise ValueError('Foreign key check failed')
        versions = [row[0] for row in db.execute('SELECT version FROM _sqlx_migrations WHERE success=1 ORDER BY version')]
        if not versions:
            raise ValueError('Not a migrated Playscale database')
        db.execute('SELECT count(*) FROM libraries').fetchone()
        return versions
    finally:
        db.close()


def sync_file(path):
    with path.open('rb') as stream:
        os.fsync(stream.fileno())


def sync_directory(path):
    if os.name == 'posix':
        fd = os.open(path, os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)


def backup(data, output):
    source = data / 'playscale.sqlite3'
    if not source.is_file():
        raise ValueError('Source database does not exist')
    output.mkdir(mode=0o700)  # Never reuse a previous backup directory.
    target = output / 'playscale.sqlite3'
    src = readonly(source)
    dst = sqlite3.connect(target)
    os.chmod(target, 0o600)
    started = time.monotonic()
    def progress(status, remaining, total):
        if time.monotonic() - started > 120:
            raise TimeoutError('Backup exceeded 120 seconds; incomplete directory retained')
    try:
        src.backup(dst, pages=256, progress=progress, sleep=.01)
        dst.execute('PRAGMA journal_mode=DELETE')
    finally:
        dst.close()
        src.close()
    versions = validate(target)
    sync_file(target)
    manifest = {'format': 1, 'created_at': datetime.datetime.now(datetime.timezone.utc).isoformat(), 'sha256': digest(target), 'bytes': target.stat().st_size, 'schema_versions': versions, 'includes': ['catalog', 'artwork blobs', 'profiles', 'progress', 'sessions', 'preferences', 'jobs', 'processing requests', 'scan schedules', 'event cursors'], 'excludes': ['source media', 'generated media cache', 'Demuxe assets', 'configuration', 'admin token']}
    with (output / 'manifest.json').open('x') as stream:
        json.dump(manifest, stream, indent=2)
        stream.write('\n')
        stream.flush()
        os.fsync(stream.fileno())
    sync_directory(output)
    return manifest


def restore(source, data):
    if data.exists():
        raise ValueError('Restore requires a new, nonexistent data directory')
    manifest_path = source / 'manifest.json'
    if manifest_path.stat().st_size > 65536:
        raise ValueError('Manifest too large')
    manifest = json.loads(manifest_path.read_text())
    database = source / 'playscale.sqlite3'
    if manifest.get('format') != 1 or digest(database) != manifest['sha256'] or database.stat().st_size != manifest['bytes']:
        raise ValueError('Backup hash, size or format mismatch')
    if validate(database) != manifest['schema_versions']:
        raise ValueError('Backup schema manifest mismatch')
    data.mkdir(mode=0o700)
    temporary = data / 'restore.sqlite.tmp'
    shutil.copyfile(database, temporary)
    os.chmod(temporary, 0o600)
    if digest(temporary) != manifest['sha256']:
        raise ValueError('Backup changed during restore; incomplete directory retained')
    validate(temporary)
    sync_file(temporary)
    temporary.rename(data / 'playscale.sqlite3')
    sync_directory(data)
    return {'restored': str(data), 'schema_versions': manifest['schema_versions'], 'admin_token': 'not restored; server generates a new token on first start'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    save = commands.add_parser('backup')
    save.add_argument('--data-dir', required=True, type=pathlib.Path)
    save.add_argument('--output', required=True, type=pathlib.Path)
    load = commands.add_parser('restore')
    load.add_argument('--backup', required=True, type=pathlib.Path)
    load.add_argument('--data-dir', required=True, type=pathlib.Path)
    args = parser.parse_args()
    result = backup(args.data_dir, args.output) if args.command == 'backup' else restore(args.backup, args.data_dir)
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
