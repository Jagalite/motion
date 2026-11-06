"""Exhaustive 0058 migration conformance, without rewriting shipped SQL."""
import pathlib
import sqlite3
import unittest

class LegacyOwnership(unittest.TestCase):
    def test_all_dispositions(self):
        migration = pathlib.Path(__file__).resolve().parents[1] / 'migrations/0058_legacy_access_ownership.sql'
        for legacy in (0, 1):
            for synthetic in (False, True):
                for count in range(3):
                    for invitation in (False, True):
                        with self.subTest(legacy=legacy, synthetic=synthetic, count=count, invitation=invitation):
                            db=sqlite3.connect(':memory:')
                            db.executescript('CREATE TABLE access_users(id TEXT PRIMARY KEY,legacy INTEGER); CREATE TABLE devices(id TEXT PRIMARY KEY,user_id TEXT); CREATE TABLE access_invitations(user_id TEXT);')
                            user='user-d0' if synthetic else 'edited-user'
                            db.execute('INSERT INTO access_users VALUES (?,?)',(user,legacy))
                            for n in range(count): db.execute('INSERT INTO devices VALUES (?,?)',(f'd{n}',user))
                            if invitation: db.execute('INSERT INTO access_invitations VALUES (?)',(user,))
                            db.executescript(migration.read_text())
                            detach=bool(legacy and synthetic and count==1 and not invitation)
                            self.assertEqual(db.execute('SELECT count(*) FROM access_users').fetchone()[0],0 if detach else 1)
                            self.assertEqual(db.execute('SELECT count(*) FROM devices WHERE user_id IS NOT NULL').fetchone()[0],0 if detach else count)
                            if not detach: self.assertEqual(db.execute('SELECT legacy FROM access_users').fetchone()[0],0)
                            db.close()
if __name__ == '__main__': unittest.main()
