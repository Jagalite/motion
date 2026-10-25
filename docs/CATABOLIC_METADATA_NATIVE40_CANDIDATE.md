# Native40 metadata contract and qualification history

This document records the contract and historical qualification below, not full
product parity. Current evidence is in [remaining product acceptance](CATABOLIC_REMAINING_PRODUCT_ACCEPTANCE.md).
Qualification uses immutable paired snapshots. No live
catalog upgrade or deployment was performed.

## Contract

Native39 retains the fixed-schema, unchanged-tag contribution cohort. Native40
adds explicit metadata contract2 and append-only schema registry/current-head
ownership. Historical source documents retain their original schema fingerprint,
raw field/evidence JSON and revision. An unrelated source policy addition does
not bump them. Reads revalidate compatible old documents in memory.

V1 remains 64 fields with its original fingerprints. V2 admits 100 external fields
per contribution and 100-byte names. The union is not capped at128: it is bounded
by the existing byte/node/snapshot/document/work budgets. Trusted motion_ fields
are a separate system policy set and cannot be replaced by public contributions.

The new dispatcher operation replaces one complete attributed source, advances
its exact source revision, extends a compatible schema when necessary, replaces
that source's tag assertions, updates only proven local suppression ownership,
records tag history, resolves projection and writes one durable receipt in one
native transaction. Failures roll back all owners. Registry/history are immutable;
there is no trigger bypass or legacy Motion metadata write.

Post23 candidate PUT requires current curation authority over every complete native
library membership, including memberships hidden from the caller. Scoped GET
retains exact reauthorization and projection admission.
Aggregate preconditions and final issuance remain under Motion writer/native
custody, with saved intent and native receipt reconciliation before fresh CAS.
Native40 tag changes use complete registered schema/tag/pin capture. Unsupported
file-tag domains remain refused. Ordinary item reads waive unknown public keys
only with independently verified schema and effective-projection equality.

## Qualification status

Immutable native29p3 metadata commands passed27/27, including
populated39→40 preservation/rollback, and the wide typed-import regression passing.
Core schema-evolution tests (21) and native40 restart passed earlier. These results
do not qualify later origin/scoped-route edits or imply full route parity.

## Source test coverage

- Actual outer router:100-field/100-byte boundary, new-field schema extension,
  tags/exclusions, removal, replay, ordinary item GET, zero legacy documents.
- Existing source CAS/replay/scoped denial/restart outer regression remains active.
- Real native dispatcher: multiple sources with schema union above200 policies,
  retained old source fingerprint/revision, capture, full100-to100 replacement,
  tag removal, old receipt replay, stale CAS, denied tag authority, name/field/byte
  overflow rollback across metadata/schema/tags/receipts.
- Populated native39→40 actual upgrade: original rows/history/projection/receipts
  and existing entity revisions preserved, FK integrity, new dependency capture,
  old receipt replay, old fingerprint history, rollback after migration40 table
  replacement injected failure.
- Pure core v2 boundary/node/byte/compatibility and explicit empty schema tests
  are present, not claimed run.
- Post22 owner tests add empty first initialization,100-field/100-byte baseline
  raw evidence preservation, system-grant denial, foreign suppression ownership
  rollback and real typed import with two100-field sources plus a protected owner
  field, retained v1 serialization and replay.

## Remaining integration

Post22 source adds explicit native40 v2 initialization and typed import, including
real empty schemas and100-field/100-byte-key baselines. This remains unqualified.
Old imported records omit default contract1/system-empty fields exactly; new40
exports declare version2 and independently granted system policy/source subsets.
Native40 migration rebuilds the subject table while preserving old rows, triggers,
original policy fingerprints and immutable raw baseline bytes. No v1 limit changes.


1. Explicit Motion-bound39→40 transition must preserve frozen import identity,
   source mappings and manifests. Native quiescence/plan/rehearsal APIs exist;
   automatic owner-open upgrade is not authorized or implemented.
2. Post23 adds atomic unchanged fresh-create origin reconciliation, requiring the
   exact full create receipt, native item revision1 and identical original metadata.
   Only proven original title moves from legacy to scan; history is retained.
   Discovery and previously changed baseline provenance remain separate work.
3. Post23 scoped curator writes now use complete audited memberships before
   intent, receipt disclosure and effects. Actual two-library outer tests cover
   partial denial, complete success/replay, and denial after a receipt exists;
   runtime qualification remains pending.
4. Native Unicode tag normalization differs from Motion. Lossy identities are
   refused before intent; full parity needs versioned normalization ownership.
5. Refresh retains Motion jobs/NFO observation decisions, but publication must
   compose native source/tag owner receipt with Motion acknowledgment and replay.
6. Matching retains existing core proposals/decisions, with native scoped candidate
   reads and Pins/LogicalEdit composition still needed.
7. Artwork retains decode/hash validation; native association, selection and byte
   authorization paths still need integration. No new product endpoints are added.

## Post25 artwork candidate (source only)

Native31 metadata_commands passed30/30, including exact
fresh-origin receipt/host/current-baseline negatives and atomic provider replacement
replay/history. The pure NFO planner's18 tests passed. These results do not qualify
the following newer artwork source or Motion HTTP workflows.

- Pure reference validation, role resolution, selection and association planning
  now live once in catabolic-core::artwork and are reexported by playscale-core.
  The original12 reference-model tests moved with the implementation; existing
  Motion resolver API and contract1 serialization/limits remain unchanged.
- Current artwork reads require one exact motion.artwork/source_projection source,
  effective projection equality and typed reference closure. Catalog poster fields
  are admitted only after this retained native proof, not from a marker alone.
- Selection uses aggregate CAS, exact metadata-source CAS, immutable intent,
  receipt-first replay, current complete-library curation and a native receipt
  followed by Motion acknowledgment. No retained artwork selection writes occur.
- The generic trusted system replacement owner has duplicate-safe bounded parsing,
  precharged retained document work and explicit v1 system classification migration
  that preserves old policy/history. It cannot reclassify v2 public fields.
- Typed metadata_artwork_attach merges the current reference state inside the sole
  native receipt transaction. Its nonserializable exact descriptor grant is separate
  from source authority. Concurrent attachment retries never replace a stale state.
- Motion uploads stage decoded/hash-verified immutable asset bytes, then pass only
  the exact descriptor grant. Native association and Motion idempotency ACK recover
  through saved intent/receipt; orphan staged bytes never grant association.
- Content uses bounded projection hints only for discovery, then current scoped
  native ownership and exact retained generation/source references. Actual bytes,
  digest, MIME, dimensions and decoding are verified while both owners are retained.
  Existing range/ETag rendering is shared. No legacy association is byte authority.

Finite list/read-selection/write-selection, upload and GET/HEAD content candidate
gates are now staged after static review and lead/server coordination. Positive outer fixtures for both slices are active; see the snapshot27p6 qualification
below. Native attach has eight owner regressions, and system replacement has
dedicated v1 migration/duplicate-key tests, included in the native33 suite.

Current non-null native-owned pins are explicit association evidence, including
historical pin-only references. New outer tests distinguish these from cleared,
stale or orphan references. Unmanaged discovery subjects with unproven protected
fields still refuse rather than reclassifying arbitrary metadata. Expired HTTP
upload keys use prior durable ACK expiry plus restore epoch as command identity;
clock passage alone does not replace a pending intent. Lost ACK is receipt-first. Refresh automatic identities,
matching and versioned lossless tag identity remain separate integration work.

## Historical qualification, snapshot25p3

All six actual metadata HTTP tests passed in 52.64 seconds,
including the truthful background-refusal fixture. Enqueue after native binding is
correctly source-frozen; that fixture checks refused enqueue and typed refresh/NFO
refusal with no fabricated job or legacy metadata effects, under both an admitted
runtime and a durable binding without one. It does not claim preservation of an
otherwise inadmissible queued job. The actual explicit
39→40 offline CLI transition also passed. These results supersede the earlier pending
labels only for those captured workflows, not for discovery-origin parity or all
possible imported-library cohorts.

## Actual artwork qualification, snapshot27p6 / native33

All ten actual metadata/artwork HTTP tests passed in 84.44
seconds. The native33 baseline had already passed all 44 metadata/artwork owner
tests and the twelve shared artwork core tests. The HTTP coverage includes native
current-source list/selection, aggregate CAS, scoped denial, restart, atomic upload
attachment, active and expired-key replay, lost-ACK recovery, content ranges/HEAD,
current historical pins, cleared/stale pins, orphan rejection and tampered bytes.

Two integration defects were fixed without widening authority: retained artwork
cutover obligations now require exact typed asset/reference and reviewed receipt
closure, and trusted work masks include the exact protected artwork field so its
independent current-source proof executes. Masked fields are compared with the
complete current native snapshot; unknown structural fields remain refused.

The finite NFO committed/import/history proof helpers, partial-observation model,
and pure matching workflow model were captured in snapshot28/native34 but remain
unrun as of this report. They do not enable refresh or matching routes. Native41
attribution/bootstrap and Motion91 workflow registration are not installed. Actual
origin-bearing import proof, public source presence/revision bridging, complete
bootstrap inventory, lossless tag identity, refresh and matching acceptance remain
separate integration obligations; artwork qualification is not full parity.
