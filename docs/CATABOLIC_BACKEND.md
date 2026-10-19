# Native Catabolic integration

Worktree branch: `feat/catabolic-main-backend`.
Pinned upstream: `6969f08c5d9b207afe8eec819555759be136644d` (Catabolic main).

The implementation provides native dependency alignment, operator catalog
admission, durable commands, consistent Motion source capture and typed metadata/pin
staging in `crates/catalog-backend`. The server still uses its existing catalog
routes/scanner. Binding an operator catalog does not activate it for the
server. No real catalog migration has been performed.

The ownership and remaining cutover steps are in
[ADR 0002](adr/0002-catabolic-native-backend.md).

## Preparation tool

Build with Kyoto, then use the emitted `motion-catalog` binary. Use a disposable
Motion database copy for integration qualification. The two stores must be
separate. Native initialization refuses an existing file; inspection is read-only.
The destination's parent must already exist and satisfy Catabolic's private
filesystem policy. The binding commands run Motion's normal database upgrades.

```
motion-catalog --catalog /private/path/catalog.sqlite3 init
motion-catalog --catalog /private/path/catalog.sqlite3 inspect
motion-catalog --catalog /private/path/catalog.sqlite3 bind --motion-db /private/path/motion-copy.sqlite3
motion-catalog --catalog /private/path/catalog.sqlite3 put-items --motion-db /private/path/motion-copy.sqlite3 --operation-id seed-001 --input items.json
motion-catalog --catalog /private/path/catalog.sqlite3 retry --motion-db /private/path/motion-copy.sqlite3 --operation-id seed-001
```

`items.json` is a Catabolic operator item array, for example:

```json
[{"id":"fixture-work","kind":"video","metadata":{"title":"Fixture"}}]
```

This is not a Motion migration format. It does not preserve editions, timelines,
physical observations, metadata provenance or application references. Do not use
item batches as a shortcut around typed migration staging and activation.

A repeated key with the same request reuses the saved intent. Different payloads
under that key are rejected. Stale generation and uncertain/expired outcomes are
reported, with a nonzero CLI exit, rather than retried under a fresh identity.
The local journal never fabricates a committed receipt or recycles an operation.

## Durable import staging

The operator CLI now accepts Catabolic's native `SnapshotManifest` and `ImportBatch`
contract wrapped as `{ "manifest": ..., "batches": [...] }`:

```
motion-catalog --catalog /private/path/catalog.sqlite3 prepare-import --motion-db /private/path/motion-copy.sqlite3 --input snapshot.json
motion-catalog --catalog /private/path/catalog.sqlite3 stage-import --motion-db /private/path/motion-copy.sqlite3 --import-id migration-001
motion-catalog --catalog /private/path/catalog.sqlite3 validate-import --motion-db /private/path/motion-copy.sqlite3 --import-id migration-001
```

Preparation validates every descriptor with the native canonical encoder, then
commits the complete supplied snapshot before dispatching any native command.
An import ID is immutable. Staging and validation journal exact native command
identities and replay them after restart, including after an interrupted batch.
No command regenerates stale envelopes or changes an import's saved input.

The native contract permits at most 16 batches, each at most 64 KiB and 64 items
(and separately bounded evidence/typed records). The CLI also caps the input at
2 MiB. Oversized input is rejected; no records are truncated. Larger catalog
migration needs an explicit partitioning/activation design before use.

Validation rehearses native typed owners without publishing rows. The Rust API
accepts independently constructed typed grants; the CLI does not infer those
grants from input JSON, so typed imports requiring grants are refused there.
Unresolved evidence remains in receipts and yields `activation_supported: false`.
Even `activation_supported: true` only describes native staging: it does not prove
that Motion writers are frozen or that application references have been audited.
There is deliberately no activation command yet.

The source exporter below now supplies Motion observations to this transport.
Complete typed mappings, reference audit, activation and
startup reconciliation remain required before server cutover.

## Motion source capture

```
motion-catalog --catalog /private/path/catalog.sqlite3 capture-motion-import --motion-db /private/path/motion-copy.sqlite3 --import-id motion-001
motion-catalog --catalog /private/path/catalog.sqlite3 stage-import --motion-db /private/path/motion-copy.sqlite3 --import-id motion-001
motion-catalog --catalog /private/path/catalog.sqlite3 validate-motion-import --motion-db /private/path/motion-copy.sqlite3 --import-id motion-001
```

Capture takes a SQLite writer transaction over Motion catalog schema 68, with
host control migrations through 69. It retains a
consistent bounded snapshot and an immutable exporter-provenance marker in the
same commit. Repeating capture returns the saved source snapshot, even after the
source metadata changes. It does not keep Motion writers frozen after commit.

Metadata values retain their local/provider/scan classification and source
revisions. Scanned origin titles have no source revision column; their export
record starts at revision 1 and explicitly records that basis. Native validation
rehearses these typed metadata imports with grants derived only from trusted
source-capture provenance. Copying a snapshot through `prepare-import` does not
copy that authority. The production core checks the exact namespace, digest and
supported exporter version before the adapter constructs any owner grants.

Export namespace `motion.schema68.export.v2` also imports current provider
identities protected by Motion's manual match state as native pins. The core
uses the same identity-withdrawal policy as the running application and excludes
retired aliases. Other identities stay evidence. Only the current source head
is available: its metadata-document revision maps explicitly to native pin
revision 1; no earlier decisions or removal history are invented. Existing v1
snapshots remain immutable and can still be validated. Pin grants are rebuilt
only from the trusted retained export.

Tags/exclusions, identity evidence, structural decisions, timelines, physical
occurrences, source bindings and artwork references remain explicit unresolved
evidence until their native mappings are implemented. Direct relational
application references retain their key/reference columns; unrelated payloads
and credentials are not copied. Embedded JSON references still need an explicit
audit. Artwork bytes remain in the Motion artifact store. Oversized records or
catalogs fail the whole capture without retaining a partial export.

This exporter records the missing reference audit and whether capture was bound
to a persistent writer freeze. Its current unresolved output cannot activate a
native catalog.
It is a migration preparation path; it does not claim semantic equivalence for
the unresolved data or authorize serving the imported catalog. Further mapping
work will require a new captured import identity, not rewriting saved snapshots.

## Durable source freeze

For an offline migration copy, inspect admission and freeze a **new** import ID
before capture:

```
motion-catalog --catalog /private/path/catalog.sqlite3 source-state --motion-db /private/path/motion-copy.sqlite3
motion-catalog --catalog /private/path/catalog.sqlite3 freeze-motion-source --motion-db /private/path/motion-copy.sqlite3 --import-id frozen-001 --expected-revision 0
motion-catalog --catalog /private/path/catalog.sqlite3 capture-motion-import --motion-db /private/path/motion-copy.sqlite3 --import-id frozen-001
```

Use the actual returned state revision; 0 above describes a fresh control state.
Freeze installs and verifies SQLite write guards and records source admission in
one writer transaction. Ordinary source/application tables and FTS shadow tables
are guarded; host command/control tables remain writable. SQLite virtual tables
cannot themselves have triggers. Normal server database startup refuses frozen
sources before migrations or application upgrade hooks. The operator connection
can inspect and reconcile them without running those hooks.

A captured export records its freeze epoch atomically. The trusted coordinator's
`require_frozen_export` rechecks the current epoch, migration ID and guard
inventory; a historical snapshot field is not current freeze authority. Reusing
an old import ID, stale release, competing ownership or altered guard inventory
is refused. Freeze retries do not append duplicate events.

Before native cutover begins, an abandoned freeze can release legacy writes:

```
motion-catalog --catalog /private/path/catalog.sqlite3 abandon-motion-freeze --motion-db /private/path/motion-copy.sqlite3 --freeze-revision 1
```

Use the returned freeze epoch, which may differ from the latest state revision.
Abandon invalidates the export's current freeze witness. The core's `Seal`
transition permanently refuses abandon; only the trusted coordinator exposes
that Rust API, and there is no CLI activation/seal command yet.

Source write-freezing does **not** stop media readers, drain processing workers,
revoke native leases, acknowledge external effects, or finish the reference audit.
Those obligations remain required before native activation. The current exporter
still retains unresolved mappings/audit evidence and cannot activate a catalog.

## Qualification

Rust compilation is submitted to `/Volumes/seed1/Projects/kyoto/kyoto.py`.
The shared policy compiles tests with `--no-run`; execution is a separate step.
Final results belong in `CATABOLIC_BACKEND_QUALIFICATION.md`.

Required before cutover: scoped user/API composition, typed migration and restart
reconciliation, real source/helper lifecycle, playback/reference audit, full
regression and backup/restore checks, and package qualification. The operator
adapter is not authorized for direct exposure to untrusted clients.
