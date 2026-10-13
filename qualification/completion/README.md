# Motion completion ledger

Single progress ledger for plan section 12
([MOTION_CONSOLIDATION_AND_COMPLETION_PLAN.md](../../MOTION_CONSOLIDATION_AND_COMPLETION_PLAN.md))
and [the single-agent handoff](../../MOTION_SINGLE_AGENT_COMPLETION_HANDOFF.md).
Baseline: main `28f4b32` (2026-10-12). CI on that commit: CI, CI full and
dependency notices passed. No open PRs at the start.

States, kept distinct:

- **Implemented:** code exists on a branch with focused tests.
- **Tested:** the named focused checks passed at the recorded SHA.
- **Merged:** on `main`.
- **Release-qualified:** the exact release artifacts passed the release matrix
  (section 8 of the handoff). Nothing in this ledger is release-qualified yet.

Missing code is "not implemented", never a test deferral. Every unfinished item
names an owner, a dependency and the next concrete step. The owner of every
item below is the single completion agent unless stated otherwise.

## Summary

| Item | State | PR | Next step |
|---|---|---|---|
| 0 Plan reconciliation | Implemented (docs), reviewed | docs PR | Merge by user |
| 1 Download correctness and retention | Implemented, tested, reviewed | downloads PR | Merge by user |
| 2 Library query scaling | Implemented, tested, reviewed | library-queries PR | Merge by user |
| 3 Production playback and preparation | 3A and 3B implemented, tested; 3C–3F not started | playback PR | Sol review |
| 4 Library-management UI | Not started | — | After item 2 |
| 5 Offline breadth | Not started | — | After item 1 |
| 6 M5 convenience and receiver | Not started | — | Marker/preview inventory |
| 7 M6 and parity backlog | Not started | — | After items 1–6 |
| 8 Release qualification | Not started | — | After feature implementation |

## 0. Plan reconciliation

- **Source SHA:** branch `plan-completion/docs` from `28f4b32`.
- **Behavior:** documentation only. Plan section 11 (the 2026-10-11 N1–N3
  assignment, previously only in a local uncommitted edit) is committed with
  a dated status banner marking it superseded, and machine paths replaced by
  neutral names. Section 12 adds corrected status claims, the milestone and
  parity matrix, and points here. Lane ledgers (M4, N2, N3) gain dated status
  notes; their historical receipts are unchanged.
- **Corrected claims:** FFmpeg tools contain zimg/libass but production HDR
  and styled/text burn-in are not implemented; live HLS WebVTT text and the
  bounded two-rung ABR are implemented; 152/156 operations served.
- **Focused evidence:** `python3 scripts/check_publication.py`-equivalent
  source scan of changed files (no machine paths or private hosts); link check
  of changed relative links. No application build is needed.
- **Review:** GPT-6.1 Sol (high effort, read-only) reviewed `8d19d28` against
  `origin/main`: no actionable findings. It independently reran the privacy
  scan, the relative-link check and the 152/156 served-operation count.
- **Release checks:** none.

## 1. Download correctness and bounded storage

- **Source SHA:** branch `plan-completion/downloads`, stacked on the docs PR
  (it shares this ledger). Commit recorded in the PR.
- **Implemented:**
  - **S30.** On a 403 from the download list the agent reconciles every owned
    root first and independently (`reconcileOwned`), then withdraws. Sync
    needs only `viewing:write` on the server; without it the server refuses
    and the events stay. Restore-epoch fencing and identity-based compaction
    are unchanged.
  - **S31.** `withdraw()` runs `cache collect` on each owned root after removing
    its copies. Nothing on a withdrawn root is wanted, so an unreferenced blob
    can only be a crash leftover. Collection is per root under the cache lock;
    another server's or principal's roots are never touched.
  - **Retention protocol.** Each sync may carry `acknowledged_through`, the
    device's own compaction point (its log base). Core `downloads::retain`
    moves a per-device-and-profile floor to it: never backward, never past
    `next_sequence - 1` (else 422 `acknowledgement_ahead`). The adapter deletes
    records through the floor in the same `BEGIN IMMEDIATE` transaction,
    before deciding the batch, and stores the floor (migration 0045,
    `offline_sequences.pruned_through`). Core `downloads::reconcile` then
    decides each event under the floor:
    - a recorded event above it is still an exact duplicate or a conflict;
    - an unrecorded sequence at or below it is a stale copy, refused as
      `sequence_acknowledged` (not consumed);
    - a new event while `MAX_RETAINED_EVENTS` (10,000) consumed events remain
      unacknowledged is refused as `acknowledgement_required` (not consumed).

    Both new refusals are non-consuming in `offline::consumed`, so the device
    never compacts them, and the agent stops reconciling that root until the next pass. Sequence
    identity (`next_sequence`) and causal lineage (`offline_lineage`) are never
    pruned. A compliant agent keeps at most one batch (200) of records on the
    server between syncs; older agents keep up to 10,000.
- **Contract:** `OfflineSyncInput.acknowledged_through` (optional, nullable);
  `OfflineEventResult` documents the two new non-consuming codes. Additive;
  older agents keep working without pruning.
- **Unsupported / limits:** event IDs are deduplicated only while their record
  is retained. A device that reused a pruned event ID with a new sequence
  would not be caught as a conflict; the agent generates fresh IDs.
  Offline `history_only`/`rejected` records are pruned with the rest. No API
  reads them, and their effect on viewing state is already applied.
- **Core/adapter boundary:** retention floor, stale-copy refusal and the bound
  are core decisions (`retain`, `reconcile`, `offline::consumed`). The adapter
  observes `(next_sequence, pruned_through)`, deletes and records the floor in
  the request transaction. The agent only forwards the cache tool's base.
- **Focused evidence (2026-10-12, darwin-arm64, Rust 1.98, Kyoto compile then
  direct execution):**

  | Check | Result |
  |---|---|
  | `playscale-core` lib (adds `retain`, stale-copy, limit and `consumed` tests) | 141 passed |
  | `downloads_model` exhaustive (Stateless) | passed: graph exhausted at 3,599,192 states and 50,388,688 transitions (437 s, 2.6 GB RSS, debug). Bounds: 4 device sequences, 2 online writes, 1 manual override, 1 timeline edit, 1 restore, retention limit 2; acknowledged sync, lost response, altered retry, stale replay, gap. New properties: records contiguous above the floor, floor never beyond the device's compaction, retained records within the limit, stale replays change nothing and get the required outcome |
  | `downloads_model` mutation checks | 2 passed; pruning past the device's compaction is detected |
  | `offline_model` (fuzz) | 3 passed |
  | `tests/v2_downloads.rs` (real router and SQLite) | 5 passed. The new retention test covers: no report, then pruning; lost-response duplicate; floor and records after reopening the database; stale copy refused; ahead/malformed reports; a 404 batch pruning nothing; the 10,000 bound and its release |
  | `motion-ui-host` lib and `store` | 1 and 11 passed |
  | `node --test apps/desktop/test/*.test.mjs packages/ui-bridge/*.test.mjs` | 80 passed. New: sync without `downloads:manage` with a lost response then exact retry; refused sync keeps events; collection on owned roots only; acknowledgement reporting and stale-copy refusal. Mutating either fix fails its test |
  | `run-download-workflow.mjs` (real server, agent, cache writer and host binaries) | passed all 8 checks; receipt [download-workflow-item1.json](download-workflow-item1.json). Its `commit` field is the base commit: the binaries were built from the uncommitted item-1 tree, whose non-test sources equal the item-1 commit |
  | Pinned clippy 1.98 with CI flags (`-p playscale-core -p playscale -p motion-ui-host --all-targets`) | clean (Kyoto `07941d71`) |
  | `cargo fmt --check`, `check_contract.py`, `contract_json.rb --check` | pass |

  Kyoto compile jobs: `fe2d1dcb` (test binaries), `e4088e14` (model).
- **Review:** GPT-6.1 Sol (high, read-only) reviewed `6faca67` against the docs
  branch: no actionable regressions in the retention protocol, transactional
  persistence or withdrawal changes.
- **Release checks:** the N3 deferred rows (installed package, large or
  interrupted transfers, physical full disk, Electron/Demuxe on an
  agent-published root) remain in section 8 of the handoff.

## 2. Library scalability and query support

- **Source SHA:** branch `plan-completion/library-queries`, stacked on item 1
  (migration order 0045 → 0046, shared ledger).
- **Implemented:**
  - **Scan history.** `listScans` gains `order=recent`, which pages newest
    first by a durable admission sequence (opaque keyset cursor), and
    `library_id`. The sequence (migration 0046: a growing counter assigned
    by an insert trigger, existing rows backfilled in insertion order) is
    neither the whole-second, clock-dependent capture time nor the rowid,
    which SQLite reuses after the newest rows are deleted.
    Authorization and the filter apply before ordering and the limit. A scan
    admitted mid-read sorts before every cursor, so it never shifts or
    repeats a later page. Retention: admitting a scan prunes the library's
    settled history beyond the newest 200 in the same transaction. The policy
    is core `demands::prunable_requests`, which never prunes the request
    being admitted or a pending one, and does not count them. Demands
    cascade; jobs are untouched. Indexes on the admission sequence, overall
    and per library.
  - **Renditions.** `listRenditions` gains `timeline_id`, applied before the
    page limit; an unknown or unreadable timeline is an empty page. Index
    `preparations(timeline_id, job_id)`.
  - **Organization.** `listCatalogItems` gains `ids` (1-100 distinct live IDs),
    a batched read under the normal scope; aliases and hidden or unknown IDs
    match nothing, so a hidden title looks like an unknown one.
  - **Facade.** `/sources` reads one page of 20 newest scans, and the manage
    page reads each timeline's renditions through the filter. `/organize`
    reads titles 100 at a time. Up to 100 unresolved IDs (aliases, hidden)
    get individual reads, and up to 500 distinct playlist timelines are read.
    Past those bounds, entries stay listed and removable as "Title not
    loaded" (collection members are left out) instead of the whole page
    becoming unavailable.
- **Query plans:** optional filters (`ids`, `timeline_id`, `library_id`,
  the cursor) are emitted as direct predicates only when present.
  `EXPLAIN QUERY PLAN` on the migrated schema shows primary-key lookups for
  `ids`, `preparations_timeline` for renditions, and
  `scan_requests_library_admission (library_id=? AND admission<?)` or
  `scan_requests_admission (admission<?)` for scan pages.
- **Unsupported / limits:** a collection with more than 100 merged-alias or
  hidden members leaves the excess unresolved. Playlists with more than 500
  distinct timelines show the rest untitled. An idempotent replay of a pruned
  scan's `createScan` returns its stored body, whose ID now reads as 404.
  The 10,000-item / 100,000-file performance qualification is a release item.
- **Core/adapter boundary:** which scans survive is a core policy
  (`prunable_requests`); the adapter reads statuses, deletes in the admission
  transaction and orders and filters in SQL. The query filters are reads, not
  state transitions, so no Stateless model was added; real SQLite tests cover
  them.
- **Focused evidence (2026-10-12, Kyoto compile `68473507`, `2cb3937a`,
  `d3feca78`; direct execution):**

  | Check | Result |
  |---|---|
  | `playscale-core` lib (adds `prunable_requests`) | 142 passed |
  | `playscale` lib | 64 passed |
  | `tests/v2_catalog.rs` | 19 passed. New: 1,100 settled scans plus a running one paged newest first across six pages with an insert mid-read (every row exactly once); fresh first page shows the new scan; restricted and hidden-library filters; bad cursor; admission prunes to 200 settled, keeps the running request and other libraries, cascades demands. Batched `ids`: scope, aliases, hidden, validation, 101 IDs refused |
  | `tests/v2_metadata_processing.rs` | 18 passed. New: 250 renditions of another timeline; the filtered page holds exactly this timeline's rendition with no next cursor; unknown timeline empty; invalid ID 400 |
  | `tests/access_api.rs` | 39 passed. New: `/organize` with a 450-title collection and a 520-timeline playlist renders every member and 20 "Title not loaded" entries (the old page failed past about 300) |
  | `tests/m3_workflows.rs` (real scan, ffmpeg, processing worker) | 1 passed |
  | Pinned clippy 1.98, CI flags, `-p playscale-core -p playscale --all-targets` | clean (Kyoto `a3a08b28`) |
  | `fmt`, contract checks | pass |
- **Review (GPT-6.1 Sol):** round 1 at `e4dd792` raised five P2 findings,
  all fixed in the follow-up commit:

  | # | Finding | Disposition |
  |---|---|---|
  | L1 | Pruning could delete the request being admitted (all sources disabled, clock stepped back or same-second ordering), failing admission | Fixed: `prunable_requests` takes the admitting request and never prunes it; pruning orders by rowid. Core test covers a settled admitting request below the kept history |
  | L2 | Same-second admissions could appear on later recent pages (whole-second capture times, random IDs) | Fixed: recent order and cursor use the admission rowid. The test admits a real scan through the API between two page reads, with history in the same second |
  | L3 | Batched `ids` used an optional `OR`, scanning the catalog | Fixed: direct predicate when given; plan uses primary-key lookups |
  | L4 | Rendition `timeline_id` used an optional `OR`, so the timeline index was unused | Fixed: direct predicate; plan uses `preparations_timeline` |
  | L5 | Recent scan pages could not seek by cursor or library | Fixed: direct `library_id=?` and `rowid<?` when given; plan seeks `scan_requests_library` |

  Round 2 at `a696a0d` (focused on the fixes) raised one P2:

  | # | Finding | Disposition |
  |---|---|---|
  | L6 | Rowids are reused after a library's newest requests are deleted, so a later page could contain a new admission | Fixed: durable admission sequence (counter and trigger, migration 0046) for recent order, cursors and pruning. The test deletes the newest history between page reads, admits through the API and checks the later page |

  Round 3 at `3fc8b84` (focused on L6) raised one P2:

  | # | Finding | Disposition |
  |---|---|---|
  | L7 | Rewriting migration 0046 breaks databases created by this PR's earlier commits (SQLx checksum mismatch) | Not applicable, with rationale: 0046 has never been on `main` or in a release. [MIGRATIONS.md](../../MIGRATIONS.md) already treats databases from pre-merge branch builds as development-only and not upgradable, and the rule against rewriting applies to merged migrations. The 0046 row there now says it was finalized before merge. If the PR is merged, 0046 is fixed from then on |

  After the round-1 fixes: core lib 142, `playscale` lib 64, `v2_catalog` 19 (the
  scan test ran three times), `v2_metadata_processing` 18, `access_api` 39,
  `m3_workflows` 1 passed; clippy clean (Kyoto `415cb9b5`, `e59a0b6c`).
  After the round-2 fix: `v2_catalog` 19, `v2_metadata_processing` 18,
  `access_api` 39, `m3_workflows` 1 passed (Kyoto `06928103`); clippy
  clean (Kyoto `83dcb092`).
- **Release checks:** frozen 10,000-item / 100,000-file browse and organize
  timings (handoff section 8).

## 3. Production playback and preparation

Item 3 is delivered in slices. Copy, hardware and ABR defaults stay unchanged.

### 3A. HDR-to-SDR tone mapping on live conversions

- **Source SHA:** branch `plan-completion/playback`, stacked on item 2.
- **Implemented:**
  - **Observation.** The scanner probes primaries, matrix (`color_space`),
    range, mastering-display peak, MaxCLL and the Dolby Vision configuration
    (profile, base-layer compatibility). They are stored in the track JSON,
    so no migration is needed. Rows probed earlier are re-probed by the next
    ordinary scan when their transfer is HDR (`color_probed`). The configured
    FFmpeg's filters are observed once per process (`ffmpeg -filters`); a
    failed probe reports none.
  - **Core policy** `color::tone_map(facts, filters)`:
    - SDR needs nothing;
    - PQ/HLG with BT.2020 primaries, BT.2020 non-constant matrix and a known
      range is tone-mapped with explicit input properties;
    - the PQ peak comes from MaxCLL, then the mastering peak (100–10,000
      nits), else 1,000 nits, recorded as assumed;
    - Dolby Vision passes only with a matching HDR10 base layer (profiles 7/8,
      compatibility 1) or HLG base layer (8.4).

    Refused: unknown or other primaries, matrix or range
    (`ColorUnknown`); other DV configurations, including profile 5; missing
    filters. `delivery::live_route_supported` admits HDR only to a
    conversion with an accepted tone map, never to a copy.
  - **Pinning.** The tone map is decided at plan and admission time and
    carried in the delivery `Pin` (`tone_map`), so execution never
    re-derives it. Admission and replans read the cached filter observation
    inside their write transactions (warmed before them). Multipart joins and
    the legacy v1 change path never tone-map; HDR there stays refused.
  - **Execution.** The N2-verified graph (zimg linearization, primaries
    conversion, Hable for the pinned peak, BT.1886 BT.709 limited range)
    runs before scaling, the bitmap burn-in overlay (subpictures are drawn on
    the SDR picture) and the adaptive split. Outputs are tagged BT.709/tv,
    including the low rung.
  - **Capabilities.** `playback.v2` reports an `hdr` limit with the tone-map
    route only when the filters are present; `hdr_conversion` stays in
    `unsupported` otherwise. `/processing/capabilities` lists the observed
    `zscale`/`tonemap`. HDR originals still go unchanged only to clients
    reporting HDR support.
- **Unsupported (explicit):** HDR preparation (the `h264720p` and
  `video_profile` recipes still refuse HDR: their recipe identity would have
  to pin the tone map); HDR on multipart timelines; dynamic HDR10+ metadata
  (static curve only); GPU tone mapping. Physical HDR/SDR display fidelity is
  a release check.
- **Core/adapter boundary:** whether and how to tone-map is a core decision
  from explicit facts (`color::tone_map`, `live_route_supported`). The
  adapters observe facts (probe, filter listing), pin the decision and
  execute it. The route-selection Stateless model is unchanged: it consumes
  `transcodable`, which now also reflects an accepted tone map. The policy is
  a pure function with exhaustive unit cases; no new state transition was
  added.
- **Focused evidence:** RESULTS_3A
- **Review:** pending (GPT-6.1 Sol).

### 3B. Text and styled subtitle burn-in

- **Source SHA:** same branch as 3A.
- **Implemented:**
  - **Core `delivery::presentation(codec, accepts_text, accepts_burned,
    libass)`** decides how an embedded subtitle reaches the client:
    - plain text as a text track when the client presents text;
    - otherwise plain or styled text rendered by libass into a conversion,
      when the client accepts burned subtitles and the observed FFmpeg has
      the `subtitles` filter;
    - bitmaps by the existing overlay.

    ASS/SSA are never flattened to WebVTT. A new plan claim
    `Subtitle::BurnedText{index, attachments}` and pin track
    `subtitle-burn-text:<n>:<attachments>` carry the exact embedded stream.
  - **Execution.** Once per delivery and before the encoder starts, the
    selected stream is extracted as ASS from the validated source descriptor
    (fingerprint re-checked before publication). The file's font attachments
    are dumped under fixed names (`burn-fonts-<n>/font<k>`, never names from
    the media file; at most 64). The encoder runs in the generation
    directory and references both by fixed relative names, so no path needs
    filtergraph escaping. Frames are shifted to file time for rendering and
    back, so a seek, a resumed start or an interval shows the right events
    without a lookback. Text is drawn after any tone map.
  - **Capabilities.** `styled_subtitles` and `text_burn_in` leave
    `unsupported` only when libass is observed; the subtitle limit then lists
    `embedded_text_and_styled_burned_in_conversion_with_font_attachments`.
- **Unsupported (explicit):** sidecar (external file) burn-in; burn-in on
  multipart versions; font rendering from attachments is wired but not
  verified against a deterministic font fixture.
- **Core/adapter boundary:** the presentation choice is a pure core decision
  from codec, client modes and the observed filter; the adapter extracts,
  checks fingerprints and executes.

### Remaining item 3 slices (not started)

| Slice | Next step |
|---|---|
| 3C per-part subtitles; bitmap cues before the 20 s lookback | Per-part subtitle occurrence; second validated descriptor for early cues |
| 3D subtitle-inclusive preparation and audio-component selection | Preparation pins by component with stale-source fencing |
| 3E bitrate preference and UI control | Contract preference field, bridge and player control |
| 3F custom video-profile preparation UI | Validated form over `video_profile` |

## 4. Library-management UI workflows

Not started. Online provider matching and provider artwork remain the approved
deferral.

## 5. Offline experience breadth

Not started. Depends on item 1.

## 6. M5 convenience and receiver work

Not started. Existing primitives to inventory first: `crates/core/src/markers.rs`,
`src/markers.rs`, migration 0030. Physical receiver qualification needs
hardware; if unavailable the exact blocked check will be named here.

## 7. M6 and wider parity backlog

Not started. Optional external/commercial/social services are out of scope.

## 8. Release qualification

Not started. Runs after feature implementation, against frozen identities.
