
### Post24 candidate: explicit bound native schema transition (unqualified)

Motion89 adds exact append-only native39→40 intent/ACK control tables, default-deny
ordinary connection guards, isolated exact-payload insert capability and full
journal schema verification. Offline prepare/execute commands retain the actual
server lock, require explicit external-reader/worker quiescence, never start HTTP
or background jobs, and never rewrite the original binding/import checkpoints.
A pending intent denies normal startup; only exact native post-commit preservation
under retained admission/writer/SQLite locks can ACK. Effective current identity
is used only after that ACK. Known39 source record envelopes remain retained and
readable by the40 source interpreter; strict catalog Identity admission is unchanged.

Native plans retain raw pending diagnostics and add the existing bounded exact
worker/companion-witness qualified inventory. Deferred terminal history may be
settled only by those actual witnesses; every unresolved lifetime/PID/lease and
truncated inventory remains blocking. Missing/changed ownership is not repaired.
Reference column/nullability/PK/FK descriptors for the two89 tables were derived
from the actual migration SQL. No41 migration is registered.

New targets are backend native_upgrade (four real39 activation/upgrade/restart,
crash-boundary, mutation and guard negatives), native_upgrade_reconciliation,
native_upgrade_inventory unit tests and the actual observer deferred-history
upgrade regression. These are source candidates, not test passes. The operator
CLI requires separate packaged-process qualification before deployment. The
post24 pre-effect render recovery and metadata background guards also remain
pending the parent validation snapshot.
