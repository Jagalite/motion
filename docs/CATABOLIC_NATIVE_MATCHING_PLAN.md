# Native matching integration plan

Status: design only after snapshot26. Existing matching HTTP routes remain closed.
Direct legacy matching services now refuse native-enabled or durably bound catalogs
before legacy effects; get/inbox also refuse retained-data fallback. This safety
boundary is not matching parity. No native41 migration is registered here.

## Existing product behavior to preserve

The v2 create-match request currently supports local matching only. A non-null
provider is already rejected. Proposals capture an original file revision and
bounded local title-match candidates. Reads redact candidates whose works are no
longer readable and refresh the visible subject description. Reject/defer change
only the reviewed proposal. Accept can mark the current work manual, attach an
external identity, or merge a single-version source work into a selected target;
any source-work version count other than exactly one requires an explicit split
first. The count must be complete and principal-independent. Reopen preserves the
previous decision as history, and manual pins survive until changed/unmatched.

## Ownership boundary

The retained match_proposals table is source-frozen after native activation. It is
not an application-table exception. Do not unfreeze it or use it as current native
catalog authority. Its imported rows, candidate JSON and decisions remain historical
source evidence. New workflow state needs an explicit Motion companion head/history
owner with exact linkage to any imported proposal baseline and frozen import identity.
That owner is application workflow state, not a substitute catalog or native grant.

The companion must retain proposal ID/revision/file revision, complete candidates,
status and immutable decisions. An imported baseline may be materialized only after
its exact original row/evidence is verified. New or revised heads must never rewrite
the frozen source row. Explicit bounds and reference-audit coverage are required.

## Current evidence and authorization

Before any proposal mutation, prepare native scope/projection outside the Motion
writer. Under the writer, reauthorize the principal and validate exact native
identity/generation. Resolve the original file through the current occurrence and
version/work binding, not media_files or an old proposal's file ID alone. Prove
current native source mapping/policy, original rather than generated provenance,
current content revision, live work and all containing libraries. Proposal-only
create/reject/defer require every library containing the file, not every library
of the entire work: using work-level curation here would incorrectly strengthen
the current product rule. Acceptance separately requires complete source/target
work curation.

content::file_facts is a reusable current projection consumer, not independent
mutation authority. It requires a validated projection under the Motion writer;
its new typed membership proof is produced by the native projection owner. Terminal
publication must also retain the native owner and revalidate the exact occurrence,
version/context/work and complete source/membership dependency closure.

Local candidate discovery uses complete bounded current native works plus the
existing pure normalize-title/parse-name/proposal rules. Overflow refuses; it must
not silently truncate the search before deciding completeness. Every read and saved
response replay independently checks current file/work visibility and redacts local
candidate content that is no longer readable. A candidate's saved title is evidence,
not an authorization grant.

## Safe workflow-only slice

List/get/create/reject/defer can use the new companion workflow owner with no native
catalog mutation. Hold Motion writer plus native owner through current evidence
validation and workflow/HTTP acknowledgment commit. Use existing pure matching
revision/status decisions. Do not fabricate a native receipt for a workflow-only
operation. Reopen needs the same current file proof and immutable decision history.

## Acceptance is a compound operation

Before dispatch, persist a proposal-revision-bound pending decision intent and exact
business payload. Enforce at most one pending decision for that proposal revision.
A native commit with a lost Motion ACK leaves the proposal apparently undecided;
therefore competing accept/reject/defer must not bypass the pending intent. Resolve
its exact native receipt first, then atomically update the workflow head/history and
HTTP ACK. Wall-clock passage or changed catalog facts must never reinterpret the
same pending operation. Current scope still gates receipt disclosure/recovery.

One native receipt must cover all applicable logical/reference replacement,
metadata/source provenance, protected match-state and explicit identity-pin effects.
The adapter supplies exact complete grants for source, target and every affected
context; all containing libraries are required. A stale native generation may use
only the existing authenticated attempt-chain protocol, not a new semantic plan
under an old idempotency identity.

Native LogicalEdit is not legacy merge parity by itself. It deliberately retains
metadata/pins on original IDs and refuses active relationships on retiring items.
It never infers timeline equivalence or rewrites saved pins. The existing Motion
merge semantics require a reviewed explicit composition that preserves source
history, overlapping contributions, relationships, references and current bindings.

Native Pins enforces uniqueness among accepted pins only. It explicitly does not
turn legacy/provider tuples into accepted decisions. External-identity acceptance
must also respect automatic identity attribution/uniqueness; that owner is part of
the held native41 refresh design. Local-only acceptance also marks the target manual,
which pins its existing provider identities even when the candidate has no external
ID. It therefore does not avoid this ownership requirement. PinBatch explicitly
requires a prior command to release a value held by another item, even when the
same proposed batch removes that holder. Atomic merge/pin transfer needs an explicit
reviewed transfer/compound policy, not sequential calls. Do not assert full identity
parity from Pins alone.

## Required evidence before any gate

- Actual native import/discovery original file proof and current content revisions.
- Complete scoped file and source/target membership, hidden candidate redaction.
- Historical proposal import binding, no source-table writes, exact CAS/history.
- Missing/stale mapping, generated files, retired works and resource overflow.
- Pending accept vs competing decision, native receipt loss/ACK loss/restart.
- Full merge metadata/pin/reference preservation and identity collision rollback.
- Existing outer-router behavior with no native test-only bypass or legacy fallback.

## Post26 source checkpoint

The legacy safety boundary is implemented in matching.rs and covered by the new
native_matching_service_refuses_legacy_effects_with_or_without_runtime fixture
(source only, unrun). It checks five public mutators plus get/inbox under both an
admitted runtime and a durable binding without a runtime. No schema/route changes
accompany that guard. The disjoint pure workflow model in
`crates/core/src/native_matching.rs` is now source-ready with 27 unrun regressions.
It cannot mint native authority, clear pending decisions, or perform acceptance.
`plan_create` consumes complete bounded candidate discovery and current original-file
evidence; `plan_decide` instead consumes file-only evidence, so rejecting or deferring
a reviewed proposal never requires rediscovering candidates. Current file-library
curation and exact file/wire/native identity remain mandatory, but unrelated work
metadata revision advances do not prevent rejection. Generation is an execution
fence, not part of the semantic review digest. The model preserves immutable review
origin, refuses same-wire native-file rebinding and pending-decision replacement,
and validates persisted workflow shape independently of its self-reported digest.
No workflow storage, observation adapter, migration or HTTP gate is added here.
The next qualification target is `cargo test -p playscale-core native_matching::`;
run against an immutable captured source pair with the documented runner.
