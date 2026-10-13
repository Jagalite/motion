import hashlib
import json
from pathlib import Path
import sqlite3
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import reconcile_development_migrations as migration


class DevelopmentMigrationCopy(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / 'source.sqlite'
        self.destination = self.root / 'copy.sqlite'
        self.canonical, self.remapped = migration.known_migrations()

    def database(self, rows):
        with sqlite3.connect(self.source) as db:
            db.execute('CREATE TABLE _sqlx_migrations(version INTEGER PRIMARY KEY, checksum BLOB NOT NULL, success INTEGER NOT NULL)')
            db.executemany('INSERT INTO _sqlx_migrations VALUES(?,?,?)', rows)
            db.execute('CREATE TABLE credentials(token TEXT)')
            db.execute("INSERT INTO credentials VALUES('must-survive-copy')")
        return self.source.read_bytes()

    def test_each_known_branch_and_main_preserve_source_and_data(self):
        branches = [[], [e for e in self.remapped if e[0] == 45],
                    [e for e in self.remapped if e[0] < 50],
                    [e for e in self.remapped if e[0] >= 50]]
        for branch in branches:
            with self.subTest(branch=[x[0] for x in branch]):
                self.source.unlink(missing_ok=True)
                self.destination.unlink(missing_ok=True)
                rows = [(44, self.canonical[44], 1)] + [(old, checksum, 1) for old, checksum in branch]
                before = self.database(rows)
                moves = migration.reconcile(self.source, self.destination)
                self.assertEqual(self.source.read_bytes(), before)
                self.assertEqual(set(moves), {(old, self.remapped[(old, checksum)]) for old, checksum in branch})
                with sqlite3.connect(self.destination) as db:
                    self.assertEqual(db.execute('SELECT token FROM credentials').fetchone(), ('must-survive-copy',))
                    for version, checksum in db.execute('SELECT version,checksum FROM _sqlx_migrations'):
                        self.assertEqual(checksum, self.canonical[version])

    def test_canonical_completion_history_is_unchanged(self):
        before = self.database([(v, self.canonical[v], 1) for v in range(44, 53)])
        self.assertEqual(migration.reconcile(self.source, self.destination), [])
        self.assertEqual(self.source.read_bytes(), before)

    def test_unknown_or_dirty_history_fails_without_publishing(self):
        for row in [(45, b'unknown', 1), (44, self.canonical[44], 0), (999, b'unknown', 1)]:
            self.source.unlink(missing_ok=True)
            before = self.database([row])
            with self.assertRaises(ValueError):
                migration.reconcile(self.source, self.destination)
            self.assertFalse(self.destination.exists())
            self.assertEqual(self.source.read_bytes(), before)

    def test_destination_is_never_overwritten(self):
        self.database([(44, self.canonical[44], 1)])
        self.destination.write_bytes(b'existing')
        with self.assertRaises(FileExistsError):
            migration.reconcile(self.source, self.destination)
        self.assertEqual(self.destination.read_bytes(), b'existing')

    def test_duplicate_canonical_destination_fails(self):
        (old, checksum), new = next(iter(self.remapped.items()))
        self.database([(old, checksum, 1), (new, checksum, 1)])
        with self.assertRaisesRegex(ValueError, 'multiple migrations'):
            migration.reconcile(self.source, self.destination)
        self.assertFalse(self.destination.exists())

    def test_full_schema_and_branch_upgrades(self):
        paths = sorted((migration.ROOT / 'migrations').glob('*.sql'))
        common = [p for p in paths if int(p.name.split('_')[0]) <= 44]
        entries = json.loads((migration.ROOT / 'scripts/development_migration_map.json').read_text())
        lineages = [list(range(45, 53)), [57], [57, 58, 59], [53, 54, 55, 56]]
        for lineage in lineages:
            with self.subTest(lineage=lineage):
                self.source.unlink(missing_ok=True)
                self.destination.unlink(missing_ok=True)
                with sqlite3.connect(self.source) as db:
                    db.execute('PRAGMA foreign_keys=ON')
                    db.execute('CREATE TABLE _sqlx_migrations(version INTEGER PRIMARY KEY, checksum BLOB NOT NULL, success INTEGER NOT NULL)')
                    for path in common + [p for p in paths if int(p.name.split('_')[0]) in lineage]:
                        version = int(path.name.split('_')[0])
                        old = next((e['old'] for e in entries if e['new'] == version), version)
                        db.executescript(path.read_text())
                        db.execute('INSERT INTO _sqlx_migrations VALUES(?,?,1)', (old, hashlib.sha384(path.read_bytes()).digest()))
                        db.commit()
                migration.reconcile(self.source, self.destination)
                with sqlite3.connect(self.destination) as db:
                    db.execute('PRAGMA foreign_keys=ON')
                    applied = {r[0] for r in db.execute('SELECT version FROM _sqlx_migrations')}
                    for path in paths:
                        version = int(path.name.split('_')[0])
                        if version not in applied:
                            db.executescript(path.read_text())
                    self.assertEqual(db.execute('PRAGMA integrity_check').fetchone(), ('ok',))
                    self.assertEqual(db.execute('PRAGMA foreign_key_check').fetchall(), [])


if __name__ == '__main__':
    unittest.main()
