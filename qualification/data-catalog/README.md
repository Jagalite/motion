# Data/catalog completion lane

Base: `2344d03`; branch: `data-catalog-completion`.

## Scope and disposition

This lane implements forward legacy progress repair (C01), public scan demands,
reviewed source relocation, relationship CRUD, timeline/version creation,
item/admin file inventory, local metadata refresh, artwork upload/selection,
and authorized smart-collection evaluation. It allocates migrations 0034–0037;
no previously applied migration is edited. Original media is never written.

- Public now: the 17 additional operations in `contracts/served_operations.txt`.
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
- Processing/preparation/schedules, storage/backup APIs, caller migration, v1
  retirement and combined-tree/package qualification remain integration or other
  lane dependencies. This branch does not claim M1/M3 or complete product cutover.
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

## Evidence status

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

## Real-process upgrade and remaining CI gates

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
Latest remote rerun, heavy tests and packaging remain pending. PR remains a draft;
no merge or release is authorized by this receipt.

## Independent reviews

GPT-6.1 Sol reviewed the original commits and the continuation through `901a781`.
Findings corrected: unsafe empty-cut fallback, scan counter and attempt selection,
admin inventory shape, timeline revision fencing, restricted shared-job visibility,
metadata false completion/publication rollback/source enablement, hidden timeline
watched scope, smart representation revisions, candidate evaluation scope and
invalid test fixtures. The latest reviewed commit has no remaining concrete code
finding. Final documentation/diff review is complete with no remaining finding.
All declared local focused runtime checks have passed.
