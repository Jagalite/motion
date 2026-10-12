# Lane N3 — Managed downloads and offline reconciliation (M5) ledger

Plan: `MOTION_CONSOLIDATION_AND_COMPLETION_PLAN.md` section 11.4.
Branch `m5-downloads` (PR #31). It was stacked on PR #30 until that merged; `main` (with #22, #28, #29, #30, #32 and #33) is now merged in, and the PR targets `main`.
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

0. The batch's `X-Motion-Viewing-Epoch` must name the current server
   history (`accepts_history_identity`); otherwise nothing is consumed or
   applied (409 `viewing_epoch_changed`, or 428 when missing).
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
| Restored or replaced server | Fenced. Every download pins the manifest's `viewing_epoch` (`server_id:restore_epoch`, read in the same snapshot as the base), and every sync sends it as `X-Motion-Viewing-Epoch`. A mismatch answers 409 `viewing_epoch_changed` before anything is consumed or applied. The device keeps its events, and the agent reports it. An explicit recovery flow (re-basing or discarding retained events after a restore) is not implemented (relates to C08). |
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

## 6. Results (before review round 1; see section 9 for the final run)

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

## 7. Independent review (GPT-6.1 Sol)

Run with `codex exec review --base origin/m3-workflows` (model `gpt-6.1-sol`,
high reasoning effort, read-only sandbox) over the N3 commits only.

**Round 1** (at `afb47a1`) raised two P1 and four P2 findings. All six are
fixed in the round-1 commit:

| # | Finding | Disposition |
|---|---|---|
| S1 (P1) | The agent compacted by the server's sequence watermark, so an event refused as `event_conflict` below it was erased | Fixed. A new core policy, `offline::acknowledged_through`, compacts only the leading run of events the server confirmed by identity; `offline::consumed` classifies results. `motion-ui-host cache acknowledge` takes the server response on stdin and applies it, so the decision is no longer in JavaScript. Covered by core unit tests, `store.rs::a_conflicting_event_is_never_compacted_by_the_watermark`, agent tests, and the reconciliation model, which now compacts through this function. |
| S2 (P1) | Sync accepted events across a restored database when sequences and bases happened to match | Fixed. The manifest returns `viewing_epoch` with the base. The download identity and every event carry it, and `syncOffline` requires `X-Motion-Viewing-Epoch` (428 if missing, 409 `viewing_epoch_changed` on mismatch). This uses the existing `accepts_history_identity` core rule inside the write transaction. The model gained a Restore input; the oracle requires that nothing is consumed or applied from another history and that the device erases nothing. Covered by `tests/v2_downloads.rs` and a real-binary restore in `run-download-workflow.mjs`. |
| S3 (P2) | `current.json` was written only after a complete pass, so one later failing transfer hid a published copy | Fixed. It is written after each publication, and one failing transfer no longer stops the others. |
| S4 (P2) | Staged partial transfers of cancelled or expired demands were never removed | Fixed. `cache status` reports staged transfers, and the agent removes those whose demand is not ready. Store and agent tests cover it. |
| S5 (P2) | The agent killed cache-tool output above 4 MiB while the event log allows 8 MiB | Fixed. The bound is now 16 MiB. |
| S6 (P2) | Originals above the cache's 64 GiB bound were transferred and could never publish | Fixed. `downloads::MAX_MEDIA_BYTES` is shared by core and host. The server refuses larger originals (422 `download_too_large`), and the agent checks before transferring. |

**Round 2** (at `586d2a0`) raised four P2 findings and no P1. All four are
fixed in the round-2 commit:

| # | Finding | Disposition |
|---|---|---|
| S7 (P2) | A refused sync (for example, 404 after a profile left the grant) aborted the pass before cleanup, so no-longer-visible copies stayed playable | Fixed. Reconciliation failures keep the events and are reported, but removal and other profiles proceed. Agent test covers it. |
| S8 (P2) | Compaction removed the events the offline host resumes from before a rebase recorded the server position | Fixed. `Store::acknowledge_results` writes the newest compacted position per download into `resume_position_ms` before compacting, without moving the causal base. Covered by `store.rs::compaction_keeps_the_resume_checkpoint_without_moving_the_base`. |
| S9 (P2) | `EPIPE` on the cache tool's stdin could crash the desktop process | Fixed. A stdin error rejects the operation; the next pass retries. |
| S10 (P2) | A blob verified before a crash was not recovered until space for a second copy was admitted | Fixed. `cache recover` (`Store::recover`) re-verifies and publishes such a blob before any admission or transfer. Store and agent tests cover it. |

**Round 3** (at `7f869d2`) raised four P2 findings and no P1. All four are
fixed in the round-3 commit:

| # | Finding | Disposition |
|---|---|---|
| S11 (P2) | After `downloads:manage` was removed, cached copies stayed openable forever | Fixed. On connect without the permission, or on a 403 from the download list, the agent withdraws every owned copy and staged transfer. Unsynchronized events are kept and nothing is transferred. Agent test covers it. |
| S12 (P2) | Each profile root received the whole quota | Fixed. Admission passes the quota minus the usage of the device's other owned roots, so it is one device budget. The decision is still `offline::admits_transfer`. Agent test covers it. |
| S13 (P2) | The agent's index files were renamed without a directory fsync | Fixed. `current.json` and `roots.json` are synced through their directory after rename. |
| S14 (P2) | Valid catalog titles over 1,024 UTF-8 bytes could never be cached | Fixed. The cached display title is shortened on a character boundary. Agent test covers it. |

**Round 4** (at `41358b5`) raised three P2 findings and no P1. All three are
fixed in the round-4 commit:

| # | Finding | Disposition |
|---|---|---|
| S15 (P2) | Roots kept from other servers or credentials were outside the quota | Fixed. Usage is summed over every managed root on the device, and removal is still limited to owned roots. Agent test covers it. |
| S16 (P2) | A corrupt crash-leftover blob made `recover` fail on every pass | Fixed. `Store::recover` verifies first, discards invalid bytes no published entry references, and returns `false`, so the next transfer proceeds. The real-filesystem test now re-transfers and publishes afterwards. |
| S17 (P2) | A requested version beyond the first 100 candidates, or eligible originals crowded out by unsupported ones, were refused | Fixed. The requested version is filtered in SQL before the bound, and whole single-part available originals sort first within a 500-row window. A server test covers an explicit version. The ordering ranks by structure and availability only, so more than 500 structurally eligible originals that are unreadable or in unsupported containers could still hide a later eligible one. That case is untested and is left as a known bound. |

**Round 5** (at `05b18fe`) raised two P2 findings and no P1. Both are fixed
in the round-5 commit:

| # | Finding | Disposition |
|---|---|---|
| S18 (P2) | A failed rebase aborted the pass before later profiles' cleanup | Fixed. Rebase failures are reported per profile and never stop removal elsewhere. |
| S19 (P2) | For timelines without a curated duration, reconciliation dropped the probed duration that authorized the download, so completion and position checks were skipped | Fixed. The adapter supplies the effective duration: the timeline's, or else the one this device's download was pinned with. Covered by `tests/v2_downloads.rs::reconciliation_uses_the_downloads_effective_duration`. |

**Round 6** (at `553d9c7`) raised one P2 finding and no P1. It is fixed in
the round-6 commit:

| # | Finding | Disposition |
|---|---|---|
| S20 (P2) | The duration fallback used the device's newest download for the timeline, not the copy that produced the event | Fixed. Every offline event carries its `download_id` (a contract addition to `OfflineEvent`). The adapter supplies that download's pinned duration, and only for the same device, profile and timeline, to the core decision. The core event and the model carry the field too. The server test now holds two demands with different durations and checks that the played one decides. |

**Round 7** (at `3f087bf`) raised three P2 findings and no P1. All three are
fixed in the round-7 commit:

| # | Finding | Disposition |
|---|---|---|
| S21 (P2) | Admission could store a title the offline host could then not snapshot for playback | Fixed. Admission headroom is the larger of the configured headroom and the largest saved or incoming title plus the host's 512 MiB reserve. The decision is still `offline::admits_transfer`. Agent test covers it. |
| S22 (P2) | Blobs orphaned by a crash whose demand then ended were never collected | Fixed. `cache collect` runs after every still-ready demand had its recovery or transfer; it is skipped when a transfer failed, so a recoverable blob is not discarded. Store and agent tests cover it. |
| S23 (P2) | The Downloads page showed only the first 200 records | Fixed. It reads every page through the bounded `list_all` (unavailable past 5,000 rather than partial) and lists only actionable (non-cancelled) records. `tests/v2_downloads.rs::the_downloads_page_lists_every_actionable_record` renders 205 records through the real presentation. |

**Round 8** (at `8edc6b1`) raised two P2 findings and no P1. Both are fixed
in the round-8 commit:

| # | Finding | Disposition |
|---|---|---|
| S24 (P2) | `Store::publish` and `Store::remove` collected every unreferenced blob, deleting another ready demand's recoverable leftover before its recovery ran | Fixed. They now release only the blob they themselves stopped referencing; global collection happens only through `cache collect` after recovery. `store.rs::crash_leftovers_are_recovered_or_collected` covers remove and publish leaving a pending blob, then recovery, then collection. |
| S25 (P2) | Without `downloads:manage` at connect, the agent never started, even after a later grant | Fixed. Passes always run; a 403 from the download list withdraws local copies, and a later grant starts transfers on the next pass. |

**Round 9** (at `1472817`) raised one P2 finding and no P1. It is fixed in
the round-9 commit:

| # | Finding | Disposition |
|---|---|---|
| S26 (P2) | The snapshot reserve was admitted on the cache volume, but the host created playback snapshots in the system temporary directory, possibly on another volume | Fixed. The host creates its anonymous playback snapshot inside the cache directory, so admission and snapshot use the same filesystem. The host unit test checks that the snapshot is anonymous in the given directory. |

**Round 10** (at `4804aa4`, reviewed against `main` after the merge) raised
three P2 findings and no P1. All three are fixed in the round-10 commit:

| # | Finding | Disposition |
|---|---|---|
| S27 (P2) | On a timeline without a curated duration, an event naming an unknown or foreign download had no effective duration, so position checks were skipped | Fixed. Such an event has no target and is rejected (`not_found`, consuming its sequence). `tests/v2_downloads.rs` covers a foreign download ID. |
| S28 (P2) | Recoverable crash-leftover blobs were not counted against the quota | Fixed. `Store::usage` counts every blob on disk once, plus staged bytes. A store test covers a leftover blocking admission. |
| S29 (P2) | A media response that stopped producing bytes blocked the agent indefinitely | Fixed. An inactivity deadline (60 s, reset per chunk; not a total timeout) covers headers and body. A stalled transfer fails alone and resumes from its staged bytes next pass. Agent test covers it. |

## 8. Deferred release checks

| Check | Exact fixture | Owner |
|---|---|---|
| Electron agent inside the desktop host against a remote HTTPS server, then Demuxe playback of the agent-published root (not a synthetic root) | `run-download-workflow.mjs` media (12 s H.264/AAC MP4, 320x180) published by the agent; `run-offline.mjs` driven against `currentRoot(offline-cache)` | A11/A12 with A13 |
| Installed-package run: packaged `motion-ui-host` as the cache tool, `userData/offline-cache` roots | Release package bytes from A14; same fixture as above | A14 (release) with A13 |
| Large-file and long-duration transfer, interrupted network, resume across app restart | ≥ 4 GiB MP4 original on a remote server; network cut at ~50 % twice; app quit mid-transfer | A13 |
| Physical out-of-space during publication | Volume with free space smaller than the media plus 1 GiB headroom | A13 |
| Multiple accounts, profiles and servers on one device with an offline root picker | Two servers × two profiles, one download each | A11/A12 (picker UI) with A13 |
| Restored or replaced server **recovery flow** (the fence itself is implemented and tested) | Backup taken before an offline session, restored after it; device reconnects and the user chooses to discard or re-base | A02/A07 (C08) with A13 |
| Server retention of `offline_events` | 10,000+ events per device and profile | A13 with A02 |
| Device transfer-state reporting (`transferring`, `verified_local`, `failed`) | Requires a new contract operation | A00 (contract) with A13 |
