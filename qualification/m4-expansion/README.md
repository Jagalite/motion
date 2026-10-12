# M4 playback expansion ledger

Owner: M4 implementation lane (A06/A07 with A08). Branch
`m4-playback-expansion`, PR #22. Input: main `b5d9ced`. Work started from
`8d6dd7f`, the baseline named in plan section 10, and was rebased onto
`db0d05c` and then `b5d9ced`. Those rebases brought in UI, prototype, plan and
dependency changes. The only dependency change this lane uses is Statelessness
0.2.0, and the models were rerun under it. Independent reviewer: GPT-6.1 Sol.

This ledger records the **completed declared backend slice** of M4 and its
evidence. It is not release qualification and not M4 closure.

**Declared slices:**
- **M4-00:** the inventory.
- **M4-01:** stream copy and VideoToolbox conversions in v2.
- **M4-02:** sidecar subtitles, bitmap burn-in, automatic subtitle policy and
  rendition audio continuity.
- **M4-03:** intervals and multipart versions.
- **M4-04:** bitrate caps.
- **M4-02 (follow-up):** text subtitles on live routes as an in-manifest HLS
  subtitle rendition.
- **M4-05:** a two-rung adaptive ladder for software conversions, off by
  default.

**Explicit deferrals:**
- **M4-04:** HDR tone mapping. No available FFmpeg has the filters it needs
  (below).
- **Subtitles:** text and styled burn-in (libass), subtitles on multipart
  versions, bitmap cues older than the 20 s lookback.

SHAs below are on the current branch, after the rebase onto `b5d9ced`.

## M4-00 capability and parity inventory

States:
- **Implemented:** code and focused server tests exist at the recorded source.
- **Qualified:** a release receipt exists for the exact delivered bytes.
- **Refused:** the route reports an explicit unsupported response.
- **Missing:** no implementation exists.

"Before" is the input main. "After" is this branch. Nothing new here is
qualified; the release checks are listed below.

| Capability | Before | After (state / v2 exposure) |
|---|---|---|
| Original byte ranges | Implemented; Chromium/Electron receipts in [FINAL](../consolidation/FINAL.md) | Unchanged |
| Prepared renditions | Default streams only; any track pin excluded every rendition | Implemented: a rendition serves a plan when it carries exactly the selected audio (recorded preparation audio of the same source revision); a pinned rendition version plays its own defaults |
| Live software transcode | ≤720p; any bitrate cap refused | Implemented: capped conversions keep a client peak bitrate (VBV, measured over every HLS peak window) |
| Live remux / audio-only conversion | v1 only, behind `processing.experimental_copy_routes` | Implemented in v2 Auto when enabled (off by default): core-decided, details observed lazily; exact-keyframe seeks fixed; the manifest declares the source's rate (probed average or measured peak, an estimate) |
| Live hardware transcode | v1 only | Implemented in v2 when `processing.experimental_live_hardware` (off by default): available/busy/unavailable observed; a failed or busy session converts in software |
| Interval (multi-episode) bindings | Refused | Implemented: conversion of the interval only; logical time from the interval start |
| Multipart versions | Refused | Implemented: conversion only; parts decoded and joined (concat filter), each held to its pinned duration; ≤7 parts, one library, same picture size; the selected audio is each part's own occurrence of the audio component (refused when a part has none); no subtitles |
| Embedded plain-text subtitles | Original byte route only; `require` took the first track | Original byte route only (WebVTT); `require` and `auto` choose by profile languages and roles |
| SRT/WebVTT sidecar subtitles | Refused | Implemented on original byte delivery, component-pinned and fingerprint-checked |
| Bitmap subtitles (PGS, DVD, DVB) | Refused | Implemented as burn-in on conversions for clients listing `burned` (the production bridge now lists it whenever it plays HLS); canvas scaled to the video; a 20 s lookback restores cues active at a start, and **cues displayed more than 20 s before a start or seek are not restored** |
| Client bitrate caps | Refused | **API-only:** the server keeps `max_bitrate_bps` (see M4-04), but no product client sends one. The bridge sends `null`, and no profile preference, contract preference field or player control supplies a value. Follow-up owners in the deferral table |
| Text subtitles on live (HLS) routes | Refused | Implemented for clients listing `hls_text`: the master playlist declares a WebVTT `SUBTITLES` rendition whose playlist mirrors the media window, with cues cut per segment in generation media time (offset by an interval's start). The bridge lists `hls_text` with HLS and selects the in-manifest track (Demuxe `selectSubtitleTrack`), never an external track on live: Shaka 5.2.11 throws 4033 (`CANNOT_ADD_EXTERNAL_TEXT_TO_LIVE_STREAM`) from `addTextTrackAsync` for infinite-duration presentations. Clients without `hls_text` are still refused |
| Styled subtitles (ASS/SSA) and text burn-in | Refused | Still refused: burning text in, including styled subtitles with their fonts and attachments, needs libass, absent from the bundled and host FFmpeg |
| Subtitles on multipart versions | Refused | Still refused (`subtitle_multipart_unavailable`): subtitle streams and sidecars belong to one part's file |
| HDR preserve | Byte delivery to clients reporting HDR support | Unchanged (attempt, not a display-fidelity receipt) |
| HDR tone-map to SDR | Refused | Still refused (deferral below) |
| Adaptive bitrate | Missing | Implemented behind `processing.experimental_adaptive_bitrate` (off by default): an uncapped single-file software conversion without burn-in is one FFmpeg process encoding a 720p rung (3 Mb/s cap) and a 360p rung (800 kb/s cap) with shared keyframes; segment *n* is published only when both rungs have it; two execution units reserved; `BANDWIDTH`, `CODECS` and `RESOLUTION` per rung. Copies, hardware, capped, burned and multipart plans stay single variant |

Fixtures are generated in the tests: FFmpeg `lavfi` media, a byte-level PGS
stream, and SRT, ASS and noise sources. The repository stores no media
fixtures. There is no HDR fixture with tone-mapping ground truth.

## Work packages

| ID | Source SHAs | API / client changes | Supported / refused | Focused checks | Review (GPT-6.1 Sol) |
|---|---|---|---|---|---|
| M4-00 | ledger | None | Inventory above | Code inspection | Final PR diff: 3 P2 (multipart HDR, binding set, copy bandwidth), fixed in `d17556a`; its review: 1 P2 (estimate from output streams), fixed in `0ab47ce` |
| M4-01 copy | `fdc70b6`, `c1b74e9`, `678fcf7`, `3dcb7a2` | Plans may return `remux` / `audio_convert` (existing enums); capabilities list `stream_copy_hls` when enabled; copied `CODECS` derived from each copy's init segment | Auto + HLS + within limits + qualified H.264 details; Convert, Original, failures, unqualified details or a disabled server convert or refuse | Core `copy_seek`, `copy_operation`; route-selection model; `stream_copy_routes_are_planned_observed_and_fall_back_to_conversion`; v1 `tests/delivery.rs` | 3 P2 (CODECS, probe bounds, probe ownership) and 1 P2 (authorize before probe), all fixed |
| M4-01 hardware | `2da18bd`, `6ef3b65` | Candidate prefix `h`; capabilities list `hardware_hls` when enabled and available | Available, unfailed session first; busy, unavailable or failed falls back to software; a plan signed before hardware was disabled cannot admit | `hardware_conversion_is_planned_when_available_and_falls_back_to_software` with `MOTION_REQUIRE_VIDEOTOOLBOX=1` (Apple encoder, no libx264 SEI; replan to software; capacity release; disabled restart) | 2 P2 (probe under the writer, enablement recheck), fixed |
| M4-02 subtitles | `c2aedf8`, `24a6686`, `38332a8`, `c06d4c2`, `abb45a6`, `f7e4d61` | Plan claims carry `Text`, `Sidecar{name,fingerprint}` or `Bitmap`; the client `burned` mode is honored; no contract or client file changes | See inventory | Core `subtitle_occurrence`, `automatic_subtitle`, `same_language`, `presents_subtitle`; HTTP sidecar, bitmap burn-in (decoded output), automatic selection, plain-text tests | P1 (live text unsupported by the player): live text reverted. P2s on transfer permits, digest, in-place edits, canvas, seek lookback, FFmpeg 6.1 compatibility, route-aware auto: all fixed |
| M4-02 renditions | `f7e4d61`, `8c732b5` | None | Rendition with the selected audio (or pinned version) by byte delivery; others convert | Core `carries_selection`; HTTP test with a real preparation of `a1` | 3 P2 (pinned version, audio-less, pool), fixed |
| M4-03 intervals | `8c732b5`, `e03488f` | Plans and pins carry the clip; `interval_requires_conversion` | Conversion only; out-of-file or unprobed bounds unavailable; a changed interval fences plans | Core `parts::layout`; model property; HTTP interval test (decoded frame at file 10.5 s, end-of-interval `ENDLIST`, fences, key reuse) | 1 P2 (digest), fixed |
| M4-03 multipart | `e03488f`, `5ca4766`, `5525506`, `6c1d504` | Plans and pins carry `parts`; `multipart_requires_conversion`, `subtitle_multipart_unavailable` | See inventory | Core `parts::locate`; model property; HTTP multipart test (Matroska part, part-2 frame after a seek, incompatible parts, live part change fails the delivery) and boundary test | 4 P2, then 3 P2, then 2 P2 (time bases, positions, audio identity, revalidation, boundaries, SAR, flags, A/V sync, canvas), all fixed |
| M4-04 caps | `0843d72`, `cfc18d0` | `bitrate_caps` capability; master `BANDWIDTH` is the cap | A conversion with at least 300 kb/s of video after audio and overhead; byte routes, copies and renditions still refuse caps | Core `rate_cap`, `peak_within_cap`; HTTP test on an ~18 Mb/s noise source capped at 1 Mb/s | 1 P2 (windows, fixed); 1 P2 (digest forms) not applicable: the only shipped form is preserved |
| M4-04 HDR | none | None | Tone mapping refused | `scale_vt` measured: retags PQ without tone mapping (mean luma unchanged) | n/a |
| M4-02 live text | `8335b3a` | Contract `subtitle_modes` adds `hls_text` (additive); bridge lists it with HLS and selects the in-manifest track; `subtitles_url` names the rendition playlist for HLS | Live text for `hls_text` clients; others refused | Core `subtitles` (WebVTT parsing, per-segment windows, playlist); HTTP plain-text test (rendition, playlist mirrors media window, cues at 2 s starts and on an interval); JS 56/56; headless Shaka 5.2.11 on the server's exact output: cue active at the expected times on VOD and rolling playlists | Clean |
| M4-05 | `5bc255f`, `857b238` | Capabilities report `adaptive_bitrate`; master lists a `low` variant (existing `variants/{variant_id}` routes) | See inventory | Core `adaptive`; HTTP `adaptive_conversions_publish_aligned_capped_variants` (identical segment lists, rung sizes, per-rung caps over every peak window, another device refused, rung retired with its delivery, capacity released, a client cap keeps one variant); `recovery_keeps_an_adaptive_encoders_two_execution_units`; headless Shaka 5.2.11 under a CDP throttle: 360p at 600 kb/s, 720p when restored | 2 P2 (restart recovery adopted the ladder's encoder at one unit; low-rung duration check stricter than core `fits_target`), fixed in `857b238`; re-review clean |

## PR #22 follow-up review (rebased onto main `c6d6e26`)

| Finding | Disposition | SHAs | Evidence | GPT-6.1 Sol |
|---|---|---|---|---|
| Multipart audio swap: same-language, same-role tracks reordered between parts played the wrong track | Fixed. The planner resolves the selected audio's component on part 1 (`components::component_of`), then that component's exact occurrence on every part (`per_file_tracks`), and pins each part's own ordinal (`PartPin::audio`). A part without it is refused (`multipart_audio_unavailable`); the worker fails closed without a per-part ordinal. Replans may switch audio (`parts::same_layout` compares only files, revisions and durations) | `112c51a`, `03f4133`, and the test fixture commit after them | Core mapping and layout tests. HTTP `multipart_audio_follows_the_component_in_every_part`: Matroska "Stereo" 440 Hz and "Director" 880 Hz in opposite orders; the default plays 440 Hz and a Director pin 880 Hz in both parts (measured), and a live delivery switches by replan | 2 P2 on `112c51a` (replan switch, serialization), fixed in `03f4133`; re-review clean |
| Production client cannot request burn-in; always sends `max_bitrate_bps: null` | Burn-in wired: the bridge lists `burned` whenever it plays HLS (the only transport a burned conversion arrives over). Bitrate caps recorded as **API-only**, with follow-up owners in the deferral table | `8a629e3` | `player.test.mjs` provider/MSE matrix; JS 55/55 | Clean |
| Live-subtitle statement too broad | Narrowed after inspecting Demuxe 1.1.1 (npm, pinned): Shaka 5.2.11 throws 4033 from `addTextTrackAsync` (the bridge's external-track path) only for infinite-duration presentations, while Demuxe lists in-manifest HLS `SUBTITLES` renditions. Motion then published one (M4-02 live text) | `8a629e3`, `8335b3a` | Package source inspection (`web/generated/internal/shaka-backend.js`, `web/vendor/shaka-player.js`); headless Shaka playback of the rendition | Clean |
| Keep subtitle and HDR limits visible | Inventory and deferral rows: styled/font handling, multipart subtitles, bitmap cues older than the 20 s lookback, client caps API-only, HDR implementation still owed after the native-tool change | `8a629e3` | This ledger, `V2_PLAYBACK.md` | Clean |

## Correctness boundary

**Core (`crates/core`) decides:**
- `playback_session::plan`:
  - route order and permitted fallbacks (byte → rendition → copy →
    hardware → software);
  - which routes may serve an interval, multipart, burned or capped request;
  - when copy details must be observed;
  - `presents_subtitle`, `rate_cap`, `within_cap`, `peak_within_cap`.
- `delivery`: `copy_operation`, `live_route_supported`, `copy_start`,
  `copy_seek` and `burnable_subtitle`.
- `playback_session::adaptive`: which plans are delivered as a ladder,
  carried in the signed claims and the delivery pin.
- `components`: `subtitle_occurrence`, `automatic_subtitle`, `same_language`.
- `subtitles`: WebVTT parsing, the cues of each segment window and the
  rendition playlist mirroring the media window.
- `parts`: `layout` and `locate`.

**Adapters:**
- Supply observations: stream facts, probes, sidecar fingerprints, hardware
  availability and capacity, preparation records, part rows.
- Recheck those facts inside the admission and replan transactions.
- Execute effects through FFmpeg.

**Adapter checks against measured output:**
- copied stream start;
- independent segments;
- copied codecs;
- per-window peak bitrate, for each adaptive rung;
- both adaptive rungs holding segment *n* before it is reported;
- sidecar fingerprint before and after conversion;
- live revalidation of every part.

**Stateless coverage (`route_selection_model`):** it exhausts 28,689 states
and 43,032 transitions. Its expected outcomes are stated independently, across:
- 3 modes;
- copy kinds and details;
- unavailable, busy and available hardware;
- burn-in, intervals and multipart;
- HLS, client limits, conversion capability, explicit streams and original
  support;
- repeated failure reports.

The playback-session and delivery models pass unchanged.

**Gaps:**
- Prepared renditions, subtitles, caps and the adaptive ladder are not in
  that model; unit and HTTP tests cover them. Ladder publication (segment
  *n* only when both rungs hold it) is an adapter conjunction feeding the
  core delivery's existing segment input, so the delivery models still
  cover what is published after it.
- Adapter probes, filters and segment measurement are covered by HTTP tests,
  not models.

## Build and test receipts

Builds ran through the shared Kyoto queue with Rust 1.98, in this worktree's own
target directory. Tests ran from the compiled binaries on macOS arm64, with
host FFmpeg 8.1.2 and VideoToolbox available. Receipts are at the PR head
after `857b238` (rebased onto main `c6d6e26`):

| Suite | Result |
|---|---|
| `playscale-core` lib | 120 passed |
| Core model and policy tests (`route_selection_model`, `playback_session_model`, `delivery_model`, `delivery_observation_model` and the other models) | Passed (Statelessness 0.2.0). `delivery_model`'s metadata now records a digest of `delivery.rs`, which outgrew the 64 KiB trace string limit |
| `tests/v2_playback.rs` | 25/25 (`MOTION_REQUIRE_VIDEOTOOLBOX=1`) |
| `tests/delivery.rs` | 12/12 |
| `tests/access_api.rs` | 32/32 |
| JavaScript (CI set) | 78/78 |
| Headless Shaka 5.2.11 (`scripts/check_hls_subtitles.mjs`) | Subtitle cues on VOD and rolling playlists; ABR 360p throttled, 720p restored |
| Pinned clippy, `-p playscale-core -p playscale --all-targets` (CI allow-list) | Clean, Kyoto `e58fb78db05a4992b07fd2d88f676276` |
| `cargo fmt --check`, contract checks (`check_contract.py`, `contract_json.rb --check`) | Pass |

## Deferred work and release checks

| Item | Owner | Requirement / expected outcome |
|---|---|---|
| Copied-stream, hardware-output and burned-subtitle playback | Clients + integration/release | Chromium and Electron on the qualified player: startup, far seek, audio switch and A/V sync. Required before `experimental_copy_routes` or `experimental_live_hardware` default on |
| Copied-stream audio start | Integration/release | After a seek within 132 ms of a keyframe, audio may start up to 132 ms after video |
| Capped and multipart/interval playback in players | Clients + integration/release | Seek across part boundaries; correct logical duration; `BANDWIDTH`-based startup |
| HDR tone mapping | Integration/release (native tools), then this lane | Add zimg (`zscale`) or libplacebo to the bundled FFmpeg. Then **this lane still has to implement** a tone-mapped conversion gated on the observed filter, and qualify it on suitable HDR/SDR displays. Until then HDR conversion stays refused |
| Live text subtitle and adaptive playback in product players | Clients + integration/release | Chromium and Electron builds: the subtitle rendition shown at startup, after far seeks and generation changes; ladder switching under real network changes. Verified so far with the pinned Shaka in headless Chromium only. Required before `experimental_adaptive_bitrate` defaults on |
| Text and styled (ASS/SSA) burn-in, fonts and attachments | Integration/release (libass in the bundled FFmpeg), then this lane | Burn text and styled subtitles with the file's attached fonts; until then refused |
| Subtitles on multipart versions | This lane | A per-part subtitle source (each part's occurrence of the subtitle component) joined on the timeline; until then refused |
| Bitmap cues older than the lookback | This lane | A cue displayed more than 20 s before a start or seek is not restored; restoring it needs reading the subtitle stream from the part's start (a second validated descriptor) |
| Client bitrate-cap source | API/integration owner (a preferences or quality-limit field in the contract) + UI lane (a player or profile control) + clients (the bridge sends it) | Today API-only: the server honors `max_bitrate_bps`, the bridge sends `null` |
| Preparation by audio component | Data/catalog + this lane | Preparation still refuses component pins; clients pin `a{n}` with the exact source |
| Wider adaptive ladders | This lane | Ladders for copies, hardware conversions and capped plans, more rungs, and an audio-only rung are not implemented; a ladder retains both rungs over the same window (about 1.3 times the storage of the main rung alone, by cap) |
| Physical A/V and performance budgets | Integration/release | Section 21.3 budgets with a frozen workload; WebKit remains deferred |

## Known pre-existing gaps found during this lane

Transcode manifests advertise a static `avc1.64001f` (High, level 3.1). The
recipe caps height at 720 but keeps the source frame rate, so 720p sources
above 30 fps encode at level 3.2. Copy routes derive their codecs from output
instead. Owner: this lane, with the player matrix, since changing
transcode manifest text affects existing player receipts.
