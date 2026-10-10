# A11/A12 implementation and qualification status

Branch `web-desktop-a11-a12`, starting at `1e1af2a`, now includes the A11 Topcoat
presentation/player bridge, A12 native lifecycle, and the presentation-only offline
helper. Committed API/catalog integration was merged from `api-terminal-a08-a10`
at `19d997b`; no unfinished sibling-worktree files were imported.

A11/A12 client implementation and the available local qualification are recorded
below. **Full production playback and release acceptance are not complete.** The
[acceptance matrix](../TOPCOAT_ACCEPTANCE.md) names each external integration and
physical/package gate instead of counting mocks as backend completion.

## Delivered

- Authorized, escaped Topcoat screens with strict CSP and content-hashed external
  assets. Real catalog home/library/search/item pages, sources/library management,
  matches, processing and diagnostics use the authenticated production v2 adapter.
  Missing preferences, viewing, continue/next, scan and playback services display
  unavailable; they never substitute mock data or false empty viewing history.
- Full player controls and bounded generation preparation/activation, exact
  command retries, viewing outbox order, lease renewal, stale-owner fencing,
  quality/version changes, logical seek and controlled close. Subtitle preferences
  map to the planning vocabulary; HLS is advertised only with native support.
  Next-title navigation flushes/retire first; autoplay denial retains manual Play.
- Native connection identity/epoch/contract checks, sandboxed content without
  Node/preload/IPC, encrypted credentials/outbox, and partition/principal isolation.
  A script-free recovery origin restores pending records before server scripts run.
  Save failure keeps the window open and preserves the browser copy.
- Local server ownership via private inherited bootstrap/readiness pipes, exclusive
  data-directory locking and shutdown of only the child actually spawned. Stopping
  a local server does not detach an unrelated remote/offline view.
- Explicit Downloads mode starts `motion-ui-host`, sharing Topcoat offline views
  through `OfflinePresentationReader`. It has no authoritative database, scanner,
  encoder, source-root or public v2 API access. Parent lifetime loss stops it.
  Cache reads are confined; media is checksum-verified into anonymous immutable snapshots
  reclaimed on process exit;
  only exact managed tickets and verified player assets are served.
- Scoped, revision-pinned offline events with consecutive decimal-u64 sequences,
  exact duplicate handling and valid rewinds. Atomic log publication is fsynced
  before acknowledgement. Controlled offline close waits for final persistence.
- Nine screens checked at narrow/wide widths, accessibility-tree names, labels,
  reduced motion and keyboard skip behavior. Processing tables scroll within the
  page instead of overflowing the viewport. Concurrent SSR/range checks are recorded.

## Correctness boundary

Offline event identity, causal pins, sequence admission, duplicate conflicts and
rewinds are production pure policies in `crates/core/src/offline.rs`. Stateless
executes that policy over 1,000 cases/50,000 transitions with a four-record bound,
checking exact resulting events/counts, acknowledgement decisions, no replacement
of prior history and bounded consecutive sequences. Inputs include forward/rewind,
exact/altered duplicates, gaps, foreign profile, stale manifest facts and restart.
A mutation regression verifies that required acceptance cannot silently disappear.

Filesystem containment, atomic rename/fsync, private pipes, timeouts, renderer
ownership and browser API observations remain adapters. Real cache tests verify
publication failure, concurrent retries, restart, corruption and path containment;
native tests verify actual process lifetimes. Server authority is never inferred
from a restored online outbox or an offline log.

## Current evidence

- Focused JavaScript: 62 passing tests covering production playback/bridge/native
  adapters and crash-recovery ordering.
- Rust presentation: 16 composition tests; offline cache: five integration tests plus an anonymous-snapshot/concurrent-range test;
  offline core: two unit tests and two Stateless/mutation tests.
- Real API integration: 32 passing access/API tests on the final production
  source, including catalog
  identity/escaping/privacy, unavailable-state rendering, source/library
  authorization and immutable static asset routing.
- `topcoat-electron-proof-darwin-arm64.json`: 26 passing checks with exact installed
  Demuxe bytes, real playback/seek/replacement and 24 concurrent HTML requests plus
  a range read. It also records HTML byte sizes and a playback process CPU/memory
  snapshot; these are observations, not sustained resource budgets.
  Viewing/delivery/scan replies are fixture services.
- `native-shell-darwin-arm64.json`: nine checks, including encryption/reconnect,
  runtime fencing, unprivileged content and independent-server survival.
- `offline-cold-start.json`: nine checks, real helper and production shell, playback,
  seek, persisted progress, restart resume, no canonical DB and parent-pipe exit.
- `owned-server.json`: five checks for real server private bootstrap, lock, authenticated Topcoat and
  assets, API fallback and restart identity. All storage is disposable and isolated.

The broad workspace run exposed a test-setup race in catalog concurrency coverage:
the first writer could commit before the second preview. Both previews now precede
a synchronized writer start; the regression asserts one success, explicit stale
revision rejection and consistent stored state. All nine persistence tests pass
after the correction. The complete workspace run recorded 289 passing tests and
that one test failure; all access, delivery, identity, offline, processing, viewing
and work models passed. Its final helper doctest used the pre-change dependency
list while the helper fix was being completed; a fresh workspace doctest run passes.
Final affected runs cover 32 access tests, nine persistence tests, six helper tests
and the snapshot test. Warnings-denied workspace Clippy and formatting pass.
See `verification.json` for log hashes, source hashes and the exact rerun boundary;
a single clean final-source workspace invocation is not claimed.

Receipts identify source/binary hashes and limitations. `latest-player-attempt.json`
is an earlier failed diagnostic attempt, superseded by the passing proof receipt.
The native runtime and Rust test executables were staged byte-for-byte internally
when external-volume launches stalled; no installed application was replaced.

## Remaining integration and release gates

1. Production v2 preferences, viewing, continue/next, planning and delivery are now
   integrated and exercised against the real server, in
   `qualification/client-playback/README.md`. Still open:
   - live HLS in the Motion player (needs the Demuxe Shaka backend)
   - subtitles
   - prepared renditions
   - cross-runtime outbox replay
   - scans
   - queue/next qualification
2. A13 production download/cache publishing, multipart/sidecar assets, transfer
   resume/quotas, acknowledged log compaction and causal server reconciliation.
   The current cache protocol and its bounds are in `crates/motion-ui-host/README.md`.
3. Physical screen reader/input and audible A/V/color/HDR checks; sustained
   production catalog/render load and other platform targets.
4. A14 installed/signed package, update and rollback qualification.

Abrupt crashes may lose observations not yet acknowledged. An online desktop-owned
server restarting on a new port cannot automatically recover browser-only records
written after the last native checkpoint at the previous origin. Offline records
already acknowledged by the helper survive restart; no remote-erasure claim is made.

## Reproduction

```sh
node --test packages/playback/*.test.mjs packages/ui-bridge/*.test.mjs apps/desktop/test/*.test.mjs
cargo test --workspace --no-fail-fast
cargo clippy --workspace --all-targets -- -D warnings
node apps/desktop/proof/run.mjs
node apps/desktop/test/run-shell.mjs
node apps/desktop/test/run-offline.mjs
node apps/desktop/test/server-owned.mjs
```

Use pinned Rust 1.98 and Electron 44.7.0. Set `MOTION_DEMUXE_DIR` to the verified
installed player, `MOTION_SERVER_BINARY` to the built server, and optionally
`MOTION_ELECTRON_BINARY` to an identical staged runtime. Offline runner supports
`MOTION_CARGO_BINARY`. `CARGO_HOME`/`CARGO_TARGET_DIR` can isolate build locks;
`scripts/run-rust-test-local.py` is an opt-in Cargo target runner for stalled
external-volume executables. These overrides are development verification aids.

## PR review follow-up (2026-10-10)

Review fixed a viewing retry that could bypass failed local persistence, enforced
full decimal-u64 bounds, refreshed offline playback after history restoration, and
kept the snapshot admission lock owned by the blocking worker after HTTP cancellation.
These are adapter durability/resource-lifetime rules; canonical viewing authority
and offline event admission remain production core policies. Regression tests cover
repeated persistence failures, stable retry identity, exhausted sequences and restored
documents. The existing cache tests exercise snapshots, ranges and durable events;
HTTP cancellation during a large snapshot is not separately fault-injected.

Fresh checks on the fix commit: 62 JavaScript tests, 16 presentation tests and six
helper tests, presentation/helper doctests, formatting, and warnings-denied Clippy
for both affected Rust packages pass. Native offline cold-start qualification was
rerun successfully (nine checks). Earlier online/native receipts remain historical;
this follow-up does not claim a fresh whole-workspace or physical-device campaign.
See `review-verification.json` for source and log hashes. Remaining gates above apply.
