# v2 playback and viewing integration (A07)

The `/api/v2` playback surface serves authenticated plans, delivery sessions,
timeline viewing, preferences, continue-watching and next-in-order over the
production router. It builds on the v2 playback adapter first written on
`integration-web-desktop` (commits `6322595`, `a7c6f7e`, `7d0dc59`).

Current consolidation decisions and exact validation are tracked in
[the playback/viewing lane receipt](qualification/playback-viewing/README.md).

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
| `getDeliveryContent`, `headDeliveryContent`, `getDeliverySubtitles`, `headDeliverySubtitles` | `GET`/`HEAD /streams/{d}/{g}/{content,subtitles.vtt}` |
| `getHls*` | `GET /streams/{d}/{g}/master.m3u8`, `…/variants/main/{index.m3u8,init.mp4}`, `…/segments/{s}` |

## Correctness boundary

Decisions in `crates/core`:

- `playback_session`: route choice (original, prepared rendition, live
  conversion), client quality limits (`conversion_fits`, including refusal of unknown facts under an explicit bound), plan
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
- Planning uses version bindings whose reviewed file revisions are still
  current: a whole file, an interval of one file, or a multipart version
  (core `parts::layout`; intervals and parts play by conversion only).
  Timeline reads check the work and the edition, like catalog timeline reads.
- Playback-authorized heartbeats carry optional ordered playhead observations,
  independent of viewing events and permission. Core fences generation, sequence,
  duration and lease expiry; duplicate/stale reports do not renew leases.
- The delivery worker revalidates current owner grants/catalog facts each tick.
  Revocation or narrowed scope closes owned work; worker termination is confirmed
  before capacity is released.
- Browser viewing outboxes and ownership proofs are scoped to server identity and
  restore epoch. They survive ordinary process restart; restored state requires
  reconciliation. Capabilities expose restore epoch separately from runtime epoch.
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

The M4 expansion changes are in [the M4 ledger](qualification/m4-expansion/README.md):
- stream copy and hardware conversions (both off by default);
- sidecar and burned subtitles, and automatic subtitle selection;
- rendition audio continuity;
- interval and multipart timelines;
- bitrate caps;
- text subtitles on live routes as an in-manifest HLS subtitle rendition;
- a two-rung adaptive ladder for software conversions (off by default).

What remains:

- Text subtitles reach live (HLS) routes only as an in-manifest `SUBTITLES`
  rendition, for clients listing `hls_text` (the bridge does with HLS and
  selects it through Demuxe `selectSubtitleTrack`). An external track on a
  live presentation is never used: Shaka 5.2.11 throws 4033 for it. The
  pair is verified in headless Chromium with the pinned Shaka, not yet
  qualified in Chromium/Electron product builds. Text burn-in, including
  styled (ASS/SSA) subtitles and their fonts, needs libass. Subtitles on
  multipart versions are refused. Burned bitmap cues displayed more than
  20 s before a start or seek are not restored.
- Client bitrate caps are API-only: the server keeps `max_bitrate_bps`, but
  the production bridge sends `null` and nothing in the product supplies a
  value yet.
- Content access issues a file-revision ticket or a delivery-generation
  playback ticket. Generation tickets authorize only that generation's bytes,
  its HLS manifests and children, and its selected subtitle. Every read
  rechecks issuer credentials, grants, source revision, ownership and the
  reducer's lease/generation serving state. A restart invalidates delivery
  tickets. Cast receiver grants and download-resource tickets remain
  unavailable.
- Track IDs (`a{n}`, `s{n}`) are revision-local and require an exact
  `source` pin. A pinned prepared version still refuses track and component
  pins.
- HDR sources are refused for byte delivery under `require_sdr`, and to a
  client reporting `hdr: unsupported` or `unknown`. No tone-mapped route
  exists.
- Adaptive bitrate (`processing.experimental_adaptive_bitrate`, off by
  default) is two fixed rungs (720p at 3 Mb/s, 360p at 800 kb/s) for an
  uncapped single-file software conversion without burn-in. Copies,
  hardware conversions, capped, burned and multipart plans stay single
  variant. Switching is verified with headless Shaka under throttling, not
  yet qualified in product players.
- Next-in-order uses `timelines.order_group_id` / `order_position`.
- v1 item-keyed progress and v2 timeline progress are separate authorities
  after migration 0032's backfill.
- Viewing sessions survive ordinary restarts and may rebind to a new
  delivery.
- Native qualification is macOS arm64 only.
