"""Run the actual 0045/0046 SQL against populated, isolated legacy databases."""
import json
import pathlib
import sqlite3

ROOT = pathlib.Path(__file__).resolve().parents[2]
POLICY = json.dumps({"library_ids": [], "allow_unrated": True,
                     "allowed_ratings": [], "blocked_labels": []})


def database():
    db = sqlite3.connect(":memory:")
    db.execute("PRAGMA foreign_keys=ON")
    for migration in sorted((ROOT / "migrations").glob("*.sql")):
        if int(migration.name[:4]) >= 45:
            break
        db.executescript(migration.read_text())
    for identity in ("untouched", "edited", "invited", "shared", "revoked"):
        db.execute("INSERT INTO devices VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                   (identity, "Same display name", "legacy", 7, '["default"]',
                    '["catalog:read"]', POLICY, identity == "revoked", 2, 9000, 100))
        db.execute("INSERT INTO credentials VALUES (?,?,?,?,?,?,?)",
                   ("hash-" + identity, identity, "device", 2, 9000, None, None))
    return db


def apply(db, version):
    migration, = (ROOT / "migrations").glob(f"{version:04d}_*.sql")
    db.executescript(migration.read_text())


for previously_managed in (False, True):
    db = database()
    apply(db, 45)
    if previously_managed:
        db.execute("UPDATE access_users SET name='Explicit person',legacy=0,revision=2 WHERE id='user-edited'")
        db.execute("UPDATE devices SET user_id='user-shared' WHERE id='revoked'")
        db.execute("""INSERT INTO access_invitations
                      (id,code_hash,user_id,user_revision,issuer_id,issuer_revision,
                       profile_ids,permissions,policy,created_at,expires_at)
                      VALUES ('invitation','hash','user-invited',1,'operator',0,
                              '["default"]','["catalog:read"]',?,100,1000)""", (POLICY,))
    before = db.execute("SELECT id,name,client_name,revision,profile_ids,permissions,policy,revoked,generation,credential_expires_at,created_at FROM devices ORDER BY id").fetchall()
    credentials = db.execute("SELECT * FROM credentials ORDER BY token_hash").fetchall()
    invitations = db.execute("SELECT * FROM access_invitations ORDER BY id").fetchall()
    apply(db, 46)
    after = db.execute("SELECT id,name,client_name,revision,profile_ids,permissions,policy,revoked,generation,credential_expires_at,created_at FROM devices ORDER BY id").fetchall()
    assert before == after
    assert credentials == db.execute("SELECT * FROM credentials ORDER BY token_hash").fetchall()
    assert invitations == db.execute("SELECT * FROM access_invitations ORDER BY id").fetchall()
    owners = dict(db.execute("SELECT id,user_id FROM devices"))
    assert owners["untouched"] is None
    if previously_managed:
        assert owners["edited"] == "user-edited"
        assert owners["invited"] == "user-invited"
        assert owners["shared"] == owners["revoked"] == "user-shared"
        assert db.execute("SELECT name,revision FROM access_users WHERE id='user-edited'").fetchone() == ("Explicit person", 2)
    else:
        assert all(owner is None for owner in owners.values())
        assert db.execute("SELECT count(*) FROM access_users").fetchone()[0] == 0
    assert db.execute("PRAGMA foreign_key_check").fetchall() == []
    assert db.execute("SELECT count(*) FROM access_users WHERE legacy=1").fetchone()[0] == 0
    db.close()

print(json.dumps({"migration": 46, "scenarios": 2, "legacy_devices_per_scenario": 5,
                  "device_and_credential_bytes_preserved": True,
                  "explicit_users_and_invitations_preserved": True,
                  "legacy_devices_unassigned": True, "foreign_key_violations": 0}))
