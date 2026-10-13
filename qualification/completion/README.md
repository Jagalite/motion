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
| 3 Production playback and preparation | 3A and 3B implemented, tested, reviewed; 3C text across parts implemented and reviewed; 3D implemented and reviewed; 3E implemented and reviewed; 3F implemented and reviewed; rest of 3C not started | playback, part-subtitles, preparation-tracks, bitrate and video-profile-ui PRs | Merge by user; then item 5B |
| 4 Library-management UI | Implemented, tested, reviewed | library-ui PR | Merge by user |
| 5 Offline breadth | 5A implemented and reviewed; 5B implemented and reviewed; 5C part 1 implemented and reviewed; 5C part 2 implemented and reviewed; 5D implemented and reviewed; 5E part 1 implemented and reviewed; 5E part 2 (poster) implemented and reviewed; 5E part 3 (metadata) implemented and reviewed; 5E part 4 server side implemented and reviewed; offline subtitle files and 5F not started | offline-roots, offline-removal, restore-recovery, restore-choice, transfer-state, prepared-downloads, offline-artwork, offline-metadata and offline-subtitles PRs | Merge by user; then 5F |
| 6 M5 convenience and receiver | 6A implemented, tested, reviewed; 6B implemented, tested, reviewed; 6C part 1 implemented and reviewed; 6C part 2 implemented and reviewed; 6C part 3 implemented (review pending); 6C rest, 6D, 6E not started | markers, embedded-chapters, previews, preview-schedules and player-previews PRs | Re-review; merge by user |
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
- **Focused evidence:** recorded under 3B, which shares the branch and runs.

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

### Item 3 evidence and review (3A and 3B)

2026-10-13, darwin-arm64, Rust 1.98; compiled through Kyoto, executed
directly. The pixel tests use the N2 media-tools FFmpeg (zimg, libass) via
`MOTION_FILTER_FFMPEG`. Host Homebrew FFmpeg/ffprobe write and inspect the
fixtures.

| Check | Result |
|---|---|
| `playscale-core` lib (`color::tone_map` refusals, peaks and DV configurations; `presentation`; `burn_fonts`; tone map only on conversions) | 147 passed |
| `playscale` lib (filter listing parser, side-data parsing, relative executables) | 67 passed |
| `hdr_sources_are_tone_mapped_to_sdr_only_with_known_facts_and_filters` | passed. 100-nit PQ patch delivered at Y ≈ 151 (limited range; a retag would give about 127), output tagged BT.709/tv. Refused with no filters and with unknown primaries/matrix. Capabilities advertise tone mapping only with the filters. Rows without colour facts are re-probed by an ordinary scan |
| `styled_subtitles_are_burned_by_libass_from_the_exact_stream` | passed. Only the selected one of two same-language ASS tracks is drawn, nothing before its event, drawn after a seek into it. The attachment is extracted under a fixed name. Refused as a text track and without libass |
| `bitmap_subtitles_are_burned_into_conversions_only`, `conversion_streams_hls_and_switches_audio_from_the_original` | passed (regressions) |
| Full `tests/v2_playback.rs` | 28 tests: 27 passed in one run; `copied_manifests_declare_the_source_rate` timed out under host load and passed rerun alone. The adaptive and hardware tests likewise passed alone after load timeouts with two threads |
| Core models (Stateless) | passed: access, delivery (760 s), delivery_observation, demands, identity, jobs, matching, offline, organization, playback_monitor, playback_session, processing, route_selection, scan_coverage and others |
| Pinned clippy 1.98, CI flags | clean after one `some_filter` fix |

**Review (GPT-6.1 Sol)**, round 1 at `0a6c6f6` against item 2: three P2
findings, all fixed in the follow-up commit:

| # | Finding | Disposition |
|---|---|---|
| P1-3 | Running the encoder in the generation directory broke relative executable paths (`--ffmpeg ./tools/ffmpeg`) for every live conversion | Fixed: `resolved_executable` joins a relative path with directory components to the server's working directory before spawning; bare names still use PATH. Unit-tested |
| P2-3 | Dolby Vision profile 7 with compatibility ID 6 (UHD Blu-ray HDR10 base layer) was refused | Fixed: `(7, 6)` maps to a PQ base layer; `(8, 6)` stays refused. Unit-tested |
| P3-3 | Font attachments were dumped without a byte bound | Fixed: the scanner records attachment MIME type and size. Core `burn_fonts` admits attachments only when every one is a font of known size (≤ 16 MiB each, ≤ 64 MiB total, ≤ 64); otherwise none are extracted and libass uses system fonts. Extraction also requires free space for the largest admissible set |

Round 2 at `a3879fb` (focused on those fixes) raised one P2, fixed in the
follow-up commit:

| # | Finding | Disposition |
|---|---|---|
| P4-3 | Attachment rows probed before this change never gain MIME type and size, so burn-in would ignore their fonts until a full rescan | Fixed: `Track::lacks_probe_facts` re-probes legacy attachment streams, like legacy HDR video, in ordinary scans. The styled test strips the facts, scans incrementally and checks they return. Core lib 147, `playscale` lib 67, `delivery` 12 and `access_api` 39 passed; HDR and styled tests passed again |

Round 3 at `14016af` (focused on that fix) raised one P2, fixed in the
follow-up commit:

| # | Finding | Disposition |
|---|---|---|
| P5-3 | Re-probing keyed on the colour flag, an unrelated fact: rows written by this branch's intermediate probe had the flag but no attachment facts | Fixed: a stream `probe_version` (current 1) records which probe wrote the facts; HDR video and attachment streams from an older probe are re-probed. The test now keeps the old flag set while removing the attachment facts and the version, and the facts return after an ordinary scan. HDR and styled tests and `playscale` lib 67 passed |

Round 4 at `a05d28e` (focused on that fix): no actionable regressions.

### 3C (text). Subtitles across the parts of a multipart version

- **Source SHA:** branch `plan-completion/part-subtitles`, stacked on 5A
  (`plan-completion/offline-roots`).
- **Implemented:** a film split across files can now play an explicitly
  chosen text subtitle (SubRip, mov_text, WebVTT) across every part.
  Previously any subtitle choice on a multipart version was refused
  (`subtitle_multipart_unavailable`).
  - The plan resolves the subtitle component to each part's own
    occurrence. This uses the same rule as multipart audio
    (`components::per_file_tracks`, generalised to `multipart_tracks`).
  - The plan pins each part's stream in `PartPin.subtitle`. The field is
    omitted when absent, so older pins serialize unchanged, and
    `same_layout` ignores it.
  - The delivery converts each part's stream from that part's validated
    descriptor and checks each descriptor again after conversion.
  - Core `subtitles::join_parts` moves each part's cues by the pinned
    durations before it. A cue is cut at its part's end, and one starting
    at or after the end is dropped.
  - The joined WebVTT is served through the live subtitle rendition used
    for single files.
- **Still refused, with `subtitle_multipart_unavailable`:**
  - a component missing from some part, or not text in every part;
  - burned (bitmap or styled) subtitles across parts;
  - sidecars;
  - automatic selection, and `require` without an explicit choice.
- **Core/adapter boundary:** two pure policies are in core:
  - which stream each part delivers (`per_file_tracks`, exact
    file-and-revision occurrences);
  - how part cues map to timeline time (`join_parts`).

  The adapter supplies probed streams and codecs and runs the converter.
  Revision fencing of every part is unchanged: later parts are validated
  sources, and a changed part fails the generation.
- **Evidence:**
  - Core lib: 149 passed. New: `join_parts` covers offsets, cutting at the
    part end, dropping, an empty part and the round trip through
    `parse_webvtt`. `same_layout` ignores the subtitle, and the pin
    serializes compatibly.
  - `playscale` lib: 68 passed.
  - `v2_playback`: 29 passed. New:
    `multipart_text_subtitles_are_joined_in_timeline_time` uses two real
    MP4 parts with mov_text English subtitles. It reads every segment of
    the live rendition: the second part's cue starts at the first part's
    pinned duration plus 1 s, and the first part's cues are unchanged. A
    part without the component, and a burned-only client, are refused.
  - Pinned clippy with CI's flags: clean.
- **Review (GPT-6.1 Sol), round 1:** clean.

### 3D. Preparation by audio component and with a text subtitle

- **Source SHA:** branch `plan-completion/preparation-tracks`, stacked on 3C
  text (`plan-completion/part-subtitles`).
- **Implemented:** `createPreparation` now accepts an audio component and
  an explicitly chosen embedded text subtitle. Previously it refused every
  component and subtitle selection. Selections are resolved on the exact
  source revision:
  - a component to its occurrence on that file and revision, never another
    file or language;
  - positional pins checked against that revision's streams;
  - a track pin and a component pin must agree
    (`track_component_conflict`).
- **What is recorded and encoded:** the record names the resolved `a{n}`
  and `s{n}` streams. The worker maps them, keeps the subtitle as the
  output's only `mov_text` stream, and otherwise strips subtitles.
- **What is refused:**
  - `subtitle_policy` `auto`, since automatic selection depends on a
    profile;
  - an explicit subtitle with `off`;
  - sidecar, bitmap and styled subtitles
    (`preparation_subtitle_unsupported`);
  - components absent from the revision (`component_unavailable`).
- **Playback:** a rendition serves a plan only for exactly the selection it
  kept, decided by core `processing::carries`:
  - audio and text subtitle must match the record;
  - a rendition with a subtitle never serves a plan without one;
  - sidecar and burned subtitles never come from a rendition.

  A kept subtitle is delivered as a WebVTT sidecar from the rendition's own
  bytes. `presents_subtitle` now takes whether the rendition carries the
  subtitle, and the pin names the rendition's only subtitle stream.
- **Core/adapter boundary:** the core holds these rules:
  - which renditions carry a selection (`processing::carries`, moved out of
    the playback adapter);
  - which routes present a carried subtitle;
  - whether a validated output must contain exactly one `mov_text`
    subtitle or none (`validate_output`).

  Component resolution uses the existing core `components` policies. The
  adapter resolves against the probed streams, runs FFmpeg and records
  the selection. Source revision fencing, the exact whole-file binding and
  publication facts are unchanged.
- **Evidence:**
  - Core lib: 150 passed. New: `carries` covers defaults, exact subtitle,
    no substitution, unrecorded and unreadable records; `presents_subtitle`
    covers a carried subtitle on a prepared route.
  - `state_policies`: 11 passed. New: output subtitle exactly once as
    `mov_text`, or none.
  - `v2_playback`: 30 passed. New:
    `preparations_keep_the_selected_audio_component_and_text_subtitle`
    covers:
    - four refusals;
    - the recorded `a1`/`s0`;
    - an ffprobe check that the rendition holds the 22.05 kHz Japanese
      audio and exactly one `mov_text` stream;
    - a plan for exactly that selection is `prepared`, while plans without
      the subtitle or with other audio are not;
    - the sidecar served from the rendition contains the caption at its
      time.
  - `v2_metadata_processing` 18 passed.
  - `m3_workflows` 2 passed.
  - Contract 5 passed.
  - Pinned clippy with CI's flags: clean.
- **Not covered:**
  - choosing the subtitle in a preparation UI (3F);
  - downloads of renditions with subtitles (5E).
- **Review (GPT-6.1 Sol), round 1:** two P2 findings, both fixed:

  | # | Finding | Disposition |
  |---|---|---|
  | P1 | A replan of a prepared delivery rebuilt its pin without the carried subtitle | Fixed: the prepared replan validates and keeps the selected text subtitle. The integration test replans the prepared delivery and requires the pending generation's `subtitles_url` |
  | P2 | A worker from before this change would strip the subtitle yet publish the job, because the recipe digest was unchanged | Fixed: a kept subtitle is part of the preparation's digest (`recipe_digest(..., subtitle)`), recomputed at publication from the stored selection. An older worker computes the historical digest and cannot publish. Preparations without a subtitle keep their digest. Unit test |

  Round 2 at `6253482`: clean.

### 3E. Streaming limit preference and player control

- **Source SHA:** branch `plan-completion/bitrate`, stacked on 3D
  (`plan-completion/preparation-tracks`).
- **Implemented:** a profile can now hold a streaming limit, a new
  optional `Preferences.max_bitrate_bps`. It is a positive decimal string,
  and null or absent means no limit.
  - **Profile page:** a "Streaming limit" select with presets from No
    limit to 1 Mbps, labelled by what each is enough for. A limit saved
    through the API outside the presets is shown as its own choice.
  - **Player:** sends the profile's limit as the plan's
    `quality.max_bitrate_bps`. It also gets a "Limit" control that replans
    with another limit through the existing quality replan. The limit is
    validated in the bridge before it is sent.
  - The server's plan decision for a cap (byte delivery cannot promise a
    peak; a capped conversion is VBV-limited and measured) is unchanged
    from earlier work.
- **Compatibility:**
  - v2 `PUT` replaces the whole document, so an absent limit means none.
  - The stored document omits the field when unset, so earlier documents
    and v1 reads are unchanged.
  - v1 `PUT` refuses a zero limit.
- **Core/adapter boundary:** no new correctness rule. The limit is a
  validated preference that the client sends; enforcement stays in the
  existing core plan rules (`conversion_fits`, `rate_cap`). One parser
  (`bitrate`) validates both preferences and plan input.
- **Evidence:**
  - `v2_playback`: 30 passed. The preferences test covers no limit by
    default, four invalid limits refused, and a saved limit round trip.
  - compose: 22 passed. New: the profile select with a nullable type, and
    the player's limit control and `data-max-bitrate`.
  - JS: 86 passed. New: `replanInput` replaces, keeps or clears the limit,
    and refuses five malformed limits.
  - `access_api` 39 passed, `m3_workflows` 2, `server` 46, contract 5.
  - Pinned clippy with CI's flags: clean.
- **Review (GPT-6.1 Sol), round 1:** clean.

### 3F. Custom video-profile preparation and subtitle choice on the manage page

- **Source SHA:** branch `plan-completion/video-profile-ui`, stacked on 3E
  (`plan-completion/bitrate`).
- **Implemented:** the title's manage page now offers both preparation
  options. Previously it offered neither, and the facade hid
  `video_profile`.
  - **Custom profile form:** "Prepare with a custom video profile" is a
    separate form per source, so a fixed recipe never sends a profile. Its
    controls mirror `VideoProfile`'s bounds:
    - encoder (software or VideoToolbox, where listed);
    - codec (H.264 or HEVC);
    - maximum width 128–3840 and height 72–2160, even only;
    - video bit rate 0.064–80 Mbps and audio 32–320 kbps, both sent as
      whole bits per second (bridge `mbps`, `kbps`);
    - frame rate: same as the source, or 23.976–60 fps as exact rationals.

    The server's own validation stays authoritative, and its refusal is
    shown by the form.
  - **Subtitle choice (3D):** both forms offer the source's text
    subtitles. A chosen subtitle sets `subtitle_policy` to `require`, and
    "None" sets `off` (bridge `subtitle-choice`). Sources without a text
    subtitle send `off` as before.
- **Core/adapter boundary:** UI only. No rule changed; the server
  validates every value (`VideoProfile::validate`, track selection).
- **Evidence:**
  - compose: 22 passed. New: the fixed form excludes `video_profile`; the
    custom form's hidden recipe and version, bounded inputs, frame-rate
    rationals and encoder choice; the subtitle choice.
  - JS: 87 passed. New: the custom profile body has whole bit rates (also
    from fractional Mbps), and the subtitle choice sets or clears its
    policy.
  - `m3_workflows`: 2 passed. New, through the real facade: the manage
    page offers the custom form; the body it encodes with its defaults is
    accepted (202), and an odd width is refused with
    `invalid_video_profile`.
  - Pinned clippy with CI's flags: clean.
- **Review (GPT-6.1 Sol), round 1:** clean.

### 3C (part 2). Automatic subtitles across parts

- **Source SHA:** branch `plan-completion/multipart-subtitles`, stacked on
  `plan-completion/offline-subtitles` (the top of the server stack).
- **Implemented:**
  - Automatic subtitle selection (`auto`, and `require` without an explicit
    choice) now works on multipart versions. The candidates are subtitle
    components that occur in every part (`multipart_tracks`). One is
    deliverable only when every part's occurrence is plain text and the
    client presents text: the new core `parts::joinable_subtitle`, which
    the explicit multipart path uses too. The chosen component is pinned
    to each part's own stream and joined in timeline time, as an explicit
    choice is.
  - Sidecars are never candidates across parts. A part without the
    component means `auto` plays without subtitles and `require` is
    refused (`subtitle_unavailable`, as for one file), never another
    part's track alone.
  - `V2_PLAYBACK.md` now states the multipart subtitle rule.
- **Core/adapter boundary:** language, role and mode selection is the
  existing core `automatic_subtitle`. What can be joined across parts is
  core `joinable_subtitle`. The adapter maps components to each part's
  tracks from observed facts. The route model is unchanged (multipart
  still only converts; no new transition).
- **Focused evidence (2026-10-13, Kyoto compile, direct execution):**

  | Check | Result |
  |---|---|
  | core | 162 passed. New: `a_subtitle_joins_across_parts_only_as_text_in_every_part` |
  | `tests/v2_playback.rs` `multipart_text_subtitles_are_joined_in_timeline_time` | passed. New: with the profile's English subtitles `always`, a part without the component gives `auto` without subtitles and `require` refused; with it in every part, `auto` selects `s0` and the delivery publishes both parts' cues joined |
  | `multipart_versions_play_their_parts_in_order` | passed. `require` without subtitles in any part now answers `subtitle_unavailable`, as a single file does |
  | `v2_playback` 32, `route_selection_model` | passed; clippy clean |
- **Review (GPT-6.1 Sol):** round 1 at `34dd3bc` is clean: no actionable
  findings.

### 3C (part 3). Bitmap cues established before a start

- **Source SHA:** branch `plan-completion/bitmap-lookback`, stacked on
  `plan-completion/multipart-subtitles`.
- **Implemented:**
  - Before a single-file conversion burns a bitmap subtitle (PGS, DVD,
    DVB) from a start, the adapter probes the validated source descriptor
    with ffprobe. It reads the subtitle stream's display sets in the 5
    minutes before the start (`-read_intervals`, no video decoding), with
    whether each draws anything, in file time.
  - Core `delivery::burn_read_from` decides where reading begins. If the
    last display set at or before the start draws and has not ended by
    itself, reading starts at its latest acquisition point (see round 2
    below). Otherwise it starts at the start itself; if the stream could not
    be read, at the old bounded 20 s.
  - The conversion keeps the original single-input graph (input seek, then
    an output trim to the start). A cue still showing at the start is
    therefore drawn, and video is decoded only from the cue's own display
    set, never a fixed window.
- **Limits:** a cue established more than 5 minutes before a start is not
  restored. Burned subtitles across parts remain refused.
- **Core/adapter boundary:** where to read from is a pure core decision
  over observed display sets (unit-tested, including the unknown case).
  The adapter only probes and builds FFmpeg arguments.
- **Focused evidence (2026-10-13, Kyoto compile, direct execution):**

  | Check | Result |
  |---|---|
  | core | 163 passed. New: `a_bitmap_cue_showing_at_the_start_is_read_from_its_display_set` |
  | `tests/v2_playback.rs` `bitmap_cues_established_long_before_a_start_are_drawn` | passed. A cue displayed from the file start and cleared 44 s later: delivered from 40 s the box is drawn; delivered from 46 s nothing is drawn and the conversion plays on |
  | `bitmap_subtitles_are_burned_into_conversions_only` | passed. From 0 s and from 8 s inside a cue |
  | `v2_playback` | 33 passed; clippy clean |
- **Review (GPT-6.1 Sol):** round 1 at `026cbdd` raised one P1 finding:
  - **B1 (P1):** the first design read the subtitle through a second,
    subtitle-only input. On FFmpeg 8.1.2 that input stalls the graph when
    its window holds no drawable cue: with nothing else from that input,
    the subtitle-to-video conversion never advances. Reproduced with a
    7-minute file.
  - **Fix:** the redesign above. Feeding the second input's video to a
    null sink also unblocked it, but cost seconds of CPU per start. The
    single-input graph cannot stall this way, and the new test's second
    case starts after the cue was cleared.

  Round 2 at `a532117` raised one P1 and two P2 findings, all fixed:

  | # | Finding | Disposition |
  |---|---|---|
  | B2 (P1) | A probe that did not exit when killed was dropped as "unavailable", so the generation (or a cancellation) could release capacity while it still held the source descriptor | Fixed: every lookback probe's `Stalled` child is handed to `lease.stuck` until it exits, as `copy_keyframe` does |
  | B3 | A PGS composition reusing objects or a palette from its epoch was read from its own time, so the decoder lacked them (`Invalid palette id`) and the cue was omitted | Fixed: PGS sets are read from the stream's own segments (`ffmpeg -c copy -f sup`, no hex inflation, bounded at 64 MiB), each with its composition state. Core `burn_read_from` reads from the latest acquisition point (epoch start or acquisition point) at or before the showing cue; if that point is outside the window the cue is not restored. Integration test `pgs_compositions_reusing_objects_are_read_from_their_epoch` (epoch at 0 s, reuse at 30 s, start at 38 s): drawn |
  | B4 | A DVD subpicture's own end time was ignored, so an expired cue made a later start decode up to 5 minutes of video needlessly | Fixed: DVD/DVB observations keep `end_display_time`; core treats a cue ended at or before the start as not showing. Core unit tests cover expired and still-showing subpictures |

  The observation is now core `DisplaySet` (time, draws, own end,
  acquisition). DVB page states are not parsed: each DVB subpicture is
  treated as self-contained. After the fixes: core 163 and `v2_playback`
  34 passed; clippy clean.

  Round 3 at `5ec48cd` raised one P1 and one P2 finding, both fixed:
  - **B5 (P1):** after handing a stuck probe to the lease's reaper, the
    generation still started an encoder on that lease, which the probe's
    exit could release early. A stuck lookback probe is now its own
    outcome (`StuckProbe`), and the generation fails instead of starting.
    Not covered by an automated test (no probe that survives a kill).
  - **B6:** SUP keeps 32-bit 90 kHz timestamps, so with `-copyts` a
    container starting late (for example at 50,000 s) wrapped and lost
    every observation. The export now applies
    `-output_ts_offset -<start_time>`, so the timestamps are file time. The
    epoch test's file now starts at 50,000 s and still draws the reused
    cue.

  After the fixes: `v2_playback` 34 passed; clippy clean.

  Round 4 at `aa963d5` is clean: no actionable findings.

### 3C (part 4). Burned bitmap subtitles across parts

- **Source SHA:** branch `plan-completion/multipart-burn`, stacked on
  `plan-completion/bitmap-lookback`.
- **Implemented:**
  - **Planning:** a subtitle component that occurs in every part as a
    bitmap (core `parts::burnable_across`) is burned for a client that
    accepts burned subtitles. This applies whether it is chosen explicitly
    or automatically: automatic candidates across parts are text, joined,
    for a client presenting text, otherwise burned bitmaps. Each part's own
    stream is pinned as `PartPin.burn`, which is additive and omitted when
    absent. Burned text (libass) across parts stays refused.
  - **Admission:** the plan's burn and every part's burn must agree.
    Each part's pinned stream must be a bitmap subtitle of that exact
    revision; otherwise 422 `subtitle_unsupported`.
  - **Conversion:** each part's subpicture is overlaid on its own pictures
    (its canvas scaled to theirs) before they are normalized into the
    concat canvas. The part playback starts in is probed like a single
    file (3C part 3): it is read from the display set still showing at
    its seek, and the extra time is trimmed in the graph (video and audio).
    Later parts play from their beginning.
- **Found while testing:** the single-file read-earlier output trim was
  also applied to a multipart conversion. It cut more than the remaining
  part, so the encoder exited before its first segment. That trim now
  applies to one file only.
- **Core/adapter boundary:** what may be burned across parts is core
  `burnable_across`; where to read from is core `burn_read_from`. The
  adapter maps components to each part's tracks, verifies pins at
  admission and builds the graph. The route model is unchanged (burning
  and multipart already require a conversion).
- **Focused evidence (2026-10-13, Kyoto compile, direct execution):**

  | Check | Result |
  |---|---|
  | core | 164 passed. New: `a_bitmap_subtitle_burns_across_parts_only_as_a_bitmap_in_every_part` |
  | `tests/v2_playback.rs` `bitmap_subtitles_are_burned_across_parts` | passed. Two parts, each with its own PGS cue: a text-only client is refused; for a burned client the conversion from 0 s draws part 1's box, and from 2 s into part 2 draws part 2's box (its display set precedes that seek) |
  | `v2_playback` 35, `route_selection_model` | passed; clippy clean |
- **Review (GPT-6.1 Sol):** round 1 at `4fb3a61` raised one P2 finding,
  fixed:
  - **A/V offset:** after reading early, video and audio were each reset to
    start at zero (`setpts=PTS-STARTPTS`). An audio stream starting later
    than the pictures then lost its delay. Both streams now shift by the
    same lookback (`setpts=PTS-<lookback>/TB`), so their offsets are kept.
    Confirmed with FFmpeg first (the first 0.2 s: −17.6 dB before, −91 dB
    after). The test's part 2 now has audio starting 0.5 s after its
    pictures. A start 0.25 s into it gives a silent first 0.2 s and sound
    from 0.4 s, with the cue drawn. `v2_playback` 35 passed; clippy clean.

  Round 2 at `ae29f6d` is clean: no actionable findings.

### Remaining item 3 slices

| Slice | Next step |
|---|---|
| 3C burned text subtitles across parts | Per-part libass extraction (`burn-text` per part, with fonts) and a per-part `subtitles` filter in the concat graph |

## 4. Library-management UI workflows

- **Source SHA:** branch `plan-completion/library-ui`, stacked on item 3.
- **Implemented** (all through existing public operations, except one
  additive read field):

  | Workflow | Where | How |
  |---|---|---|
  | Saved filters and smart collections | `/organize` "Saved filters" | Typed condition fields (kind, title contains, genre, tag, year from/to, watched, available) encoded by a new bridge `term` field type. "Make smart collection" sends the exact body for that filter. Edits are If-Match guarded and carry conditions the form cannot show unchanged. A filter in use is not offered for deletion (the server refuses it too) |
  | Play orders and timeline placement | Manage page "Play order" | Create an order (aired, DVD, absolute, custom) for an edition against the item validator; place each timeline with its validator and the destination group's revision |
  | Merge and split | Manage page "Duplicates and versions" | Merge candidates are readable titles found by this title, each with a precomputed body pinning both revisions. Split chooses versions and a new title. Both preview first and commit exactly that plan (existing bridge plan flow) |
  | Library and source deletion | `/sources` disclosures | States what is and is not removed (no media is deleted; a used source is not offered; a source with cataloged files is disabled instead). Requires an explicit confirmation and the revision the page showed |
  | Backdrop, banner and thumbnail | Manage page artwork | A role selector per image. The API does not report non-poster selections, and the page says so |
  | Readable match proposals | `/matches` | `Match.subject` (additive): the file name, plus the title it is cataloged under when the caller may read it |
- **Unsupported / limits:** current backdrop/banner/thumbnail choices are
  not shown (no read exists). Merge candidates come from a title search (20),
  not a duplicate detector. Play orders list up to 20 timelines per title.
- **Core/adapter boundary:** no new state-dependent rule. Revision
  preconditions, idempotency, plan tokens, merge/split validity, deletion
  refusal and filter validation are existing server/core decisions; the
  pages carry exact validators and bodies only.
- **Focused evidence (2026-10-13, Kyoto compile, direct execution):**

  | Check | Result |
  |---|---|
  | `crates/motion-ui/tests/compose.rs` | 22 passed. New assertions cover filter forms (typed terms, kept unrepresentable conditions, guarded edit, smart-collection body), play-order forms (item and timeline validators, group revision), merge and split plan forms, deletion disclosures (confirmation, validator, a used source not offered) and artwork role selection |
  | `tests/m3_workflows.rs` | 2 passed. New `library_management_workflows_run_through_the_pages` runs against the real server and scanner. It saves a filter and makes a smart collection from the page's exact body; the edit is guarded (a stale replay gets 412); the in-use filter is not offered for deletion. It creates a play order and places the timeline through the page's form values, reads a match proposal, previews and commits a merge (the retired ID resolves to the target), and deletes the library through its guarded form while the title and the media file remain |
  | `tests/v2_metadata_processing.rs` | 18 passed. `Match.subject` names the file and its readable title |
  | `tests/access_api.rs` | 39 passed |
  | `node --test` bridge and desktop suites | 81 passed. New: typed filter terms, only chosen criteria sent, an empty conjunction kept |
  | Contract checks (`check_contract.py`, `contract_json.rb --check`) | pass |
- **Review (GPT-6.1 Sol):** round 1 at `6763cee` against item 3 raised two
  P2 findings, both fixed in the follow-up commit:

  | # | Finding | Disposition |
  |---|---|---|
  | U1 | A smart collection made on the page could not be browsed: its evaluated members were never shown | Fixed: `/organize` lists the first 24 members the server evaluated (read-only links) and "and N more"; the end-to-end test asserts the scanned film appears |
  | U2 | Unlabeled versions all read "Original · " in the split picker | Fixed: `version_label` uses the label or origin, the first file's size and length, and a short ID, which is always distinct. Unit-tested |

  After the fixes: `playscale` lib 68, compose 22, `access_api` 39 and
  `m3_workflows` 2 passed. Pinned clippy 1.98 is clean after two test-only
  lints.

## 5. Offline experience breadth

### 5A. Choosing among several offline libraries

- **Source SHA:** branch `plan-completion/offline-roots`, stacked on item 6A (`plan-completion/markers`).
- **Implemented:** a device can hold saved titles from several servers and
  profiles (one cache root each). The desktop now lists every root that has
  saved titles, labelled by the server host it was saved from and the
  profile, and lets the person choose which to open offline. Previously the
  most recently synchronized root opened, with no choice.
  - The agent records a display label per root in `labels.json`, separate
    from the cache scope the cache tool validates.
  - `offlineRoots` reads the root index and manifests read-only.
  - `currentRoot(base, choice)` opens a chosen root only if it is a saved
    root of this device.
  - The main process validates the name before starting the offline host.
  - The chrome page shows the chooser only when there is more than one
    library. Labels are set with `textContent`.
  - The chooser re-reads the roots before every open, after every attempt
    and on focus. A selection is kept only while its root still exists.
- **Unsupported (still open):** removing titles from the offline host;
  prepared or track-selected downloads; the device transfer-state contract;
  the restore recovery flow. Each is listed below with its next step.
- **Core/adapter boundary:** no correctness rule changed. Which root may
  open is a membership check against the device's own root index (adapter),
  and the cache scope, lock and verified manifest still govern what the host
  serves.
- **Focused evidence:** `node --test apps/desktop/test/*.test.mjs
  packages/ui-bridge/*.test.mjs`: 86 passed. New tests:
  - Two servers, one with an empty root, are listed with labels and the
    current one marked.
  - A chosen root opens; an empty or unknown root is refused.
  - Driving the production chrome script: a selected root removed after
    startup is not resubmitted; a root saved later appears; a valid
    selection survives refreshes; with one library left the choice is hidden
    but that library is still the one opened.
  - The chrome test's harness without the new IPC still passes.
- **Review (GPT-6.1 Sol), round 1:** two P2 findings, both fixed.
  - The chooser kept its startup options, so it resubmitted removed roots
    and never showed new ones.
  - The `label` display rule overrode `[hidden]`.
- **Review, round 2:** one P2, fixed. When only one library remained, the
  hidden chooser named no root, so the emptied most recent root opened. The
  surviving root is now always named.
- **Review, round 3:** clean.
- **Release checks:** the Electron run of the chooser with two real servers.

### 5B. Removing a saved title on the device

- **Source SHA:** branch `plan-completion/offline-removal`, stacked on 3F
  (`plan-completion/video-profile-ui`).
- **Implemented:** a person can now remove a saved title from the offline
  downloads page with "Remove from this device" (it asks first).
  Previously the offline host could not remove anything.
  - **Host:** the running host removes the title as the cache writer, under
    the exclusive lock it already holds (`Store::open_held`, which checks
    that the handle is this root's `events.lock`). It serializes removal
    with opening media and invalidates an open player's ticket for that
    title.
  - **Progress:** offline progress recorded for the title is kept and
    synchronized later.
- **Correctness rule:** a removed title is never saved again behind the
  person's back. Its removal is a durable intent (`removals.json`), written
  before the copy goes.
  - Core `offline::may_publish` refuses publication and crash recovery of
    that download while the intent is pending.
  - When online, the download agent deletes the download on the server
    and settles the intent with the answer. Core `offline::settles`
    settles on deleted or absent and keeps the intent on failure.
  - Until settled, the agent skips that download even if the server still
    lists it.
- **Core/adapter boundary:**
  - **Core:** the two decisions above, with the server's answer as an
    explicit `Deletion` input (deleted, absent, failed).
  - **Store:** enforces them in the writer's locked, durable transitions,
    in order: intent, then manifest, then blob release.
  - **JS agent and host:** perform the HTTP and filesystem effects.
- **Evidence:**
  - Core lib: 151 passed. New: `may_publish` and `settles`.
  - `motion-ui-host` store: 13 passed. New:
    - a removed title's copy and blob go;
    - publication and recovery are refused, durably across reopen;
    - a failed answer keeps the intent and an absent one settles it, after
      which publication works again;
    - a staged transfer can be removed;
    - only this root's held lock admits `open_held`.
  - `motion-ui-host` cache: 7 passed. New: the offline page offers removal;
    an unauthenticated removal is refused; removal is idempotent (404 the
    second time); the page and blob are gone; and an open player's media
    ticket stops serving.
  - JS: 88 passed. New: an unreachable server keeps the removal pending
    and nothing is transferred; a later absent answer settles it.
  - compose 22 passed.
  - Pinned clippy with CI's flags: clean.
- **Not covered:** the Electron run of removal in the packaged offline
  window (5F).
- **Review (GPT-6.1 Sol), round 1:** five P2 findings, all fixed:

  | # | Finding | Disposition |
  |---|---|---|
  | R1 | A removal settled in the pass was transferred again from the list read before the deletion | Fixed: every removal id read at the start of the pass stays out of it. JS test: a settled removal is not transferred, and collection still runs |
  | R2 | A crash after the intent but before the copy went left the title presented indefinitely | Fixed: every writer open (`Store::open_with`) completes pending removals, and the host replays them under its lock before presenting. Store test |
  | R3 | A cancelled removal handler released the serialization guard while the removal ran, and skipped ticket invalidation | Fixed: the guard, removal and invalidation live in the blocking task |
  | R4 | Opening media released the guard before installing its ticket, so a removal could complete in between | Fixed: the guard returns with the snapshot and is held until the ticket is installed |
  | R5 | A removal whose manifest write committed but whose blob release failed left the host's manifest and ticket stale | Fixed: the host presents what the store committed, whatever the result, and invalidates the ticket whenever the title is no longer presented. Host test with an unwritable blob directory |

  After the fixes: store 14, cache 8 and JS 89 passed; pinned clippy is
  clean. R3 and R4 are ordering fixes without a deterministic
  cancellation test.

  Round 2 at `40cd3f7` raised three P2 findings, all about replaying an
  earlier interrupted removal inside a later removal call:
  - a replay whose cleanup failed hid its committed manifest change;
  - only the requested title's ticket was invalidated;
  - a retry of an already-completed removal reported 404 as a failure.

  All are fixed by one rule: a title with a pending removal is never
  presented (`offline::may_publish` over the durable intents).
  - Replay is explicit and best effort (`Store::replay_removals`) and
    retried at every open.
  - The host presents the committed manifest minus pending removals, and
    judges a removal by whether the title is still presented.
  - Every ticket of a no-longer-presented title is invalidated.
  - The page reloads on 404 (already gone).
  - Tests: completing A while removing B invalidates A's ticket; a host
    whose replay cannot write (read-only root) still presents nothing of
    the pending title; a committed removal whose blob release fails is
    reported as removed. Cache 9, store 14 and JS 89 passed; pinned
    clippy is clean.

  Round 3 at `3e5df95` raised one P2: with replay best effort, a server
  confirmation could clear the intent while a copy that a failed cleanup
  left behind was still saved, so the title reappeared. Fixed: core
  `offline::settles(outcome, saved_locally)` settles only when the server
  confirms and no copy remains here. Settlement first completes the local
  removal, best effort. Store test: the cleanup fails (read-only staging);
  a server confirmation does not settle; a reopen keeps the intent and the
  host does not present the title; once the cleanup can run, it settles.
  Core 151, store 15 and cache 9 passed.

  Round 4 at `0496fc7`: clean.

### 5C (part 1). Sequence continuity after a server restore

- **Source SHA:** branch `plan-completion/restore-recovery`, stacked on 5B
  (`plan-completion/offline-removal`).
- **Defect fixed:** after the server was restored from a backup, a device
  that had already compacted acknowledged events stopped forever. This
  happened even with nothing retained.
  - The device reports its compaction point as `acknowledged_through`, and
    the restored server, which never consumed those sequences, refused it
    as `acknowledgement_ahead`.
  - Without the report, each new event was a `sequence_gap`.
- **Implemented:**
  - **Device:** the log records the history its compaction point belongs
    to (`EventLog.base_viewing_epoch`, from core
    `offline::compacted_epoch`). The first batch sent under another
    history carries a re-anchor (core `offline::reanchor`, exposed by the
    cache tool's `events`), which the agent forwards.
  - **Server:** core `downloads::reanchor` continues the device's sequence
    after `through` and retains nothing at or below it.
    - Accepted only from this server's own earlier history; another
      server's history or the current one is refused with
      `invalid_reanchor`.
    - It only moves forward; a retried request changes nothing.
    - It is applied in the sync's write transaction before retention and
      the batch.
  - **Contract:** `OfflineSyncInput.reanchor` (optional).
- **Core/adapter boundary:** core decides both whether a batch must
  re-anchor and whether the server accepts it. Each takes the histories
  and sequences as explicit inputs. The adapters:
  - persist the compaction history with the compaction (one durable write
    of `events.json`);
  - apply the new floor and pruning in the sync transaction.
- **Stateless:** `downloads_model` (model and properties version 4) now
  re-anchors after its restore:
  - The device tracks its compaction history, and the server applies
    `downloads::reanchor`.
  - The oracle requires a batch under the new history to be decided from
    the re-anchored sequence.
  - The invariants that were relaxed for the whole run after a restore
    (`retention.never_prunes_beyond_device_compaction`,
    `reconcile.device_never_compacts_unrecorded_events`) are now relaxed
    only while the device's compaction point belongs to the earlier
    history.
  - Result: 4,166,162 states and 58,326,268 transitions, exhausted, in
    134 s release.
  - New scenario and mutation test: a server ignoring the re-anchor fails
    `reconcile.required_outcomes`.
- **Evidence:**
  - Core lib: 153 passed (`compacted_epoch`, `reanchor` on both sides).
  - `v2_downloads`: 6 passed. New:
    `offline_sync_reanchors_the_devices_sequence_after_a_restore` covers a
    simulated restore (new restore epoch, the device's sequence records
    gone):
    - without the re-anchor, `acknowledgement_ahead`;
    - with it, the event is consumed and the next sequence is 4;
    - the retry is a duplicate;
    - the current history, another server, or a malformed sequence is
      refused.
  - `motion-ui-host` store: 15 passed (compaction history recorded;
    pending re-anchor computed, then none once compacted in the current
    history).
  - JS: 90 passed (the batch carries the cache's re-anchor).
  - Contract: 5 passed.
  - Pinned clippy with CI's flags: clean.
- **Not covered (5C part 2):** progress retained from the earlier history
  (events pinned to it) still waits. The person's explicit choice to
  discard it or re-apply it onto the restored server is next.
- **Review (GPT-6.1 Sol), round 1:** one P2, fixed. When a restored
  backup already held later sequences, a re-anchor returned early without
  moving the retention floor. Without an `acknowledged_through`, all those
  records stayed retained and the next event could be refused as
  `acknowledgement_required`, despite the contract. The floor now always
  moves to `through`, the sequence never moves backward, and the adapter
  persists either change. Covered by a core case and an integration case
  (`next_sequence` 10, `through` 8: the sequence stays and the floor is 8).
  The model is unchanged and still passes: 4,166,162 states.

  Round 2 at `04a7c62`: clean.

### 5C (part 2). The person's choice for progress from before a restore

- **Source SHA:** branch `plan-completion/restore-choice`, stacked on 5C
  part 1 (`plan-completion/restore-recovery`).
- **Implemented:** offline progress recorded against the server's earlier
  history (before a restore) used to wait on the device indefinitely.
  The desktop window now lists, per profile, how many saved positions for
  how many titles are waiting, and offers two choices:
  - **Keep my offline progress (re-apply):** the cache re-records each
    earlier observation, in order, against its download's viewing base
    *as restored* (core `offline::reapply`, pins from
    `restored_viewing_revision`; see review C1): same position, status and
    device time, a new identity, and the next sequences after every
    retained event. The
    earlier events are compacted, and the next batch re-anchors past them
    (part 1). The server decides the new events against its current
    state, so progress newer on the server still wins (`viewing_newer`).
    Events whose download has no current pin are dropped and counted.
  - **Discard it:** the cache compacts the earlier events without
    sending them (core `offline::discard`), so the restored server keeps
    its own state. The next batch re-anchors.
  - Each choice is one durable replacement of the event log, validated
    as a log against its own pins.
  - Retained events are never renumbered.
  - Nothing is resolved without the person's choice.
  - The agent learns the server's current history from
    `/system/capabilities`, only for the root's own server.
- **Core/adapter boundary:**
  - **Core:** what each choice does to the log (`foreign_events`,
    `discard`, `reapply`), with the pins and the current history as
    explicit inputs.
  - **Cache tool:** applies it under the cache lock
    (`Store::resolve_restore`, `cache restore`).
  - **Agent and window:** re-pin first (re-apply only) and present the
    choice.
- **Stateless:** `downloads_model` (v5) adds the two choices after its
  restore.
  - New transition properties: discarding drops exactly the earlier
    progress; re-applying keeps every observation in order under the
    current history and the re-pinned download.
  - The existing oracle requires re-applied events to be decided against
    the restored server's state.
  - Result: 5,083,512 states and 81,336,192 transitions, exhausted, in
    182 s release.
  - New scenario test: a stuck completion, once kept, is applied and
    counts as watched; once discarded, nothing is sent.
- **Evidence:**
  - Core lib: 154 passed (discard, reapply with a dropped download, log
    validity).
  - `motion-ui-host` store: 17 passed (re-apply onto the manifest pin with
    a pending re-anchor; a second choice changes nothing; discard; the
    host still opens the log).
  - JS: 92 passed. New:
    - agent: listing counts; unknown choice or root refused; re-apply
      re-pins before the cache restore, with the current history;
      discard needs no re-pin;
    - chrome: the panel appears after the agent's status, the choice is
      sent, and the outcome is reported.
  - Pinned clippy with CI's flags: clean.
- **Not covered:** the packaged Electron run of the choice (5F).
- **Review (GPT-6.1 Sol), round 1:** one P1 and three P2 findings, all
  fixed. The design of Keep changed.

  | # | Finding | Disposition |
  |---|---|---|
  | C1 (P1) | Keep pinned the earlier progress to the *current* viewing base read at the choice, so viewing written on the restored server after the restore was overwritten. The promised "newer server progress wins" did not hold | Fixed: a restore now captures each timeline's viewing as restored. Migration `0047_restore_baselines` adds `restore_baselines` and `restore_baseline_epoch`; `db::restore_into` fills them in the same transaction that starts the new history. The download manifest exposes `restored_viewing_revision` and `restored_manual_epoch` (null when unknown). Kept progress is pinned to that baseline, so any later write is newer and the server answers `viewing_newer`. The agent no longer re-pins entries before Keep; the next pass does. Model v6 pins Reapply to the restored baseline and tracks writes since the restore. New scenario test Restore → OnlineProgress → Reapply → Sync answers `viewing_newer`, and a mutation that applies it fails the oracle. Persistence test: a real restore captures the baseline under the new epoch |
  | C2 | An earlier observation re-applied after a retained, current-history observation of the same download came last | Fixed: core `reapply` drops earlier observations of a download the device already observed in the current history (counted as dropped). Core test |
  | C3 | A second restore between reading the history and the manifests dropped kept progress permanently | Fixed: if any download reports another history, Keep changes nothing and the choice stays offered. The cache refuses pins not under the given history or not matching its download (store test). JS test |
  | C4 | The native library view covered the revealed panel | Fixed: the chrome page asks the host to lay out again (`motion:layout`) whenever the panel or status changes. Chrome test |

  After round 1:
  - Core 155, store 17, cache 9, `v2_downloads` 6,
    `catalog_persistence` 14, contract 5 and JS 92 passed.
  - `downloads_model` v6: 4,765,164 states and 76,242,624 transitions,
    exhausted, in 186 s.
  - Pinned clippy is clean.

  Round 2 at `c22d5db` raised one P1 and one P2, both fixed:

  | # | Finding | Disposition |
  |---|---|---|
  | C5 (P1) | The baseline was captured only by `db::restore_into`; the supported `scripts/backup.py restore` rotated the epoch without it, so Keep dropped everything | Fixed: both restore paths record the new epoch as a *pending* baseline (`restore_baseline_epoch.captured = 0`) in the transaction that rotates it. The Python tool creates the table in an older backup with the migration's DDL, which is now `IF NOT EXISTS`. The first server start captures the baseline in `db::connect`, after migrations and before serving. A baseline counts only when captured for the current epoch. Tests: Python (the restore leaves a pending baseline for its epoch); Rust (a pending marker is captured at the next start and never moves afterwards; `restore_into` captures it); `v2_downloads` (the manifest names the restored base only for a captured restore of the current epoch, 0 without a row, null when pending or of another epoch) |
  | C6 | Keep left the downloads pinned to the old history until the next pass, which may not run before offline playback, so new observations were again from before the restore | Fixed: Keep re-pins the downloads to the current state right after rewriting the log (kept events keep their restored base). A crash in between only offers the choice again for observations made meanwhile. JS test |

  After round 2: `catalog_persistence` 15, `v2_downloads` 7, Python
  backup 2 and JS 92 passed; pinned clippy is clean.

  Round 3 at `5dc2c8d` raised one P2: the post-Keep re-pin could race a
  scheduled pass. The pass could synchronize the kept events and advance
  viewing after the re-pin had read its manifest, leaving the download
  pinned behind the server. Fixed: the agent serializes passes and restore
  choices, in arrival order, whoever starts them. JS test: a choice made
  during a pass runs after it. JS 93 passed.

  Round 4 at `27e2c8b`: clean.

### 5D. Device-reported transfer state

- **Source SHA:** branch `plan-completion/transfer-state`, stacked on 5C
  part 2 (`plan-completion/restore-choice`).
- **Implemented:** the server shows what each download's own device
  reports about its copy. It never derives that from its own state.
  - **Server:** `Download.state` stays the server's lifecycle (ready,
    cancelled, expired). A new optional `Download.device_transfer` carries
    the device's last report as stored: queued, transferring,
    verified_local, failed or removed, with bytes, the device's sequence,
    the download revision it observed, and the server's receive time.
  - **Operation:** `reportDownloadTransfer` (`PUT
    /downloads/{id}/transfer`) accepts reports only from the download's own
    principal and device; anyone else gets 404.
  - **Desktop agent:** reports best effort; a failed report never blocks a
    transfer. It sends:
    - `transferring` at start and at most every 15 s;
    - `verified_local` after publication or recovery;
    - `failed` when a transfer fails;
    - `removed` when it removes the copy of an ended download that is
      still listed.

    Its sequence is persisted per download, so it keeps increasing across
    restarts.
  - **Downloads page:** a new "On the device" column ("Saving — 35%",
    "Saved and verified", ...), separate from the server's state.
- **Correctness rule:** core `downloads::accept_transfer` admits a report:
  - only about the download's current revision (`download_changed`);
  - consistent with its size: `verified_local` only with every byte,
    never beyond the size, `removed` with none (`invalid_transfer`);
  - in the device's order: an exact retry changes nothing; an earlier
    sequence is `transfer_report_stale`, and a reused one with other
    content is `transfer_report_conflict`;
  - after the download ended, only `removed` (`download_ended`).
- **Core/adapter boundary:**
  - **Core:** the whole admission rule, with the previous report, the
    revision, the size and whether the download ended as explicit inputs.
  - **Adapter:** stores the report as reported (migration
    `0048_download_transfers`, one row per download, deleted with the
    profile) in the request's write transaction.
- **Stateless:** new `transfer_reports_model` against
  `accept_transfer`:
  - three reports (partial, verified, removed) delivered in any order and
    repeatedly, one cancellation, and a revision refresh;
  - properties: only what the device sent is stored; verified only when
    complete; never backward; only about the current revision; required
    outcomes;
  - result: 682 states and 5,456 transitions, exhausted;
  - a mutation test catches a server accepting reports out of order.
- **Evidence:**
  - Core lib: 156 passed (`accept_transfer`).
  - `v2_downloads`: 8 passed. New:
    `devices_report_their_own_copies_in_their_own_order` covers:
    - no report until the device makes one;
    - retry, conflict, stale, changed revision and invalid progress;
    - another device of the principal refused;
    - the listing carries the report;
    - after cancellation, only removed.
  - compose: 22 passed ("On the device" column and wording).
  - JS: 94 passed. New: transferring then verified with increasing
    sequences and the listed revision; a failed report does not block;
    the sequence persists across agents.
  - Contract: 5 passed (`reportDownloadTransfer` served).
  - Pinned clippy with CI's flags: clean.
- **Not covered:** queued reports (the agent has no queue yet), and the
  packaged Electron run (5F).
- **Review (GPT-6.1 Sol), round 1:** four P2 findings, all fixed:

  | # | Finding | Disposition |
  |---|---|---|
  | T1 | A lost terminal report (verified or removed) was never retried, and copies saved before reports existed never reported | Fixed: every pass reconciles the server's view with what the device knows. A saved copy whose current report is not `verified_local` reports it; an ended download with no copy here, whose current report is not `removed`, reports that. JS test |
  | T2 | A title removed through the offline window never reported `removed` | Fixed by the same reconciliation, with the post-cancellation revision from the next listing. JS test |
  | T3 | Failing to persist the report sequence aborted the transfer and the pass | Fixed: a sequence is sent only once persisted, and a persistence failure is a reporting failure only. JS test with an unwritable sequence file |
  | T4 | A sequence beyond `i64` passed admission and failed the database constraint (500) | Fixed: numbers beyond 9223372036854775807 are refused as `invalid_transfer` and documented in the contract. Integration case |

  After the fixes: JS 95, `v2_downloads` 8 and contract 5 passed; pinned
  clippy is clean.

  Round 2 at `e7b517d` raised one P2: a removal report was lost when the
  server had never accepted an earlier report for that copy (for example
  one saved before reports existed). Fixed: removals the device makes are
  kept durably (`transfer-removed.json`) until the server accepts their
  `removed` report, and retried on later passes. JS test: a refused
  removal report of a never-reported copy is retried and then stops. JS
  96 passed.

  Round 3 at `91f32c8` raised one P2: if the removal journal itself could
  not be written, the copy was removed anyway and its evidence lost.
  Fixed: a copy is removed, and a person's removal settled, only once the
  removal is noted; otherwise it waits for a later pass. JS test with an
  unwritable journal. JS 97 passed.

  Round 4 at `f7cd792`: clean.

### 5E (part 1). Downloading a prepared version with its kept selection

- **Source SHA:** branch `plan-completion/prepared-downloads`, stacked on
  5D (`plan-completion/transfer-state`).
- **Implemented:** a device can save a ready prepared version (3D) instead
  of the original. Previously a recipe was always refused.
  - **API:** `createDownload` with a `recipe_id` resolves the requested
    tracks on the source revision each rendition was prepared from (the
    same resolver as `createPreparation`). It chooses the ready rendition
    of that recipe that is current for its source and carries exactly that
    selection, decided by core `processing::carries`. Otherwise it answers
    `download_preparation_unavailable`. Nothing is prepared on request and
    another selection is never substituted. An original download keeps
    refusing track pins, since it is copied whole.
  - **Expiry:** a prepared download expires when its rendition is no
    longer current for its source (`renditions::available`), not only
    when its own binding changes.
  - **`Rendition`:** gains `recipe_id` and the `tracks` it kept (optional,
    additive). The title page offers each ready prepared version, in
    words ("H.264 720p · audio 2 · subtitle 1"), as "Save this version for
    offline". The form sends exactly that recipe and selection.
  - The device agent is unchanged: a prepared version is one MP4 file. A
    kept text subtitle travels inside it as `mov_text`.
- **Core/adapter boundary:**
  - **Core:** the choice among originals and prepared renditions
    (`downloads::choose` with `Candidate::prepared`); whether a rendition
    carries the selection (`processing::carries`); whether it is current
    (`renditions::available`).
  - **Adapter:** reads the rendition facts and resolves the selection on
    the exact source revision.
- **Evidence:**
  - Core lib: 157 passed. New: the exact rendition is chosen among other
    recipes and other selections; refusal without one; a stale rendition
    is no candidate; an original request never answers with a rendition.
  - `v2_playback`: 30 passed. The 3D test now also downloads the real
    prepared rendition:
    - its manifest pins the rendition's digest;
    - other audio, or no subtitle, is refused with
      `download_preparation_unavailable`;
    - after the source changes, the download is `expired`.
  - compose: 22 passed (prepared version offered with its recipe and kept
    selection).
  - `v2_downloads` 8, `v2_metadata_processing` 18, `m3_workflows` 2,
    contract 5 and JS 96 passed.
  - Pinned clippy with CI's flags: clean.
- **Not covered:**
  - A device without `processing:request` sees only the original option,
    because `listRenditions` requires it; the API accepts the request
    either way.
  - Subtitles as separate offline files, and artwork and metadata files in
    the manifest, remain (5E part 2).
- **Review (GPT-6.1 Sol), round 1:** five P2 findings, all fixed:

  | # | Finding | Disposition |
  |---|---|---|
  | E1 | `require` without a subtitle pin, or `off` with one, could match a rendition | Fixed: one core rule, `processing::kept_subtitle`, for preparing and downloading (explicit and required, or none). Refusals `subtitle_policy_conflict` and `preparation_tracks_unsupported`. Core and integration tests |
  | E2 | Every offered version sent `version_id: null`, so the server could save a different rendition of the same recipe and selection | Fixed: `Rendition.version_id` (the generated version binding its output) is listed. The title page pins it, and the server honours the pin. Integration test downloads by the listed version |
  | E3 | Readiness checked revisions only, so an unavailable source or disabled library still admitted the rendition and kept prepared downloads ready | Fixed: admission and observation use `processing::rendition_status` over the facts `listRenditions` reports (`output_rendition_status`) |
  | E4 | The 200-candidate window could hide a matching rendition behind newer ones of other recipes | Fixed: the recipe is filtered in the query, and candidates are read page by page (up to 10 pages of 200) |
  | E5 | The title page read only the first page of renditions | Fixed: every page is read (bounded) before keeping ready ones |

  After the fixes: core 158, `v2_playback` 30, `v2_downloads` 8, compose
  22, `v2_metadata_processing` 18, `m3_workflows` 2 and contract 5 passed;
  pinned clippy is clean.

  Round 2 at `e40bfdc` raised two P2 findings, both fixed:
  - **Pin:** the listed `version_id` could name another generated
    version that binds only an interval or several parts of the same
    output. It is now restricted to the downloadable binding: whole
    output, single part, at its revision. The integration test adds such
    a version, sorting first, and checks that it is never listed.
  - **Cap:** past the read cap the title page discarded every rendition
    it had read. Read rows are kept (`list_up_to`), and the form says when
    only some prepared versions are offered (round 3: the note no longer
    points to the manage page, which has the same cap).
  - Round 3: one P2 (that note), fixed. Round 4 at `65c651e`: clean.

  `v2_playback` 30, compose 22 and `m3_workflows` 2 passed; pinned clippy
  is clean.

### 5E (part 2). The title's poster saved with the download

- **Source SHA:** branch `plan-completion/offline-artwork`, stacked on 5E
  part 1 (`plan-completion/prepared-downloads`).
- **Implemented:** the offline downloads page now shows each title's
  poster, saved with the download. Before, titles were text only.
  - **Server:** the download manifest names the title's selected poster
    as an `artwork` file, pinned by its SHA-256 (an artwork asset's ID is
    its digest), with its size and asset URL. Only posters the device can
    fetch are named (assets of the title, as asset content requires), up
    to 8 MiB, PNG, JPEG or WebP.
  - **Agent:** saves one media file and at most one poster. It fetches
    the poster whole and verifies size and digest before staging it. A
    poster that cannot be saved never blocks the title, which is then
    saved without one.
  - **Cache store:** publishes the poster into its own blob before the
    media, so a published entry never names a missing or unverified image.
    The entry records the poster (`CachedDownload.artwork`). Removal,
    replacement and collection treat it as the entry's bytes. Crash
    recovery needs it in place.
  - **Host:** serves `/cache/artwork/{id}` from the blob, verified again,
    behind the same cookie as every cache route, with `nosniff`.
- **Core/adapter boundary:** no new correctness rule. Integrity is digest
  verification in the store and host; selection and pinning stay with the
  existing manifest.
- **Evidence:**
  - `v2_downloads`: 9 passed. New: no artwork without a selected poster;
    a selected poster that is not an asset of the title is not offered;
    otherwise media plus artwork, with the digest, size and name, and the
    device fetches exactly those bytes.
  - Store: 18 passed. New: a mismatched poster publishes nothing; the
    poster blob is published, kept by collection and released by removal.
  - Cache: 10 passed. New: the page shows the poster, the route serves it,
    and it is refused without the cookie.
  - JS: 98 passed. New: a verified poster is staged and published; a bad
    one leaves the title saved without it.
  - compose 22 and contract 5 passed.
  - Pinned clippy with CI's flags: clean.
- **Not covered:** subtitle sidecar files and a metadata document offline;
  for now, a prepared version's kept text subtitle travels inside its MP4.
- **Review (GPT-6.1 Sol), round 1:** one P1 and two P2 findings, all
  fixed:

  | # | Finding | Disposition |
  |---|---|---|
  | A1 (P1) | The poster fetch had no deadline whenever a connection signal existed, so a stalled poster could block every download | Fixed: the poster fetch always has a 60 s deadline, combined with the connection signal |
  | A2 | The poster was written before space admission, so a title that fitted without it could be refused forever | Fixed: the poster counts against the same admission. Without room for it the title is saved alone, and the poster is fetched only after the media is staged. JS test |
  | A3 | A poster staged alone escaped lifecycle cleanup | Fixed: `Store::staged()` reports downloads with a staged poster too (deduplicated). Store test |

  After the fixes: store 19, cache 10 and JS 99 passed.

  Round 2 at `b59749d` raised two P2 findings, both fixed:
  - **Disconnect:** a disconnect during the poster fetch was swallowed
    and the staged media still published. Only the poster's own failures
    are tolerated now; a disconnect stops before publication.
  - **Recovery:** crash recovery published without the poster even when
    its verified blob was in place. Recovery is tried with the poster
    first (the cache checks it is in place), then without it.

  JS test covers both. JS 100 passed.

  Round 3 at `2b762ab` raised one P2 finding, fixed:
  - **Corrupt poster:** a corrupt poster blob made recovery with the
    poster fail, so the media-only fallback never ran and every pass
    failed. `Store::recover` now verifies the poster too. A corrupt one
    is discarded (unless another entry names it) and recovery declines,
    so the media is recovered without it.

  Covered by a store test and an agent test against the real cache tool
  (it runs when `MOTION_UI_HOST_BINARY` names a built binary). Store 20
  and desktop JS 56 passed, with the real-cache test run.

  Round 4 at `bf78128` is clean: no actionable findings.

### 5E (part 3). The title's metadata saved with the download

- **Source SHA:** branch `plan-completion/offline-metadata`, stacked on
  `plan-completion/offline-artwork`.
- **Implemented:**
  - **Core:** `offline::offline_metadata` decides what of a title's resolved
    metadata travels offline: release year, description and genres. Text is
    cut at a character boundary (4,000 description characters, 10 genres of
    64 characters, deduplicated). `offline::kept_metadata` decides what a
    device keeps: only metadata within those bounds and not empty. Anything
    else is dropped, never refusing the title.
  - **Server:** the download manifest carries `metadata` (additive and
    optional; contract schema `OfflineMetadata`) resolved in the same read
    as the manifest, so a local correction outranks an NFO. It is inline,
    so the manifest pins it.
  - **Agent:** passes only the known, well-typed fields to the cache, which
    refuses unknown fields, so a later server field cannot block a title.
  - **Cache store:** applies `kept_metadata` on publication and recovery and
    records the result in `CachedDownload.metadata`.
  - **Offline pages:** the list shows the year. The title page shows year,
    genres and the description (escaped).
- **Why inline, not a `metadata` file:** current agents refuse a manifest
  naming any file role they cannot save, so a metadata file would stop
  every download for them. The contract's `metadata` file role stays
  unused.
- **Core/adapter boundary:** selection, bounds and what a device keeps are
  core policies shared by the server and the cache. The adapters only read
  the resolved values and record the result.
- **Focused evidence (2026-10-13, Kyoto compile, direct execution):**

  | Check | Result |
  |---|---|
  | core `offline` | 12 passed. New: bounds, deduplication, cut at a character boundary, nothing when none; what the server produces is kept, out-of-bounds or empty metadata is dropped |
  | `tests/v2_downloads.rs` `manifests_carry_the_titles_bounded_metadata` | passed. No `metadata` without any; NFO plus a local correction resolve to the local description, the NFO year and the first 10 genres; cast does not travel |
  | Store | 21 passed. New: metadata within bounds is kept; 11 genres are dropped while the title is still published |
  | Cache | 11 passed. New: the list shows the year and the title page shows year, genres and the escaped description |
  | Desktop JS (with `MOTION_UI_HOST_BINARY`) | 58 passed. New: field filtering; with the real cache tool, a title is saved with its metadata minus an unknown field, and out-of-bounds metadata never blocks a title |
  | `v2_downloads` 10, contract 5, compose 22 | passed; clippy clean |
- **Review (GPT-6.1 Sol):** round 1 at `9f3a6e7` raised one P2 finding,
  fixed:
  - **Manifest budget:** valid metadata on many entries could exceed the
    cache's 2 MiB manifest bound before its 500-download limit, so
    publication failed on every retry. A new entry now keeps its metadata
    only while the manifest stays within the bound less 256 KiB (room for
    other entries' later growth); otherwise the title is saved without it.
    Store test: 140 titles with the largest description are all saved, and
    the later ones have none. Store 22 passed; clippy clean.

  Round 2 at `333b824` is clean: no actionable findings.

### 5E (part 4, server). A selected text subtitle pinned as WebVTT

- **Source SHA:** branch `plan-completion/offline-subtitles`, stacked on
  `plan-completion/embedded-chapters` (migration 0050 follows its 0049).
- **Implemented (server side; the device side is the next slice):**
  - **Core `downloads::choose`:** an original may name one text subtitle
    track of its exact revision with `subtitle_policy: require`. Audio pins,
    components, a named track without `require`, or `require` without a
    track are still refused (`download_tracks_unavailable`). A prepared
    rendition keeping an explicit subtitle carries it as its first subtitle
    track. Either way the pinned track must be a text subtitle that converts
    to WebVTT without losing meaning (`delivery::sidecar_subtitle`: SubRip,
    mov_text, WebVTT, text). Otherwise the request is refused with the new
    `download_subtitle_unavailable`, never substituted. The choice records
    `subtitle_track_id`.
  - **Pin:** `downloads.subtitle_track_id` (migration 0050, additive).
    `Download.subtitle_track_id` reports it.
  - **Artifact:** the manifest converts the track once per download from
    the pinned file revision. It uses the delivery converter
    (`delivery::download_subtitles`: opened beneath its root only while its
    fingerprint is the recorded one, a conversion slot held, bounded,
    parsed as WebVTT). The file is kept at
    `download-subtitles/<download>.vtt` beside the processing and delivery
    directories, so its digest is stable for the download. Conversion runs
    in its own task; a manifest read waits up to 30 s and otherwise answers
    503 `download_subtitles_unavailable` (retry). Cancelling a download
    releases the file.
  - **Manifest:** a `subtitle` file (`subtitles.vtt`) with its digest,
    size and the URL of the new `getDownloadSubtitles`. The operation
    serves the bytes only while the download is ready and only at exactly
    that digest (409 `download_subtitles_changed` otherwise).
- **Unsupported / limits:**
  - Sidecar subtitle files beside an original are not yet selectable.
  - Styled (ASS/SSA) and bitmap subtitles are refused, not burned in.
  - Files of expired downloads stay until the download is cancelled.
  - The device does not save the file yet: until the next slice the
    current agent refuses a manifest naming a `subtitle` file and says so.
    A subtitle is only named when the person explicitly selected one.
- **Core/adapter boundary:** which subtitle may travel with which download
  is decided in `downloads::choose` from observed track facts. Conversion,
  storage and serving are adapter effects bound to the pin (exact file
  revision and track). The digest in the manifest is the device's integrity
  rule. The Stateless downloads model is unchanged: the subtitle is fixed
  at creation by a pure decision covered by unit tests, and adds no state
  transition.
- **Focused evidence (2026-10-13, Kyoto compile, direct execution):**

  | Check | Result |
  |---|---|
  | core `downloads` | 10 passed. New: an original's named text subtitle is pinned on the first candidate carrying it; no carrier refuses; malformed selections refuse; a rendition's kept subtitle travels as `s0` only when it is text |
  | `tests/v2_downloads.rs` `an_originals_text_subtitle_travels_as_pinned_webvtt` | passed with a real ffmpeg MP4 (mov_text). Policy and missing-track refusals; the manifest names `subtitles.vtt` with digest and size; the served bytes are that digest and contain the cue; repeated manifests are identical; another digest is refused; cancelling releases the file and serving stops |
  | `v2_downloads` 11, `downloads_model` 6 (unchanged model, exhausted), contract 5, `v2_playback` 32 (delivery converter shared), compose 22, `access_api` 39, `m3_workflows` 2 | passed; clippy clean |
- **Review (GPT-6.1 Sol):** round 1 at `2ce2560` raised two P2 findings,
  both fixed:

  | # | Finding | Disposition |
  |---|---|---|
  | S1 | Concurrent or retried manifest reads each started a conversion, so one download could hold both shared subtitle slots | Fixed: conversion runs under a per-download lock and rechecks the kept file under it, so reads share one conversion. Integration test: three concurrent first reads give one conversion and identical files |
  | S2 | A conversion finishing after cancellation published its file anyway, and the read reported success | Fixed: under the same lock the conversion checks that the download is active before converting and again before the rename. Removal (now in the background after cancel) takes the lock too, so nothing is published after a cancellation's removal. Integration test holds the lock, cancels while a read waits, and sees 409 `download_cancelled`, no conversion and no file |

  After the fixes: `v2_downloads` 11 passed (three runs); clippy clean.

  Round 2 at `dc43cce` raised two P2 findings, both fixed:
  - **S3:** a failed conversion was not shared. Reads already waiting (whose
    callers may have stopped waiting) each converted again. A failure is
    now recorded briefly per download, and reads that were waiting before
    it return it without converting; a later read tries again. Integration
    test: a phantom text track makes conversion fail; three queued reads
    share one attempt, and a later read makes the second.
  - **S4:** the test-only per-download conversion counter grew for the
    process's lifetime. It is now a single process-wide counter, and tests
    assert deltas.

  After the fixes: `v2_downloads` 11 passed (three runs); clippy clean.

  Round 3 at `085d2cf` raised two P2 findings, both fixed:
  - **S5:** a remembered failure lost its kind. Readers waiting on an
    expired source got a retryable 503 instead of 409 `download_expired`.
    The failure now records whether the source expired, and waiting reads
    replay exactly that.
  - **S6:** a remembered failure was replayed before the cancellation
    check, so a reader waiting through a failure and then a cancellation
    got 503 instead of 409 `download_cancelled`. Cancellation is checked
    first.

  Integration test (a remembered failure ordered against a waiting read
  with the lock): an expired failure replays as `download_expired`; a
  failure followed by a cancellation answers `download_cancelled`; neither
  converts. `v2_downloads` 11 passed (three runs); clippy clean.

  Round 4 at `3d66651` is clean: no actionable findings.

### Remaining item 5 work (not started)

| Slice | Next step | Dependency |
|---|---|---|
| 5E remaining: subtitle files on the device | The agent saves the pinned `subtitle` file (verified like the poster, required when selected); the cache store publishes it; the offline host serves it and the offline player adds it as a text track; the title page offers the selection for originals | 5E part 4 (server) |
| 5F end-to-end | Real Electron/Demuxe download → offline → reconnect run | All of the above |

## 6. M5 convenience and receiver work

### Inventory (before implementation)

| Primitive | State at `3fc8b84` |
|---|---|
| Marker storage (migration 0030), core `markers` (validation, provenance precedence, effective view), adapter `src/markers.rs` | Implemented, not served |
| `listMarkers`, `createMarker` contract operations | Contract only (`implementation_target`) |
| Embedded chapter extraction, intro/credits detection | Missing (no producer of `embedded`/`detected` markers) |
| Server-generated previews (trick-play thumbnails) | Missing; `preview` schedules refused (`schedule_task_unavailable`) |
| Player chapter/skip UI | Missing |
| TV/cast receiver | Missing; `cast` listed unsupported |

### 6A. Marker API, correction UI and player exposure

- **Source SHA:** branch `plan-completion/markers`, stacked on item 4.
- **Implemented:**
  - `listMarkers` and `createMarker` are served (`src/v2/markers.rs`).
    Reading needs `catalog:read` and a readable timeline. Creating needs
    `catalog:write`, the whole work in scope, an Idempotency-Key and the
    timeline revision the caller reviewed (409 `timeline_changed` otherwise).
    Created markers are `manual`. Core precedence makes a manual marker the
    effective one over embedded, imported and detected markers, which stay
    listed. `listMarkers?effective=true` (additive) returns the core
    effective view for the current revision. Every marker carries its
    provenance and the revision it was validated against.
  - Manage page "Chapters and skip markers": every marker per timeline in
    words ("detected, unconfirmed"; "not in effect (earlier timeline)") and an
    "Add marker" form (seconds entered, milliseconds sent) checked against the
    revision shown.
  - Player: effective chapters as seek buttons, and "Skip intro/recap/credits"
    buttons the player shows only while playback is inside the interval,
    labelled "(detected)" when uncertain. Seeks go through the generation API
    like the position form.
- **Unsupported / limits:** no marker deletion or editing operation exists
  in the contract; corrections add a manual marker that takes precedence.
  Nothing produces `embedded` or `detected` markers yet (chapter extraction
  and intro/credits detection are not implemented).
- **Core/adapter boundary:** validation, precedence and the effective view
  are the existing core decisions; the adapter reads facts and enforces the
  revision fence and scope in one write transaction.
- **Focused evidence (2026-10-13, Kyoto compile, direct execution):**

  | Check | Result |
  |---|---|
  | `tests/v2_catalog.rs` `markers_are_revision_bound_and_manual_corrections_take_effect` | passed. Detected and embedded markers are listed with provenance; a manual intro outranks the detected one in `effective=true`; exact replay is idempotent; refusals cover a stale revision (409), an open-ended intro (422), no write permission (403) and outside scope (404); after the timeline changes the old markers stay listed but none is in effect |
  | `tests/contract.rs` (served-operation ledger) | 5 passed with `listMarkers` and `createMarker` served |
  | `crates/motion-ui/tests/compose.rs` | 22 passed. New: manage-page marker list and form; player chapter and skip buttons with the detected label |
  | `tests/access_api.rs`, `tests/m3_workflows.rs` | 39 and 2 passed (pages that changed) |
  | `node --test packages/ui-bridge` | 43 passed. New: skip buttons only inside their interval; seconds sent as milliseconds |
- **Review (GPT-6.1 Sol):** round 1 at `43b0827` against item 4 raised four
  P2 findings, all fixed in the follow-up commit:

  | # | Finding | Disposition |
  |---|---|---|
  | M1 | "Skip credits" ending exactly at the duration sent an invalid seek position | Fixed: `seekTarget` seeks just before the duration; JS test |
  | M2 | Reads silently stopped at 2,000 markers, before pagination and before the effective view | Fixed: core `MAX_MARKERS` (1,000) is enforced at creation (409 `marker_limit`, `admits_another`); reads load every marker and refuse explicitly if the bound is ever exceeded; the effective view always sees all of them. Integration test fills a timeline to the bound, pages through every marker and is refused one more |
  | M3 | The manage page and player kept only the first page of markers | Fixed: both read every page within the bound; past it the manage section reports unavailable |
  | M4 | Positions beyond JSON-safe integers (or `i64`) were stored or failed with 500 | Fixed: core `validate` bounds positions by playback's `MAX_POSITION_MS` even without a known duration (422 `outside_timeline`); unit and integration tests |

  After the fixes: core lib 148, `playscale` lib 68, compose 22, `v2_catalog`
  20, contract 5, player JS 25 passed.

  Round 2 at `0bb34b3` raised two P2 findings, both fixed:

  | # | Finding | Disposition |
  |---|---|---|
  | M5 | Effective pages were each decided in their own transaction, so a change between pages could join two effective sets (embedded chapters plus a later manual one) | Fixed: an effective-view cursor names a snapshot (digest of the timeline revision and every marker ID and revision); a change between pages is refused with 409 `markers_changed`. The player restarts the read, up to three times, then shows none. Integration test changes a marker between pages and is refused; a plain cursor is refused by the effective view |
  | M6 | The older adapter `markers::create` did not enforce `MAX_MARKERS` | Fixed: both creation paths share `markers::admission` in their write transaction; the integration test is refused by both at the bound |

  After round 2: `v2_catalog` markers test, `catalog_workflows` 27,
  contract 5, `playscale` lib 68 passed; pinned clippy with CI's flags
  clean.

  Round 3 at `916bc2d`: clean.


### 6B. Embedded chapters from the container

- **Source SHA:** branch `plan-completion/embedded-chapters`, stacked on
  `plan-completion/offline-metadata` (it needs migrations 0047 and 0048).
- **Implemented:**
  - **Scan:** ffprobe records each file's container chapters (start, end and
    title) in `media_files.chapters_json` (migration 0049, additive). It is
    NULL until probed and an empty array when there are none. A file that
    is unchanged and was scanned before chapters were recorded is probed for
    chapters alone. It is not hashed again: the probe reads the opened file
    and checks the fingerprint afterwards, and on any failure the next scan
    tries again. A probe never clears stored chapters.
  - **Core:** `markers::embedded_chapters` turns them into `embedded`
    chapter markers. Chapters outside the duration, past `MAX_POSITION_MS`
    or at a duplicate start are dropped rather than guessed. Labels are
    trimmed and bounded, and `MAX_EMBEDDED_CHAPTERS` (200) applies.
  - **Read path:** `listMarkers` derives the embedded markers at read time
    from the timeline's whole, single-file original, at the timeline's
    current revision. They are file facts and follow the timeline while it
    binds that file revision. Their identities name the file revision, so
    new content means new IDs. They take part in the effective view and its
    snapshot digest, so manual corrections outrank them through the
    existing precedence.
- **Unsupported / limits:** chapters of multi-part, cut (in/out) or
  generated versions are not mapped onto the timeline; those titles have no
  embedded markers. Intro and credits detection (`detected`) is not
  implemented.
- **Core/adapter boundary:** what counts as a valid chapter and how it
  ranks against other provenances are core decisions (`embedded_chapters`
  plus the existing `effective`). The scanner and read path only supply
  probed facts and the binding they belong to. Deriving at read time rather
  than storing markers means no state can go stale against a timeline
  revision.
- **Focused evidence (2026-10-13, Kyoto compile, direct execution):**

  | Check | Result |
  |---|---|
  | core `markers` | 3 passed. New: ordering, drops (duplicate start, beyond the end), trimmed labels, manual precedence, the 200 bound |
  | `tests/v2_playback.rs` `container_chapters_are_embedded_markers_of_the_timeline` | passed. A real ffmpeg file with two chapters lists them as `embedded` and in effect; they stay in effect after a timeline revision; a manual chapter outranks them while they stay listed; a file whose chapters were never recorded is probed for them on the next scan |
  | `v2_playback`, `v2_catalog`, `scans`, `catalog_workflows`, `catalog_persistence` | 31, 20, 8, 27, 15 passed |
- **Review (GPT-6.1 Sol):** round 1 at `d0ca53f` raised one P1 and three
  P2 findings, all fixed:

  | # | Finding | Disposition |
  |---|---|---|
  | C1 (P1) | The source original was chosen without the caller's scope, so a chaptered copy in another library could disclose its chapter titles and positions | Fixed: candidate originals are filtered as playback does (enabled library, `readable` on the file's facts) before one is chosen. Integration test with a timeline whose originals are in two libraries: each scoped caller sees only its own library's chapters |
  | C2 | `coalesce` carried stored chapters onto replaced content whose chapters could not be probed, and they were never probed again | Fixed: stored chapters are kept only while the content revision is unchanged; otherwise NULL, so the next scan retries. Integration test replaces the file with unprobeable bytes |
  | C3 | Scan-created timelines have no duration, so chapters past the media's end were not dropped | Fixed: the whole file's probed duration bounds them when the timeline has none. Integration test |
  | C4 | The chapter-only probe was awaited directly, ignoring cancellation and shutdown for up to 30 s | Fixed: it runs under the same job-state and shutdown polling as a full inspection; dropping it kills ffprobe. Not covered by a dedicated test (no stalling ffprobe fixture) |

  After the fixes: `v2_playback` 32, `scans` 8, `v2_catalog` 20 passed; clippy clean.

  Round 2 at `5f97d25` raised one P2 finding, fixed:
  - **C5:** candidate sources were limited to 100 before the readability
    check, so 100 unreadable (or unavailable) chaptered copies ordered
    first hid a readable one. Candidates are now paged by (version, file)
    until a readable available original is found. The integration test
    adds 100 unreadable chaptered copies ordered first; the scoped caller
    still sees its own chapters.

  After the fix: `v2_playback` 32, `v2_catalog` 20 passed; clippy clean.

  Round 3 at `26f9578` is clean: no actionable findings.

### 6C (part 1). Server-generated trick-play previews

- **Source SHA:** branch `plan-completion/previews`, stacked on
  `plan-completion/multipart-burn` (migration 0051 follows 0050).
- **Implemented:**
  - **Core `previews`:**
    - `layout`: 160 px thumbnails with the picture's shape, 10×10 per
      sheet, one every 10 s or wider so there are at most 2,000.
    - `valid_sheets`: exactly the promised sheets, each a full grid, none
      over 4 MiB.
    - `PublicationFacts`: the attempt is current, the source revision is
      current, the sheets are valid and the source unchanged while
      rendering, and the requester is still authorized.
    - `tile_at`: where a position's thumbnail is.
  - **Jobs:** `preview_jobs` (migration 0051) with the shared job reducer
    (`jobs::transition`). The job appears in `api_jobs` as kind `preview`,
    so `getJob`, `listJobs`, `cancelJob` and `retryJob` work. A stopped
    server's attempts are recovered (requeued or cancelled) at the next
    start.
  - **Worker:** reserves maintenance execution capacity and opens the file
    at exactly its revision through the delivery source validation. FFmpeg
    decodes key frames only (`-skip_frame nokey`, `fps`, `tile`). It stops
    on cancellation, server stop, lease termination or a 30-minute
    deadline; a renderer that will not exit goes to the lease's reaper. It
    checks the source fingerprint afterwards, observes the sheets'
    decoded size, then publishes or discards (`PublicationFacts`) under the
    jobs lock in one write transaction: durable sheets, then an immutable
    `previews/<file>/<revision>/` directory, then the `previews` row.
  - **Retention:** rows whose revision is no longer current are removed
    first, then their directories. Attempt staging and orphan directories
    are swept under the jobs lock.
  - **API** (contract additions, served):
    - `getTimelinePreviews`: ready with layout and sheet URLs, pending, or
      none, for the timeline's readable whole single-file original.
    - `createTimelinePreviews`: needs `processing:request` and an
      Idempotency-Key. It is never duplicated: it answers with the active
      or publishing job.
    - `getPreviewSheet`: JPEG, only for the file's current revision and to
      a caller who may read the file.
- **Not yet:** `preview` schedules are still refused (part 2), and the
  player does not show the thumbnails yet (part 3).
- **Core/adapter boundary:** layout, validity, publication and lookup are
  core policies (unit-tested); the job lifecycle is the core reducer
  (`jobs_model`). The adapter renders, observes and executes publication
  atomically.
- **Focused evidence (2026-10-13, Kyoto compile, direct execution):**

  | Check | Result |
  |---|---|
  | core | 167 passed. New: layouts bound tiles and follow the picture shape; only the promised sheets validate and publish; a position finds its thumbnail |
  | `tests/v2_playback.rs` `previews_are_rendered_once_served_by_revision_and_retired` | passed. A reader cannot request rendering. A repeated request answers with the same job. A cancelled queued job never renders, and a new request queues again. The worker publishes one 1600×1200 sheet of three 160×120 thumbnails (25 s, one every 10 s). Another sheet number, no session, or a device scoped away from the library is refused. After a revision change, retention retires the previews |
  | `v2_playback` 36, `jobs_model` 3, contract 5, `v2_downloads` 11, `access_api` 39, `m3_workflows` 2, `catalog_workflows` 27, `catalog_persistence` 15, `scans` 8, `v2_catalog` 20 | passed; clippy clean |
- **Review (GPT-6.1 Sol):** round 1 at `38d016e` raised one P1 and five P2
  findings, all fixed:

  | # | Finding | Disposition |
  |---|---|---|
  | V1 (P1) | The renderer was spawned directly: killing the server left it running unaccounted, and recovery could start a replacement beyond capacity | Fixed: the renderer runs under the FFmpeg supervisor, which ends it when the owner's pipe closes, with an ownership witness (`lease.started`, `confirm_exit`). Recovery re-registers held witnesses (`execution.recovered`) before requeuing, and retention leaves a held attempt's staging alone |
  | V2 | The `fps` rounding rendered fewer thumbnails than the layout promises (0 for 3 s, 1 for 11 s), so sheets either failed or showed blank tiles | Fixed: `fps=…:start_time=0:round=down:eof_action=pass`, verified exact for 3 s, 11 s, 20 s, 25 s, 1,001 s (101 thumbnails) and sparse key frames. FFmpeg writes one thumbnail per tile, core `valid_thumbnails` checks the exact count and size, and the sheets are assembled from them in Rust, never padded |
  | V3 | Retrying a cancelled job could requeue it beside a newer active job, or after publication, and bypassed the queue bound | Fixed: a preview retry is refused (409 `preview_duplicate`) beside an active job or published previews of the revision, and 503 when the queue is full. The active-job index is now unique per revision |
  | V4 | A database restored without its preview files kept rows reporting `ready` while the sheets were gone | Fixed: retention withdraws rows whose sheets are not all on disk, so a request renders them again |
  | V5 | Publication synced the sheets but not their directory entries or a new file-level directory before the commit | Fixed: the witness is removed, then the sheets, the staging directory, a newly created ancestor and the published directory and its parent are synced before the row commits |
  | V6 | Deduplicated requests by another requester returned a partial `Job` | Fixed: `createTimelinePreviews` now answers with the `TimelinePreviews` manifest, which names the job only by ID (its details stay its requester's) |

  Test additions: the retry duplicate is refused; a missing sheet is
  withdrawn and rendered again on request; responses are manifests. Core
  168, `v2_playback` 36, `access_api` 39, contract 5, `jobs_model` 3
  passed; clippy clean.

  Round 2 at `7549b60` raised one P1 and two P2 findings, all fixed:
  - **V7 (P1):** round 1 had edited the already-pushed migration 0051
    (its checksum). That would stop the server for any database that ran it.
    0051 is restored byte for byte, and the unique active-job index comes in
    a new migration, 0052. It first cancels any duplicate active jobs,
    keeping the oldest per revision.
  - **V8:** when a renderer's exit was not confirmed, `finish` still
    removed its staging and held witness, so a restart could not account
    for it. A stuck attempt's staging is now kept; retention leaves it
    until the witness is free.
  - **V9:** `round=down` put the last frame of each interval in the tile
    before it (seconds 9, 19, 24 for tiles 0, 10, 20). `round=up` keeps the
    latest frame at or before each position (0, 10, 20), with exact counts
    re-verified (3, 11, 20, 25, 1,001 s; sparse key frames). The test's
    video now encodes its second as brightness, and thumbnails 0, 1 and 2
    are checked to show seconds 0, 10 and 20. `v2_playback` 36 passed;
    clippy clean.

  Round 3 at `12107f8` is clean: no actionable findings.

### 6C (part 2). Preview schedules

- **Source SHA:** branch `plan-completion/preview-schedules`, stacked on
  `plan-completion/previews`.
- **Implemented:**
  - `preview` schedules are accepted for a caller with
    `processing:request` (`optimize` stays refused).
  - Each run takes up to 200 of the library's files that are whole
    single-file originals. It queues a preview job only for a file the
    schedule's requester may read, with pictures and a known duration
    (core `previews::layout`), whose revision has no active job and no
    published previews. The partial unique index enforces that too.
  - Runs stop at the preview queue bound (100 active) and resume from a
    durable cursor (`schedule_dispatch_progress`), like
    `metadata_refresh`.
  - The Sources page offers "Generate previews" as a schedule task.
- **Core/adapter boundary:** eligibility uses core `previews::layout`; the
  job lifecycle, publication and retention are part 1's. The adapter only
  selects candidates and enqueues within the bounds.
- **Focused evidence (2026-10-13, Kyoto compile, direct execution):**

  | Check | Result |
  |---|---|
  | `tests/v2_metadata_processing.rs` `preview_schedules_queue_each_revision_once_within_the_queue` | passed. Accepted through the API. A run queues exactly the two eligible originals, skipping one with published previews and one without pictures. A second run adds nothing. With 99 other active jobs a run queues one and keeps its cursor; the next run, with room, queues the other and clears the cursor |
  | `v2_metadata_processing` 19, compose 22 (the form offers `preview` but not `optimize`), `m3_workflows` 2 | passed; clippy clean |
- **Review (GPT-6.1 Sol):** round 1 at `40d2d1a` raised one P2 finding,
  fixed:
  - **Starvation:** a preview schedule stopped by the full queue kept its
    old due time, so 100 such schedules took every tick's 100 dispatch
    slots and starved scan and metadata schedules. Core
    `maintenance::next_run` now decides a claimed schedule's next due time
    from its outcome: done means its interval, more work means at once, and
    blocked means a 5-minute backoff, never later than its interval, with
    its cursor kept. Core unit test. The integration test checks that a
    blocked schedule backs off and resumes. With 100 queue-blocked preview
    schedules due first, a later-due scan schedule is still dispatched
    within two ticks. Core 169, `v2_metadata_processing` 19,
    `m3_workflows` 2 passed; clippy clean.

  Round 2 at `7268760` is clean: no actionable findings.

### 6C (part 3). Server thumbnails in the player

- **Source SHA:** branch `plan-completion/player-previews`, stacked on
  `plan-completion/preview-schedules`.
- **Implemented (`packages/ui-bridge/player.js`):**
  - `previewFrame(manifest, seconds, origin)` is pure, mirroring core
    `previews::tile_at`. It returns the thumbnail at or before a position
    as its sheet and crop. It returns nothing beyond the last thumbnail or
    for a manifest that is not ready, is malformed, or points at a sheet
    outside this server's route.
  - When a player opens, `attachPreviews` reads `getTimelinePreviews` and,
    when ready, registers a Demuxe `AuthoredPreviewProvider` (highest
    priority) in the player's preview lane.
  - Demuxe fetches authored preview images without credentials, and the
    sheet route requires the session. So each sheet is fetched once with
    the session and handed to Demuxe as a local `blob:` URL with the crop.
    A failed sheet is retried on a later lookup, and all sheets are
    revoked when the player is disposed.
  - Optional throughout: without ready previews, or on any failure, the
    player keeps its own previews.
- **Core/adapter boundary:** the position-to-thumbnail rule is core
  `tile_at`, mirrored and tested in the bridge. Fetching and handing over
  sheets is presentation glue.
- **Evidence (2026-10-13):**

  | Check | Result |
  |---|---|
  | `node --test packages/ui-bridge` | 46 passed. New: crops at and between positions and on a second sheet; nothing beyond the end, for a pending or malformed manifest, or for a foreign sheet URL |
  | Electron 44.7.0 with the deployed Demuxe (manual harness: local server, `demuxe-player` on a sample MP4, `AuthoredPreviewProvider` returning a `blob:` sheet with a crop, then `preview.request` at 12 s) | Demuxe validated and returned the frame unchanged: path `motion-previews`, time 10 s, 160×90, `blob:` URI, crop at x=160. That is the result shape `attachPreviews` produces |
- **Not covered:** an automated end-to-end run of the online player
  showing thumbnails while scrubbing (needs the full server and player in
  a browser). The online player's other paths have the same gap; it
  remains an item 8 release check.
- **Review (GPT-6.1 Sol):** round 1 at `2a7b705` raised three P2 findings,
  all fixed:
  - **Replacement delay:** disposal awaited pending sheet downloads (up to
    10 s) before a version switch could resume playback. Release is now
    synchronous: downloads share an abort controller that is cancelled,
    and only object URLs already made are revoked.
  - **Superseded lookups:** a slow sheet blocked Demuxe's newer previews
    because the lookup ignored the request's abort signal. The lookup now
    settles as soon as Demuxe aborts it (`unlessAborted`), while the shared
    download continues for later lookups.
  - **Normal close:** `close()` never revoked the sheets. It now releases
    them like `disposePlayer`.

  Unit test: an aborted wait settles while its download continues;
  release cancels, revokes each made URL once, and returns without
  awaiting. ui-bridge 47 passed.

### Remaining item 6 work (not started)

| Slice | Next step | Blocker |
|---|---|---|
| 6D desktop workflow/focus/accessibility fixes | Exercise the new flows with keyboard and screen reader | None |
| 6E TV/cast receiver | Reviewed device/control contract, then one physical receiver proof | **Physical receiver hardware is not available to this agent.** Contract work can proceed; qualification cannot |

## 7. M6 and wider parity backlog

Not started. Optional external/commercial/social services are out of scope.

## 8. Release qualification

Not started. Runs after feature implementation, against frozen identities.
