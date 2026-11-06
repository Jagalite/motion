# Processing, preview and sign-in flow boundaries

Local qualification snapshot, 2026-11-02. This describes production callers and
bounded evidence, not completion of every audit item or native release gate.
Rust checks use the repository-pinned 1.98 toolchain, a shared task-only target,
`CARGO_INCREMENTAL=0`, and development/test debug information disabled.

## Production boundaries

### P02 — preview publication and reclamation

`crates/core/src/previews.rs` owns publication/reuse, exact attempt and layout
admission, withdrawal, orphan/staging eligibility, and identity-bearing removal
acknowledgements. `src/v2/previews.rs` supplies the observations and executes SQL,
filesystem operations and ownership checks.

Migration `0067` adds nullable `previews.storage_key`, durable removal intents,
replacement/withdrawal triggers, and a durable cleanup pagination cursor. Existing
NULL keys still resolve to `file/revision`; new publications use never-reused
`.published/job-attempt-UUID` generations. Publication synchronizes staged bytes,
renames into a fresh generation, and commits the selected row under the writer
boundary. It never overwrites bytes named by the previous committed row. A
rename-before-row-commit failure leaves an orphan, not corrupted prior bytes.

Row replacement/withdrawal queues old-generation removal atomically. Reclamation
rechecks row references under the writer before deletion, retains the intent on
failure, and acknowledges only durable absence. On Unix this includes parent
directory synchronization; this run does not qualify non-Unix filesystem crash
durability. UUID nonreuse also separates a cancelled caller's detached blocking
removal from later publication. Cleanup advances its durable cursor before
physical work, so 100 permanently blocked entries do not starve entry 101.

Staging remains while its exact job attempt is active or its worker witness is
held. A genuinely absent witness is distinguished from unreadable evidence:
non-NotFound I/O errors are unknown ownership and preserve the staging directory.
Staging is reconstructible work, not a published-generation removal receipt.

### P03–P06 — processing facts and identities

- **P03:** The existing typed `WholeBinding`/`BindingEvidence` boundary remains the
  authority for exact whole-file original preparation binding. It checks
  timeline/file/revision, origin, edition ownership, part count and absence of
  an interval. Playback and download callers have their own explicit eligibility
  contracts; a whole-file preparation rule is not substituted for multipart
  playback. Core tests exercise binding facts rather than only an admission
  boolean, and API tests retain wrong-timeline/changed-dependency coverage.
- **P04:** `processing::preparation_selection` resolves exact revision-bound
  components and per-kind track ordinals, rejects conflicting pins and missing
  occurrences, and permits only supported embedded text subtitles. Backend
  compatibility, video-profile bounds and half-frame duration admission are
  shared core decisions. `SourceContract::{Legacy,Preparation}` preserves the
  intentional preexisting difference: legacy fixed H264720p accepts unprobed
  input; preparation H264720p and both VideoProfile paths require known dimensions
  and SDR observations. FFmpeg arguments, probing and output validation stay in
  adapters. This is not a newly imposed legacy SDR restriction.
- **P05:** Core `recipe_identity` preserves historical fixed-software receipts and
  binds parameterized codec, dimensions, bitrates, frame rate, backend, subtitle
  handling and the adapter's versioned encoder-semantics descriptor. Core
  `previews::layout_identity` centralizes the existing duration/track JSON digest;
  it does not silently introduce semantic normalization of stored track JSON.
- **P06:** Core decisions govern restored generated-output invalidation, completed
  receipt expiration, unknown preview running observations, and persisted job
  kind to control-permission mapping. Output invalidation and receipt expiration
  now commit together under one writer transaction. Recovery reserves generic
  execution capacity and, for hardware or unknown backend provenance, hardware
  capacity until the inherited process witness is released. No demonstrated
  historical hardware oversubscription is claimed; this makes joint recovery
  accounting explicit. Failed running-state observation now stops rendering;
  publication still independently rechecks current facts.

### B01–B02 — real adapter ownership

- **B01:** Preparation component lookup calls `timeline_components_in` with the
  already-reserved connection, rather than acquiring a second pool connection.
  The real-router regression occupies three of the four pool slots and requires
  component-pinned preparation to finish using the remaining slot.
- **B02:** The actual burn-subtitle extraction caller owns an owned mutex guard
  and semaphore permit together. Blocking source/space/final-metadata observations
  retain both owners through an `Arc`, even after timeout or caller cancellation.
  The test injects a blocked source observation into that caller, not merely an
  isolated ownership helper, and checks neither owner is released until the
  blocking closure exits. Protocol execution, inherited descriptors, process
  supervision, subtitle bytes and atomic file publication remain adapters.

### A01/A02/A07 — sign-in flow slice

`signin_flows` supplies purpose-specific authority and fresh-action requirements,
method availability, exact email recipient bindings or a silent placeholder,
completion effect selection, consumed registration receipt replay, protocol
failure, and identity relink plans. `src/v2/signin/flows.rs` calls these rules.

Recipient decisions bind user/identity revisions while preserving transaction
browser/epoch/actor/deadline facts. Disabled/foreign/ineligible recipients still
follow the outward nonenumerating email response. Production `signin::reserve`,
`redeem_code` and `complete` remain the single-use phase authority. Completion
selects Test, Link, exact Session identity, PendingApproval or Registration;
registration policy itself remains the separate production registration workflow.
Consumed registration replay returns its stable receipt, never another session.
Relinking returns exact identity, next revision, constrained ceiling and dependent
family IDs; the adapter applies revocation and revision together under the writer.
This slice does not claim ownership of all A07 logout/store policies.

Sign-in-start trimming already expired pending requests. The additional callback
capacity check addresses requests that expire **between start and callback**, and
removes duplicated policy; it is not evidence of an indefinite OIDC-only outage.
Start trimming, OIDC callback admission and email registration now use the same
`expire_requests` adapter, calling core `decide_request(Expire)`. Capacity uses
core `registration::capacity_available(ApprovalRequired, count)` and one observed
callback time. No remote protocol, SMTP, hash comparison, credential generation,
authorization lookup, SQL transaction or session-cookie operation moved into core.

## Bounded model evidence

All listed executions exhausted their declared graphs with zero skipped checks.
Bounds below are states / transitions / depth, not claims about unbounded systems.

| Production model | Explored result | Search bound / domain |
|---|---:|---|
| Preview publication | Running 3/288; Cancelling 2/192; Queued 1/96, each repeated for three prior-publication identities | 100 / 5,000 / 5 per seed |
| Preview retention | 14 states / 294 transitions | 100 / 4,000 / 10; withdrawal, failed removal, lost/wrong acknowledgement, referenced stale intent |
| Preparation selection | 8 / 104 | 32 / 1,000 / 5; revision changes, conflicting/matching pins, sidecar and bitmap observations |
| Recipe identity → `finish_preparation` | 50 / 700 | 100 / 3,000 / 10; every encoder-input family, cancellation, stale attempt and failed commit |
| Email issuance → completion | 32 / 320 | 500 / 10,000 / 12; disabled/foreign/disabled-user observations, exact effect and failed commit |
| OIDC Login/Link/Test/Reauth and registration replay | Five seeds, each 4 / 56 | 20 / 500 / 6 per seed; reservation, terminal protocol failure, completion identity/count, receipt-only replay |
| Method configuration sequences | Provider 56 / 1,176; email 96 / 2,016 | 3,000 / 100,000 / 20; explicit four-update domain, test evidence, enable/edit/override, dependent invalidation |
| Action-proof sequences | 320 / 8,320 | 2,000 / 100,000 / 12; approval, actor credential rotation, revocation, owner/epoch/expiry changes, exact-once consumption |

The last two models exercise the shared production `signin::change_method`,
`proof_pending`, `can_approve_proof`, `proof_valid` and `ProofAuthority` decisions;
configuration/proof adapters belong to the broader authority lane. Model effects
are persisted only on successful modeled commit. These are not substitutes for
real SQL/filesystem/protocol tests.

## Executed adapter evidence and remaining reruns

- Application library suite: **115 passed** in the subsequent fast-tier run, including actual preview replacement
  after rename/row failure, failed deletion and lost acknowledgement, referenced
  intent protection, duplicate acknowledgement and 100-blocked-plus-101st cleanup;
  real migration 1–66 → 0067 with legacy row/bytes, generation replacement and old
  path cleanup; unknown-witness typed-error regression; actual subtitle caller
  timeout/cancellation; restored-output rollback on failed receipt persistence
  and joint recovered hardware/execution ownership; and 1,000 expired/live OIDC
  request capacity.
- `v2_metadata_processing`: **28 passed**, including single-connection component
  preparation, admission/replay identity, source/HDR eligibility, parameterized
  profile identity and preparation dependency revisions.
- Full-tier `v2_playback::previews_are_rendered_once_served_by_revision_and_retired`
  passed with real FFmpeg, including revision-serving, corruption/refusal and
  reconstruction. The initial suite had unrelated media timestamp fixture failures;
  after test-only timestamp normalization, the whole playback target passed 40/40.
- `signin_api`: **26 passed**, including nonenumeration, one-family issuance,
  enrollment/reenrollment ceilings, initiator/deadline changes, concurrent OIDC
  reservation, cancelled exchange/restart, and stable registration replay.
- The preview adapter fixture initially violated the unique live-job constraint:
  a stale worker intentionally left the current attempt running, then the test
  inserted a replacement. It now finishes the current attempt through production
  `finish`, and admits a separate later live staging fixture only after replacement
  completes. The invariant/index was not weakened; the subsequent library run
  passed all 114 tests.
- Focused core tests/models above passed on pinned 1.98. The sign-in model rerun
  after clippy layout cleanup preserved all state/transition counts.

The later fast-tier run passed all 115 application library tests, all 28 metadata
processing tests, all 26 sign-in API tests and all models listed above after the
final `trim` delegation and explicit time-9/time-10 assertion. Boxing the internal relink plan and lint-only model restructuring
do not change wire contracts. Workspace strict-clippy also passed in the final
lint run. The clean canonical fast tier passed 899 tests; real-process integration
passed all 16 checks; exact released-baseline upgrade/rollback/restore passed.
The full-tier terminal receipt is tracked in the main matrix. Native macOS arm64
packaging remains blocked in this Linux x86_64 execution environment.
