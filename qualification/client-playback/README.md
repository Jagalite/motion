# Client playback against production APIs (A11/A12 integration)

Recorded October 10, 2026 on macOS 26.5 arm64. The receipt is
[receipt-2026-10-10.json](receipt-2026-10-10.json). Screenshots, network and
console logs, server logs and browser videos were kept outside the repository
with the run.

## Production evidence versus fixture tests

| Kind | What runs | Establishes |
|---|---|---|
| **Production evidence** (`scripts/client_playback_e2e.mjs`) | The real `playscale` binary (`--topcoat --access-mode restricted`) with a startup library scan of locally generated FFmpeg media. A paired device credential is exchanged for a cookie session. The Topcoat pages, the bundled player bridge and Demuxe 1.1.0 (browser-only deployment) all run. No mock service. | Client ↔ v2 playback/viewing/delivery/stream API behavior in real engines. |
| Server conformance (`tests/v2_playback.rs`) | The production router over real SQLite, scans and FFmpeg output, in process. | Route authorization, idempotency, generations, restart fencing. The HLS segment's audio track is probed with ffprobe. |
| Fixture/unit tests | `packages/**/*.test.mjs` (stubbed transport), `crates/motion-ui` compose tests (mock facade), and `apps/desktop/proof` (mock `proof_server`). | Client coordinators and rendering in isolation. They are not backend evidence. |
| Model checks | `crates/core/tests/playback_session_model.rs` (Stateless). | Plan admission, delivery ownership and replan rules over 115,456 states. |

## Engines

| Engine | Driver | Result |
|---|---|---|
| Chromium 156.0.8078.4 | Playwright 1.64 | 23 pass, 2 blocked, 0 fail |
| WebKit 27.2 | Playwright 1.64 | 23 pass, 2 blocked, 0 fail |
| Electron 44.7.0 (Chromium 152) | Electron main-process adapter: Playwright's launcher flags are rejected by Electron 44. The renderer uses the production sandbox/partition/same-origin settings. | 23 pass, 2 blocked, 0 fail |

The connection chrome and OS credential storage of `apps/desktop/src/main.mjs` are
not part of this run (see `apps/desktop/test/shell-smoke.mjs`).

## Checked per engine

- Item page renders real viewing state ("Not started", "Stopped at", Resume).
- **Original playback:** the plan chooses `http_range`/`original`, and the bytes
  are served as 206 ranges from `/api/v2/media/files/{id}/content`.
- **Seek:** a `seek` change stages generation 2 at 40 s, which is activated and
  replaces the player at logical 40 s.
- **Progress:** consecutive viewing events are accepted, and the server position
  matches what was played.
- **Resume:** reopening from the item page starts at the saved position.
- **Track switching:** the audio selector lists the planned version's tracks.
  Choosing the second track replans the same delivery to a live HLS conversion
  (`video_transcode`, `a1`), and the server serves master/variant/init/segments.
  The integration test probes the delivered segment as the 22.05 kHz second track.
- **Outage and restart:**
  - An event sent while the server is down is queued in the durable outbox.
  - After restart, that same `event_id`/sequence is accepted.
  - The player reopens at the durable position and a new viewing session starts.
  - One 409 is expected when the previous page's unload event arrives after
    render; the client adopts it only when it is its own unchanged session.
- **Leaving:** leaving the player retires that player's own delivery, which is
  gone within 5 s against a 30 s lease.
- **Home:** continue watching lists the title.
- **Conversion-only title:** an MPEG-2/AC-3 Matroska title is planned as a live
  HLS conversion.

## Blocked: Demuxe live HLS

Two checks are reported as **blocked**, never as passed. Each records its evidence:

- the in-player switch to the HLS conversion
- the conversion-only title playing in the Motion player

Demuxe's browser-only deployment opens HLS natively only when the browser reports
a finite (VOD) duration. A live, rolling conversion needs Demuxe's Shaka backend,
which is not deployed. Demuxe reports: "Native manifest has no finite VOD
duration; live playback requires explicit Shaka live permission".

The client handles this as designed. A failed switch keeps the original player
playing at the same position. The conversion-only title fails with that cause
instead of a generic error.

The server does not label a rolling window as VOD (core `media_playlist`). Closing
this gap needs a packaged Demuxe deployment with its Shaka backend (A14
packaging/licensing), or a Demuxe change. An earlier WebKit run did complete the
in-player switch, so WebKit's reported duration is timing-dependent.

## Diagnostic only (not the product path)

The same authorized conversion was also played in a plain `<video>`:

- **WebKit:** plays the server's live HLS.
- **Chromium 156 and Electron 44:** fail live with `DEMUXER_ERROR_COULD_NOT_PARSE`.
  A static copy of the same server playlists and segments plays in Chromium,
  including without `#EXT-X-ENDLIST`. The failure therefore depends on live
  serving (for example a short, growing playlist), not on the segment bytes.
  This is unresolved and needs investigation before Chromium native HLS is relied on.

## Reproduce

```sh
cargo build -p playscale
python3 scripts/install_demuxe.py <demuxe-1.1.0.tgz>                       # web/vendor/demuxe
npm ci --prefix apps/desktop                                              # Electron 44.7.0
npm i --prefix <tool-dir> playwright@1.64
MOTION_PLAYWRIGHT_DIR=<tool-dir> node scripts/client_playback_e2e.mjs \
  --engines chromium,webkit,electron --out <new empty dir>
```

The run needs FFmpeg/FFprobe on `PATH`. It is not CI-ready (browsers, Electron,
real media). The receipt records the server binary's SHA-256, the Demuxe version,
the media hashes, every check with its detail, and `result`, which is one of
`qualified`, `pass-with-blocked-gaps` or `fail`.

## Remaining gaps

- **Subtitles:** a subtitle selection is planned as blocked
  (`subtitle_delivery_unavailable`). No client sidecar rendering exists, and the v2
  delivery contract has no sidecar field.
- **Live HLS in the Motion player:** the Demuxe/Shaka gap above. Chromium native
  live-HLS parsing also remains unresolved.
- **Prepared renditions:** not offered by the v2 planner yet. Conversion is live
  only.
- **Restart outbox reconciliation:** outbox records left from before a restart
  are retained and not replayed. That is the A11 design, which needs a server
  restore/runtime signal before cross-runtime replay can be safe. The banner
  shown in that case is informational.
- **Server items (moving to the playback API workstream):**
  - timeline-keyed viewing with a real manual epoch and stored event IDs
  - `PUT viewing-sessions/{id}/delivery`
  - ticket auth on stream routes
  - the transactional and authorization review findings recorded in that
    workstream's PR
- **Other platforms:** Windows, Linux, physical devices, installed/signed packages
  and long-run load have not been run.
