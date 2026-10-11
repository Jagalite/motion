# Playback/viewing consolidation lane (2026-10-10)

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

Pending final receipts.
