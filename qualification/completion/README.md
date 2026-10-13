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
| 3 Production playback and preparation | 3A and 3B implemented, tested, reviewed; 3C text across parts implemented and reviewed; 3D implemented and reviewed; rest of 3C, 3E and 3F not started | playback, part-subtitles and preparation-tracks PRs | Merge by user; then 3E |
| 4 Library-management UI | Implemented, tested, reviewed | library-ui PR | Merge by user |
| 5 Offline breadth | 5A implemented; 5B–5F not started | offline-roots PR | Sol review |
| 6 M5 convenience and receiver | 6A implemented, tested, reviewed; 6B–6E not started | markers PR | Re-review; merge by user |
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

### Remaining item 3 slices (not started)

| Slice | Next step |
|---|---|
| 3C bitmap cues before the 20 s lookback; burned and automatic subtitles across parts | Second validated descriptor for early cues; per-part burn inputs in the concat graph; automatic selection over components present in every part |
| 3E bitrate preference and UI control | Contract preference field, bridge and player control |
| 3F custom video-profile preparation UI | Validated form over `video_profile` |

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

### Remaining item 5 work (not started)

| Slice | Next step | Dependency |
|---|---|---|
| 5B offline removal | A removal action in the offline host that takes the cache lock and calls `Store::remove` | Host lifecycle (lock held by the running host) |
| 5C restore recovery | When sync answers `viewing_epoch_changed`, offer "re-base on the restored server" or "discard", as an explicit choice applied by the cache tool | Core rule for re-basing retained events onto a new history |
| 5D transfer state | Contract operation for device-reported progress, owned by the device; never inferred by the server | Contract review |
| 5E prepared and track-selected downloads | Manifest files for renditions, subtitles and artwork with pins | Item 3D (preparation by component) |
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


### Remaining item 6 work (not started)

| Slice | Next step | Blocker |
|---|---|---|
| 6B embedded chapter extraction | Probe `-show_chapters` at scan; record `embedded` markers per timeline revision | None |
| 6C server-generated previews | Bounded trick-play thumbnail job with publication validation, cancellation, retention and access control; then enable `preview` schedules | None |
| 6D desktop workflow/focus/accessibility fixes | Exercise the new flows with keyboard and screen reader | None |
| 6E TV/cast receiver | Reviewed device/control contract, then one physical receiver proof | **Physical receiver hardware is not available to this agent.** Contract work can proceed; qualification cannot |

## 7. M6 and wider parity backlog

Not started. Optional external/commercial/social services are out of scope.

## 8. Release qualification

Not started. Runs after feature implementation, against frozen identities.
