# Playback, viewing and resume boundary qualification

Recorded 2026-11-02 against the working tree based on
`6edaa2d59822bdba1a1eb2c20843588a6fd7d22b`. This is per-boundary implementation
and evidence, **not blanket approval of all 73 audit entries**. Broader HTTP,
FFmpeg and final workspace gates remain the completion owner's responsibility.

## Final checked-in receipt — 2026-11-02 09:04 UTC

**Canonical full PASS: 77 passed, 0 failed, 0 ignored; exit 0 in 4,837.5 seconds
(80.625 minutes).** The exact command was `python3 scripts/ci/test_tier.py full`
on pinned Rust 1.98, unchanged debug profiles, two build jobs/two test threads,
and nocapture. Source was commit `5e73ec4f6c7428f41be3ca1c3cdf6033275201a0`;
`downloads_model.rs` SHA256
`74d5b6d3c69b2c30c5a6a135167b64c07b56d3b0dabf8012556a892c7e659efc`.
The nine `+= 1` test-only lint rewrites supersede the earlier c21f file hash.

Evidence: `/workspace/shared/motion-full-tier-budget.log` and the adjacent
`.resources.jsonl`. Target counts: delivery **11**, v2 playback **40**, access
model **7**, delivery model **6**, downloads model **9**, timeline-viewing model
**1**, work model **3**. All exhaustive models terminated with their unchanged
`GraphExhausted`, no-failure and zero-skipped assertions satisfied. Observed graph
counts exactly match the prior v1 full receipt below, including downloads
**4,765,164 states / 76,242,624 transitions** and timeline viewing
**1,890,681 / 52,877,296**. The two new downloads tests are the cold/eviction and
all-field equality regressions; no graph domain, ceiling or property was removed.

The final downloads target took **2,305.34 seconds**, versus 3,765.71 for v1.
Its observed per-process kernel VmHWM was **1,117,003,776 bytes**. Whole-run
sampled aggregate RSS peaked at **2,791,112,704 bytes**; terminal `getrusage` child
peak RSS was **2,668,851,200 bytes** (a different measurement). Minimum sampled
free disk was **2,648,027,136 bytes**; no reserve cutoff occurred. The unchanged
90-minute job budget was met locally with **562.5 seconds** to spare, without a
competing test/build workload. This measured local headroom is not a native CI
or cold-cache compile-time guarantee.

This final receipt supersedes all historical pending canonical-full and model
optimization execution entries below. The completion owner separately records
canonical fast, strict Clippy, formatting and publication-contract passes.
Native-platform CI, real VideoToolbox hardware and end-to-end desktop acceptance
remain distinct limitations, not claims inferred from Linux server/model passes.

## Evidence conventions

- **Executed**: this lane ran the named core test and inspected its result, or
  inspected the application log named below. All core runs reported here used
  pinned `rustc 1.98.0 (88d9e12ae 2026-08-18)`, low-debug profiles and
  `CARGO_INCREMENTAL=0`.
- **Owner-reported**: another lane's qualification records a run; this lane did
  not inspect a separate raw log for it.
- **Pending here**: test source exists, but this lane has no inspected execution
  result. It is not counted as passed merely because application compilation or
  a focused model passed.
- Every exploration below terminated `GraphExhausted`, with zero skipped checks.
  Exhaustion applies to the stated finite domain, not arbitrary unbounded traces.

### Focused production-core evidence

The seven added test targets contain 19 passing tests. The most recent behavior
changes were rerun in their affected targets; subsequent lint-only reordering and
an equivalent `10..=12` pattern do not introduce a new behavioral test claim.
Commands use `cargo test -p playscale-core --test <target> -- --nocapture`.

| Target / policy | Domain and configured ceilings (states / transitions / depth) | Observed graph |
| --- | --- | --- |
| `viewing_selection_model`: delivery admission | Seven delivery phases; owner/file/context combinations; 2 / 200 / 2 | 1 state / 112 transitions |
| Same: next timeline | 52 ordered candidates; visible at none, 0, 48, 49, 50, 51; page sizes 1, 49, 50, 51; 100 / 1,000 / 60 per configuration | Visible at 0: 5 / 20; each other configuration: 53 / 212 |
| Same: offline resume | Paused/ended positions 0, 1, 9,000, 9,999, 10,000; successful/failed checkpoint compaction and rebase including out-of-range 10,001; 1,000 / 20,000 / 20 | 55 / 990 |
| `organization_disclosure_model` | Three entry occurrences (two share a timeline), four visibility masks, five current-pointer choices; 100 / 1,000 / 5 | 20 / 180 |
| `delivery_selection_model` | Competing partial changes, two selections, selection generation 0–4, four generations maximum; 1,000 / 10,000 / 8 | 7 / 50 |
| `live_publication_model` | Up to four segments per variant; invalid bytes/duration/independence/start, low/main ordering, finish and gap; 10,000 / 100,000 / 20 per configuration | Uncapped 76 / 181; copied+capped 13 / 41; adaptive+capped 80 / 559 |
| `playback_binding_model` | Single interval and two-part layouts; 13 identity/revision/order/bounds/library/duration mutations each; 10 / 100 / 2 | 1 / 26 |
| `playback_selection_policy_model`: composition | Three operations, six composition flags, four bitrate observations; 2 / 1,000 / 2 | 1 / 768 |
| Same: multipart burn | Off/bitmap/text, absent/ordinal-0/ordinal-1 pin in each of two parts, missing/bitmap/styled first codec; 2 / 100 / 2 | 1 / 81 |
| `preference_authority_model` | Current-session authority, signed timestamps/revisions -1/0/1, exact IDs a/b; 100 / 5,000 / 5 | 36 / 1,296 |

Additional passing direct regressions in those targets cover reference admission,
32 generated-lineage combinations, preference API compatibility and normalization,
browser capabilities, track conflicts, backend/subtitle/provenance decisions,
full-profile codec identity, and the boxed replan wire representation.

The improved publication model compares **exact** `Ready` origin/start/peak,
`Segment` peak, `Wait`, `Failed`, begin and completion receipts. Its independent
oracle retains the whole accepted-byte history while production evicts its rolling
window; four segments cross the three-second window-retention boundary. Separate
traces cover two individually-too-short fragments whose combined qualifying window
exceeds the cap, expected copied start ±1 microsecond versus ±2/missing, the exact
64 MiB size boundary, and 6.499-second versus 6.500-second/zero durations.

### Inspected application evidence

`/workspace/shared/motion-lib-tests3.log` records **114 passed, 0 failed**:

- `delivery::tests::interrupted_segment_observation_retries_without_duplicate_window_or_ready`
- `playback_monitor::tests::sqlite_authority_loader_preserves_current_and_all_ties`
- `v2::content::lineage_tests::lineage_policy_matches_join_scope_for_stale_orphaned_and_generated_sources`

The cancellation test executes the same async qualification seam used by both
production observation loops, with a controlled pending probe. It cancels between
observations, verifies the entire qualification state is unchanged, retries a
4-second plus 2-second sequence under a 1.2 Mb/s cap, and checks exact Ready/Segment
receipts, peak 1,133,333, and repeated-index Wait without duplicate accounting.
It covers copied and noncopied output. This is **not** a claim of canceling a real
FFmpeg subprocess in that test; real worker/transport tests remain separate.

`motion-state-machine-check2.log` records an application `cargo check` pass before
later qualification-seam/Box changes. The subsequent lib-test log establishes
compilation and execution of the qualification seam. Final strict workspace
Clippy is not claimed passed here: iterations found and corrected the replan enum
size, test-module ordering, range pattern and orphan documentation comment; other
lanes' failures remained in the last inspected log.

## Canonical fast-tier update (inspected 2026-11-02)

`/workspace/shared/motion-fast-tier.log` supplies later execution evidence. The
**overall fast command failed** on three catalog tests and `mail_owner_model`;
those failures are not erased by the following scoped passes. The completion
owner is fixing/retesting them.

- Host `store` **26 passed** and `cache` **14 passed**, including both new
  checkpoint tests. This supersedes the earlier owner-reported-only qualification.
- Application library **115 passed**, including the three inspected adapter seams.
- `tests/server.rs` **46 passed**, including the three legacy rendition/preference
  tests named below, `playback_planner_tests::planner_preferences_conversion_reuse_and_active_job`,
  and `video_profiles_tests::video_profile_jobs_are_immutable_and_plans_match_exact_profile`.
- `tests/v2_organization_viewing.rs` **8 passed**, including every V03 HTTP test
  listed below. This closes that section's formerly pending focused adapter run.
- V02 `next_timeline_searches_past_hidden_pages_without_crossing_groups` **passed**
  individually; the encompassing catalog target was not wholly green in this log.
- The updated `playback_session_model` test
  `plan_admission_ownership_and_replans_survive_revocation_expiry_and_source_change`
  **passed** with explicit binding observations. Its configured ceiling remains
  2,000,000 states / 60,000,000 transitions / depth 100; this non-nocapture log
  does not report its explored counts, so none are invented here.
- All seven newly added core targets passed again in the canonical run. The
  focused counts above remain the inspected nocapture evidence for their bounds.

At that earlier fast-tier checkpoint, the full-tier targets had not run. The update above supersedes the earlier pending status of the
specific fast-tier tests below, not unrelated checks or whole audit completion.

## Canonical full terminal receipt (2026-11-02 07:34 UTC)

`/workspace/shared/motion-full-tier-final.log` completed **exit 0**, with
**75 passed / 0 failed / 0 ignored** across all seven full-only targets. It used
pinned Rust 1.98, two build jobs/two test threads, nocapture, the checked-in
`SharedDownloads` representation, and the unchanged canonical full-tier command.
All exhaustive tests asserted `GraphExhausted`, no property failure and zero
skipped checks; seeded runs separately asserted their configured completion.

| Full-only target | Passes | Observed exhaustive graph / additional bounds |
| --- | ---: | --- |
| delivery | 11 | Real FFmpeg HTTP tests, corrected fixtures |
| v2_playback | 40 | Real HTTP/FFmpeg tests; Linux hardware limitation remains |
| access_model | 7 | 674,980 states / 12,731,599 edges |
| delivery_model | 6 | Lifecycle 586,804 / 32,957,279; window 329,040 / 9,193,372; playhead 2,611 / 107,505; seeded 3,000 cases / 450,000 transitions |
| downloads_model | 7 | 4,765,164 states / 76,242,624 transitions; exact original domain, representation parity included |
| timeline_viewing_model | 1 | 1,890,681 states / 52,877,296 transitions |
| work_model | 3 | 576,828 states / 15,254,640 edges; seeded 2,000 cases / 200,000 transitions |

The resource receipt `motion-full-tier-final.resources.jsonl` records 6,306.2
seconds total, sampled owned-session peak RSS **2,791,567,360 bytes**, terminal
`getrusage` child peak RSS **2,669,228,032 bytes**, and minimum sampled free disk
**2,653,749,248 bytes**. Session sampling sums simultaneously live descendants;
`getrusage` peak is a different measure and is not substituted for aggregate RSS.
No disk reserve cutoff occurred. Downloads finished in 3,765.71 seconds.

This closes the pending canonical full qualification below, but **105.1 minutes
exceeds the existing 90-minute CI job budget** on this machine. Separate scratch
cache/equality optimizations were benchmarked without changing this tested source;
they are not incorporated or covered by this full success. Native platform CI and
hardware/desktop limitations are still distinct gates.

## Final-candidate gate update (2026-11-02 05:35 UTC)

The corrected complete real-media targets are now **passed**, not pending:
`/workspace/shared/motion-playback-fixed.log` records `delivery` **11/11** and
`v2_playback` **40/40**, supervisor exit **0** (235.4 seconds total). This includes
all five previously failed cases, exact cue/timestamp and GOP assertions, V01
rebind authority, R07 exhausted preference revisions, and online resume routes.
`motion-playback-fixed.resources.jsonl` retains owned-session CPU/RSS and disk
samples; the 700 MiB reserve was never reached. Only test fixture packet timestamps
were normalized; no production admission gate was relaxed. The hardware-specific
test can return early without VideoToolbox on Linux: this is not hardware playback
qualification or a full end-to-end desktop acceptance claim.

The completion owner reports clean canonical fast gate exit 0 with **899 passed,
0 failed, 0 ignored** across 96 summaries in `motion-fast-tier-final.log`, and
strict Clippy pass. The earlier initial fast failures have therefore been fixed
and retested. The initial full command is still running its heavy models; its
known fixture failures remain in its historical log. A separate clean canonical
full command against the final source snapshot is still pending. These later
actual results supersede the per-ID historical pending fast/media items below,
not the remaining canonical full gate or hardware/desktop limits.

## Initial canonical full-tier and fixture diagnosis

The first canonical full-tier run uses `CARGO_BUILD_JOBS=2`,
`RUST_TEST_THREADS=2`, and a wrapper that checks disk space every two seconds and
terminates only its owned process group below 700 MiB. Log:
`/workspace/shared/motion-full-tier.log`. No cleanup runs while it is active.

The initial real-media targets completed as follows (remaining full-only models
were still running when this section was written):

- `tests/delivery.rs`: **8 passed / 3 failed**. The copy fixture itself failed its
  pre-admission zero-start assertion (AAC at -0.021 s); the open-GOP control was
  refused by the existing source timestamp gate; the offset subtitle fixture
  produced 1.021/40.521 s cues instead of its asserted 1.000/40.500 s.
- `tests/v2_playback.rs`: **38 passed / 2 failed**. `copied_manifests_declare_the_source_rate`
  and `stream_copy_routes_are_planned_observed_and_fall_back_to_conversion` selected
  conversion rather than copy for the same negative-start fixture family.
  Adaptive/capped conversion, interval/multipart, HDR, prepared audio, subtitle,
  original resume, viewing rebind and exhausted-preference tests passed in this
  run. VideoToolbox success is **not** inferred: that test explicitly returns
  without exercising hardware when the platform has no encoder session.

A bounded experiment, before invoking application code, preserved the exact
FFmpeg argv and full FFprobe facts in
`/workspace/shared/motion-fixture-probe-20261102/fixture-probe.json` (eight fixtures).
FFmpeg 7.1.5 produced AAC start -0.021 s and AC3 start -0.005 s; the test-only audio
bitstream filter `setts=pts=PTS-STARTPTS:dts=DTS-STARTDTS` produced exactly zero
for audio and video while leaving the video bitstream/GOP settings unchanged.
For the 5-second-offset subtitle fixture, it changed the observed container start
from 4.979 to 5.000 s and the independently extracted VTT cue from 1.021 to
1.000 s. This establishes fixture facts, not permission to loosen production
eligibility.

Only those fixture generation commands were normalized in `tests/delivery.rs`
and `tests/v2_playback.rs`. Pre-application exact stream-start assertions were
added where missing; all route, GOP, timestamp, manifest and cue assertions remain.
**Production code is unchanged by this correction.** Both entire real-media targets subsequently passed in `motion-playback-fixed.log`,
as recorded above; the historical failing run remains retained.

## Per-audit-ID dispositions

### V01 — Viewing delivery admission

- **Production**: `playback_session::{ViewingDelivery, admit_viewing_delivery}`
  checks typed reducer phase, principal/file authority and exact existing
  profile/timeline. `delivery::live_viewing` supplies one locked live snapshot;
  `v2/viewing::{create_viewing, replace_viewing_delivery}` executes the decision.
  Foreign/unavailable delivery rejection precedes context mismatch disclosure.
- **Executed**: 112-transition admission model above.
- **Executed, media log**: `tests/v2_playback.rs::viewing_authority_rebinds_fences_overrides_and_resolves_release_order`,
  extended with a wrong-timeline live-delivery rejection using the correct ETag;
  `original_playback_seek_resume_and_ordered_viewing`.
- **Retained adapter boundary**: SQLite reauthorization/transactions, live-session
  lookup and reducer lock, HTTP status mapping, durable session writes.

### V02 — Next readable timeline across hidden pages

- **Production**: `timeline_viewing::next_timeline` returns exact `Found`,
  `NeedMore` or `Exhausted`. `v2/viewing::next_timeline` retains one read snapshot
  and keyset-pages `(order_position,id)` in batches of 50 within the same group.
  A hidden full page no longer means exhaustion. Non-404 observation errors
  propagate instead of being treated as hidden catalog members.
- **Executed**: six search configurations and page-boundary graphs above.
- **Executed, fast log**: `tests/v2_catalog.rs::next_timeline_searches_past_hidden_pages_without_crossing_groups`
  tests a readable successor after 49, 50 and 51 hidden rows and group exhaustion.
- **Retained adapter boundary**: ordered SQL enumeration, current visibility
  observations, snapshot lifetime and response materialization.

### V03 — Organization references and current disclosure

- **Production**: `organization::{Reference, reference_admitted, disclose_items,
  disclose_entries}` is called by `src/organization.rs` reference validation and
  `v2/organization.rs` ordinary and historical-response projection. Saved filters
  retain exact profile ownership; retired work is rejected; current pointers name
  a retained exact entry occurrence, never another occurrence of the same timeline.
- **Executed**: 20-state disclosure model and direct reference-admission cases.
- **Executed, fast log**: `tests/v2_organization_viewing.rs`:
  `collections_validate_and_never_disclose_members_outside_the_scope`,
  `replays_are_redacted_for_the_current_catalog_scope`,
  `queues_round_trip_seeds_and_hide_the_current_entry_outside_the_scope`, and
  `playlists_keep_entries_outside_the_scope_in_place`.
- **Retained adapter boundary**: fetch liveness/ownership/visibility facts, SQL
  mutation atomicity, receipt storage and preservation of original JSON fields.
  Existing unrestricted-scope projection semantics are not silently tightened.

### R01 — Live output qualification and adaptive publication

- **Production**: `delivery::publication::{Publication, Policy, Segment, Decision}`
  owns ordered verification count, rolling byte window, failed/end state, size,
  duration, independence, copy-start alignment, paired-variant publication and
  paired completion. `src/delivery::{observe, verify_low}` supplies observations
  and executes reducer inputs. Both use `qualify_observed_segment`, committing
  qualification only after all asynchronous observations finish.
- **Executed**: improved exact-output/rolling-window models and boundary traces;
  the inspected application cancellation/retry test named above.
- **Executed, media log**: `tests/v2_playback.rs::{bitrate_caps_are_kept_by_capped_conversions,
  adaptive_conversions_publish_aligned_capped_variants,
  stream_copy_routes_are_planned_observed_and_fall_back_to_conversion}`.
- **Retained adapter boundary**: filesystem reads/free-space observations,
  playlist/MP4 parsing, FFprobe and FFmpeg, cached video-track parsing, worker
  cancellation, reducer locks and actual byte serving. Oversized metadata avoids
  the unnecessary IDR probe before core refusal. Models do not replace real
  capped/adaptive transport qualification.

### R02 — Exact playback binding

- **Production**: `playback_session::{PlaybackBinding, BoundFile}` replaces the
  old `Observed.bound` boolean. Core matches exact ordered identities, binding
  revisions, intervals, part numbers/counts, multipart current revisions,
  durations and source library. `v2/playback::observe_binding` gathers raw rows
  for admission preflight, transaction reauthorization and replanning.
  `parts::{observed_part, layout}` supplies shared layout semantics; negative
  stored endpoints are invalid, not converted into absent endpoints.
- **Executed**: 26-transition interval/multipart mutation model and targeted
  `playback_session::tests::plan_admission_rechecks_every_pinned_fact`.
- **Executed**: existing `playback_session_model` integrated fast-tier rerun
  with actual binding observations; media-log HTTP `interval_bindings_play_their_clip_of_the_file`,
  `multipart_versions_play_their_parts_in_order`, and
  `multipart_boundaries_follow_pinned_part_durations`.
- **Retained adapter boundary**: SQL row gathering in the caller's transaction,
  finite duration conversion and signature verification. Preparation/download
  eligibility remains a distinct policy, rather than incorrectly imposing whole
  original-file preparation rules on interval/multipart playback.

### R03 — Partial-replan selection identity

- **Production**: `delivery::Input::Change.expected_selection` carries the exact
  pending-or-active generation read before asynchronous merging. The reducer
  rejects a stale basis, even when an intervening generation has an equal pin.
  Both v1 `change` and v2 `change_live` pass this identity; adapter pin-equality
  rejection closures no longer own the rule.
- **Executed**: seven-state competing-change model. The strict-Clippy fix boxes
  the replan `Pin`; `boxed_replan_preserves_the_serialized_command_contract`
  verifies unchanged tuple/object JSON and deserialization roundtrip.
- **Executed**: initial full-tier `delivery_model` all six tests passed
  (1,359.82 seconds); replacement/replan HTTP cases passed in the media log. The focused model proves the core rejection, not thread scheduling
  in a real asynchronous request race.
- **Retained adapter boundary**: catalog/probe awaits, mutex serialization,
  idempotency receipt lookup and effect execution under that same lock.

### R04 — Valid selection before admission/replan

- **Production**: `playback_session::validate_selection` composes interval,
  multipart, bitrate, adaptive, hardware and burned-subtitle constraints and
  returns the effective cap. Planning, first admission and replanning call it.
  `delivery::{backend_supported, burn_selection_supported, admit_sidecar}` and
  `parts::burn_layout` consume platform/codec/attachment/identity facts. Initial
  admission and replanning both enforce multipart bitmap pin shape; later-part
  codecs use the existing core bitmap codec predicate.
- **Executed**: 768-composition and 81-multipart-burn cases; backend, attachment
  count, sidecar identity and codec regressions.
- **Executed, media log**: HTTP bitmap/styled/text/sidecar, interval, bitrate and
  adaptive tests, including `bitmap_subtitles_are_burned_across_parts`.
- **Retained adapter boundary**: observe actual platform/filter/backend state,
  read exact sidecar registration and stream ordinals, reserve execution capacity,
  validate source descriptors and render commands. Existing core live-route,
  presentation, bitmap and rate-cap rules are reused, not reimplemented.

### R05 — Compatibility, tracks and subtitle composition

- **Production**: `playback::browser_support` consumes actual codecs/HDR/container
  and client claims. `playback_session::{compose_tracks, retry_with_burn,
  resolve_optional_subtitle}` governs conflicting component/track identities and
  route-dependent retry/optional policy. Existing `plan`, `automatic_subtitle`
  and presentation helpers still choose routes/subtitles. Legacy planning calls
  `legacy_processable`, `within_average_budget`, `local_output_contract` and
  `legacy_recipe_matches`; recipe and full profile now come from the same verified
  completed local job, and full profile comparison retains H264/HEVC identity.
- **Executed**: browser support/track conflict, 32 local provenance combinations,
  subtitle/backend and complete-profile codec regressions.
- **Executed**: media-log automatic/sidecar/bitmap/styled subtitles, prepared
  audio, HDR and multipart cases in `v2_playback`; fast-log legacy
  `tests/server.rs` rendition tests. No isolated SQL regression for competing legacy jobs' profile provenance
  is claimed by the focused core test.
- **Retained adapter boundary**: database stream/registration loading, capability
  probe I/O, extracting/rendering subtitles and reacquiring a transaction after
  observation. Core decides whether an additional burn attempt is appropriate;
  the adapter performs that attempt and supplies its result.

### R06 — Generated-file authorization lineage

- **Production**: `renditions::{LineageMembership, library_scope}` derives original
  direct memberships or generated memberships only through registered/current
  output and original-source revisions. `v2/content::file_facts` supplies raw
  lineage, including missing source facts; it no longer returns scope selected
  solely by a revision-filtering authorization join.
- **Executed**: 32 core combinations; inspected application SQLite conformance
  `lineage_policy_matches_join_scope_for_stale_orphaned_and_generated_sources`
  compares the production loader with the prior efficient join for 64 combinations.
- **Executed, fast log**: content-ticket/generated-rendition HTTP regressions,
  including `tests/server.rs::rendition_sessions_survive_offline_original_but_reject_replaced_source`.
- **Retained adapter boundary**: SQLite availability/registration/membership
  observations. Source availability is intentionally not substituted for source
  revision: already generated media retains prior offline-original semantics.

### R07 — Preferences and monitor authority

- **Production**: `playback_session::normalize_preferences` has explicit Legacy
  and V2 compatibility policies, used by both preference adapters. It preserves
  ten versus sixteen languages, forced-subtitle and legacy threshold/cap behavior.
  Both adapters use checked `revision::advance`; v2 exhausted revisions cannot
  overflow. `playback_monitor::prefer_viewing` orders recorded current-session
  authority, signed update time, revision, exact ID. One streamed SQLite loader
  is used by both telemetry flush and v2 monitor projection.
- **Executed**: preference compatibility/normalization cases; 36-state authority
  model; inspected `sqlite_authority_loader_preserves_current_and_all_ties`.
- **Executed**: media-log `exhausted_preference_revision_never_wraps_or_overwrites`,
  and fast-log legacy `playback_preferences_validate_revision_and_remain_profile_local`.
- **Retained adapter boundary**: wire decimal parsing, authorization and CAS/write
  transactions, one-statement SQLite snapshot/streaming, telemetry sampling and
  persistence acknowledgements. Authority selection consumes actual rows, not a
  preselected SQL winner.

### D05 — Durable offline resume across compaction

- **Production portion owned here**: `timeline_viewing::resume_position` with
  `ResumePolicy::Offline` is shared by host open, checkpoint and rebase. Exact
  paused-at-duration remains paused there; ended resumes at zero. Public
  `Store::acknowledge` writes the manifest checkpoint durably before deleting any
  acknowledged event; direct callers cannot bypass that order.
- **Executed**: 55-state success/failure/rebase model.
- **Executed adapter evidence**: inspected canonical fast log records host
  `store` 26 + `cache` 14 passes after final host changes. The store target includes
  `paused_at_duration_checkpoint_survives_compaction_restart_and_rebase` and
  `failed_checkpoint_persistence_does_not_compact_the_event_log`.
- **Retained adapter boundary**: durable rename/fsync, exclusive cache ownership,
  server acknowledgement parsing and actual event-log replacement. Broader
  causal identity/cache publication contracts belong to the storage lane.

### I04 — Online resume selection

- **Production portion owned here**: the same `resume_position` policy is used
  by `src/presentation.rs` in Online mode and by offline host/store paths in
  Offline mode. Online watched/final-one-second restart semantics are explicit;
  offline exact-duration paused semantics are not accidentally overwritten by
  compaction or rebase. Zero storage normalization does not change interpreted
  resume position.
- **Executed**: resume boundary unit cases and the shared 55-state model; host
  execution is inspected as specified under D05.
- **Executed, media log**: `original_playback_seek_resume_and_ordered_viewing`.
  End-to-end desktop acceptance is not claimed by this server HTTP test.
- **Retained adapter boundary**: newest retained event selection and manifest
  persistence, profile viewing reads and server/manual watched facts.

## Remaining verification boundary

All listed production policies are invoked by real callers. SQLite, filesystem,
processes, locks and protocol translation remain adapters deliberately. A model
pass establishes only the enumerated policy/workflow outcomes. Application lib
passes establish the three named adapter seams above; the real-media targets
separately passed all 51 tests. The final canonical full aggregate remains a
separate gate; hardware/desktop limits above are not erased by server test passes.

## Full-only graph resource correction (historical development evidence)

The initial full run's `downloads_model` process was killed by signal 9 after
its five direct tests passed; it did **not** establish exhaustive success. The
completion owner observed memory pressure immediately before that exit. The
remaining full-only targets were allowed to continue (`--no-fail-fast`).

The model-only `SharedDownloads` wrapper in `crates/core/tests/downloads_model.rs`
retains every field of the original `World`, interning equal immutable View,
record/log, lineage, download and first-event components. `Rc` equality and hashing
compare contents, not allocation identities. Original production transitions,
checks, inputs and JSON codec remain delegated unchanged; every full-graph edge
asserts exact successor unpacking. No production type, dependency, input domain,
search ceiling or property was removed. Stored state is 112 inline bytes rather
than 512, plus interned components shared across states. This is not by itself a
claim about measured peak RSS or completed full-domain exploration.

`motion-download-model-representation.log` records six direct tests passed, with
the expensive graph explicitly filtered out. The new reference-versus-wrapper
check compares every state, action, output, disposition, property result and
state/input/output codec over a five-level breadth-first prefix. Running the
actual enumerator on both representations gives identical **2,453 states /
8,768 transitions / DepthBound / zero skipped checks**. The full run still uses
**12,000,000 states / 200,000,000 transitions / depth 100**, requiring
`GraphExhausted` and zero skipped checks. Independent review and full runtime evidence subsequently passed, as recorded
in the canonical terminal receipt above. The scratch cache experiments are not
part of the proven checked-in representation.

## Reviewed model-only CPU optimization (2026-11-02 07:36 UTC)

After preserving the successful v1 full proof, independent review cleared a
model-only two-entry exact-content LRU cache of materialized Worlds, borrowed
intern lookups, and allocation-free cross-representation equality. The latter
exhaustively destructures every World field (no `..`), compares the same facts as
`unpack() == World` on every edge, and is checked on every actual unpack. New tests
mutate each of the 21 fields independently and exercise cold/reordered callbacks
and interleaved unrelated-state eviction. Cache misses always reconstruct the
exact World; input enumeration, production step and property checks are unchanged.

All **8 non-heavy tests passed**, full graph explicitly filtered, in
`motion-download-model-cache-tests.log`; original-versus-shared enumerator prefix
remains **2,453 states / 8,768 transitions / DepthBound / zero skipped checks**.
The initial integrated file SHA256 was
`c21f88758046c4fc7c376528f6bc340bd0b83a580d80370f6369daf0c9fb618b`;
the final lint-only revision and successful full run are recorded at the top.

Separate scratch depth-nine benchmarks preserved identical **373,992 states /
2,120,768 transitions / DepthBound**: original owned model 45.85 s, v1 shared
120.49 s, two-entry FIFO cache 88.70 s, LRU-only 97.85 s, reviewed direct-equality
LRU 69.21 s. These are contention-sensitive local measurements, **not a proof of
CI runtime or final full-domain success for the new wrapper**. The original v1
full receipt remains valid for its recorded source. At that checkpoint the new wrapper's strict lint and canonical full/budget
gate were pending; both subsequently passed as recorded at the top. No profile, timeout, domain,
ceiling, property, production or dependency changes were made.
