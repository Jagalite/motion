"""Exercise migration 0045 against the real preceding SQLite schema, in memory."""
import json
import pathlib
import sqlite3

root = pathlib.Path(__file__).resolve().parents[2]
db = sqlite3.connect(':memory:')
db.execute('PRAGMA foreign_keys=ON')
for migration in sorted((root / 'migrations').glob('*.sql')):
    if int(migration.name[:4]) >= 45:
        break
    db.executescript(migration.read_text())
policy = json.dumps({'library_ids': [], 'allow_unrated': True, 'allowed_ratings': [], 'blocked_labels': []})
for identity in ('legacy-a', 'legacy-b'):
    db.execute('INSERT INTO devices VALUES (?,?,?,?,?,?,?,?,?,?,?)',
               (identity, 'Same display name', 'legacy-client', 3, '["default"]', '["catalog:read"]', policy, 0, 2, 9000, 100))
    db.execute('INSERT INTO credentials VALUES (?,?,?,?,?,?,?)',
               ('hash-'+identity, identity, 'device', 2, 9000, None, None))
before = db.execute('SELECT * FROM devices ORDER BY id').fetchall()
credentials = db.execute('SELECT * FROM credentials ORDER BY token_hash').fetchall()
db.executescript((root / 'migrations/0045_access_management.sql').read_text())
after = db.execute('SELECT * FROM devices ORDER BY id').fetchall()
assert [r[:-1] for r in after] == before
assert len({r[-1] for r in after}) == 2
assert db.execute('SELECT * FROM credentials ORDER BY token_hash').fetchall() == credentials
assert db.execute('SELECT count(*) FROM access_users WHERE legacy=1 AND name=?', ('Legacy device: Same display name',)).fetchone()[0] == 2
assert db.execute('PRAGMA foreign_key_check').fetchall() == []
print(json.dumps({'migration': 45, 'legacy_devices': 2, 'identity_and_grants_preserved': True,
                  'credential_rows_preserved': True, 'distinct_explicit_owners': True,
                  'foreign_key_violations': 0}))
