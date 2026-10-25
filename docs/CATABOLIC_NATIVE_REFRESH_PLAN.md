# Existing NFO refresh: native ownership plan

Status: pure planning and filesystem evidence candidate only. No native41 schema
has been registered, no refresh gate has been opened, and no live migration ran.
Native/bound background refresh currently fails explicitly before legacy effects.

## State and decisions

`catabolic_core::catalog_metadata::nfo_refresh::plan_refresh` models the complete,
ordered observation batch against exact source, attribution, pin, item, schema,
file binding and catalog generation heads. Its serializable input is not a grant
or completeness proof. A native store owner must assemble and revalidate it.

The new owner must keep automatic NFO identity claims separate from accepted
manual Pins and timeline/version identity. Proposed versioned durable state:

- A current source attribution head with independent monotonic revision, source
  revision, optional exact profile/file origin and optional automatic identity.
- Immutable attribution history with command identity and import provenance.
- Indexed origin-to-all-prior-work lookup and unique nonnull automatic identity
  claims within the NFO identity namespace. Overflow refuses; no truncated scan
  can establish absence or uniqueness.

Parsed observations first honor the current work's manual pin. A mismatch is a
completed no-op before any old-owner withdrawal. Otherwise previous owners of
that exact origin are withdrawn, independently pin-guarded, before claiming the
new identity. Collision retains values/origin but does not steal the identity.
Absent observations withdraw only proven origins of that exact file. Unusable,
stale, incomplete or zero observations abort the whole refresh. Ordered files
operate on evolving heads; identical document/identity values do not advance the
source revision, though origin-only movement advances attribution revision.
Retired owners may release historical attribution/automatic claims without
rewriting retained metadata or guessing a split child.

## Required import and upgrade proof

Current exports archive nfo_origins as evidence but do not activate its semantics.
Typed import must establish every current NFO automatic identity, including API
contributions without sidecar origins, and link each imported origin through the
activated exact PhysicalMapping, never a path or arbitrary JSON marker.

Native40 replacement intentionally preserves public contribution identity
semantics but replaces its source evidence. A future migration must therefore
prove the original trusted import identity plus every intervening known
identity-preserving receipt/history transition, or use explicit earlier owner
carry-forward provenance. Latest evidence alone is insufficient. Old manifests
and v1 serialization remain unchanged. Registration and bound transition to41
are held until40 workflows and the explicit39-to40 transition qualify.

## Physical and application custody

Complete native work capture supplies all original eligible versions/bindings,
files, source policies and current occurrences. Exact projection mappings are
translation records only; retained Motion media_files is not current authority.
Reuse occurrence_bridge::project_with_host, sources::check_mapping_in and native
source occurrence owners. Generated provenance must be checked natively.

`nfo::native_observation` retains filesystem evidence only: expected source root
identity, cap-relative nonblocking regular-file opens, bounded parser input,
retained descriptor/full fingerprint and same-path or absence revalidation.
The caller must retain current native source/occurrence authorization, complete
membership curation and Motion attempt custody through validation and effects.

The compound dispatcher must invoke private metadata/tag child owners under one
outer command identity and one receipt, not nested command dispatch. Complete
work capture must include attribution/claim heads so origin-only changes also
invalidate aggregate revisions.

Before native execution, durably save the exact job attempt and command intent.
After a crash, reconcile that same native receipt rather than re-observe under
its operation ID. Motion completion acknowledges only under current attempt,
cancellation and authorization fences. Two database commits are not one atomic
commit and must never be described as such.

## Qualification still required

- Actual populated imports, automatic identity collisions and manual pins.
- Multiple files, origin-only/no-op revisions, split/retired withdrawals.
- Late-failure rollback of source, tags, attribution, identity and sole receipt.
- Lost ACK/restart, cancellation/revocation and stale attempt/source negatives.
- Full scoped authorization for target and every prior owner.
- Exact byte/node/closure bounds and complete aggregate dependency changes.
- Actual outer refresh route and background worker, with no legacy writes.

## Native41 planning after native33 owner qualification

Qualification passed for captured native33 metadata/artwork, logical,
relationship-lineage and empty-edition owner targets. Native41 design may proceed;
registration and any public refresh gate still require a coherent owner and new
qualification. Existing40 APIs, request serialization and policy fingerprints must
remain unchanged. Lossless Unicode tag identity is a separate contract evolution.

The proposed additive schema contains versioned attribution history/current heads,
exact profile/file origin indexes and globally unique automatic NFO claims. Current
metadata JSON is never a migration seed. Bootstrap must prove the original typed
import and every intervening identity-preserving native source-history transition,
then link imported origin evidence to exact activated physical mappings. All NFO
sources must be included, including API contributions without sidecar origins.

Full native receipt bodies can expire, but permanent tombstones retain canonical
payload digests and committed generations while Motion retains exact immutable
command envelopes. A narrowly typed historical-provenance verifier may therefore
prove a committed identity-preserving transition without accepting an expired
receipt for replay or returning its deleted result. This proof path is under audit;
existing receipt/import reconciliation behavior must not be relaxed. Unknown
operations, mismatched bytes, missing ancestry or unproven import linkage refuse.

A bootstrap barrier must prevent partial claims from being treated as a complete
uniqueness inventory. Before choosing one-command versus owner-selected keyset
batches, measure the existing supported import/catalog bounds and actual proof
sizes. If segmentation is required, native source writers need an explicit baseline
barrier; each batch needs exact receipt/cursor replay, cross-batch uniqueness and
restart recovery. Final visibility requires independent native inventory exhaustion,
not caller-supplied counts. Multiple batch commits must never be described as one
atomic transaction. Existing40-to41 offline transition evidence must remain distinct
from the qualified39-to40 witness and preserve frozen original import identity.

Automatic identity changes also affect the current protected
motion_catalog.external_ids projection. Atomic refresh must update precisely its
NFO entry while preserving other providers, structure and match state, or supply an
equivalently typed derived projection. Updating only source values/tags and the new
claim index would leave ordinary work/detail reads stale. Manual Pins remain a
separate reviewed owner; collisions and pin precedence must be checked under the
same retained native writer as all source, attribution and projection effects.

### Additive partial-observation model candidate

The new `plan_refresh_v2` keeps strict `plan_refresh` contract1 unchanged. Its
version2 terminal observations distinguish proven missing/unavailable/disabled or
changed files from missing authorization/ownership/lookup proof. Proven stale and
unusable observations skip without withdrawal; valid observations use the same
pin/collision/ordered publication rules as v1. Every row has an explicit outcome
and the plan reports observed/processed/skipped counts. Empty/all-skipped input
returns `NoObservationsProcessed`, never an implied successful job. All supplied
parsed documents remain bounded even when skipped. Seventeen new model tests are
source-ready and unrun; there is still no native refresh dispatcher or public gate.

The isolated native `catalog_nfo_attribution::verify_committed_command_in` prerequisite
also has three unrun tests. It requires an existing retained native40 writer,
exact current generation/incarnation and full receipt-only host authority, and
checks the complete canonical command against immutable commit facts. It returns
no deleted/full result bytes and preserves `ReceiptExpired` replay behavior. This
is only historical commit evidence: bootstrap must still prove each exact source
transition, schema lineage, imported snapshot and origin mapping independently.

### Post28 read-only provenance and public-presence prerequisites

The unregistered native groundwork now includes an additive public-presence model
with twenty unrun tests. It distinguishes identical refresh publication from
explicit PUT (which always advances the public document), and keeps native source
history monotonic while withdrawal omits the public source and reappearance starts
public revision 1. Internal captured aggregate/source/attribution/presence stamps
fence retry reuse; public revision alone does not acquire stronger legacy CAS.

A separate private physical-origin verifier reconstructs exact activated metadata
and physical parts, including a physical-only part, checks every frozen batch
descriptor and the unique original PhysicalMapping, and retains the exact native
file revision. It does not look up today's path or grant access to bytes. Its
nonserializable host grant must come from a complete archived nfo_origins audit
and exact saved Motion-file mapping within the root cutover. Activation receipts
alone do not prove that host-side linkage. The three new tests use real native
bind/scan and real typed part activation, but supply that independent host grant;
they do not qualify the as-yet-unimplemented Motion audit builder.

Origin-bearing metadata baseline composition requires the exact metadata
activation, root/audit identity and current retained proof generation. Complete
source-history proof carries both the full original PhysicalMapping and its proof
digest. Positive audited no-origin construction remains distinct; absent proof is
never interpreted as absent origin. Compacted receipts prove only reconstructible
historical effects, not replay results or inferred assigned IDs. These helpers
remain read-only and unrun, with no schema41 registration or refresh dispatch.

Historical external identity bytes are opaque under legacy metadata PUT, which
rejects only empty or over-256-byte values. The import/history proof now preserves
such bytes rather than trimming them. Strict refresh v1/v2 snapshot validation
still has a separate compatibility obligation before processing those retained
API identities; proof preservation alone does not complete refresh parity.

### Snapshot30/native36 prerequisite feedback and remaining baseline coverage

Qualification ran all55 pure NFO/presence tests successfully. Native36
private proof tests passed25 of28, including exact v1/v2 deep imports, duplicate
inner document rejection, cumulative proof admission, source-history preservation
and compacted import receipts. The three physical-origin cases stopped before
proof execution: their direct Store scan lacked a runtime retained-child cleanup
witness, so import activation correctly refused unresolved ownership. The live
fixture repair uses the actual qualified runtime helper, drain, cleanup and reopen;
its result must be recorded separately, not inferred from the other25 passes.
The official native migration/mutation runners now explicitly build/select and
verify the helper prerequisite for these non-ignored tests. No schema41 exists.

Complete bootstrap needs two baseline families: activated imported sources and
native-created first NFO sources. Current40 HTTP can create the latter with
metadata_contribution_replace expected_revision0. A future production verifier
must reconstruct that exact first command and immutable schema/source history;
test-only baseline construction in history tests is not authority. Its optional
origin_creation_intent proves original work-title provenance, not sidecar origin.
Positive complete no-origin auditing remains required independently. Native39
first Patch/initialize-and-patch history needs its own explicit compatible branch.

Public revision seeding must distinguish deployment histories. Native40 currently
serves source.revision as the public metadata revision, not the archived imported
Motion source_revision. A40-to41 transition must preserve that already-observable
CAS value. A fresh41 legacy import may preserve the original Motion revision.
The presence model permits both, but no builder may silently substitute one for
the other or infer source incarnation from a reused public revision alone.
