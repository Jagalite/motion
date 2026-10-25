# Native source relocation: proposed owner contract

Status: design only. No relocation route, dispatcher, schema, or worker activation is authorized by this document. Existing native relocation gates remain closed.

## Existing product boundary

`src/v2/libraries.rs` exposes legacy preview and commit handlers. Preview requires current `SourcesManage`, an idempotency key and `If-Match`; it stores a principal-bound plan expiring after five minutes. Exact preview replay does not require the candidate to remain mounted. Commit uses a separate idempotency key, the saved plan's source revision, current authorization, and terminal expiry validation.

`src/libraries.rs::{preview_relocation,observe_relocation,commit_relocation_in}` hashes required candidate files against retained content revisions, then checks the complete catalog tuple set, source/binding revisions, candidate root identity, file fingerprints and root overlap. It moves no bytes. Success increments source and binding revisions, preserves file IDs/content revisions, enables the source, makes verified files available and excludes out-of-scope files. For an enabled legacy source, only currently available in-scope files are required; for a disabled source, all in-scope retained files are required. `administration::candidate_root` rejects overlap with server data; collision checks include other registered and managed roots. The v1 relocation path explicitly rejects active scans and processing. These legacy table writes are not valid native authority.

## Identity and reusable owners

A native file registry ID uses the catalog/source/relative-path locator. That stable locator is not evidence that two physical files contain the same media. Relocation must independently prove full content equality and retain exact candidate descriptors/fingerprints. Native `components::occurrence` includes root and file identity; changing a binding changes the physical occurrence revision. `components::revision` excludes ctime, but ctime is still an independently checked physical mutation fence. A content digest never substitutes for that fence.

`HostSession::register_source` delegates to `register_host_source`, which rejects a changed existing binding. Lower-level `observation::bind` can replace a binding and delete observations. Neither is the relocation owner. In particular, a bind-then-scan sequence would expose an unacknowledged intermediate binding and cannot preserve exact typed identity atomically.

`catalog_identity_mutation` already validates expected context, version, timeline and binding heads and appends exact-file revisions. Reuse that validation/revision machinery inside a new narrowly typed command. Do not reuse a broad editorial grant or infer equivalence from a locator.

## Proposed bounded first cohort

Only ordinary registered sources with complete current immutable mapping and active lifecycle qualify initially. Private generated roots and tombstones are refused. Reactivating a withdrawn source is a separate unresolved product decision: legacy disabled-source relocation reactivates, whereas explicit native withdrawal is an administrative denial. Do not silently map one to the other.

Preview is descriptive evidence, never a serializable authority token. It captures source revision, binding revision, lifecycle epoch, exact mapping digest, native catalog/incarnation/profile, binding/policy, complete retained file set, and the native head closure below. Candidate root/files are opened capability-relative, regular-only, with pre/post full fingerprints and independently computed full content digests. Bounds must be explicit; exceeding them refuses the complete operation rather than truncating it. Candidate extras are not discovered or adopted by relocation.

Commit requires fresh actor/credential and source authority under the Motion writer. Recheck the complete closure, lifecycle, source/binding CAS, expiry, overlap (including private generated roots), and all affected owners. Pending/running/cancelling scans, unresolved discovery effects, rendering and reserved publications, native leases/external cleanup or other uncertain owners block commit. Conservative global blockers may initially remain; unknown ancestry never permits relaxation. Detached validation must retain its actual writer/IO/descriptor custody until completion or cancellation cleanup.

## Complete affected-version closure

The native owner, not Motion projection rows, enumerates the complete source file registry and current physical facts. Starting from every file whose binding will change, collect every live typed version binding that references that exact file, then its owning version, timeline, edition/association and segmentation context. Include shared references and every binding part of each affected version, not merely the selected work or first original version. Compare all context/version/timeline/binding heads and exact file revisions before effect; query overflow or unsupported graph shape refuses.

Preserve all item, version, timeline, binding and file IDs. Advance only the native revisions necessary to repin verified physical occurrences and their owning version/context contracts; do not manufacture an editorial timeline change or equivalence claim. Unaffected bindings retain their exact pins. The command must validate the complete resulting version contract using existing identity rules. If those rules require unsupported multi-context succession or new equivalence semantics, the first cohort refuses that closure. Exact legal head increments and equivalence-evidence renewal remain a native-owner design dependency, not a promise of this proposal.

## Unavailable, nonrequired, excluded and historical facts

- Verified required files receive new current physical occurrences only after exact byte equality and descriptor revalidation.
- Previously unavailable, nonrequired files remain unavailable. A matching candidate pathname alone cannot revive them. Preserve retained IDs, old exact-file snapshots, typed history and unavailable reason; a later genuine scan/discovery owner may establish new facts.
- Excluded files remain unavailable at the relocated root. Do not hash/adopt them merely because a candidate contains them, and do not delete their history.
- All live typed references to unavailable/excluded files still belong to the affected closure. They must remain explicitly unavailable rather than be falsely repinned to a present candidate. Whether the native identity owner can retain those historical pins while atomically changing the source binding must be proven. Until then, refuse such mixed closures rather than silently drop versions or broaden the first cohort.
- Retired versions and historical exact snapshots are immutable. Preserve them unchanged, with no assertion that their old physical occurrence is currently readable. Playback/history IDs and user viewing history are not reassigned.
- Independently receipt-verified generated outputs retain their own physical binding. Relocation does not rewrite them. Their inherited source-policy proofs must become stale against the new mapping/lifecycle revision and be regenerated only through the existing exact ancestry owner.

## Durable transaction and recovery boundary

Persist a bounded immutable Motion intent with request/principal/CAS/plan digest and the exact original native envelope before dispatch. A new native command must atomically validate and succeed the source binding, install only independently verified occurrence facts, append the supported typed head closure, and write its receipt. There is no separate raw bind, observation scan, or generated cleanup seal.

The result binds old/new mapping, source binding/policy, complete file disposition and exact resulting heads. Motion acknowledges that exact receipt in one writer transaction, appending source mapping/lifecycle succession and invalidating stale projection/scope/watch/schedule proofs even if an independent cache generation appears current. Existing immutable imported/native-created descriptor records remain historical baseline evidence; they are not overwritten. The new current mapping succession owner and its admission protocol are required durable state, not schema already implemented.

Between native commit and Motion ACK, changed-binding physical admission fails closed. Restart reconciles the original/latest explicitly witnessed attempt receipt first and reconstructs only its exact committed result. Receipt absence does not authorize rerender, new IDs, replacement intent or fresh dispatch. Any permitted retry must independently reauthorize and revalidate retained candidate evidence under the same owner custody. Unknown effects remain reserved and visible.

## Acceptance matrix

| Case | Required outcome |
| --- | --- |
| Genuine moved root; independent same-byte copied root | Stable IDs/content identity; exact new physical pins and receipt; old history retained |
| Same path, changed bytes; same bytes with post-preview inode/ctime change | Refuse stale proof, no partial binding/head update |
| Candidate root swap, missing required file, collision, server/private managed overlap | Refuse without effects |
| Source/catalog/head/actor/lifecycle changes after preview | Fresh terminal refusal; no stale plan grant |
| Available plus unavailable/excluded/shared-version closure | Exact complete disposition or explicit unsupported refusal; never truncate |
| Active scan/render, pending discovery, reserved publication, unresolved native cleanup | Busy/refusal; no owner abandonment |
| Caller drops during hash or commit | Actual custody remains until completion/cleanup; no false clean seal |
| Native commit before Motion ACK; restart and exact replay | Receipt-only reconstruction, one succession, unchanged IDs/history |
| Old byte/scope/watch/schedule proof after ACK | Refuse/reprepare against new current mapping and lifecycle |
| Bound overflow or unsupported version graph | Explicit complete-operation refusal |

Required implementation coordination: a disjoint native source-succession command/authority module, native identity-owner review of complete closure and head increments, and Motion append-only mapping/lifecycle ACK owner. Coordinate with the metadata observation owner before extracting shared descriptor verification. No schema allocation or dispatcher edits have been made for this design.

## Permanent succession protocol (design refinement)

The durable domain is a native source within one catalog incarnation and profile, not an ambient pathname. The proposed permanent typed history has one genesis/current chain per `(catalog_id, incarnation, profile, native_source)`. Each successor records an exact predecessor revision and predecessor record digest, a monotonically checked next revision, and immutable old/new binding snapshots. A first successor must independently bind the original registered/imported mapping; absence of history cannot imply a permissive baseline. A current-head index may be derived or transactionally maintained, but is not independent authority. No UPDATE/DELETE of historical effects, no fork with the same predecessor, and no cross-incarnation replay are permitted.

The exact native record must contain:

- Contract/canonicalization versions; catalog/incarnation/profile/source; predecessor and resulting source-head revisions/digests.
- Old/new native root binding (canonical locator, root identity including native device/inode/volume facts) and observation/identity policy snapshots. Policy relaxation is not relocation.
- Original/new Motion mapping commitment: Motion source ID, native source/profile/catalog identity, Motion root/root identity/binding revision, exclusions and exact native binding/policy. The native owner validates the native domain; Motion independently validates its own mapping and source/lifecycle revision succession. A host mapping digest is provenance, not a native permission grant.
- Complete source-file disposition, including unchanged registry IDs/relative locators, old occurrence revisions and independent ctime fences, verified new occurrence revisions/ctimes, sizes and content SHA256, and explicit unavailable/excluded dispositions. Retired exact snapshots remain retained.
- Complete affected context/version/timeline/binding membership and expected old heads, exact resulting heads, unchanged-member commitments, and disposition/effect digest. Any unsupported closure refuses before a write.
- Permanent command key `(catalog_id, host_namespace, operation_id)`, payload digest, admission incarnation, expected/committed generations and exact result identity/digest. The receipt result and permanent typed record refer to the same effect; a result cannot substitute another chain element.

The full strict result/effect data needed to reconstruct Motion ACK must live in permanent typed records or immutable child rows, with explicit row/byte bounds and exact child-set digest/count. Storing only an unrecoverable digest is insufficient. Row splitting is storage organization, not permission to execute a prefix. No command/result JSON is accepted as proof of descriptor custody.

### Physical proof and atomic application

A future nonserializable `VerifiedCandidates` is minted only by an independent host bridge using retained Motion admission evidence plus read-only root/file descriptors. Expected content hashes must come from that validated admission, not arbitrary command JSON. It covers every required file exactly once, includes complete original association/head membership, and records actual pre/post full fingerprints and independently hashed candidate bytes. Stable source/path registry IDs select what is being compared; they never establish equality. Reopening a requested user pathname after custody is lost cannot renew this proof.

The native command revalidates the retained original/current state, candidate descriptors, root binding, full file tuples including separate ctime, and all expected head/membership sets immediately before effects. The source binding, occurrence dispositions, new exact-file snapshots, complete supported version/context renewal, permanent succession record and receipt commit together in the existing native writer transaction. All error paths roll back. The initial capture module is descriptive only and does not yet implement this authority or mutation.

Capture domains are deliberately separate:

1. Motion administrative capture: current actor/credential, source revision/lifecycle epoch, complete logical membership and access, old mapping commitment, outstanding scan/render/discovery/publication ownership, and intended candidate root.
2. Native structural capture: catalog incarnation/generation, source binding/policies, complete file registry and current typed-reference/context closure, including unrelated members whose heads constrain the effect.
3. Filesystem capture: retained roots and read-only regular descriptors, independent content digests, full fingerprints/ctimes and stable candidate-root identity.
4. Atomic effect: resulting native occurrence/head/disposition set and permanent command provenance. Only this domain proves that succession committed.

No one domain substitutes for another. A preview token proves none of current authorization, terminal file stability or successful effect.

### Motion ACK and compaction-safe admission

Motion keeps the original registry/import descriptor immutable. An append-only mapping-succession ACK links exact predecessor mapping/lifecycle revisions to the new mapping, native successor revision/digest, command key/payload digest and exact effect/result identity. Its own source revision, binding revision and lifecycle epoch advance under one Motion writer. Existing metadata/history identities are not rewritten. A current resolver validates the full baseline-to-head chain or an independently validated immutable checkpoint, never the newest row merely by timestamp.

After a native effect but before Motion ACK, admission that needs a current binding fails closed because old mapping and native current binding differ. The reserved operation remains recoverable and blocks conflicting replacement. Recovery first reconciles the exact saved command. If its full receipt remains, compare it to the permanent typed effect before ACK. If the full result was compacted, a dedicated typed owner may return the retained complete effect only after checking the permanent receipt key/payload/incarnation/generation provenance and the saved intent's exact identity. Generic expired-receipt behavior is unchanged; no replay authorization is inferred from absence or from the current filesystem.

Current byte admission requires the verified latest Motion ACK, matching native successor head/binding/policy, current Motion lifecycle/access and independently live physical proof. It does not need indefinitely retained generic `result_json`. Historical reads may inspect older successor records but cannot turn them into current byte grants. Restored or cloned catalog incarnation mismatches fail closed under the existing explicit admission/upgrade protocol.

An exact repeated command can resolve only its original effect. A conflicting payload under the same key is refused. A later chain element does not replace a missing earlier result, and delayed ACK of an older committed effect cannot overwrite a newer Motion head. It may only verify already-recorded history or advance the exact expected predecessor when still current. Failed/no-effect candidates remain distinguishable from committed successors; unknown outcomes retain their reservation.

### Additional protocol tests and unresolved decisions

Required tests: full-result compaction followed by lost-ACK reconstruction; exact retry versus same-key altered payload; two successive relocations with preserved historical snapshots; delayed old ACK after a newer successor; missing/reordered child disposition rows; wrong predecessor/incarnation/effect digest; current-head/binding mismatch; and original/generated history continuity after new physical pins.

Pending decisions before schema/dispatch work: shared native version allocation (candidate41 remains unregistered); the permanent history and bounded child-row layout; exact legal context/version head increments and equivalence renewal; handling mixed unavailable/excluded or unsupported graph closures; and explicit withdrawn-source reactivation semantics. No native or Motion schema number is allocated by this document. Existing snapshot/backup owners must capture the new permanent domain when it is eventually registered; a generic receipt body is not its backup substitute.
