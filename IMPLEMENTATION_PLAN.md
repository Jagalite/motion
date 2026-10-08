# Playscale core implementation

Catabolic is a design reference, not a runtime dependency. Playscale owns its Rust domain model, SQLite schema, public API, and job lifecycle. Demuxe owns playback; Stateless tests the same reducers the server executes.

## First deliverable

A local server that registers media folders, executes durable scans, exposes catalog and job APIs with OpenAPI, serves original files through a minimal Demuxe interface, and persists viewing progress. Native FFprobe supplies technical metadata. Server transcoding, remote metadata matching, TV naming inference, casting, and the wider roadmap follow this deliverable.

## Model and boundaries

- Content items have stable IDs and a kind/title; an edition belongs to an item; a media file belongs to an edition and library. Tracks are technical metadata of that file revision. Initial discovery creates an unclassified video/audio item and an Original edition, without guessing movies versus episodes.
- Libraries retain a canonical root and filesystem identity. Files retain a relative path, content hash, and observed fingerprint. A unique exact-content move inside a library preserves file/item identity; ambiguous duplicates remain separate. Same-path replacements preserve catalog identity but change the revision. No fuzzy identity matching or cross-library merging in this milestone.
- Profile progress belongs to content identity. The first profile is Everyone; API-created selectable profiles are conveniences, not access-control identities. Session updates use monotonically increasing sequence numbers; older sessions cannot overwrite newer progress. Legacy writes remain last-accepted-write-wins only until sessions are used for that profile/item.
- Metadata is stored as per-source contributions with optimistic revisions and external IDs. Local fields override imported fields; conflicting providers remain explicit. Tags merge by normalized identity with provenance; local exclusions suppress imported tags. Scans never overwrite curation. Catabolic can supply contributions through the public API without owning Playscale's schema.
- Pre-existing renditions are registered through the API using cataloged output/source file IDs and exact revisions, external identity, label, and recipe provenance. Registration does not execute a recipe. Playback options expose originals and valid renditions; changes to either revision invalidate the recorded availability. Remote fetching and automatic transcoding remain separate future operations.
- Jobs have explicit attempts and queued/running/cancelling/terminal states. One scanner runs at a time. Persist admission and attempt state before execution; publish results and terminal state in the same SQLite transaction. Startup requeues interrupted scans and completes interrupted cancellation. A process lock prevents competing server owners.
- Core reducers and byte-range decisions are deterministic. Axum, SQLite, filesystem access, subprocesses, and timers remain adapters. Catalog reads and media bytes do not flow through a global state machine.

## Lessons retained from Catabolic

Incomplete traversal, unreadable roots, changed root identity, cancellation, and failed inspection must not be interpreted as confirmed file disappearance. Successful scans may mark absent paths unavailable without deleting metadata/history. Stage before publishing; reject stale attempts. Keep original files read-only. Derived-media recipes and provenance will be separate records when processing is introduced.

Playscale does not adopt Catabolic's general query language, projections, application integrations, grant engine, or worker deployment. Administration initially uses a protected local token; default viewing requires only access through the configured Tailscale endpoint. Bind HTTP to loopback, validate Host and mutation origins, and keep local paths out of viewer responses.

## Delivery order and acceptance

Future processing will support local FFmpeg and external Catabolic adapters behind one Playscale job/result contract. Requests carry job/attempt IDs, source file and revision, immutable recipe/options, and an idempotency key. External completion may arrive by authenticated callback or polling; accept it only for the active attempt, validate/register the output, then publish readiness transactionally. Duplicate callbacks are idempotent, stale callbacks cannot resurrect cancellation, and external status must distinguish cancellation requested from confirmed. Persist external job identity for reconciliation after restart. Callback authentication, output transfer/storage, and capability negotiation are required before enabling that adapter. Existing rendition registration does not imply processing execution or callback support.

1. Establish the domain schema, migrations, and job reducer; exercise actual reducer transitions through the pinned Stateless snapshot.
2. Implement root registration, bounded scans, FFprobe inspection, atomic reconciliation, cancellation and restart handling.
3. Expose libraries, items/files, profiles/progress, scans/jobs, and binary media through documented public routes. Require admin access for roots and job control.
4. Serve a minimal browser client and a matching Demuxe package/runtime. Provide a reproducible installation workflow and report the exact package tested.
5. Verify API isolation, persistence, source replacements/moves, incomplete scans, cancellation/recovery, byte-range and conditional behavior, and body resource cleanup. Run a real browser viewing/resume loop with generated media.

Acceptance evidence must distinguish unit/model, HTTP, subprocess, and browser checks. Local browser success does not qualify Tailscale transport, NAS failure modes, every codec, or every browser. Record remaining deployment qualification separately. Existing unrelated research files are outside this implementation.

## Catalog API milestone

Implemented explicit logical movie/series/season/episode structure with optimistic
revisions and strict parent/number rules. Scanned items can be classified without
changing file identities or progress. Edition APIs create, rename, and reassign
files within an item. Conventional metadata fields have shared validation; arbitrary
provider extension fields remain available. Artwork imports decode bounded image
bytes, retain source contributions, expose conflicts, and allow immutable local pins.
SQLite migrations preserve the initial core database. The current webpage is
unchanged; automatic matching, cross-item reconciliation, artwork fetching/cleanup,
and richer browsing remain later milestones. See API.md for request contracts.

## Viewing-state API milestone

Viewer state now distinguishes sticky automatic completion from a nullable manual
watched override. Session admission uses a viewing-state revision; ordered events
and exact retries operate through a pure transition policy, with SQLite publication
inside the serialized writer boundary. Superseded sessions and legacy unsequenced
writes cannot overwrite session-managed progress. Manual overrides invalidate
current sessions. Continue-watching and next-episode APIs use effective watched
state; preferences are profile-local persisted hints. Existing progress migrates
without inferring watched status. The bundled UI will adopt these APIs separately.

## Operations milestone

Strict JSON configuration complements CLI overrides. A macOS LaunchAgent runs a
pinned local executable and matching Demuxe assets with persistent local data,
startup at login, and restart after exit. Readiness checks database/worker state;
admin diagnostics expose operational counts and bounded dependency checks. SQLite
online backups carry integrity/hash/schema receipts and restore only to new data
directories. SIGTERM shutdown drains work and bounds runtime waiting on blocked I/O.
Automated retention/scheduling, log rotation, signed packaging, and pre-login
system service deployment remain outside this milestone.


## Processing, events, maintenance, and viewing UI milestone

Implemented fixed local FFmpeg recipes with durable idempotent requests and the
existing production job reducer. A single worker snapshots source bytes, supervises
subprocesses across server death, validates/decodes outputs, and publishes generated
renditions atomically. Software and explicit macOS VideoToolbox H.264 backends are
available; callback dispatch remains a future adapter to the job/result contract.

Persistent, bounded SSE invalidation events accompany committed catalog/viewing/job
writes. Scan schedules survive restarts. Owned cache cleanup respects configured
retention, active processing, and recent playback, without touching originals.

The bundled UI now uses ordered sessions, exact network retries, continue watching,
watched controls, saved language/subtitle preferences, version selection, optional
next-episode autoplay, processing controls, live refresh, and scan schedules. API-only
clients use the same contracts. Broader library browsing, automatic metadata matching,
HDR/burn-in/adaptive streaming, and external dispatch remain separate roadmap work.
