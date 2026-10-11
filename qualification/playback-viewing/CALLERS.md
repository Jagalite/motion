# Playback/viewing caller cutover contract

Every viewing command carries `X-Motion-Viewing-Epoch: server_id:restore_epoch`
from the observation that created it. A process `server_epoch` change does not
invalidate history. A restore-epoch change does: clients reconcile or reload,
never replace an old outbox's header with a freshly fetched value.

| Caller | Required behavior / current disposition |
|---|---|
| Topcoat player bridge | Rendered epoch on session admission, rebind, event and watched commands. Outbox ownership uses the same identity. Playback-only heartbeat reports ordered active-generation playheads; pending-generation renewal carries no predecessor position. |
| Electron host | Uses the same bridge and persisted outbox protocol; native installed-package acceptance remains with clients. |
| `v2_playback_smoke.py` | Captures capabilities before writes; retains that history identity across SIGKILL/restart. Public preparation setup must succeed; no legacy provisioning fallback. |
| `operations_smoke.py` | Captures epoch before manual watched write; restored server rejects the old identity and accepts a fresh observed command after reconciliation. |
| `client_playback_e2e.mjs` | Production bridge writes history; API helper reads history. Diagnostic plain-video heartbeat is lease-only and is not product pacing evidence. |
| Go CLI/TUI | No direct viewing writer; launches the authenticated Topcoat player. Capability negotiation and browser profile scope require real-server acceptance. |
| Upgrade harnesses | Only the baseline binary writes v1 progress. Candidate observations use canonical v2 history. Retain these historical writes and migration readers. |

Generation responses now include nullable `subtitles_url`. The bridge consumes
selected original-file WebVTT; other clients must attach it or explicitly refuse
a selected subtitle. Unpinned automatic forced/foreign-audio selection is
explicitly unavailable when embedded captions are observed; `off`, an explicit
track/component, or `require` choosing the first embedded track are supported. An HLS ticket is propagated to every manifest child URL.
Ticket-only players still require an authenticated controller for heartbeats and
changes. A ticket is not a lease renewal capability.

## Legacy writer removal gate

Clients/integration must compose and accept the Topcoat product-entry cutover
before removing `save_progress`, `viewing::watched`, `viewing::start`,
`viewing::event` and legacy preference writer mounts in `src/api.rs`.
The legacy default web UI still calls these routes. The composed clients lane has migrated `scripts/smoke.py` and
`viewing_smoke.py` to v2, including observed epochs and restart outbox checks.
Client commits `a371793` and `f883f19` also migrate current-server package and
reliability viewing assertions to public v2, including observed epochs. These
callers are composed here; the legacy default UI remains the retirement blocker. The old-binary portions of
`upgrade_restore_check.py` and `client_playback_upgrade.mjs` are the historical
allowlist, not current-server exceptions.

After cutover, assert anonymous and authenticated legacy GET/PUT/POST requests
return 404/410 (never HTML success), and snapshot legacy plus canonical history
tables to prove attempted writes have no effect. Keep legacy tables, attribution
receipts and upgrade readers. Their removal is a separate migration decision.
