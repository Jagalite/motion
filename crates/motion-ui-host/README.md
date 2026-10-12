# Offline presentation host (A11/A12)

`motion-ui-host` is an explicitly selected desktop cache process. It has no
SQLite, authoritative server, scanner, matching, migration or encoder dependency.
The desktop launches it only from **Open downloads offline**, authenticates over
private inherited pipes, and embeds its loopback origin in an unprivileged view.
Leaving offline mode stops the child; losing the parent lifetime pipe exits the
helper. An unreachable remote server never starts it automatically.

## Cache port

`motion_ui::offline::OfflinePresentationReader` supplies the shared Topcoat views.
The included file adapter consumes A13-style cache fixtures through this internal
protocol version 1. This is an integration port, not a new public `/api/v2` API or
a completed A13 download/reconciliation service.

A cache root contains `manifest.json`, `blobs/<lowercase SHA-256>`, `staging/`,
and the helper's `events.lock` and `events.json`. One root identifies one server,
principal, profile and device. Every writer holds the exclusive `events.lock`, so
the cache writer never runs beside the presentation host or another writer.
`events.json` is `{"base_sequence":"N","events":[...]}`: consecutive events after
the highest sequence the server acknowledged. A log written before compaction
existed is a bare event array and reads as base 0. Device sequences are never
reset or renumbered. The log stops at 10,000 retained events/8 MiB until the
server acknowledges and the device compacts them.

The manifest is the serialized `CacheManifest` in `src/lib.rs`:

```json
{
  "protocol": 1,
  "scope": {"server_id":"server","principal_id":"principal","profile_id":"profile","device_id":"device"},
  "downloads": [{
    "identity": {"download_id":"download","timeline_id":"timeline","timeline_revision":"1","source_revision":"source-revision","base_viewing_revision":"4","base_manual_epoch":"2"},
    "title":"A saved title",
    "duration_ms":12000,
    "sha256":"<64 lowercase hex characters>",
    "size":123456,
    "content_type":"video/mp4"
  }]
}
```

Paths never come from URLs or manifest filenames. Capability-relative reads
confine blobs and metadata to the cache. Before serving media, the helper copies
and hashes it into a private snapshot; later cache mutation cannot change an
admitted stream. Snapshots are created on the cache's own volume (where download admission reserves room for them) and unlinked immediately and reclaimed by the OS even
on abrupt process exit. Concurrent range bodies keep independent cursors through
bounded, serialized seek/read operations off the async executor. A random process-local ticket and HttpOnly session protect
range reads. Only receipt-listed, checksum-verified player assets are served.
The host rejects public catalog routes, wrong Host/Origin, unauthenticated reads,
bootstrap replay, foreign event scopes and altered duplicate event identities.

Entries may carry `resume_position_ms`: the server position recorded when the
download was last rebased, used after acknowledged events were compacted.

## Cache writer

`motion-ui-host cache --cache-dir ROOT --scope JSON <command>` is the one-shot
writer used by the desktop download agent (`apps/desktop/src/downloads.mjs`). It
prints one JSON result; a failure prints `{"error": ...}` and exits non-zero.

| Command | Effect |
|---|---|
| `status` | manifest, byte usage, retained event count and next sequence |
| `admit --incoming --quota --headroom` | whether a transfer fits the quota and leaves free headroom (`offline::admits_transfer`) |
| `publish` (entry on stdin) | verify `staging/<download_id>.part` size and SHA-256, rename it into `blobs/`, fsync, then replace the manifest; mismatches discard the staged bytes |
| `remove --download-id` | drop the entry and staged bytes; collect unreferenced blobs |
| `events` | the retained log |
| `acknowledge --through N` | compact through the server's `next_device_sequence - 1` (`offline::compact`); refuses acknowledgements beyond the local log |
| `rebase --download-id --viewing-revision --manual-epoch [--resume-position-ms]` | pin later events to the server state read after reconciliation |

Publication order is blob rename and directory fsync, then manifest replacement
and directory fsync. A crash between them leaves an unreferenced blob: the next
publication of that download verifies it in place, and any later write collects
blobs no entry references. Repeating a completed publication is a no-op.
`crates/motion-ui-host/tests/store.rs` covers these on a real filesystem,
including a write failure (read-only `blobs/`) that leaves the manifest unchanged.
Out-of-space is enforced by `admit` before a transfer starts; a physical ENOSPC
during publication is a write failure of the same kind but is not separately
exercised.

Limits: 500 manifest entries, 2 MiB manifest, 64 GiB per media snapshot, 256 MiB
verified player assets, and one admitted snapshot per helper. Snapshot admission
requires 512 MiB free headroom and a second copy of the selected media. This
adapter supports the listed complete MP4/WebM/audio files; multipart media and
subtitle/artwork sidecars are refused by the server and reported by the agent. No cached data grants server viewing
authority, and disconnected copies cannot be remotely erased.

## Correctness and persistence

`crates/core/src/offline.rs` decides identity, pinned causal facts, consecutive
device sequences, exact duplicates and valid rewinds. Stateless runs the same
production policy. The adapter serializes append operations, writes a temporary
whole log, fsyncs, renames, fsyncs the parent and then acknowledges. Restart
validates the retained sequence; failures never silently rebase events.

The browser queues observations every five seconds and on status changes. A
controlled close must finish the final local append before the native host stops.
An unexpected crash can lose observations not yet acknowledged; acknowledged
records survive. Delivery to the authoritative server is the desktop agent's
job (`POST /api/v2/offline/sync`); the server decides `applied`, `history_only`,
`duplicate` and `rejected` in `playscale_core::downloads::reconcile`.

## Development and verification

```sh
cargo test -p playscale-core --test offline_model -- --nocapture
cargo test -p motion-ui-host --test cache --test store
cargo test -p motion-ui
node --test apps/desktop/test/*.test.mjs
node apps/desktop/test/run-offline.mjs
```

The native runner builds the helper, generates a synthetic media/cache fixture,
and tests real Demuxe playback, seek, durable progress, restart resume, shutdown
and parent-pipe loss. Set `MOTION_DEMUXE_DIR` to a verified installed player tree.
For development launches, `MOTION_UI_HOST_BINARY` and `MOTION_CACHE_DIR` override
the helper and cache paths. Packaged builds ignore these overrides and use
resource/userData paths. Current inherited-pipe transport is Unix-only; packaging
and a Windows pipe adapter remain separate qualification gates.

Use `MOTION_CARGO_BINARY` to select the pinned Cargo executable and
`MOTION_ELECTRON_BINARY` for a byte-identical runtime staged internally when an
external volume stalls. `CARGO_TARGET_DIR` can isolate build artifacts; the
optional `scripts/run-rust-test-local.py` Cargo runner stages test bytes with a
verified SHA-256. Native receipts are under `qualification/desktop` and distinguish
cache fixtures, real helper behavior and production server evidence.
