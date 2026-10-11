# Data/catalog completion lane

Base: `2344d03`; branch: `data-catalog-completion`.

## Scope and disposition

This lane implements forward legacy progress repair (C01), public scan demands,
reviewed source relocation, relationship CRUD, timeline/version creation,
item/admin file inventory, local metadata refresh, artwork upload/selection,
authorized smart-collection evaluation, preparation/capabilities/recipes/renditions,
logical schedules, storage accounting and durable backup requests. It allocates
migrations 0034–0041;
no previously applied migration is edited. Original media is never written.

- Public now: the original 17 plus 12 newly owned contract operations in `contracts/served_operations.txt`.
  Timeline placement uses the production order policy; the regression connects
  the public timeline writer to the public next-timeline resolver.
- Existing organization APIs remain public. Smart collections evaluate canonical
  timeline watched state, filter hidden bindings, and invalidate revisions when
  membership dependencies change. Evaluation admits at most 10,000 visible works
  and returns at most 1,000 members; exceeding either bound returns a conflict.
- Later public surface: standalone order-group management and existing-timeline
  reordering, file reassignment/replacement confirmation, component discovery
  and marker authoring, and M6 interchange export/import. Existing internal services remain; these operations
  are not advertised as served or required for this lane's declared adapter set.
  Group provisioning in the ordering regression uses the existing service, not
  a public group-creation endpoint. Full authoring/cutover acceptance remains open.
- Restricted scan clients control their demand; shared physical job IDs are
  deliberately omitted because those clients do not own the shared job.
- Metadata refresh currently means local NFO refresh, not remote-provider fetch.
- This lane explicitly owns preparation/processing, recipes/renditions, schedule
  CRUD and storage/backups. Exact interfaces are in
  [the client handoff](../../DATA_CATALOG_CLIENT_HANDOFF.md). Initial recipes use
  existing software processing; selected audio is honored and unsupported
  components/subtitles are refused. H.264 requires a probed SDR first video.
- Schedule scan and local metadata refresh dispatch existing services; preview
  and optimize remain explicit `schedule_task_unavailable` gaps because no
  existing scheduled services implement them. Metadata fanout persists a cursor
  and queues at most 200 items per schedule per tick. Revoked owners disable
  future dispatch; already admitted jobs retain their own authorization checks.
- Generated-media backup inclusion copies and verifies exact revisions. Restore
  tooling restores the SQLite snapshot; automated generated-cache relocation is
  not yet provided. Snapshot retention preserves public job/request history.
- Caller migration, v1 retirement and combined-tree/package qualification remain
  integration gates. Serving these routes is not full M1/M3 milestone acceptance.
- Client Next-button acceptance remains a client-lane gate; HTTP resolver coverage
  here is not browser evidence.

## Correctness boundaries

Core owns exact legacy attribution/import/retraction, canonical authority
preservation, relocation validity, version binding/order decisions, job attempt
and cancellation transitions, metadata refresh completeness, artwork candidate
selection and smart predicates. SQLite transactions supply current scope,
revision, source and binding facts, persist effects and replay acknowledgements
atomically, and fence publication. Filesystem observations and bounded image
validation remain adapter work because they require I/O; their observations feed
production policies. Smart candidate visibility is a scoped SQL read, applied
before the evaluation bound; it introduces no parallel domain state machine.

The legacy-import Stateless model calls production import/retraction policies.
It covers exact/orphaned attribution with/without old backfill, duplicate repair,
rollback, manual reset and session authority for one timeline and one canonical
action. Bounds: 100 states, 1,000 transitions, depth 20 per configuration. SQL
atomicity is covered separately by upgrade/failure/reopen adapter tests. This
model does not establish whole-catalog concurrency or metadata-worker conformance.

## Expanded adapter qualification

Production changes through `e8c5628` and final diff through `77ebbb8` were
independently reviewed by GPT-6.1 Sol with no remaining concrete blocker.
`26fe6aa` corrects test-only idempotency keys and `77ebbb8` corrects two
validation-status expectations (400→422); production source is unchanged.
Review findings fixed publication fencing, HDR admission, schedule starvation,
provenance pruning and backup recovery/retention. Review is separate from runtime
qualification.

Core now owns preparation publication admission from current authorization,
source enablement/scope, exact whole-file timeline binding and recipe identity,
plus rendition ready/stale classification and backup receipt publication policy.
The existing job reducer continues to own attempts, cancellation and retries.
Adapters perform FFmpeg/filesystem observations, copy and hash bytes, fsync and
rename snapshots, and persist the acknowledgement only after a verified receipt.
Schedule claims and demand creation share the reserved transaction; bounded
metadata cursors make interrupted dispatch resumable.

Compilation attempts `e219f483c1a146ff9490a98e30f78084` and
`161dc8a6395f47feb306f7318994a30e` found a Problem-to-anyhow conversion and a borrow
error, respectively; both were fixed. `bb030d53c1814ffe844ae185395fc52c` was
stopped by Kyoto for sustained memory pressure and cancelled before resubmission
with one Cargo build job. An earlier `7410c0109cc04c84ad3936e8e7c422c5` invocation
was cancelled because it overlapped snapshot checkout and is excluded entirely.
No test pass is inferred from these attempts. A subsequent one-job compilation,
Kyoto `9372c40a9a08436491edbe45d9f3c5ec`, succeeded at `e8c5628`.
Persistence (13), catalog (15), scoped organization (7), contract (4), publication
transaction (2), preparation core (2), storage (2), and Stateless model (2) tests
pass. Kyoto `58b6893e45ad4eed9d97f331191a5339` compiled the key corrections at
`ee842e4`; that HTTP run passed 15/17 and exposed the two incorrect status
expectations. Kyoto `4f4f5cf3a7404fbd812ee036b5ed6568` compiled `77ebbb8`; all 17 HTTP
tests now pass. The final focused total is **64 passing tests**, zero failures or
skips; [completion receipt](completion-api-receipt.json) records binary hashes
and source boundaries.

The preparation model exhausts 894 states and 10,856 transitions with zero
skipped checks (bounds: 10,000 states, 100,000 transitions, depth 30, two attempts).
It exercises the production publication policy under cancellation, retry,
recovery, stale completion and changes to authority, source, binding and recipe.
Adapter tests separately check transaction enforcement.

[Preparation/backup receipt](preparation-backup-receipt.json) records five passing
real HTTP/FFmpeg/FFprobe checks: exact replay, requested timeline, selected second
audio stream, revision-pinned output and generated-media backup bytes/inventory.
[Schema-41 receipt](upgrade-schema41-receipt.json) verifies schema 9→41 upgrade,
canonical repair, stable restart receipts, old-binary refusal, rollback through
both backups and restored-instance epoch rotation. Both runs use the exact
`e8c5628` binary. No browser or installed-package claim is made.

## Current CI and integration gates

At `ee842e4`, pinned Rust 1.98 strict Clippy, formatting, contract and notices pass.
Fast CI there passes the catalog/persistence/organization targets, all 43 server
unit and 96 core unit tests, but fails the two corrected preparation assertions
and the two unchanged Linux `motion-ui-host` cache tests described below.
At `77ebbb8`, all 17 HTTP tests pass remotely; the fast job still fails only the
two unchanged cache tests. Real-process integration/upgrade/restore CI passes at `77ebbb8`
(run `38113194982`); heavy tests and packaging are still running;
local receipts do not turn these into remote passes. The clients lane already
owns the Linux pinned-directory fsync fix; integration must compose and validate
it. PR #17 remains draft and is not ready for an unconditional merge.

## Earlier catalog evidence (before expanded adapters)

Final focused compilation and all 50 tests pass at `3f06331`; binary hashes and
results are in [receipt.json](receipt.json). Contract schema validation
passes: 97 paths, 146 operations, 148 schemas, 2,566 references. Publication checks
and strict remote Clippy pass. No whole-workspace test, installed package, browser
or release qualification is claimed here.

Kyoto owner: `68ca1c87-a6cb-4f03-b41c-410f701dea10`. Builds use an isolated detached
snapshot and target directory; the shared queue policy is unchanged. Local
compilation uses Homebrew Rust 1.99.0; pinned Rust 1.98.0 is checked by CI.

| Source | Kyoto job | Result |
|---|---|---|
| `c610398` | `6f882884ab6a4c489021d339dbfc902e` | Failed: missing Stateless Input Eq/check_state; fixed in `7956bbe`. |
| `7956bbe` | `c2dd2d6e4cc5409c9b39031a4ef34dfa` | Failed: relocation vector/map mismatch; fixed in `384e747`. |
| `384e747` | `2602b0f3e87648c5829ffa063bfafc42` | Compiled. Upgrade 13, contract 4, metadata 10 and legacy model 1 passed. Catalog inventory failed on SQL bindings; smart fixture failed on invalid media kind. |
| `3834d58` | `426f434c7c764614bb9aef7ae47d1ac9` | Compiled; catalog 15 and organization 7 passed after those fixes, including public placement-to-next and scoped inventory. |
| `3834d58` | `5a94856418ad46228ba159acb98f44d6` | Compiled; timeline-viewing core 8 and metadata refresh core 1 passed. |
| `3f06331` | `d9116b6097824c52b88f9ebe2766aed3` | Compiled; all 50 tests across the six affected targets passed, no skips. |

The four legacy model configurations exhaust their graphs: respectively 3/8,
4/12, 4/12 and 3/8 states/transitions for (orphaned/no backfill),
(orphaned/backfill), (exact/no backfill), (exact/backfill); zero skipped checks.
Properties check exact-only publication, preservation of canonical actions,
rollback identity and retention of canonical revisions.

An accidentally launched direct mutation helper was stopped during its baseline
and excluded from evidence. No mutation result or queue compliance is claimed
for that invocation.

## Earlier real-process upgrade and CI evidence

The corrected harness (`901a781`) passes with the candidate binary from `3f06331`
and the existing schema-9 baseline artifact: in-place upgrade to schema 37,
exact canonical progress and repair receipt preservation, stable restart receipts,
old-binary refusal, rollback via both backups, candidate backup/restore and epoch
rotation. [Upgrade receipt](upgrade-restore-receipt.json) records both binary
hashes. This run reuses the baseline artifact; it does not requalify its build.
The harness still observes legacy endpoints for cross-version state comparison;
it is not evidence for client v2 cutover or v1 retirement.

CI on `3f06331` passed contract checks, notices and strict Clippy. All catalog
lane tests passed under pinned Rust 1.98.0. The overall fast job failed only in
two unchanged `motion-ui-host` cache tests (`ordered_events_are_durable_exact_and_never_rebased`
and `failed_publication_cannot_acknowledge_or_consume_a_sequence`): HTTP 409 where
200 is expected on Linux. The host and offline core are unchanged from base
`2344d03`. Independent review traced the pinned Linux directory handle to O_PATH; fsync
requires the readable pinned handle. Existing fixes are clients commit `f31d031`
and consolidation commit `28f6332`. Compose the appropriate client fix during
integration; this branch does not duplicate that lane's change or count the
overall fast job as passed.

The real-process CI job initially failed because its receipt inventory omitted
the new forward-repair receipt. `901a781` fixes that assertion and additionally
verifies canonical timeline progress; the local real-process rerun passes.
At that checkpoint remote rerun, heavy tests and packaging were pending. PR remains a draft;
no merge or release is authorized by this receipt.

## Earlier independent reviews

GPT-6.1 Sol reviewed the original commits and the continuation through `901a781`.
Findings corrected: unsafe empty-cut fallback, scan counter and attempt selection,
admin inventory shape, timeline revision fencing, restricted shared-job visibility,
metadata false completion/publication rollback/source enablement, hidden timeline
watched scope, smart representation revisions, candidate evaluation scope and
invalid test fixtures. The latest reviewed commit has no remaining concrete code
finding. Final documentation/diff review is complete with no remaining finding.
All declared local focused runtime checks have passed.
