# Historical native integration qualification notes

Archived on 2026-10-20 after snapshot15. These entries preserve the evidence and unresolved findings as recorded at each milestone; their “pending” statements are historical, not current status. See [the current ledger](CATABOLIC_INTEGRATION_STATUS.md).

This is an implementation/verification checklist, not a completion claim. No deployment or user library migration has been performed. The unpublished pre-reset work was not recovered; current code is reconstructed from the published branch.

## Ownership and cutover

- Implemented: exact catalog/incarnation binding; original gate drain witness; durable activation envelope; receipt-only single-cutover reconciliation; application write exceptions only after matching host acknowledgment. Positive empty activation and lost-ack recovery are covered by prior immutable snapshot tests; newer schema transitions remain separately qualified.
- Implemented: immutable bounded export partition manifest, sequential durable stage/validate/activate, complete-set acknowledgment and retained native aggregate restart. Pure core complete-set rule rejects prefixes, reordering, duplicate identities, restored incarnations, and wrong freeze epochs. Full 1100-work activation/replay/restart test passed on immutable snapshot4p1 after reducing host partitions to respect the unchanged native operation budget.
- Implemented/under qualification: schema and exact column inventory, preserved raw/nested source metadata, safe logical-only nonempty migration, typed tags/relationships/aliases. Physical and application-reference-rich migration still being expanded; unresolved evidence remains closed.
- Pending qualification: process-kill fault injection at each host/native transaction boundary, partial set restart and cross-partition logical edge activation in Motion, schema-upgrade path for an already sealed installation.

## Server operations

| Operation | Current source implementation | Remaining qualification |
|---|---|---|
| Fresh native installation/default | Fresh default initializes native through audited empty cutover; existing legacy data requires explicit migration | Snapshot10p2 process tests passed create/read/register/restart and configuration-loss refusal; no silent legacy fallback |
| Native startup/restart | Retained owner, exact receipts, revision bridges, default-deny projection | Supported paths tested; process-kill coverage at every transaction boundary remains pending |
| Create/update work and metadata | Durable native commands, scoped Motion authorization, exact revision bridge | Basic HTTP and imported-revision tests passed; schema85 confirmed-stale attempt chain pending snapshot12 qualification |
| Get/browse/search work | Native owner reads and compatibility projection; source observations retain original media family | Scoped pagination and broader imported DTO coverage remain incomplete |
| Tags/pins/aliases and graph edits | Typed import owners present | Interactive graph mutations, complete scoped authorization and conflict coverage remain incomplete |
| Library/source ownership | Distinct logical/physical IDs, exact source mappings, native source registration | Imported registry acknowledgment/schema86 and richer saved-query membership pending snapshot12 qualification |
| Scan/discovery | Native observation, durable demand queue, cancellation custody, current physical bridge | Two cancellation tests passed; two normal queue tests exposed audio-kind mismatch. Shared exact family mapping and exporter rehearsal added, not yet tested |
| Physical compatibility | Native current manifest/ctime checks, original revision bridges, unavailable historical owners | Real current-file application tests passed; historical-only projection corrections and tombstones pending snapshot12 |
| Viewing/downloads | Motion application owner, exact native currentness checks, original history and idempotency retained | Snapshot10p2 application 4/4 passed, including real pre-cutover MP4 download/replay and viewing. Snapshot11 public viewing 4 passed; typed-edit fixture correction pending |
| Playback/processing | Unsupported paths fail closed; legacy catalog writers do not run in native mode | Full playback coverage and external processing retirement qualification remain open. Process-group observations are not cleanup authority |
| Auth/delivery/metrics | Motion owner retained; app-only workers isolated from catalog writes | Broader end-to-end isolation and recovery coverage remains pending |

## Validation record

Latest evidence is from immutable snapshots, not the current working tree:

- Snapshot10p2: exporter 35 passed; core 192 passed; application 4 passed; default/bootstrap process 2 passed; native runtime 21 passed; basic HTTP 2 and imported revision 1 passed; control 1 and retired-locator 7 passed. These are distinct test targets, not a total integration completion claim.
- Snapshot11: exporter 34 passed, 2 failed. The historical projection collection bug was fixed after the snapshot; the unavailable-source fixture now asserts the exact allowed provisional timeline mutation instead of incorrectly requiring no mutation. Public viewing 4 passed; a fifth typed-edit fixture used the wrong association and was corrected for snapshot12.
- Snapshot12: schema86, command-attempt journal, imported source acknowledgments, descriptive historical owners and tombstones are awaiting qualification. Audio discovery mapping is included; the separate exporter audio-family mapping/rehearsal was added afterward and is also pending. No physical grant is inferred from historical metadata or tombstones.

Earlier milestones (retained for provenance):

- Immutable original Motion baseline: 28 backend tests passed; one preexisting observation fixture failed and was fixed in reconstructed source. Python: 45 passed, 8 skipped.
- First reconstructed Motion snapshot: backend 43 passed, three failures diagnosed (missing source-origin fixture, guard-drift fixture, native source-name bound); fixes applied to local source. Root workspace all-target check passed.
- Immutable snapshot4p1: backend 78 passed, one physical authority rehearsal failed, one intentional child helper ignored. The real 1100-work activation/replay/restart, interrupted-prefix recovery, projection crash rollback, and source observation/cancellation tests passed. The physical grant fix is in later source, not yet qualified.
- Immutable snapshot5p1: root workspace all-target compilation passed. Runtime HTTP qualification exposed exact zero-effect upgrade receipts being incorrectly treated as unsupported; a narrow retained-receipt audit fix is in progress.
- Native extension tests and subsequent Motion snapshots are run separately against immutable copies. Consult current test evidence before describing a feature as qualified; code presence, compilation, and runtime behavior are different claims.

No legacy SQL catalog writer or request-supplied authority is an acceptable fallback. Disposable compatibility projections must be reconstructible from native identity/generation-bound records and must not themselves grant physical access.

## Bounded migration operator flow

The aggregate path is used for nonempty catalogs that exceed a native import's
bounded batch representation. With the Motion source first stopped and backed up,
use the operator binary with the same explicit `--catalog` and `--motion-db` paths:

1. `freeze-motion-source --import-id <root> --expected-revision <current>`
2. `capture-motion-export-plan --root-import-id <root>`
3. `prepare-motion-partition-cutover --root-import-id <root>`
4. `activate-motion-partition-cutover`

Preparation refuses unresolved mapping/reference obligations and more partitions
than the native restart contract supports. It does not silently truncate. Once
sealed, retry the last activation command to resume only recorded immutable
partition identities and command intents. Do not abandon or recapture a sealed
source. Serving requires every exact partition receipt and final host
acknowledgment; a successfully imported prefix is never a serving catalog.

## Projection authority boundary

The logical compatibility projection now has a concrete implementation in
`crates/catalog-backend/src/projection.rs` (logical/current physical paths tested; newer historical-only paths remain
under qualification). Every normal Motion SQLx connection installs a default-deny
`motion_native_projection` function. A private, isolated, never-pooled connection
may write only explicit work/edition/timeline/version/file compatibility tables
and their disposable `search_dirty` side effects. Physical projection is still
under qualification; this capability does not open physical HTTP routes. Authentication, history, source bindings, cutover and control tables
cannot obtain this bypass. There is no SQL-callable setter or durable enable flag.

The rebuild accepts a retained native host owner, not caller-supplied rows. It
reads generation-fenced native pages while the runtime retains exclusive host
custody, verifies Motion's native binding and activated seal, and commits rows
plus generation bookkeeping atomically. Rows are upserted by preserved IDs, never
blindly deleted/reinserted. Missing native IDs are not treated as deletion proof;
explicit retirement remains in the cache. Native routes remain authoritative and
must not use stale compatibility rows to grant physical access or permissions.

SQLite/FK/transaction mechanics live in the adapter. Exact identity, immutable
cutover acknowledgment and complete-set admission are core rules. Rebuild
regressions cover default denial, no SQL setter, capability disposal, ordinary
writer refusal, application FK/history preservation and rollback after a partial
projection failure. Process-kill and richer physical projection qualifications
remain separate pending work.

## Stabilization additions (not yet qualified)

- Atomic native import/discovery receipts now retain actual created item heads.
  Immutable Motion revision bridges preserve source CAS bases rather than reset
  revision9 to native1. Native create-only receipts anchor fresh base1.
- Physical projections use exact native current timeline coverage/equivalence,
  full observed ctime fingerprints, and per-row generation/readiness guards.
  Unknown/fractional or offset timelines remain unavailable rather than rounded.
- New source descriptor FK projection, path-collision history, complete physical
  reference qualification, durable authenticated HTTP scans, and native processing
  publication remain open work. No successful playback integration claim yet.

New native source FK descriptors use a separate, insert-only capability on the
same isolated projection connection. It matches the entire exact descriptor row
and is revoked immediately after insertion. Every normal connection defaults to
denial. Materialization requires the saved immutable registration/mapping plus
current native catalog/profile/binding/policy/root proof; it grants no library
membership, permission or scan authority. Existing descriptor updates/deletes
remain denied. Capability-mechanics tests are added; end-to-end physical source
qualification remains pending. Path collisions with retained historical files
still fail closed rather than inventing replacement locators.

Snapshot6p2 runtime evidence: 13 native server unit tests passed, including
cancellation/revocation custody and lock ordering. Bound-but-unprojected
application denial and actual packaged-process refusal/restart checks each passed.
The initial root FIFO filter selected zero tests and is not coverage; the exact
root FIFO regression is pending the next snapshot (backend source-root FIFO
coverage is separate). These do not substitute for successful HTTP serving; export
empty-contract/inventory fixture failures are being corrected before that claim.

Snapshot7 selected runtime suite: 24 genuine tests passed (native units14,
root FIFO1, basic HTTP2, imported revision bridge1, application HTTP2, packaged
refusal/restart1, core history3). Basic native create/read/browse/update/replay
and restart are now qualified. This is not complete physical serving coverage.

Post-snapshot7 startup changes (pending qualification): absent catalog_path
selects data_dir/catabolic.sqlite3, never legacy startup. Fresh installation uses
an immutable durable ownership intent and a separately durable native identity
before no-clobber publication from private staging. Interrupted publication is
reconciled before opening either runtime. Existing unbound data requires explicit
offline migration. Existing exact cutover intents are replayed rather than
re-created. Filesystem ordering remains an adapter responsibility; catalog
identity and activation acceptance use the production core admission rules.

Snapshot8 selected runtime evidence: default native packaged startup now passed
create/read/source registration and restart; custom-path configuration loss
refuses fallback. Bootstrap ownership/recovery tests passed. Physical fixture
activation exposed an upstream schema37-only cleanup gate even on supported
schema39; a narrow native-owner fix is being validated, not bypassed.

Schema84 (pending Rust qualification) makes retained physical locators explicit:
media_files.native_history preserves original IDs, paths and revisions, with one
current locator per source/path. Core retirement requires an exact current native
missing/replacement observation; missing inventory alone cannot retire a row.
The transactional rebuild asserts old schema/FKs, copies every row, restores
exact guards/indexes/triggers/views, and enforces full final row/FK checks before
commit. Unknown schemas and preexisting corruption refuse the migration. The
ordinary connection cannot mark native history after cutover. Physical HTTP
serving still depends on its separate end-to-end authorization/byte-open tests.

## Current stabilization boundary (2026-10-20, snapshot9)

The supported-schema39 retained observation fix passed both the no-witness
negative regression and a real packaged helper scan/activation/replay test.
The original cleanup and exact drain witness requirements remain in force.
Physical adapter fixtures now reach activation; byte-open, download and queued
scan runtime tests remain in progress. Public physical and scan gates have not
been opened on source presence alone.

Schema84's first Rust run passed five migration regressions and exposed a sealed
SQLx ledger insertion failure in the positive migration test. A narrow fix is
implemented but not yet qualified: an owned, never-pooled connection may bypass
only the migration ledger guard for the exact compiled 83-to-84 migration.
Original schema/guard inventory and pending migration identity are checked before
running, and the capability is revoked before post-migration verification. The
operator connection retains a verified pre-upgrade backup. Ordinary pooled
connections and projection connections receive no ledger permission. Error-path
regression checks that a rejected migration leaves schema, ledger and guards
unchanged. Future migrations are intentionally not implicitly authorized.

Remaining functional acceptance is tracked by operation, not by compilation:
- Native work create/read/browse/update/replay/restart and imported revision CAS:
  selected actual HTTP tests passed.
- Fresh default native startup, source registration and restart, configuration
  loss without fallback: actual packaged-process tests passed.
- Physical viewing/download/byte range admission and source replacement: current
  production adapter fixtures pending runtime results; public gates stay closed.
- Durable scan queue, cancellation and fresh media discovery: production worker
  fixtures pending runtime results; background/HTTP admission stays closed.
- Imported source descriptors and logical-library membership: immutable source
  proof adapters in progress; metadata alone does not grant access or scanning.
- Playback sessions, generated delivery bytes, offline sync and history continuity:
  preserve existing Motion owners; full native physical end-to-end qualification
  is incomplete.
- Native relationship, edition/timeline/order, tag/artwork/matching mutations and
  processing publication: remaining parity work, not implied by projection reads.
- Repository publication and live deployment/migration: not performed.

Post-snapshot10 work remains unqualified until its own runtime results:
- Dynamic typed source extents translate only exact integral-millisecond,
  contiguous native layouts. Unknown/gapped/fractional extents remain unavailable.
  Native mapped timeline revisions replace cache-trigger increments.
- Explicit historical owners preserve unverified source file/timeline/version and
  FK records without manufacturing occurrences or ready versions. Exact receipt
  item heads make that imported evidence immutable; changed history metadata
  refuses reconstruction. Original source descriptors must match all fields.
  The historical activation candidate retains unknown-reference/cross-edition
  blockers. A real missing-root activation/projection fixture is pending.
- Receipt-only startup recovery now covers committed work creation and discovery
  acknowledgment gaps. Missing receipts never cause startup dispatch. Actual
  lost-ack/cancellation tests remain the qualification boundary.
- A principal-independent aggregate work ETag contract is still needed for rich
  child-dependent responses. Native item-head CAS alone does not prove that a
  version/timeline/availability-dependent response is unchanged.
- Native processing begins with receipt-backed enqueue admission using existing
  artifact ownership. Encoder lifecycle and generated typed version publication
  are not yet integrated; legacy catalog publication remains disabled.

Snapshot10p2 runtime evidence: schema84 retired-locator tests7 and actual
operator upgrade/verified backup test1 passed; native units21, basic HTTP2,
imported revision HTTP1, application adapters4, packaged processes2 and core192
passed. The physical adapters include real range bytes, ctime replacement
refusal, retained viewing/history/download pins, restart and missing-file
expiration. This is not yet a public playback/content router qualification.
Scan queue cancellation2 passed, but normal publication2 failed with an opaque
worker error; diagnostic tracing is in snapshot11. Exporter baseline35 passed.

Schema85/86 are now staged, not yet qualified. They add immutable bounded command
attempt records and exact imported source descriptor provenance. The sealed
upgrader permits only exact compiled contiguous transitions83→84→85→86, including
partial-prefix restart; altered SQL and future migrations refuse admission.
New control tables do not become catalog or authorization grants. Saved older
audit compatibility exempts only those two explicitly added control inventory
entries, never arbitrary prefixed tables or unknown application references.

## Snapshot12/13 stabilization evidence (2026-10-20)

- Snapshot12 all-target compilation passed. Five application fixtures passed,
  including actual native timeline/version revision2 edits, exact Motion revision
  bridges, unchanged historical state, repeated-read stability, schema86 source
  descriptor fidelity across restart, and public viewing authorization/HEAD.
- Native scan queue: five of six fixtures passed. Real observation/probe/native
  publication, lost host acknowledgment recovery, and cancellation passed; one
  late public catalog read still returned `native_projection_unavailable`.
  Snapshot13 adds exact diagnostic location/stage, not a relaxed proof.
- Snapshot12 exporter failures produced one real correction: reserve the native
  tombstone metadata field against source metadata collision. Two fixture
  corrections preserve the intended foreign-key and historical receipt checks.
- Snapshot13 all-target compilation passed. Runtime qualification is pending for
  public original content/download gates, scoped library reads, independent
  frozen-proof source descriptor reconstruction, historical owners/tombstones,
  imported audio kind mapping, and the latest scan-to-read diagnostics.
- Aggregate work revisions have a deterministic core model and an inactive
  crate-private durable issuance adapter with rollback/identity tests. There is
  no active schema or serving integration yet. Complete, principal-independent
  native dependency capture is required before issuance can be enabled.
- Restricted external processing remains disabled. Linux process restrictions
  and real installed-tool compatibility fixtures are under qualification; no
  external-child cleanup witness, schema40 lifecycle, generated typed version,
  or cross-platform processing parity is claimed.

Remaining route families are explicitly tracked by
`native_catalog::unsupported_serving_routes()`. Passing the supported subset,
compilation, or a restart test does not establish complete integration. No live
library migration, deployment, or repository publication has been performed.
