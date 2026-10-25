
### Explicit bound native39→40 operator transition (candidate, not yet qualified)

Stop the service and every other reader/worker of both catalogs and managed
outputs first. The command acquires the actual Motion server lock, opens no HTTP
listener and starts no background work. The assertion flag is an explicit
operator statement about external processes, not a fact inferred from an idle
queue or advisory lock.

- `playscale --data-dir PATH --native-schema-upgrade prepare --confirm-native-upgrade-quiescence`
- Review the returned exact saved plan and backup/forward-recovery requirements.
- `playscale --data-dir PATH --native-schema-upgrade execute --confirm-native-upgrade-quiescence`

Use `--catalog-path PATH` for a custom native database. This never initializes a
missing catalog, rewrites the original binding, or starts legacy fallback. A
pending intent denies ordinary native startup. If interrupted after native
commit, repeat the execute command: only exact original-state preservation under
native admission, writer and SQLite exclusive locks may append the Motion ACK.
Any subsequent native mutation before ACK requires operator investigation; schema
40 alone is not evidence. Native requires rollback-journal mode and refuses WAL;
this command does not silently convert journal mode. Original frozen imports,
receipts and source envelopes remain intact. This candidate must pass the native
upgrade reconciliation and Motion native_upgrade targets before deployment.
