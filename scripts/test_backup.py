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
            db.close()
            self.assertEqual(server, 'server')
            self.assertNotEqual(epoch, 'original')
            self.assertRegex(epoch, '^[0-9a-f]{32}$')


if __name__ == '__main__':
    unittest.main()
