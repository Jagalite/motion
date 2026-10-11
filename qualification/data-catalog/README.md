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

Compiled validation is in progress. Contract schema validation passes: 97 paths,
146 operations, 148 schemas, 2,566 references. No whole-workspace, installed
package, browser or release qualification is claimed.

Kyoto owner: `68ca1c87-a6cb-4f03-b41c-410f701dea10`. Builds use an isolated detached
snapshot and target directory; the shared queue policy is unchanged.

- `6f882884ab6a4c489021d339dbfc902e`: failed at `c610398` because the new
  Stateless model omitted Input Eq and check_state; fixed in `7956bbe`.
- `c2dd2d6e4cc5409c9b39031a4ef34dfa`: failed at `7956bbe` because relocation
  observations used a vector where core expects a keyed map; fixed in `384e747`.
- `2602b0f3e87648c5829ffa063bfafc42`: focused compile at `384e747`, pending.
- An accidentally launched direct mutation helper was stopped during its baseline;
  excluded from evidence. No mutation result or queue compliance is claimed for it.

## Independent reviews

GPT-6.1 Sol reviewed the original commits and the continuation through `1a6c3f7`.
Findings corrected: unsafe empty-cut fallback, scan counter and attempt selection,
admin inventory shape, timeline revision fencing, restricted shared-job visibility,
metadata false completion/publication rollback/source enablement, hidden timeline
watched scope, smart representation revisions, candidate evaluation scope and
invalid test fixtures. The latest reviewed commit has no remaining concrete code
finding. The final documentation and compiled validation remain pending.
