# v2 playback and viewing integration (A07)

The `/api/v2` playback surface serves authenticated plans, delivery sessions,
timeline viewing, preferences, continue-watching and next-in-order over the
production router. It builds on the v2 playback adapter first written on
`integration-web-desktop` (commits `6322595`, `a7c6f7e`, `7d0dc59`).

## Operations

| Operation | Route |
|---|---|
| `planPlayback` | `POST /playback/plans` |
| `createDelivery`, `getDelivery`, `closeDelivery` | `POST /playback/delivery-sessions`, `GET`/`DELETE …/{delivery_id}` |
| `heartbeatDelivery`, `changeDelivery`, `activateGeneration` | `POST …/{delivery_id}/heartbeat`, `…/changes`, `…/generations/{g}/activate` |
| `createViewingSession`, `getViewingSession` | `POST /playback/viewing-sessions`, `GET …/{session_id}` |
| `recordViewingEvent`, `replaceViewingDelivery` | `POST …/{session_id}/events`, `PUT …/{session_id}/delivery` |
| `getViewingState`, `setWatchedState` | `GET`/`PUT /profiles/{p}/timelines/{t}/viewing` |
| `getPreferences`, `replacePreferences` | `GET`/`PUT /profiles/{p}/preferences` |
| `listContinueWatching`, `getNextTimeline` | `GET /profiles/{p}/continue-watching`, `GET /profiles/{p}/timelines/{t}/next` |
| `getHls*` | `GET /streams/{d}/{g}/master.m3u8`, `…/variants/main/{index.m3u8,init.mp4}`, `…/segments/{s}` |

## Correctness boundary

Decisions in `crates/core`:

- `playback_session`: route choice (original, prepared rendition, live
  conversion), client quality limits for conversions (`conversion_fits`), plan
  token admission against current permission, profile grant, catalog scope,
  version binding and source revision, delivery control and replan
  compatibility, and who may record viewing progress.
- `timeline_viewing` (new): one state per profile and timeline; the session
  authority; consecutive sequences; exact retries versus conflicting reuse of
  a sequence or event identity; manual overrides that advance a manual epoch
  and fence earlier sessions; sticky automatic completion at the profile's
  threshold; rebinding a session to another delivery of the same timeline.
- `delivery` (existing): generations, leases, activation, pacing, restart
  interruption. `renditions::available` decides whether a prepared rendition
  is still revision-valid.

Adapters supply the facts and commit the decided records:

- Every v2 mutation (delivery commands, viewing writes, preferences, watched
  state) re-derives the caller inside the SQLite write transaction and
  rechecks the core decision there, so a revocation commits either before the
  check or after the effect. Reads use the caller resolved for that request.
- Viewing state, session, event receipt and superseded session commit in one
  transaction (`migrations/0032`). A viewing session's idempotency receipt
  commits with the session it acknowledges.
- The delivery owner (principal, profile, timeline, version, source) commits
  with the delivery's first record and its admission receipt
  (`migrations/0033`). After a restart the owner reads the delivery as
  `interrupted`; commands are `409 delivery_closed`; close is idempotent.
- Change (seek/replan) and activation acknowledgements are recorded under the
  delivery's lock together with the transition they acknowledge (core
  `delivery_admission::decide`), and also in `idempotency_records` within the
  command's write transaction, so an exact retry replays after a restart too.
- An exact retry of an acknowledged admission is reauthorized without plan
  expiry (`playback_session::reauthorize`); only a new admission needs a
  fresh token (`fresh`).
- Planning uses only whole, single-part version bindings whose reviewed file
  revision is still current; timeline reads check the work and the edition,
  like catalog timeline reads.
- Accepted viewing events report the logical playhead to the bound delivery,
  which paces its encoder.
- Admission refusals keep their v2 problem code (for example `plan_expired`)
  instead of being re-mapped through the v1 error type.

Decisions kept in adapters, with reasons:

- The delivery generation named by a viewing event is stored for diagnosis
  but does not decide acceptance. Positions are on the logical timeline, so
  the generation does not change where the playhead is; rejecting it would
  block a durable outbox after a rebind.
- Browser codec support is an untrusted capability observation mapped to
  `Support` in the adapter; the core decides only what it may do with it.

## Verification

- Unit tests in `playback_session` and `timeline_viewing`.
- Stateless: `crates/core/tests/timeline_viewing_model.rs` exhausts the
  timeline viewing graph (two sessions over two deliveries with known and
  unknown duration, two events each, one override, one rebind, exact retries,
  reused identities, repeated and skipped sequences, boundary positions).
  `playback_session_model.rs` covers plan admission and ownership, now with
  the prepared route. Expected outcomes are stated from a ghost history, not
  from the production rules.
- In-process HTTP over the production router, real SQLite, real scans and
  real FFmpeg: `tests/v2_playback.rs` (original playback and seek, HLS with an
  audio switch, restart fencing and outbox drain, revocation, rebinding,
  manual epochs, completion threshold, change receipts, release order,
  quality-limit refusal).
- Real process over TCP: `scripts/v2_playback_smoke.py` runs the built
  binary in restricted mode with a paired device, a real processing job for
  the prepared rendition, live HLS, SIGKILL and restart.

These are server evidence. Browser and Electron playback against these
routes is qualified separately (A11/A12).

## Remaining gaps

- Subtitles: plans that select or require a subtitle are blocked with
  `subtitle_delivery_unavailable`; the v1 WebVTT sidecar has no v2 route yet.
- Media tickets: stream and media routes accept bearer, cookie or ingress
  credentials. Delivery-generation content-access tickets (for players that
  cannot attach headers) are not implemented.
- Component selection (`audio_component_id`, `subtitle_component_id`) is
  blocked with `component_selection_unavailable`; track IDs (`a{n}`) are
  revision-local stream indexes.
- Live conversion uses the software recipe only; hardware encoding and copy
  routes are not offered by the v2 planner. A conversion is refused when the
  client sets any bitrate limit, because the CRF recipe has no bitrate cap.
- Multipart versions and interval (multi-episode) bindings are not planned:
  they are reported as unavailable until a part-aware delivery exists.
- Encoder pacing follows viewing events. A principal that plays a live
  conversion without `viewing:write` sends no playhead, so its encoder pauses
  45 s ahead of the requested start. The v2 heartbeat carries no position.
- HDR sources are refused for byte delivery under `require_sdr` or to a
  client reporting `hdr: unsupported`; no tone-mapped route exists.
- Revision-local track pins (`a{n}`) require an exact `source` pin.
- Next-in-order uses `timelines.order_group_id`/`order_position` only. Main has
  no placement writer until PR #4 merges, so tests place timelines by SQL.
- v1 item-keyed progress and v2 timeline progress are separate authorities
  after migration 0032's one-time backfill (exact attributions and
  single-timeline works only). The v1 web UI does not see v2 progress.
- The `interrupted` viewing-session status is not produced; sessions survive
  restarts and are rebound to a new delivery.
- Native qualification is macOS arm64 only.
