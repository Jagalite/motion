# Native preparation-job controls: bounded public source candidate

The private candidate passed the root-owned actual-runtime rerun in
`recovered63p2-motion-3.log`: eight native publication tests, including all four
private controls cases. The post-source66 public candidate now exposes only exact
POST `/jobs/{id}/cancel` and `/jobs/{id}/retry` for genuine retained native
preparations. Its new public/UI/client tests are source-written, not yet qualified.
No migration, native schema change, live data change, remote write, or deployment
is part of this patch. Go execution still requires the pending toolchain approval.

## Ownership and state rules

- Only Motion `processing_jobs` with an existing immutable native preparation
  admission and preparation record belong to this cohort. Descriptive legacy
  history and scan, metadata, backup and preview jobs are not adopted. Controls
  never use the frozen `api_jobs` union.
- The actual `app.jobs` mutex and Motion immediate writer serialize admission,
  worker claim, cancellation phase changes and publication reservation. Current
  caller authentication, owner/admin visibility, ProcessingRequest permission,
  mutation and its exact replay receipt share the writer. An old caller snapshot
  or old admin grant is not authority. Native controls have separate idempotency
  operation names from legacy controls.
- The production core decides reservation/output/revision/phase/attempt barriers.
  Queued cancellation becomes cancelled; running cancellation becomes cancelling.
  Already-terminal nonpublished jobs keep their real recorded state. Cancellation
  does not require a healthy source or the original requester's live credential.
  Its retained-admission identity is historical: it does not require a current
  source projection or matching live incarnation just to stop work or replay an
  old stop response. Any new retry must pass the current incarnation checks.
- Every historical native command reservation blocks both controls, even with
  a completed ACK or a later processing attempt. A command without a local
  receipt is committing/unknown, not proof of no effect. A retained receipt is
  reported separately. Controls never erase records, change the reserved attempt,
  manufacture completion, attempt receipt recovery, or dispatch native effects.
- Retry requires a reviewed revision and a failed/cancelled job with attempts
  remaining, no recorded output and no historical reserved command. The original
  preparation's requester credential, permissions, source scope, lifecycle,
  recipe, exact native version/manifest and occurrence must still be current.
  The ordinary queue bound also applies. The native validation block is extracted
  unchanged from the actual renderer, not a new SQL-row authority constructor.
- Retry only queues the unchanged attempt. The ordinary worker increments it
  after acquiring real execution capacity and I/O custody, reacquires source
  descriptors and native publication authority, and retains supervisor/witness
  ownership through confirmed termination. A queued retry does not bypass a
  stuck or still-held owner. Planned-but-unreserved output records stay retained.

Current policy/preparation facts and bounded queue decisions belong to core.
Native manifest/occurrence authentication, source descriptors, transaction
ordering, credential reload and receipt provenance remain existing adapters.
No SQL observation is promoted into a native publication or byte grant.

## Cancellation latency and remaining acceptance

Cancellation is cooperative. In particular, native input snapshot copying holds
the Motion writer and currently checks service cancellation rather than job
phase. A cancellation request may wait or fail busy until that copy releases its
custody. This candidate neither promises hard prompt interruption nor releases
permits, process witnesses, or cleanup ownership early.

The new real-worker custody test cancels at the existing pre-I/O running boundary
and then proves that a queued retry waits for the retained owner before executing.
It does not establish interrupt latency for an already-running FFmpeg child or
an in-progress source snapshot. Full latest-source outer-router/UI/bridge/client
acceptance, live-encoder cancellation and snapshot latency remain unfinished. This
local public candidate must not be represented as qualified or deployed before
root-owned acceptance completes.

## Bounded public transport and client contract

`processing.native_job_reads` still advertises list/get only. The separate
`processing.native_preparation_controls` leaf declares contract version 1,
operations cancel/retry and cohort native_preparation. It does not open generic
scan/metadata/backup/preview jobs, which retain their own independently admitted
owners. Unknown methods, invalid IDs, extra path segments and sibling job routes
stay closed. Missing native runtime never enables legacy fallback.

Native job list/get and new control receipts carry an optional versioned `controls`
descriptor. Cohort membership comes from the same retained admission/preparation
check as actual controls. Action-specific new-request hints use the production
core's phase/attempt/output/all-history-publication rules plus fresh caller scope
and ProcessingRequest. They are observations, not native authority or a promise
that a future retry will pass current source/requester/recipe/native/queue checks.
The reads do not rebuild native projections or acquire source descriptors.

Exact replay precedes new phase/publication/hint decisions, but never current
credential, ownership, permission or retained-cohort checks. A retry response whose
ACK was lost remains replayable with its original key and reviewed If-Match even
after the worker publishes and new-action hints are false. Stored response bodies
and ETags remain exact, not recomputed current status. Clients must distinguish a
previous request acknowledgement from a fresh observation of the current job.

Online Rust UI and Go TUI offers require the separate recognized capability and
per-row descriptor; mixed historical rendition rows remain read-only. Older servers
with neither native leaf preserve existing behavior, while missing/disabled/unknown
native support fails closed. The browser bridge and TUI retain already-uncertain
intent identity for exact explicit recovery, never generate a new key/ETag to evade
changed hints. Unsubmitted confirmation is withdrawn on support/permission loss.
The CLI retains explicit server-authoritative calls without adding capability or
fresh-review requirements to exact replay. Offline motion-ui-host remains unchanged
and still denies API forwarding.

## Written tests and root-owned runtime filters

These filters describe the latest source, not new pass claims. Run only through the root's selected
recovered validation environment, locked dependencies and verified packaged
observer/supervisor; do not build in or mutate frozen source snapshots.

- Core filter `native_processing_control`: five deterministic policy tests cover
  all six phases, attempts 0/1/exhausted, every publication barrier, output history,
  reviewed retry, all 32 preparation fact combinations, queue bounds and both
  cancellation-versus-reservation orders using the real reservation policy, plus
  advisory action hints that cannot authorize a new transition.
- Root filter `native_control_`: actual SQLite migration and
  immutable publication rows, planned-only versus reserved/recorded/ACKed history
  across multiple attempts, and unrelated job isolation. Empty JSON in this one
  test exercises only conservative barrier classification, never native authority.
  The new hint/cohort SQL test needs no api_jobs/native projection/source grant and
  rejects missing/mismatched admissions, permission loss and publication barriers.
- Root filter `processing::native_publication_tests::native_processing_controls_`:
  six genuine native-runtime fixtures cover exact public method closure, unknown/
  foreign/legacy job refusal, receipt-write rollback, missing/stale If-Match,
  exact replay, later retry refusal, fresh ownership/admin/permission/credential
  checks, original-requester revocation, real source withdrawal followed by
  cancellation/replay (and retry refusal), actual worker capacity retention and
  eventual retry publication, real pre-effect reservation, real native commit
  with injected Motion ACK failure, immutable receipt preservation and exact
  ACK recovery/restart without a new attempt or publication. Two new outer-router
  cases exercise mixed history/UI, public rollback, owner/admin/permission/revocation,
  source withdrawal and exact retry-key replay after real worker publication.
- Rerun `processing::native_publication_tests` and the existing native generated
  renderer/recovery integration target because their setup and renderer admission
  now reuse the extracted validation block. Rerun `v2_native_jobs` and exact native
  route-gate tests to demonstrate the narrow public cohort and retained history
  semantics. `tests/contract.rs` checks the optional versioned Job schema; root also
  owns contract YAML/JSON parity, presentation/compose and ui-bridge tests. Terminal
  API/operations/CLI/TUI source tests require the separately authorized Go toolchain.

No tests or compilers were run by the implementation worker. Root owns runtime
qualification and must record actual results separately before this source can
be described as qualified.
