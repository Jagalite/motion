# Native Catabolic catalog backend

Decision: begin replacing Motion's catalog implementation with Catabolic, pinned
initially to main commit 6969f08c5d9b207afe8eec819555759be136644d. This supersedes
the old reference-only architectural direction. It does not activate a live
catalog or claim the migration is complete.

Catabolic will own inventory, logical catalog identity, metadata resolution,
relationships and search. Motion retains application authorization, viewing,
playback/delivery, downloads, queues/playlists and UI. Each store has one owner;
Motion must eventually keep only references and disposable catalog read models.

## Deployment and dependencies

Use native Rust calls. Upgrade the server toolchain to 1.99 and retain SQLx 0.8.6
with the recorded one-line SQLite binding compatibility patch under vendor/.
Both owners use libsqlite3-sys 0.38.2. Catabolic's custom fancy-regex is pinned
from the same Git revision, and its SQLite flags are applied at the host root.
Do not inherit qualification from Catabolic's historical Linux dependency probe.
The old private-process inventory prototype remains separate, unchanged.

## First implemented boundary

The `motion-catalog-backend` crate is shared by the server, operator CLI and
focused native tests. It opens an existing catalog without implicit upgrade/recovery.
Motion persists an immutable catalog/incarnation/schema binding. Production core
policy compares all three facts against the same opened Store used by an action.
A changed catalog or restore incarnation requires explicit reconciliation.

For operator item commands, Motion commits the exact command envelope to its own
SQLite journal before dispatch. Catabolic owns mutation validation and commits
its effect and receipt atomically. Retrying reads the original envelope, including
its generation precondition. A crash after Catabolic commit but before receipt
delivery replays the same command; stale generation is not silently rebased.
This is two durable commits with replay, not a cross-database transaction.

Native operations run on blocking workers. A semaphore and native authorization
lease move into each worker, so cancelling an async caller cannot release them
early. Revocation closes admission before waiting for operations to drain.

The new CLI is an operator preparation tool, not an application-user API. Its
item commands must not be exposed to browser/device clients. Public integration
must compose existing Motion permission checks with Catabolic's scoped grants,
field masks and shared revocation gates. There is no production backend switch
in this slice and no automatic fallback to Motion after Catabolic errors.

## Verification boundaries

Core tests enumerate 729 binding comparisons and distinguish restore, replacement
and schema drift. Adapter tests cover shared SQLite identity/function flags,
a separate full-application migrations/reopen test, native host admission, durable pre-dispatch intent,
lost-response replay, key conflicts, stale generations, immutable bindings and
closed admission. This is not model coverage for migration/cutover or proof of
worker-helper, filesystem, platform or user authorization behavior.

The adapter compares canonical request identity to enforce the immutable journal
storage contract. Canonicalization and actual receipt replay decisions use
Catabolic's production command core, not a second replay implementation.

Catabolic's existing production core owns replay/metadata/identity decisions;
Motion does not copy those algorithms into a parallel model. The new Motion core
policy owns only host binding admission. Database transactions and blocking work
stay in adapters and are checked using real disposable databases.

## Import staging boundary

Motion retains the supplied manifest and all batch bytes in an immutable SQLite
row before native staging begins. Native import IDs determine journal keys;
resume replays original envelopes. Snapshot validation, duplicate identity
rejection, typed authority checks and rehearsal are delegated to Catabolic's
production owners. Motion adds the durable pre-dispatch ordering and immutable
input storage contract; those adapter obligations are tested using real SQLite
transactions and an injected journal failure between committed batches.

No new parallel import state machine is introduced. Native `ImportReceipt` facts
are preserved rather than flattened to a success flag. The snapshot is not a
claim of a frozen Motion source. No activation capability is minted by this path.
Future host freeze/reference decisions need explicit production state and adapter
enforcement before an activation entrypoint can be exposed.

## Motion export authority

Source capture and its immutable provenance marker share a `BEGIN IMMEDIATE`
transaction in Motion's database. The pure `ExportProvenance::admits` policy in
`crates/core` decides whether the retained source namespace, digest and mapper
version authorize typed source interpretation. The adapter supplies those
observations and enforces atomic storage; native metadata owners perform the
actual rehearsal. Caller-provided JSON cannot acquire a source-capture marker.

Core tests enumerate 432 provenance combinations. SQLite tests exercise the
actual source schema, failed capture rollback, retained-input replay after source
changes, and refusal of copied JSON as grant authority. This is finite policy
coverage plus adapter tests, not a Stateless model or proof of a frozen source.
The exporter records unsupported facts and missing reference/freeze evidence;
it cannot activate until those remaining obligations are implemented.

## Source freeze and adapter enforcement

`crates/core::catalog_source` owns source-write admission, revision checks,
non-reusable migration identities, stale epoch rejection, idempotent freeze/seal,
and the irreversible seal boundary. The same transition runs in production and
the Stateless model. The adapter stores state plus one event per changed
transition, with guard installation/verification in the same SQLite writer
transaction. Guards use the stored admission projection, checked against the
serialized core mode. Conflicting writers cannot commit across that transaction.

The bounded model covers two migration identities, revisions/epochs 0 through 5,
invalid IDs, stale events and duplicates. It exhausts 13 states and 390 transitions,
checking admission/output count, exact target, revision advancement, immutable
identity binding and sealed-state persistence. It is not proof of SQLite or
worker behavior. Integration tests separately check SQL refusal, durable rebind,
competing freeze ownership, captured epoch binding and guard inventory drift.

Normal application startup refuses frozen sources before its upgrades run.
Operator-only control access can abandon an unsealed freeze, after which old
captured exports lose current freeze authority. No worker drain, reference audit
or native activation is inferred from source admission alone.

## Remaining integration work

1. Export a consistent Motion catalog with explicit ID/reference/provenance
   mappings; use Catabolic's typed staging and activation/restart contracts.
2. Compose library and user permissions with scoped native reads and writes,
   including delivery leases and revocation. Adapt catalog/search wire models.
3. Route source registration, observations, metadata and identity mutations to
   Catabolic; integrate helper supervision with Motion resource admission.
4. Move playback and viewing catalog reads to native IDs without changing their
   ownership or temporal semantics. Audit downloads, aliases and playlists.
5. Validate a real migration copy, crash/restart/restore, concurrent commands,
   package/helper provenance and the full affected regression suite.
6. Activate one catalog only after both durable stores agree; then remove the
   obsolete scanner/resolver/catalog writers. A journal binding is not activation.
