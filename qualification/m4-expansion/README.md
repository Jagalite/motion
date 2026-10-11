# M4 playback expansion ledger

Owner: M4 implementation lane (A06/A07 with A08). Branch
`m4-playback-expansion`, PR #22. Input: main `b5d9ced`. Work started from
`8d6dd7f`, the baseline named in plan section 10, and was rebased onto
`db0d05c` and then `b5d9ced`. Those rebases brought in UI, prototype, plan and
dependency changes. The only dependency change this lane uses is Statelessness
0.2.0, and the models were rerun under it. Independent reviewer: GPT-6.1 Sol.

This ledger records implementation slices and their evidence. It is not
release qualification and not M4 closure.

**Declared slices:**
- **M4-00:** the inventory.
- **M4-01:** stream copy and VideoToolbox conversions in v2.
- **M4-02:** sidecar subtitles, bitmap burn-in, automatic subtitle policy and
  rendition audio continuity.
- **M4-03:** intervals and multipart versions.
- **M4-04:** bitrate caps.

**Explicit deferrals:**
- **M4-04:** HDR tone mapping. No available FFmpeg has the filters it needs
  (below).
- **M4-05:** adaptive bitrate, as a release deferral.
- **Subtitles:** text subtitles on live routes and text burn-in.

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
| Multipart versions | Refused | Implemented: conversion only; parts decoded and joined (concat filter), each held to its pinned duration; ≤7 parts, one library, same picture size and audio roles; no subtitles |
| Embedded plain-text subtitles | Original byte route only; `require` took the first track | Original byte route only (WebVTT); `require` and `auto` choose by profile languages and roles |
| SRT/WebVTT sidecar subtitles | Refused | Implemented on original byte delivery, component-pinned and fingerprint-checked |
| Bitmap subtitles (PGS, DVD, DVB) | Refused | Implemented as burn-in on conversions for clients listing `burned`; canvas scaled to the video; 20 s lookback for cues active at a start |
| Styled subtitles (ASS/SSA), text on live routes, text burn-in | Refused | Still refused. Live text: Shaka 5.2.11 refuses external text on a live presentation. Text burn-in: needs libass, absent from the bundled and host FFmpeg |
| HDR preserve | Byte delivery to clients reporting HDR support | Unchanged (attempt, not a display-fidelity receipt) |
| HDR tone-map to SDR | Refused | Still refused (deferral below) |
| Adaptive bitrate | Missing | Release deferral (below) |

Fixtures are generated in the tests: FFmpeg `lavfi` media, a byte-level PGS
stream, and SRT, ASS and noise sources. The repository stores no media
fixtures. There is no HDR fixture with tone-mapping ground truth.

## Work packages

| ID | Source SHAs | API / client changes | Supported / refused | Focused checks | Review (GPT-6.1 Sol) |
|---|---|---|---|---|---|
| M4-00 | ledger | None | Inventory above | Code inspection | Final PR diff: 3 P2 (multipart HDR, binding set, copy bandwidth), fixed in `d17556a` |
| M4-01 copy | `fdc70b6`, `c1b74e9`, `678fcf7`, `3dcb7a2` | Plans may return `remux` / `audio_convert` (existing enums); capabilities list `stream_copy_hls` when enabled; copied `CODECS` derived from each copy's init segment | Auto + HLS + within limits + qualified H.264 details; Convert, Original, failures, unqualified details or a disabled server convert or refuse | Core `copy_seek`, `copy_operation`; route-selection model; `stream_copy_routes_are_planned_observed_and_fall_back_to_conversion`; v1 `tests/delivery.rs` | 3 P2 (CODECS, probe bounds, probe ownership) and 1 P2 (authorize before probe), all fixed |
| M4-01 hardware | `2da18bd`, `6ef3b65` | Candidate prefix `h`; capabilities list `hardware_hls` when enabled and available | Available, unfailed session first; busy, unavailable or failed falls back to software; a plan signed before hardware was disabled cannot admit | `hardware_conversion_is_planned_when_available_and_falls_back_to_software` with `MOTION_REQUIRE_VIDEOTOOLBOX=1` (Apple encoder, no libx264 SEI; replan to software; capacity release; disabled restart) | 2 P2 (probe under the writer, enablement recheck), fixed |
| M4-02 subtitles | `c2aedf8`, `24a6686`, `38332a8`, `c06d4c2`, `abb45a6`, `f7e4d61` | Plan claims carry `Text`, `Sidecar{name,fingerprint}` or `Bitmap`; the client `burned` mode is honored; no contract or client file changes | See inventory | Core `subtitle_occurrence`, `automatic_subtitle`, `same_language`, `presents_subtitle`; HTTP sidecar, bitmap burn-in (decoded output), automatic selection, plain-text tests | P1 (live text unsupported by the player): live text reverted. P2s on transfer permits, digest, in-place edits, canvas, seek lookback, FFmpeg 6.1 compatibility, route-aware auto: all fixed |
| M4-02 renditions | `f7e4d61`, `8c732b5` | None | Rendition with the selected audio (or pinned version) by byte delivery; others convert | Core `carries_selection`; HTTP test with a real preparation of `a1` | 3 P2 (pinned version, audio-less, pool), fixed |
| M4-03 intervals | `8c732b5`, `e03488f` | Plans and pins carry the clip; `interval_requires_conversion` | Conversion only; out-of-file or unprobed bounds unavailable; a changed interval fences plans | Core `parts::layout`; model property; HTTP interval test (decoded frame at file 10.5 s, end-of-interval `ENDLIST`, fences, key reuse) | 1 P2 (digest), fixed |
| M4-03 multipart | `e03488f`, `5ca4766`, `5525506`, `6c1d504` | Plans and pins carry `parts`; `multipart_requires_conversion`, `subtitle_multipart_unavailable` | See inventory | Core `parts::locate`; model property; HTTP multipart test (Matroska part, part-2 frame after a seek, incompatible parts, live part change fails the delivery) and boundary test | 4 P2, then 3 P2, then 2 P2 (time bases, positions, audio identity, revalidation, boundaries, SAR, flags, A/V sync, canvas), all fixed |
| M4-04 caps | `0843d72`, `cfc18d0` | `bitrate_caps` capability; master `BANDWIDTH` is the cap | A conversion with at least 300 kb/s of video after audio and overhead; byte routes, copies and renditions still refuse caps | Core `rate_cap`, `peak_within_cap`; HTTP test on an ~18 Mb/s noise source capped at 1 Mb/s | 1 P2 (windows, fixed); 1 P2 (digest forms) not applicable: the only shipped form is preserved |
| M4-04 HDR | none | None | Tone mapping refused | `scale_vt` measured: retags PQ without tone mapping (mean luma unchanged) | n/a |
| M4-05 | none | None | Not advertised | n/a | n/a |

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
- `components`: `subtitle_occurrence`, `automatic_subtitle`, `same_language`.
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
- per-window peak bitrate;
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
- Prepared renditions, subtitles and caps are not in that model; unit and
  HTTP tests cover them.
- Adapter probes, filters and segment measurement are covered by HTTP tests,
  not models.

## Build and test receipts

Builds ran through the shared Kyoto queue with Rust 1.98, in this worktree's own
target directory. Tests ran from the compiled binaries on macOS arm64, with
host FFmpeg 8.1.2 and VideoToolbox available. The final receipts are at
`d17556a`. Compilation was through Kyoto (`f84a3f490289462d88f430bb7f35e646`):

| Suite | Result |
|---|---|
| `playscale-core` lib | 114 passed |
| `route_selection_model` | Graph exhausted, 28,689 states |
| `playback_session_model`, `delivery_model`, `delivery_observation_model` | Passed (Statelessness 0.2.0) |
| `tests/v2_playback.rs` | 22/22 (`MOTION_REQUIRE_VIDEOTOOLBOX=1`) |
| `tests/delivery.rs` | 12/12 |
| `tests/access_api.rs` | 32/32 |
| JavaScript | 54/54 (no client files changed) |
| Contract checks | Pass (contract unchanged against main) |
| Pinned clippy, `-p playscale-core -p playscale --all-targets` (CI allow-list) | Clean, Kyoto `f5c29d2b9d8a4a9fb10f583b9154e8d6` |
| `cargo fmt --check`, contract checks | Pass |

## Deferred work and release checks

| Item | Owner | Requirement / expected outcome |
|---|---|---|
| Copied-stream, hardware-output and burned-subtitle playback | Clients + integration/release | Chromium and Electron on the qualified player: startup, far seek, audio switch and A/V sync. Required before `experimental_copy_routes` or `experimental_live_hardware` default on |
| Copied-stream audio start | Integration/release | After a seek within 132 ms of a keyframe, audio may start up to 132 ms after video |
| Capped and multipart/interval playback in players | Clients + integration/release | Seek across part boundaries; correct logical duration; `BANDWIDTH`-based startup |
| HDR tone mapping | Integration/release (native tools), then this lane | Add zimg (`zscale`) or libplacebo to the bundled FFmpeg. Then implement a tone-mapped conversion gated on the observed filter, and qualify on suitable HDR/SDR displays. Until then HDR conversion stays refused |
| Text subtitles on live routes / text burn-in | Clients (live text tracks), integration/release (libass) | Either an HLS subtitle rendition the provider plays live, or libass in the bundled FFmpeg |
| Styled (ASS/SSA) subtitles | This lane after libass | Burn-in with fonts from attachments |
| Subtitles on multipart versions | This lane | A per-part subtitle source joined on the timeline |
| Bitmap cues older than the lookback | This lane | A cue displayed more than 20 s before a start is not restored |
| Preparation by audio component | Data/catalog + this lane | Preparation still refuses component pins; clients pin `a{n}` with the exact source |
| **M4-05 adaptive bitrate** | This lane, after the single-route matrix is qualified | Release deferral (plan 10.3). Design: one FFmpeg process per generation encoding aligned variants. It would use `-var_stream_map`: split the picture, force shared keyframes, cap every variant. Each variant would be verified (independent segments, peak windows) and segment *n* reported only when every variant has it. Each variant's `BANDWIDTH` would be its cap, and a generation would reserve both execution units. The contract's `variants/{variant_id}` routes already allow it. Manual prepared-file switching is not ABR |
| Physical A/V and performance budgets | Integration/release | Section 21.3 budgets with a frozen workload; WebKit remains deferred |

## Known pre-existing gaps found during this lane

Transcode manifests advertise a static `avc1.64001f` (High, level 3.1). The
recipe caps height at 720 but keeps the source frame rate, so 720p sources
above 30 fps encode at level 3.2. Copy routes derive their codecs from output
instead. Owner: this lane, with the player matrix, since changing
transcode manifest text affects existing player receipts.
