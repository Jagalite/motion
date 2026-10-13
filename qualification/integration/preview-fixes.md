# Preview, schedule, player and accessibility reconciliation (#62–65)

Status: implementation fixes; Rust compilation and final browser qualification pending.
Base: `origin/plan-completion/accessibility`. No existing migration was edited.
Additive migration `0060_preview_layout_identity.sql` was allocated by the integration owner.

## Review dispositions

- #62 shared requester job handle: already corrected at the base to return the contract's shared preview manifest; private job details remain owner-only.
- #62 duration mismatch / unusable first original: acceptance and rendering both require the file's probed duration; timeline-only duration does not promise a render. Candidate selection skips originals without a usable layout.
- #62 layout identity: jobs and publications now carry a digest of duration and track facts, checked at admission, rendering, publication, manifest and sheet access. Sheet URLs bind the layout identity. Technical metadata changes invalidate old work without requiring different media bytes. Existing publications without an identity are regenerated.
- #62 rotated sources: probe records display rotation (probe version 2); display dimensions drive interactive, scheduled and renderer layouts. Earlier video probes refresh on scanning.
- #62 duplicate/stale/unauthorized retry and #63 scheduled admission: all three admission paths execute the same core policy with source identity, readability, equivalent work and capacity observations under the jobs lock and SQLite write transaction. Scheduled cursor advancement stops before the first capacity-blocked candidate; deduplicated candidates may advance even when full.
- #62 renderer ownership: supervised process/witness path already present at base. Source preparation now additionally holds the execution reservation through a non-cancellable blocking open, including timeout/cancellation and late completion.
- #62 durability: root parent is synced before publication, in addition to existing sheet, staging, root and file-directory syncs.
- #62 sheet bounds: metadata check plus a MAX+1 bounded descriptor read; oversize files cannot allocate their full length. Sheet reads share the publication lock so layout replacement cannot exchange bytes beneath the checked row.
- #62 retention: unavailable or metadata-obsolete sources are retired. Published rows are inspected in pages of 100, with a durable cursor. Filesystem orphan and staging walks retain enumeration cursors and inspect at most 100 entries per pass. Stale-directory deletion remains under the publication lock, preventing deletion of a concurrent rebuild.
- #62 job visibility: additive AFTER INSERT trigger publishes queued job invalidation immediately.
- #63 permissions: preview schedule task is offered only with processing:request.
- #64 source identity: attachment requires the delivery's exact file and revision, including alternate-version replacement. Hidden-control HLS players do not attach authored sheets. Production CSP explicitly permits blob fetches.
- #64 cleanup and cancellation: already present at base; retired players abort their download controller, revoke sheets, and superseded lookups settle immediately.
- #65 stable form identity: already present at base. Restoration now preserves an already focused error alert instead of moving focus to main.
- #64/#65 evidence commands use the runnable Node test glob.
- #65 historical accessibility receipt: retained as historical/inconclusive, explicitly identifying the wrong revision and library Tab anomaly. No invented replacement browser result. A fresh audit of the integrated committed artifact is still required.

## Correctness boundary and verification

The production core admission policy owns decisions from authority, revision/layout,
equivalent work and queue capacity. Existing core publication and job transitions
remain authoritative. Adapters observe database and filesystem facts, serialize
admission/publication/deletion, retain execution ownership, and enforce durable ordering.

Added verification:
- core table covers 64 admission fact combinations including capacity boundaries;
- Stateless model is configured to explore all 32 fact states and six request/invalidation inputs,
  checking exact enqueue count and no duplicate admission after an accepted request;
- server regression extends preview lifecycle with metadata-only invalidation,
  stale retry rejection and oversized sparse sheet refusal;
- schedule fixture now represents an explicitly matching layout identity;
- rotation unit test checks 0/90/-90/180/270 degrees and old-probe refresh;
- Node regression checks exact source identity and error-alert focus preservation.

Observed locally: 49 Node tests passed; all migration SQL applied to an empty
in-memory SQLite database; formatting and whitespace checks passed.
Rust execution is not claimed: shared Kyoto policy requires test --no-run.
Scoped server/core test compilation is queued through Kyoto, currently held by
host memory-pressure admission. This does not qualify browser CSP behavior,
real rotated FFmpeg output, power-loss durability, screen-reader behavior, or
combined-branch migration upgrades. Integration owner must retain these boundaries.
