"""Restore establishes a fresh event epoch so old cursors reset."""
import pathlib
import sqlite3
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import backup  # noqa: E402


def database(path):
    db = sqlite3.connect(path)
    db.executescript("""
        CREATE TABLE _sqlx_migrations (version INTEGER PRIMARY KEY, success INTEGER);
        INSERT INTO _sqlx_migrations VALUES (11, 1);
        CREATE TABLE libraries (id TEXT PRIMARY KEY);
        CREATE TABLE server_identity (singleton INTEGER PRIMARY KEY, server_id TEXT, restore_epoch TEXT);
        INSERT INTO server_identity VALUES (1, 'server', 'original');
        CREATE TABLE devices (id TEXT PRIMARY KEY, revoked INTEGER NOT NULL);
        INSERT INTO devices VALUES ('restored-device', 0);
        CREATE TABLE credentials (token_hash TEXT PRIMARY KEY, device_id TEXT REFERENCES devices(id));
        INSERT INTO credentials VALUES ('still-unexpired', 'restored-device');
        CREATE TABLE content_tickets (id TEXT PRIMARY KEY, credential_hash TEXT REFERENCES credentials(token_hash));
        INSERT INTO content_tickets VALUES ('device-ticket', 'still-unexpired'), ('operator-ticket', NULL);
        CREATE TABLE pairings (id TEXT PRIMARY KEY);
        INSERT INTO pairings VALUES ('approved-pairing');
        CREATE TABLE idempotency_records (id TEXT PRIMARY KEY);
        INSERT INTO idempotency_records VALUES ('secret-replay');
    """)
    db.commit()
    db.close()


class RestoreEpoch(unittest.TestCase):
    def test_restore_rotates_epoch_and_keeps_server_identity(self):
        with tempfile.TemporaryDirectory() as root:
            root = pathlib.Path(root)
            (root / 'data').mkdir()
            database(root / 'data' / 'playscale.sqlite3')
            backup.backup(root / 'data', root / 'backup')
            backup.restore(root / 'backup', root / 'restored')
            db = sqlite3.connect(root / 'restored' / 'playscale.sqlite3')
            server, epoch = db.execute('SELECT server_id, restore_epoch FROM server_identity').fetchone()
            for table in ('credentials', 'content_tickets', 'pairings', 'idempotency_records'):
                self.assertEqual(db.execute(f'SELECT count(*) FROM {table}').fetchone()[0], 0, table)
            self.assertEqual(db.execute('SELECT revoked FROM devices').fetchone()[0], 1)
            db.close()
            with sqlite3.connect(root / 'restored' / 'playscale.sqlite3') as restored:
                histories = {row[0] for row in restored.execute('SELECT viewing_epoch FROM server_viewing_histories')}
                self.assertEqual(histories, {'server:original', f'server:{epoch}'})
            original = sqlite3.connect(root / 'data' / 'playscale.sqlite3')
            self.assertEqual(original.execute('SELECT revoked FROM devices').fetchone()[0], 0)
            original.close()
            self.assertEqual(server, 'server')
            self.assertNotEqual(epoch, 'original')
            self.assertRegex(epoch, '^[0-9a-f]{32}$')

    def test_restore_records_a_pending_viewing_baseline_for_the_new_epoch(self):
        with tempfile.TemporaryDirectory() as root:
            root = pathlib.Path(root)
            (root / 'data').mkdir()
            database(root / 'data' / 'playscale.sqlite3')
            backup.backup(root / 'data', root / 'backup')
            backup.restore(root / 'backup', root / 'restored')
            db = sqlite3.connect(root / 'restored' / 'playscale.sqlite3')
            epoch = db.execute('SELECT restore_epoch FROM server_identity').fetchone()[0]
            pending = db.execute('SELECT restore_epoch, captured FROM restore_baseline_epoch').fetchone()
            db.close()
            self.assertEqual(pending, (epoch, 0))


if __name__ == '__main__':
    unittest.main()
