# Client and catalog boundary qualification

Status snapshot: 2026-11-02, after the focused catalog rerun. This records
evidence for C01–C10 and L01–L04, not completion of the entire refactor. The
catalog HTTP/persistence targets below have now passed. The branch-wide full
tier remains owned by the coordinator; this document does not claim its result.

## Executed evidence

- Latest real-host Node gate: **203 passed, 0 failed, 0 skipped**. This includes
  playback, UI bridge/auth, desktop orchestration and the previously skipped
  real cache fixtures. Log: `/workspace/shared/motion-node-real-host-tests2.log`.
- Terminal operations: `go test ./internal/operations -count=1` passed, including
  the shared browser/terminal settlement cases. Log: `/tmp/client-go-operations.log`.
- Latest catalog core gate: **10 boundary tests and three Stateless models passed**.
  `catalog_lifecycle_models` exhausted 114 source states / 676 transitions and
  179 matching-history states / 508 transitions. `catalog_split_model` exhausted
  24 states / 84 transitions. All three report zero skipped checks.
  Log: `/tmp/catalog-final-core.log`.
- Actual SQL visibility/reference equivalence passed in the application library
  tests: `v2::catalog::catalog_boundary_tests::multipart_visibility_sql_matches_core_for_every_small_scope`.
  It invokes production `version_ids` for 62 combinations: 0–4 parts, every
  per-part visibility combination, restricted/admin scope.
- Independent review's L03 persistent-callback P1 was fixed and closed. The
  earlier deferred-pause writer/fence finding was also fixed. This is not a
  claim of independent approval for every catalog row or the entire branch.
- Current catalog adapter targets passed: `catalog_persistence` **22/22**,
  `catalog_workflows` **31/31**, `libraries` **11/11** in
  `/workspace/shared/motion-fast-tier.log`; `v2_catalog` **25/25** in
  `/workspace/shared/motion-fast-fixes2.log`. The latter rerun fixes only new
  fixture expectations (canonical 422/412 statuses) and two short idempotency
  keys, without weakening production checks. It executes the final
  history-bound inline/reference/relocation paths.

## Catalog

All core paths below are under `crates/core/src`; application paths are under
`src`. `catalog_boundaries` names refer to `crates/core/tests/catalog_boundaries.rs`.

| ID | Production decision and caller | Executed model/unit evidence | Real adapter evidence and retained boundaries |
|---|---|---|---|
| C01 | `sources::{registration,source_removal,source_scope_mutable,library_removable}` → `libraries` registration, exclusion changes and `delete_*_in` | `catalog_lifecycle_models::source::exhaustive_source_registration_work_removal_recovery_graph`: **pass**, 114/676. One root, two observed storage identities, membership, work success/failure, retained files/history, scope changes, removal and re-registration. Earlier lifecycle matrix also passed. | **Pass:** `libraries::{exact_root_registration_reuses_identity_and_rejects_changed_storage,deleting_libraries_and_sources_never_touches_originals,a_source_with_active_processing_cannot_be_removed,re_registration_keeps_library_edits_and_relocation_honors_exclusions}`. Filesystem identity observation and transactional SQL remain adapters. |
| C02 | `identity::{plan_split_occurrences,commit_split_occurrences}` → `curation::{preview_split_on,split_in_transaction,apply_split}`. Reviewed plans bind occurrence IDs/current edition/content, exact version pins and destination assignments. | `catalog_boundaries::{split_occurrences_pin_copy_ownership_and_reject_stale_inventory,split_review_fences_pins_even_when_current_destination_would_be_identical}` **pass**. `catalog_split_model::exhaustive_review_change_commit_ownership_graph` **pass**, 24/84: two versions, one copy, two copy/pin revision values, preview/intervening change/commit/duplicate commit. | **Pass:** `catalog_persistence::{split_review_fences_independent_occurrence_revision,split_does_not_steal_replaced_occurrence_bound_by_retained_version,split_sql_matches_core_application_and_replay_is_rejected,whole_timeline_split_matches_core_application}` and `v2_catalog::large_split_plan_reference_fences_actor_inventory_and_expiry`. The model found and fixed a retained-bound-file case: current bytes matching moved content must not make a file bound by a retained version into a movable free copy. |
| C03 | `catalog::merge_contributions`, `identity::{plan_merge_contributions,commit_merge_contributions}` → curation preview/transaction planning, commit validation and literal transfer execution | `catalog_boundaries::merge_contribution_target_and_source_order_are_explicit` **pass**: target precedence, two retired-source orders, NFO origin and artwork independence. Exact transfer winners are included in `MergePlan`, not recomputed only during SQL application. | **Pass:** `catalog_persistence::merge_sql_matches_core_application_and_is_fenced`; `catalog_workflows::{nfo_ownership_follows_contributions_through_merges_and_splits,sidecar_fifo_pins_and_merges_are_handled,pinned_identity_cannot_be_withdrawn_and_moves_with_a_merge}`. Existing metadata-generation triggers remain the durable revision authority; their composed transfer behavior is exercised by these passing real adapter runs. |
| C04 | `identity::{merge_footprint,split_footprint,order_footprint}` → v2 merge/split authorization and order mutation, under the current writer and grants | `catalog_boundaries::receipt_projection_and_mutation_footprint_follow_current_owners` **pass**: complete supplied relationship/order-owner footprint and revocation of each owner. | **Pass:** `v2_catalog::{merge_plans_are_reviewed_scoped_and_committed_once,existing_playable_timeline_order_is_fenced_scoped_and_preserves_identity,split_authorizes_prior_order_owner_before_invalidating_membership}`. Added split authorization of previous order-group owners, since invalidation mutates those groups. Queries load ownership facts; core selects the mutation footprint; current access policy authorizes it. |
| C05 | `matching::{reconcile_history,reopen_with_open}` → `matching::{propose_in_transaction,reopen}` | `catalog_boundaries::matching_history_preserves_decisions_and_one_open_identity` **pass**. `catalog_lifecycle_models::matching_history::bounded_two_proposal_history_and_reopen_graph` **pass**, 179/508: two proposals, two content revisions, at most seven actions, proposal/reject/reopen and immutable exact decision receipts. | **Pass:** `catalog_workflows::{changed_file_stales_open_proposal,reproposing_a_decided_file_keeps_the_decision,proposal_history_ignores_wall_clock_order,fix_match_unmatch_and_manual_identification}`. Durable insertion order is supplied by SQL; unique-open index and transaction remain enforced. |
| C06 | `sources::{relocation_file_required,relocation_complete,relocation_envelope}` plus `catalog::review_admission` → relocation preview/commit and v2 admission before/after filesystem observations | `catalog_boundaries::{relocation_requires_exact_scope_and_live_envelope,reviewed_authority_rejects_missing_or_restored_history_without_adoption}` **pass**. Required-set coverage includes 32 enablement/inventory combinations and omission of each required entry. Typed source mismatch preserves the existing `AccessError::StaleRevision` mapping (412); invalid actor/expiry plans retain 422. | **Pass:** `libraries::relocation_is_previewed_then_committed_against_unchanged_facts`; `v2_catalog::{source_relocation_preview_commit_and_retry_are_fenced,reviewed_catalog_and_relocation_tokens_reject_a_new_restore_history}`. Hash/stat stay in adapters; exact coverage, actor, source, deadline and server history are core decisions. |
| C07 | `identity::{order_placement,invalidate_order}` → `curation::{place_timeline_in,revalidate_order}` | `catalog_boundaries::order_effects_are_exact_and_exhaustion_has_no_plan` **pass**: identical placement, cross-group movement, stale revisions, timeline/old/new group integer exhaustion, membership invalidation and exact revision effects. | **Pass:** `v2_catalog::{existing_playable_timeline_order_is_fenced_scoped_and_preserves_identity,public_timeline_order_writer_drives_next_resolver}` plus merge/split persistence tests. Identical placement deliberately emits no revision changes. SQL applies exact planned revisions in one transaction. |
| C08 | `catalog::{reassignment_precondition,item_precondition}`, `identity::timeline_owner` → v1 file assignment, v2 item update and curation timeline creation | `catalog_boundaries::catalog_command_preconditions_cover_stale_foreign_and_immutable_facts` **pass**: stale edition/foreign work, derived membership, immutable family and foreign timeline edition. | **Pass:** `catalog_workflows::{reassigning_a_file_moves_its_version_within_the_work,reassignment_keeps_reviewed_content_with_its_copy}` and the v2 catalog write/ownership suite. Parsing, row lookup and status translation remain adapters. |
| C09 | `catalog::project_receipt` → v2 `item_receipt`, using current authorized `item_response`; `authorize_plan_receipt` uses current owner lookup and existing curation authorization | `catalog_boundaries::receipt_projection_and_mutation_footprint_follow_current_owners` **pass**: revoked libraries/parents, changed timeline/artwork and idempotent projection. | **Pass:** `v2_catalog::reconciliation_replays_recheck_current_catalog_scope`. Saved scalar receipt values are not refreshed; historical identities survive only when still visible in the current projection. Owner lookup is query materialization, not another authorization state machine. |
| C10 | `matching::{local_candidate_title,local_candidates}`; `catalog::{file_availability,default_timeline,generated_version,split_root_kind,version_visible}` → matching, curation and v2 catalog | `catalog_boundaries::{local_candidates_use_precedence_evidence_order_and_cap,catalog_command_preconditions_cover_stale_foreign_and_immutable_facts}` **pass**. Actual SQL/reference multipart visibility test **passed**, 62 cases. | **Pass:** broader browse/version scope and catalog workflow suites. SQL `version_ids` stays a scoped query; differential testing compares it to the core reference predicate rather than moving ordinary query materialization into a state machine. Filename extraction is adapter input; normalized title/year/source precedence, evidence and cap/order are core policy. |

### Reviewed-plan storage and restore

Migration `0068_catalog_review_plans.sql` separates expiring pending proposals
from immutable committed `catalog_receipts`. Small plans retain signed inline
tokens. Larger exact plans use fixed-size, domain-separated MAC-authenticated
opaque references. The durable envelope is bounded at 8 MiB both in the adapter
and schema; expiry cleanup occurs when issuing another plan. No credential or
CSRF secret is stored in the envelope.

Both catalog token forms and source relocation plans bind their principal,
deadline and `server_id:restore_epoch`. Old envelopes without history fail
closed. The shared restore withdrawal inventory deletes pending references, not
immutable receipts. The following new Rust/API tests have now **passed**:

- `catalog_persistence::oversized_review_documents_are_bounded_without_weakening_audit_receipts`
- `catalog_persistence::restore_withdraws_pending_review_references_but_retains_committed_receipts`
- `v2_catalog::large_split_plan_reference_fences_actor_inventory_and_expiry`
- `v2_catalog::reviewed_catalog_and_relocation_tokens_reject_a_new_restore_history`

The storage lane separately verified the installed Python restore tool for
current and legacy snapshots. That is separate evidence from the passing Rust/API tests.

## Clients

| ID | Production boundary and caller | Executed evidence | Retained boundaries/limits |
|---|---|---|---|
| L01 | `packages/ui-bridge/mutation.mjs::{mutationOutcome,mutationScope}` → bridge command/plan submission and auth mutations; terminal settlement shares the conformance fixtures | **Pass:** 14 `mutation-cases.json` cases exercised by Node `mutation.test.mjs` and Go `mutation_conformance_test.go`; actual bridge/auth scripts test unknown outcomes followed by 401/403/404, receipt conflicts, definitive domain conflicts, retry identities, reload and principal/server-history scope. | First definitive refusal and refusal after uncertainty are different states. Non-secret bridge keys persist in scoped session storage; auth secret-related pending keys are memory-only by design. Tests execute scripts in DOM/VM harnesses, not live browser automation. |
| L02 | `apps/desktop/src/download-reconciliation.mjs::{downloadReconciliation,advanceDownloadReconciliation}` → `downloads.mjs` pass | **Pass:** 1,296 two-demand inventory/tombstone/recovery/sync combinations; exact completion identity and duplicate/failure tests; actual orchestrator transfer/resume/recovery/removal/report tests; real-host cache fixtures in the **203/203, zero-skip** gate. | Filesystem, network and API calls execute ordered effects. Incomplete inventory or foreign cache ownership cannot authorize destructive cleanup; failed recovery defers collection; saved removal identities survive stale inventory. Bounded model does not enumerate arbitrarily many demands. |
| L03 | `packages/playback/replacement.mjs::{deliveryReplacement,advanceDeliveryReplacement}` → player `switchDelivery`; operation fencing is separate from installed `playbackOwner` lifetime | **Pass:** 19,607 bounded coordinator event transitions plus exact effects; actual caller tests for deferred predecessor pause/close/restart, successful cross-delivery swap, cross-delivery rollback, same-delivery rollback and deferred same-delivery activation. Tests invoke actual installed viewing/heartbeat callbacks after the change promise settles, then verify delivery replacement/close retires them. Independent P1 callback-lifetime finding **closed**. | Preparation/rebind I/O remains adapter work. Unknown rebind cannot resume the predecessor; old readback does not prove rejection. Persistent callbacks bind epoch/delivery/player/generation, not the temporary operation token. No live browser/media-backend automation claim. |
| L04 | `packages/playback/viewing.mjs::{viewingAuthorityUnchanged,adoptableRevision,markRetryRevision}` → viewing writer and bridge watched-state conflict recovery | **Pass:** viewing and bridge tests for same-session revision adoption, changed manual epoch/server history/session authority, immutable pending-event replay after reload, late acknowledgements and u64 sequence boundaries, within the full real-host Node gate. | Shared authority checks do not equate position-based adoption with manual watched-state retry. Persistence and requests remain adapters; revisions stay decimal strings/BigInt. No live browser validation claim. |

## Final integrated gates and limits

Clean canonical fast: 899 passed. Final canonical full: 77 passed in 4,837.5 seconds,
within the unchanged 90-minute local budget. Strict Clippy/formatting, 203 real-host
Node tests, 16 real-process integration checks and exact released-baseline
upgrade/rollback/restore passed. Final source and log identities are in the main
matrix and verification.json. Native macOS arm64 packaging and remote CI remain
unrun; DOM/VM caller tests are not live browser automation. The bounded models
establish their stated finite properties, with separate SQL/filesystem/network
adapter evidence rather than an inferred global proof.
