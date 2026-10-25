# Native scan schedule owner: isolated candidate

Current evidence: Motion40p5 root204 passed, including actual schedule CRUD,
timer/adoption and watch owner tests. Motion44 stages five exact outer schedule
methods and a scheduler genuinely owned and joined by NativeService, with TCP,
scan-completion, shutdown and restart acceptance fixtures. That newer public/service
cohort is not yet runtime-qualified. See [remaining product acceptance](CATABOLIC_REMAINING_PRODUCT_ACCEPTANCE.md).
Canonical migration0092 and the native scan-only companion preserve the legacy
schedule DTO. Legacy maintenance/watch is never a native fallback.

## Companion and atomic firing

Keep `api_schedules` and its legacy logical-library foreign key intact. A separate
native scan-only companion should retain definitions (including tombstones),
requester ID, credential hash/ingress reference, revision, interval, and enabled
state. A cursor stores definition revision, original due time, and retry time.
An immutable occurrence is the full tuple `(schedule ID, definition revision,
due_at)`, uniquely linked to its native scan request and exact accepted scope
proof. Never cascade-delete occurrence history.

Capacity backoff changes retry time, not occurrence identity. The pure reducer
uses existing maintenance interval/overflow/backoff rules. The expected cursor
is a SQL CAS input, not authority. A completed admission advances the cursor only
in the same Motion writer transaction as the request and occurrence insertion.
An uncertain commit is reconciled by that original tuple, not a newly minted key.

Before firing, authenticate the retained credential/ingress through the existing
current principal owner and require its principal ID to equal the retained
requester. Under the admission writer require current SourcesManage, current
library scope, exact complete native source selection/binding/policy, and active
source lifecycle proofs. Captured principal JSON is audit evidence only. Store
exact lifecycle epochs in the accepted occurrence proof; native generation alone
cannot attest continued source admission. Only pre-BEGIN SQLite BUSY may defer a
claim as idle; post-BEGIN/commit errors retain their real uncertainty.

## Historical adoption and list completeness

Under one writer snapshot each original `api_schedules` row, preserving all of:
ID, revision, library ID, task, interval, enabled, next_run, requester ID,
credential hash, and ingress ID. Also preserve the optional
`schedule_dispatch_progress.after_item`. Bind the complete canonical snapshot to
an immutable adoption link containing the original table/ID, admitted native
catalog ID/incarnation, and the same companion schedule ID. Store exact evidence,
not merely a hash. Replay compares the complete link; changed originals cannot
silently re-adopt or overwrite a previous interpretation.

For scan rows only, actual native library-definition proof permits adoption with
the same revision, identity, actor references, and enabled flag, and
`due_at = retry_at = original next_run`. Do not reset overdue time or dispatch
during adoption. Scan rows with any fanout progress, including an empty stored
cursor, are refused: that cursor belongs to metadata fanout and must not be
silently discarded. The pure planner returns only a candidate, not this proof.

Non-scan tasks and unresolved native definitions remain explicitly retained as
unsupported/pending, with their original time and progress visible. Read/list
must union retained originals and companion rows by ID, suppressing an original
only when its exact adoption link is verified. Startup must not substitute an
empty native-only list for prior schedules. Historical rows and jobs are never
deleted by adoption. Credential references remain private and are not added to
the public schedule DTO.

## Required acceptance before activation

- Pure due/backoff/revision/restart tests preserve exact occurrence keys and the
  unchanged maintenance interval/overflow semantics.
- Real historical adoption preserves disabled state, IDs, CAS revisions, actor
  references, due time, and metadata progress; unsupported rows remain listed.
- Real current requester revocation, credential expiry, changed library scope,
  source withdrawal, and changed binding deny new request admission.
- Actual held-writer and competing-claim tests prove no duplicate request and no
  cursor advancement without its durable occurrence/request transaction.
- Crash before commit has no accepted effect; restart after commit finds exactly
  the original request. Definition replacement creates a new revision but does
  not reinterpret an old occurrence. Tombstones preserve history.
- A genuine native scan request from a schedule completes through the existing
  observer/discovery owner and survives restart without legacy queue writes.

Watch support is a later bounded hint producer at the same admission seam. It
requires explicit current service/requester authority, durable debounce/overflow
and catch-up semantics, not legacy `mark_dirty` or a fabricated operator grant.

## Conditional terminal proof

Manual accepted-demand contract1 is unchanged. Schedule acceptance requires all
three reserved schedule fields together: version1, exact occurrence, and a
complete unique active lifecycle proof set. Admission compares the set to the
actual source mappings under its writer. Before each effect, the demand owner
requires its exact immutable occurrence/request scope, complete accepted demand
source set, and unchanged active lifecycle for the executing source. Completed
siblings need not remain active. This does not retroactively reinterpret the
accepted bounded-service policy using later requester roles or library edits.

The canonical owner fixture creates the legacy source, library and schedule
through HTTP, performs actual qualified native cutover, adopts/fires once, and
checks missing/altered occurrence, source-set mismatch, preserved original
evidence and updated cursor. Actual cancel, library detach and source withdrawal
then verify that the old proof cannot authorize an effect at a new lifecycle
epoch. Restart/worker dispatch and public schedule routes remain separate
acceptance work; this fixture does not claim those paths are enabled.

## Historical post-36 adapter boundary

At the post-36 boundary, native CRUD was behind a closed route allowlist and
the timer had no startup caller. The newer Motion44 public/service candidate
described above supersedes that wiring, but still requires its own runtime evidence.

The draft `native_status` response field was removed after contract review.
The adapter preserves the existing Schedule DTO exactly. Pending adoption remains
explicit in retained owner inventory, tick diagnostics, and mutation refusal;
legacy rows are never silently omitted from list/read. Fresh create/update
currently require a complete, nonempty qualified source scope, which is a bounded
cohort rather than parity with empty legacy libraries. Legacy non-scan and
actorless v1 schedules remain explicit adoption obligations.

The worker uses an ephemeral deterministic `(retry_at, id)` keyset sweep, advancing
for every observed row including refusals. It resets only on sweep exhaustion or
process restart. This cursor cannot authorize admission or alter durable due
identity. The storage regression covers 102 unchanged due rows across a refused
100-row prefix, then the later rows, reset, and unchanged durable cursors. Actual
CRUD, current-principal refusal, and worker/restart fixtures remain required.
