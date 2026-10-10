"""Upgrade a database written by the released baseline server, then prove rollback.

Real processes, generated media and disposable directories only; no user library.

    python3 scripts/upgrade_restore_check.py --work artifacts/upgrade-restore

builds the baseline server from Git with its pinned toolchain (via rustup) and target directory
under --baseline-dir (reused when it matches), and uses target/debug/playscale as
the candidate unless --binary is given. A JSON receipt is written to --work.

Checks, in order:
  1. The baseline server catalogs generated media, records curated metadata and a
     saved position, and the baseline backup tool takes an operator backup.
  2. The candidate opens that data directory in place with its default (restricted)
     access mode: it takes exactly one verified pre-upgrade backup, preserves item,
     library and file identities, bytes, curation and the position, applies every
     migration on disk, passes SQLite integrity and foreign-key checks, and records
     one schema receipt and one exact legacy-progress attribution.
  3. Restarting the candidate takes no further upgrade backup and records no
     further data-upgrade receipts.
  4. The baseline server refuses the upgraded database with its unknown-migration
     diagnostic and leaves it usable.
  5. Rollback: the candidate's restore tool restores the baseline operator backup
     to a new directory and the baseline server serves the original data from it;
     the automatic pre-upgrade backup is likewise usable by the baseline server.
  6. The candidate's own backup of upgraded data restores to a new directory and
     serves the same data with a rotated restore epoch.
"""
import argparse
import hashlib
import io
import json
import os
import pathlib
import shutil
import sqlite3
import subprocess
import sys
import tarfile
import tomllib

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from smoke import free_port, request, stop, wait_for  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parents[1]
# The released Motion package (VALIDATION.md, 2026-10-08) was built from this commit.
BASELINE = '52491bf3e6ef323a33dd546687129bdb3b51d4d1'
TITLE = 'Upgrade Signal'
POSITION = 1.25


def sha256(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as stream:
        for block in iter(lambda: stream.read(1 << 20), b''):
            digest.update(block)
    return digest.hexdigest()


def latest_migration():
    return max(int(p.name[:4]) for p in (ROOT / 'migrations').glob('[0-9][0-9][0-9][0-9]_*.sql'))


def build_baseline(directory):
    """Build the baseline server from Git into its own source and target directories.

    An existing build in `directory` is reused only if it records this baseline commit.
    """
    binary = directory / 'target' / 'debug' / 'playscale'
    tool = directory / 'src' / 'scripts' / 'backup.py'
    stamp = directory / 'commit'
    if stamp.exists() and stamp.read_text() == BASELINE and binary.is_file() and tool.is_file():
        return binary, tool
    if directory.exists():
        shutil.rmtree(directory)
    source = directory / 'src'
    archive = subprocess.check_output(['git', 'archive', '--format=tar', BASELINE], cwd=ROOT)
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        tar.extractall(source, filter='data')
    # Build with the baseline's pinned toolchain explicitly: a `cargo` earlier on
    # PATH than the rustup proxy (e.g. Homebrew's) would ignore rust-toolchain.toml.
    channel = tomllib.loads((source / 'rust-toolchain.toml').read_text())['toolchain']['channel']
    env = dict(os.environ, CARGO_TARGET_DIR=str(directory / 'target'))
    env.pop('RUSTUP_TOOLCHAIN', None)
    with (directory / 'build.log').open('wb') as log:
        subprocess.run(['rustup', 'run', channel, 'rustc', '--version'], cwd=source, env=env,
                       stdout=log, stderr=subprocess.STDOUT, check=True)
        subprocess.run(['rustup', 'run', channel, 'cargo', 'build', '--locked', '-p', 'playscale',
                        '--bin', 'playscale'], cwd=source, env=env, stdout=log, stderr=subprocess.STDOUT,
                       check=True)
    stamp.write_text(BASELINE)
    return binary, tool


class Server:
    def __init__(self, binary, data, media, log, extra=()):
        self.port = free_port()
        self.data = data
        self.command = [str(binary), '--listen', f'127.0.0.1:{self.port}', '--data-dir', str(data),
                        '--library', str(media), '--demuxe-dir', str(data.parent / 'no-demuxe'), *extra]
        self.log = log

    def __enter__(self):
        self.stream = self.log.open('ab')
        self.process = subprocess.Popen(self.command, stdout=self.stream, stderr=subprocess.STDOUT)
        try:
            # Generous: an upgrade start backs up and migrates before serving, and
            # a shared, swapping host has taken over 50 s for that in debug builds.
            wait_for(lambda: self.process.poll() is not None or request(self.port, 'GET', '/health')[0] == 200, 180)
            assert self.process.poll() is None, f'server exited with {self.process.returncode}; see {self.log}'
        except BaseException:
            # Keep the startup failure as the reported error: a server that is
            # still starting may not handle SIGINT, so do not assert its exit.
            self.process.kill()
            self.process.wait()
            self.stream.close()
            raise
        return self

    def __exit__(self, exc_type, exc, traceback):
        try:
            if exc_type is None:
                stop(self.process)  # a clean, bounded SIGINT shutdown is part of the check
            else:
                # Report the failure that ended the block, not a shutdown assertion.
                if self.process.poll() is None:
                    self.process.kill()
                self.process.wait()
        finally:
            self.stream.close()

    def auth(self):
        return {'Authorization': 'Bearer ' + (self.data / 'admin-token').read_text().strip()}

    def json(self, method, path, body=None, auth=True, expected=200):
        status, _, data = request(self.port, method, path, body, self.auth() if auth else None)
        assert status == expected, (method, path, status, data[:500])
        return json.loads(data) if data else None


def observe(server):
    """The externally visible state this check preserves across versions."""
    items = server.json('GET', '/api/v1/items')['items']
    assert len(items) == 1, items
    item = items[0]
    status, _, body = request(server.port, 'GET', item['media_url'], headers=server.auth())
    assert status == 200, status
    progress = server.json('GET', f'/api/v1/profiles/default/progress/{item["id"]}')
    libraries = server.json('GET', '/api/v1/libraries')
    return {
        'item_id': item['id'],
        'file_id': item.get('file_id'),
        'title': item['title'],
        'library_ids': sorted(library['id'] for library in libraries),
        'media_sha256': hashlib.sha256(body).hexdigest(),
        'position_seconds': progress['position_seconds'],
    }


def database_facts(path):
    db = sqlite3.connect(f'file:{path}?mode=ro', uri=True)
    try:
        return {
            'integrity': db.execute('PRAGMA integrity_check').fetchone()[0],
            'foreign_key_violations': len(db.execute('PRAGMA foreign_key_check').fetchall()),
            'migrations': [v for (v,) in db.execute(
                'SELECT version FROM _sqlx_migrations WHERE success=1 ORDER BY version')],
        }
    finally:
        db.close()


def upgrade_receipts(path, item_id):
    """Data-upgrade receipts and the legacy progress attribution for the fixture."""
    db = sqlite3.connect(f'file:{path}?mode=ro', uri=True)
    try:
        receipts = db.execute('SELECT id, kind, document_json FROM catalog_receipts '
                              "WHERE kind LIKE 'migration:%' ORDER BY kind, id").fetchall()
        attribution = db.execute('SELECT profile_id, item_id, outcome, timeline_id, candidates_json, receipt_id '
                                 'FROM legacy_progress_attribution ORDER BY profile_id, item_id').fetchall()
        timelines = [t for (t,) in db.execute(
            'SELECT t.id FROM timelines t JOIN editions e ON e.id = t.edition_id WHERE e.item_id = ? ORDER BY t.id',
            (item_id,))]
    finally:
        db.close()
    kinds = [kind for _, kind, _ in receipts]
    assert kinds == ['migration:legacy_progress', 'migration:schema'], kinds
    progress_receipt, schema_receipt = receipts
    assert json.loads(progress_receipt[2]) == {'outcomes': {'exact': 1}}, progress_receipt
    # The fixture has one timeline holding one original: attribution is exact,
    # to that timeline, with it as the only candidate.
    assert len(timelines) == 1, timelines
    expected = [('default', item_id, 'exact', timelines[0], json.dumps([[timelines[0], 1]], separators=(',', ':')),
                 progress_receipt[0])]
    assert attribution == expected, (attribution, expected)
    return {'receipts': receipts, 'attribution': attribution, 'schema': json.loads(schema_receipt[2])}


def upgrade_backups(data):
    directory = data / 'upgrade-backups'
    return sorted(directory.glob('pre-upgrade-*.sqlite3')) if directory.exists() else []


def run(baseline, baseline_backup_tool, candidate, work):
    import backup as candidate_backup_tool  # the candidate's scripts/backup.py
    checks = {}
    media = work / 'media'
    media.mkdir()
    fixture = media / 'Upgrade-Signal.mp4'
    subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i',
                    'testsrc2=size=320x180:rate=24', '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000',
                    '-t', '3', '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-c:a', 'aac', '-movflags', '+faststart',
                    str(fixture)], check=True)
    data = work / 'data'
    log = work / 'servers.log'

    # 1. Baseline writes real state.
    with Server(baseline, data, media, log) as server:
        item = wait_for(lambda: (server.json('GET', '/api/v1/items')['items'] or [None])[0], 60)
        server.json('PUT', f'/api/v1/items/{item["id"]}/metadata/catabolic',
                    {'expected_revision': 0, 'external_id': 'upgrade-item', 'values': {'title': TITLE}, 'tags': []})
        server.json('PUT', f'/api/v1/profiles/default/progress/{item["id"]}', {'position_seconds': POSITION}, auth=False)
        before = observe(server)
    assert before['title'] == TITLE and before['position_seconds'] == POSITION, before
    assert before['media_sha256'] == sha256(fixture)
    baseline_facts = database_facts(data / 'playscale.sqlite3')
    operator_backup = work / 'operator-backup-baseline'
    subprocess.run([sys.executable, '-I', str(baseline_backup_tool), 'backup', '--data-dir', str(data),
                    '--output', str(operator_backup)], check=True, stdout=subprocess.DEVNULL)
    checks['baseline_state_and_operator_backup'] = {'observed': before, 'schema': baseline_facts['migrations']}

    # 2. In-place upgrade with the candidate's defaults.
    latest = latest_migration()
    with Server(candidate, data, media, log) as server:
        after = observe(server)
        anonymous = request(server.port, 'GET', '/api/v1/items')[0]
        capabilities = server.json('GET', '/api/v2/system/capabilities')
    assert after == before, (before, after)
    assert anonymous in (401, 403), f'restricted mode served the legacy catalog anonymously: {anonymous}'
    assert capabilities['schema_version'] == str(latest), capabilities
    backups = upgrade_backups(data)
    assert len(backups) == 1, backups
    expected_name = f'pre-upgrade-{baseline_facts["migrations"][-1]}-to-{latest}-'
    assert backups[0].name.startswith(expected_name), backups[0].name
    upgraded = database_facts(data / 'playscale.sqlite3')
    on_disk = sorted(int(p.name[:4]) for p in (ROOT / 'migrations').glob('[0-9][0-9][0-9][0-9]_*.sql'))
    assert upgraded == {'integrity': 'ok', 'foreign_key_violations': 0, 'migrations': on_disk}, upgraded
    assert database_facts(backups[0]) == baseline_facts
    receipts = upgrade_receipts(data / 'playscale.sqlite3', before['item_id'])
    assert receipts['schema']['from_version'] == baseline_facts['migrations'][-1], receipts
    assert receipts['schema']['to_version'] == latest, receipts
    assert pathlib.Path(receipts['schema']['verified_backup']).name == backups[0].name, receipts
    checks['in_place_upgrade'] = {'from': baseline_facts['migrations'][-1], 'to': latest,
                                  'legacy_anonymous_status': anonymous, 'upgrade_backup': backups[0].name}

    # 3. A second start is not an upgrade.
    with Server(candidate, data, media, log) as server:
        assert observe(server) == before
    assert upgrade_backups(data) == backups
    assert upgrade_receipts(data / 'playscale.sqlite3', before['item_id']) == receipts
    checks['restart_is_not_an_upgrade'] = {'upgrade_backups': 1, 'receipt_ids_unchanged': True}

    # 4. The baseline refuses the newer schema without damaging it.
    refused = subprocess.run(Server(baseline, data, media, log).command, stdout=subprocess.PIPE,
                             stderr=subprocess.STDOUT, timeout=60)
    with log.open('ab') as stream:
        stream.write(refused.stdout)
    assert refused.returncode != 0, 'baseline server accepted a newer schema'
    # The baseline's sqlx migrator rejects the first migration it does not know.
    unknown = f'migration {baseline_facts["migrations"][-1] + 1} was previously applied but is missing'
    assert unknown.encode() in refused.stdout, refused.stdout[-2000:]
    assert database_facts(data / 'playscale.sqlite3') == upgraded
    with Server(candidate, data, media, log) as server:
        assert observe(server) == before
    checks['baseline_refuses_newer_schema'] = {'exit_code': refused.returncode, 'diagnostic': unknown}

    # 5. Rollback to the baseline from either pre-upgrade copy.
    rollback = work / 'rollback-from-operator-backup'
    candidate_backup_tool.restore(operator_backup, rollback)
    with Server(baseline, rollback, media, log) as server:
        assert observe(server) == before
    automatic = work / 'rollback-from-upgrade-backup'
    automatic.mkdir(mode=0o700)
    shutil.copyfile(backups[0], automatic / 'playscale.sqlite3')
    with Server(baseline, automatic, media, log) as server:
        assert observe(server) == before
    checks['rollback_to_baseline'] = ['operator_backup_via_candidate_restore', 'automatic_pre_upgrade_backup']

    # 6. Candidate backup and restore of upgraded data.
    snapshot = work / 'operator-backup-candidate'
    manifest = candidate_backup_tool.backup(data, snapshot)
    restored = work / 'restored-candidate'
    candidate_backup_tool.restore(snapshot, restored)
    with Server(candidate, restored, media, log) as server:
        assert observe(server) == before

    def epoch(path):
        db = sqlite3.connect(f'file:{path}?mode=ro', uri=True)
        try:
            return db.execute('SELECT server_id, restore_epoch FROM server_identity').fetchone()
        finally:
            db.close()
    (server_id, original), (restored_id, rotated) = epoch(data / 'playscale.sqlite3'), epoch(restored / 'playscale.sqlite3')
    assert server_id == restored_id and original != rotated
    checks['candidate_backup_restore'] = {'schema_versions': manifest['schema_versions'][-1:], 'restore_epoch_rotated': True}
    return checks


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--work', type=pathlib.Path, required=True, help='new directory for all outputs')
    parser.add_argument('--binary', type=pathlib.Path, default=ROOT / 'target/debug/playscale')
    parser.add_argument('--baseline-dir', type=pathlib.Path, default=ROOT / 'artifacts/baseline-server',
                        help='baseline source and build directory, reused when it matches BASELINE')
    args = parser.parse_args()
    work = args.work.resolve()
    work.mkdir(parents=True)  # never reuse earlier results
    baseline, tool = build_baseline(args.baseline_dir.resolve())
    candidate = args.binary.resolve()
    receipt = {'baseline_commit': BASELINE, 'baseline_sha256': sha256(baseline),
               'candidate_sha256': sha256(candidate), 'checks': run(baseline, tool, candidate, work)}
    (work / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt, indent=2))


if __name__ == '__main__':
    main()
