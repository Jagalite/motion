# Playback/viewing consolidation lane (2026-10-10)

The continuation on the combined tree is tracked in [COMBINED.md](COMBINED.md).
The source and evidence below are historical input-branch receipts, not combined
acceptance. Explicit compiler-path provenance in the new receipt supersedes
older toolchain claims based only on the cargo invocation.

Owner: lane 2, branch `playback-viewing-completion`, base `2344d03`.
Independent reviewer: GPT-6.1-Sol. Review and execution receipts are recorded below
once completed; submission of a Kyoto job is not a passing check.

Owned interfaces: production delivery/viewing policies, playback/viewing adapters,
capability service facts, canonical watched aggregation, and their tests.
Coordinated shared patches: contract YAML/JSON, Topcoat `UiPrincipal`, presentation
identity and the player heartbeat/outbox bridge. No migration number, root manifest
or lockfile changes. Integration must retain these changes when composing the
client and data/catalog lanes.

## Decisions

- `catalog.v2` and `playback.v2` are implemented and enabled. Qualification remains
  `unqualified`; feature limits name admitted playback routes and refusals. This
  does not claim complete M1/M4 operations or product-player qualification.
- Optional heartbeat `playhead: {sequence, position_ms}` paces delivery under
  `playback:request`, independent of `viewing:write`. The core owns active-generation,
  sequence, duration and lease fences. Sequence is a positive decimal string,
  monotonic per active generation; gaps are allowed. Older and duplicate reports
  have no effect; conflicting reuse returns 409. Newer reports may move backwards.
  Lease-only heartbeats retain their candidate-readiness behavior.
- Viewing events only write history. SQLite adapters atomically persist viewing
  records and receipts; pacing has no database/history acknowledgement dependency.
- Work watched aggregation is **any effectively watched timeline**. Alternative
  cuts retain separate progress and overrides. Missing timelines are unwatched.
  Smart-filter facts read canonical timeline state; retained legacy rows are not
  an alternate input. Public smart-collection composition belongs to lane 1.
- Ordinary process restart interrupts delivery, while durable viewing sessions and
  exact outbox retries survive. Browser outboxes and ownership proofs use server
  identity plus restore epoch; a restored/replaced identity requires reconciliation.
  Capabilities expose `restore_epoch` separately from process `server_epoch`.
  Every viewing mutation requires `X-Motion-Viewing-Epoch: server_id:restore_epoch`
  from the observation that produced the command. Core compares identities;
  the adapter observes current identity inside the reserved writer transaction,
  before receipt replay or mutation. This also fences an already-open page after
  restore. Missing/malformed headers return 428; changed identity returns 409.
  Do not refresh the header to blindly replay an older outbox. Current browser and
  TCP smoke callers carry the original identity; old v2 writers must update.
- Each delivery-worker tick observes current device grants and catalog facts.
  Production core decides eligibility. Revocation, profile/playback/catalog scope
  removal, missing devices or observation failure close owned delivery work.
  Adapters stop/reap workers and retain capacity until exit is confirmed. Existing
  request authorization independently denies new access immediately.

## Cutover dependency

C05 is not globally closed by changing smart-filter facts: the baseline still
mounts legacy progress/viewing routes and serves the old web application. Per
plan sections 4.4–4.5, remove these mounts with the client-lane cutover after its
caller acceptance passes, retaining legacy tables and upgrade readers. This lane
must not silently claim global v1 retirement or delete migration evidence.

C07 real product-player HLS, package qualification and integrated M1–M4 closure
remain with clients/integration. Server HTTP/FFmpeg tests are not browser playback
qualification.

## Validation

Qualification source: `db2bdbc7e6742b04d78b7b6ae79d30b352bbf9da` (macOS arm64).
The final documentation commit does not change executable inputs.

GPT-6.1-Sol reviewed the complete lane diff and the final fixture/import repairs;
no actionable findings remained. Earlier findings concerning replacement playhead
identity and rollback sequence reuse have regression coverage.

- JavaScript: 72/72 playback, UI bridge and desktop host tests passed.
- Contract: 97 paths, 146 operations, 148 schemas and 2,573 references validated;
  generated JSON matches YAML. Rust formatting and diff whitespace checks passed.
- Core: 92 unit tests passed with pinned Rust 1.98.0. Ordered observations exhaust
  313 states / 2,867 transitions: four events, two generations with activation,
  three sequences and positions, lease expiry, close/restart. Properties check
  observation/playhead identity, ordering, lease effects, rejection stability and
  exact duplicate behavior. The same target also checks watched aggregation and
  revoked/missing/narrowed owner policies (3/3 tests).
- Pinned Rust 1.98.0 real SQLite/FFmpeg router checks passed 8/8 playback, 7/7 organization
  and 32/32 access tests. The playback-only 180-second conversion advanced beyond
  100 seconds without any history rows; revocation denied new access and released
  the worker/output in 1.780 seconds (test limit: 10 seconds).
- Pinned Rust 1.98.0 real-process TCP smoke passed 10/10: original/range delivery, prepared
  and live conversion, audio switching, ordered events, SIGKILL/restart and outbox
  drain. The focused Go capability-negotiation test passed against that server.

The inherited local builds used Homebrew Rust 1.99.0 despite the toolchain file.
Their results are supplemental; pinned Rust 1.98.0 rebuild and execution receipts
are recorded in `receipts.json`. All compilation used the shared Kyoto queue.
The smart-watched fixture initially lacked metadata origin rows; after supplying
those production-required rows, the entire organization target passed.

Additional pinned checks: playback-session exploration exhausted 253,696 states
and 3,890,752 transitions (two principals/timelines, permission and scope removal,
source changes and expiry). Delivery fuzz completed 3,000 cases / 450,000
transitions with seed 20261009; exact heartbeat boundary checks also passed.
The unpinned full delivery-model run was interrupted when qualification moved to
the pinned toolchain, so it supplies no passing receipt. The full timeline-viewing
model was compiled but not executed in this run.

### Unresolved integration gates

- The broader Go pairing/browser test fails before its catalog workflow:
  `/?profile_id=not-granted` returns 200 rather than 403. The independent capability
  check is passing evidence only for negotiation; it is not TUI/browser acceptance.
- [CI on the qualified source](https://github.com/Jagalite/motion/actions/runs/38109450892)
  passed contract, format and clippy. Fast tests failed in the unchanged
  `motion-ui-host` cache tests `ordered_events_are_durable_exact_and_never_rebased`
  and `failed_publication_cannot_acknowledge_or_consume_a_sequence` (409 vs 200).
  The client/integration owner must resolve that gate; no green-CI claim is made.
- The notices workflow fails the publication privacy check because the shared
  consolidation plan contains absolute developer-machine paths, already present
  at base `2344d03`. The integration owner must sanitize that shared artifact.
- Global C05 retirement, product-player acceptance and combined-tree/full CI are
  still required as described above. This lane's passing server tests do not close
  those gates, and the PR remains a draft until integration resolves them.
