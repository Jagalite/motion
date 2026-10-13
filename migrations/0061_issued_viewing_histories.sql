-- Re-anchoring an offline sequence requires an issued predecessor identity.
-- Restore retains this ledger and records its predecessor before rotation.
CREATE TABLE IF NOT EXISTS server_viewing_histories (
 viewing_epoch TEXT PRIMARY KEY
);
INSERT OR IGNORE INTO server_viewing_histories(viewing_epoch)
 SELECT server_id || ':' || restore_epoch FROM server_identity;
