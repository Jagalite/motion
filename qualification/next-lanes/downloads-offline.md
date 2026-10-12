# Lane N3 — Managed downloads and offline reconciliation (M5) ledger

Plan: `MOTION_CONSOLIDATION_AND_COMPLETION_PLAN.md` section 11.4.
Worktree `/Volumes/seed1/Projects/motion-m5-downloads`, branch `m5-downloads`.
Starting SHA: `c6d6e266c76f0ea7ec1274bca10e30f9b4bdf658` (origin/main, 2026-10-11).

This ledger separates implementation, missing wiring, unsupported cases,
focused checks, review findings and deferred release checks. "Works" means the
real code path was exercised as stated in section 4. It is not release
qualification.

## 1. Starting inventory

| Piece | At the starting SHA |
|---|---|
| `crates/core/src/offline.rs` | Client event admission only: consecutive device sequence, exact duplicates, pinned causal identity. No compaction. |
| `crates/motion-ui-host` | Presentation-only cache host over a hand-seeded manifest; durable local event log; verified snapshot playback. No writer, transfer, quota or removal. |
| Contract `offline` tag | `createDownload`, `listDownloads`, `getDownload`, `deleteDownload`, `getDownloadManifest`, `syncOffline` (all `implementation_target`, none served). |
| `issueContentAccess` | Served. The `download_id` family returned 404. File-purpose `download` tickets worked for exact revisions. |
| Desktop | "Open downloads offline" launched the host on a single fixed cache directory. Nothing filled the cache. |

## 2. Ownership split (frozen for this slice)

- **Server** (`playscale_core::downloads`, `src/v2/downloads.rs`). It authorizes
  a download for the caller's own device and pins one exact timeline revision,
  version, original file and SHA-256 content revision. The state is `ready` or
  `cancelled` as stored; `expired` is derived on every read and never stored. It
  is derived when the timeline revision, version binding or file revision
  changed, or the caller can no longer read it. Byte grants exist only for a
  ready download, and cancelling revokes them. The server reconciles offline
  events into the canonical timeline viewing state.
- **Device** (`motion-ui-host cache`, `apps/desktop/src/downloads.mjs`). It owns
  transfer staging and resume, quota and headroom, SHA-256 verification,
  atomic publication, local removal, the device event log, acknowledged-event
  compaction and rebasing.
- The server keeps no transfer progress. `preparing`, `transferring`,
  `verified_local` and `failed` from the contract enum are **not produced** by
  this slice; see section 3.

### Reconciliation rule (core `downloads::reconcile`)

Sequences are per device and profile. Exactly one cache root maps to that pair,
because the root scope is server, principal, profile and device.

1. A recorded event with the same sequence or ID: an identical retry is
   `duplicate`; it reports the original outcome in `code` and changes nothing.
   Different content is `rejected/event_conflict` and consumes nothing.
2. A sequence other than the next one is `rejected/sequence_gap` and consumes nothing.
3. Every other event consumes its sequence exactly once:
   - invalid, or an unreadable or missing timeline: `rejected` (`invalid_event`, `not_found`);
   - outside the timeline duration, or `ended` too early: `rejected/invalid_position`;
   - timeline revision changed since the download: `history_only/timeline_changed`;
   - a manual watched change after the base: `history_only/manual_override_newer`;
   - another session or device advanced viewing after the base, and this
     device's own applied chain is not the current revision:
     `history_only/viewing_newer`;
   - otherwise `applied`. The view revision advances, the position is set,
     automatic completion is sticky, and any open session is superseded. The
     device's lineage (base, produced revision) is recorded so its next event
     can continue the chain.
4. An unusable profile makes the whole batch return 404 and consumes nothing,
   so the device keeps its log.

After a complete sync the agent reads the current viewing state and rebases
the download's base. Later offline events then apply on top of whatever the
server held at reconnect.

## 3. Missing or explicitly unsupported

Nothing is silently substituted. Each refusal has a stable code.

| Case | Behaviour |
|---|---|
| Prepared renditions (`recipe_id`) | 422 `download_preparation_unavailable` |
| Component/track pins, `subtitle_policy: require` | 422 `download_tracks_unavailable`. The whole original, with embedded tracks, is downloaded. |
| Multipart or interval versions | 422 `download_multipart_unavailable` |
| Containers other than MP4/M4V/WebM/M4A/MP3/Ogg | 422 `download_container_unavailable` |
| Unknown duration (no timeline duration and no probed duration) | 422 `download_duration_unknown` |
| Content changed but not yet reviewed into the version | 404. No revision can be pinned until the replacement is confirmed. |
| Subtitle, artwork or metadata sidecar files | Not produced by the server. If a manifest lists them, the agent reports "needs files this device cannot save yet" and transfers nothing. |
| Server transfer states (`preparing`, `transferring`, `verified_local`, `failed`) | Not produced. Reporting device progress needs a new contract operation (A00 review). The Downloads page states that progress is device-owned. |
| Offline-mode removal | Removal happens on the next online pass, after a server cancel. There is no remove button in the offline host. |
| Choosing between several cache roots offline | The desktop opens the most recently synchronized root (`current.json`). Other accounts, profiles and servers keep separate roots but have no picker yet. |
| Restored or replaced server | A restored server's sequence row is behind the device. Sync then answers `sequence_gap` or a foreign acknowledgement, which the cache refuses. Nothing is applied or erased, and the agent reports it. An explicit recovery flow is not implemented (relates to C08). |
| Server-side retention of `offline_events` | Unbounded per device and profile. Compaction or retention policy is deferred. |
| Windows | The inherited-pipe host transport is Unix-only (unchanged). |

## 4. Focused checks

Results recorded in section 6.

| Check | What it exercises |
|---|---|
| `crates/core` unit tests (`downloads`, `offline`) | choice and refusal order, derived expiry, idempotent cancel, reconciliation outcomes, compaction, quota |
| `crates/core/tests/downloads_model.rs` (Stateless, exhaustive) | production `offline::admit/compact` and `downloads::reconcile`, interleaved with online writes, a manual override, a timeline edit, lost responses, altered retries, gaps and rebases; outcomes checked against an independent "who touched it since the base" oracle |
| `crates/core/tests/offline_model.rs` (Stateless, fuzz) | the extended device log with base, compaction and foreign acknowledgements |
| `tests/v2_downloads.rs` | real router and SQLite: refusals, idempotent create, manifest digest, ticket range resume, device isolation, expiry on content change, cancel revoking grants, and sync outcomes (applied chain, duplicate, conflict, gap, manual override, rebase, invalid position, foreign device, unusable profile, missing timeline) |
| `crates/motion-ui-host/tests/store.rs` | real filesystem: verified/idempotent publication, crash-leftover recovery, blob collection, write failure, quota, scope and lock exclusion with the host, legacy log, compaction, rebase |
| `crates/motion-ui-host/tests/cache.rs` | existing host tests on the versioned log |
| `crates/motion-ui/tests/compose.rs` | Downloads page permission gate, cancel command, item "Save for offline" body |
| `apps/desktop/test/downloads.test.mjs` | agent: resume Range, full-response restart, quota refusal, unsupported manifest, ordered sync and acknowledgement, removal, root selection |
| `packages/ui-bridge/*.test.mjs` | desktop-only reveal; offline player unchanged |
| `apps/desktop/test/run-download-workflow.mjs` | real server, agent, cache writer and host binaries: download, stop server, offline cold start, ranges, offline progress, restart resume, reconnect with a lost response, manual override, cancel |

## 5. Correctness boundary (AGENTS.md)

Core state and transitions hold download choice and refusal, derived expiry,
cancel, transferability, the offline reconciliation rule with lineage,
device-log admission, compaction and the transfer quota. The SQLite adapter
observes facts in one `BEGIN IMMEDIATE` transaction per request. It persists
the event record, sequence row, view, superseded session and lineage
together. Filesystem atomicity (staging, verification, rename, fsync order,
lock exclusion) stays in the cache writer and is checked with real-filesystem
tests. Network sequencing stays in the desktop agent. It makes no correctness
decision: publication, compaction and authority are decided by the cache tool
and the server.

## 6. Results

Run on 2026-10-12, macOS arm64, Rust 1.98.0 (compiled through kyoto, test
binaries executed outside kyoto per its policy), Node 22.

| Check | Result |
|---|---|
| `cargo test -p playscale-core --lib` | 103 passed |
| `downloads_model` (exhaustive) | passed. Graph exhausted at 2,545,062 states and 30,540,744 transitions; bounds: 4 device sequences, 2 online writes, 1 manual override, 1 timeline edit. The mutation test detects stale offline progress overwriting a newer session. |
| `offline_model` (fuzz) | passed. 1,000 cases, 50,000 transitions; mutation tests detect lost events, renumbering and foreign-acknowledgement erasure. |
| `tests/v2_downloads.rs` | 2 passed |
| `tests/contract.rs` (served operations ledger) | 4 passed |
| Regression: `tests/access_api.rs`, `tests/v2_organization_viewing.rs` | 32 and 8 passed |
| `motion-ui-host` lib, `cache` and `store` | 1, 6 and 6 passed |
| `motion-ui` `compose` | 19 passed |
| `node --test apps/desktop/test/*.test.mjs packages/ui-bridge/*.test.mjs` | 60 passed |
| `run-download-workflow.mjs` (real binaries) | passed all 7 checks: transfer verified and published; repeat pass idempotent; offline cold start serves verified ranges; offline restart resumes; reconnect reconciles once despite a lost response and compacts; newer manual state not overwritten; cancel removes the device copy. Receipt: `download-workflow.json`. |
| `cargo clippy --workspace --all-targets` with CI flags | clean |
| `cargo fmt --check`, `scripts/check_contract.py` | clean |

Not run in this lane: the Electron `run-offline.mjs` smoke. The host's
presentation path is unchanged apart from the event-log format, and the smoke
was updated to read either format.

## 7. Deferred release checks

- Electron run of the agent inside the desktop host against a remote HTTPS
  server, followed by Demuxe playback of the agent-published root. The existing
  `run-offline.mjs` proves Demuxe playback from a cache root, but its root is a
  synthetic fixture.
- Installed-package run (packaged `motion-ui-host` used as the cache tool),
  long-duration and large-file (multi-GB) transfer, interrupted network, and a
  physical out-of-space volume.
- Multiple profiles and servers on one device with the root picker.
- Restored-server recovery flow (C08).
- Independent GPT-6.1 Sol review: not available to this session; requested in the PR.
