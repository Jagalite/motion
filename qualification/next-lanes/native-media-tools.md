# Lane N2 — native-tool support and remaining playback transforms

Plan: `MOTION_CONSOLIDATION_AND_COMPLETION_PLAN.md` section 11.3.
Worktree `motion-native-media-tools` (section 11.1 worktree root), branch
`native-media-tools`, started from `origin/main` `c6d6e26` (2026-10-11).

**Status: tool-enablement slice.** The packaged FFmpeg can now perform the
selected HDR-to-SDR tone map and libass text/styled subtitle rendering, proven
on real pixels. No production playback route uses either yet. Production
policy, execution and client wiring belong to the M4 owner (A06/A07, PR #22),
as described under "Handoff" below. Nothing here implies HDR or subtitle parity.

> **Status note (2026-10-12, main `28f4b32`).** This lane merged (PR #33).
> Section 6's handoff is now owned by completion item 3 (plan section 12,
> [the completion ledger](../completion/README.md)). Since this ledger was
> written, PR #22 added live HLS WebVTT text renditions for `hls_text` clients,
> so the "future HLS rendition" in section 5 exists for plain text; styled and
> burned text are still refused.

## 1. Starting inventory (main `c6d6e26`, PR #22 head)

| Area | Fact | Where |
|---|---|---|
| Bundled FFmpeg | 8.0 + static x264, `--disable-autodetect`; has `tonemap`, `overlay`, `scale_vt`, `colorspace`; **no `zscale` (zimg), no `subtitles`/`ass` (libass)** | `scripts/build_media_tools.py`; measured with `ffmpeg -filters` on the integration-release build |
| Homebrew FFmpeg on the build host | also lacks `zscale` and libass | `ffmpeg -filters` (Homebrew 8.x) |
| Tool location | `processing.ffmpeg` / `ffprobe` config; packaged app uses `Resources/tools/` | `src/config.rs:88-124` |
| Capability probing | none for filters; only a cached VideoToolbox trial encode; v2 `/processing/capabilities` hardcodes `filters:["scale"]` | `src/processing.rs:998-1031`, `src/v2/processing.rs:102-190` |
| HDR facts probed | only `color_transfer` and `pix_fmt`; no primaries, matrix, range, mastering/CLL or Dolby Vision side data | `src/scan.rs:306,370-371` |
| HDR routes | live and preparation refuse HDR conversion (`hdr_unsupported`, `preparation_video_unsupported`); HDR is preserved only as original bytes for clients reporting HDR support; `hdr_conversion` advertised unsupported | `crates/core/src/delivery.rs:56-57,124-126`, `src/v2/playback.rs:662-684`, `src/v2/system.rs:121` |
| Subtitles, main | embedded plain text → WebVTT on the Original route only; ASS/SSA, sidecars and bitmap refused | `crates/core/src/delivery.rs:113-119`, `src/delivery.rs:3055-3170` |
| Subtitles, PR #22 | adds SRT/VTT sidecars (Original), **bitmap burn-in via `overlay` on conversions** for clients listing `burned`, automatic selection by language/forced/SDH; still refuses styled ASS/SSA, text on live routes, text burn-in, multipart subtitles | PR #22 `src/v2/playback.rs:1538-1545,1804,1822`, `src/delivery.rs:576-583`, `src/v2/system.rs:134-142` |
| Client advertisement | UI bridge sends `subtitle_modes: ['text']`, `hdr: 'unknown'`; contract already allows `text|styled|bitmap|burned` | `packages/ui-bridge/player.js:63,717`, `contracts/Motion_Server_API_v2.yaml:10935-10944` |
| Demuxe 1.1.1 (Shaka backend) | `addTextTrack`/`addSubtitle` → Shaka `addTextTrackAsync`, which throws 4033 (`CANNOT_ADD_EXTERNAL_TEXT_TO_LIVE_STREAM`) while duration is Infinity; manifest `EXT-X-MEDIA TYPE=SUBTITLES` WebVTT renditions are read and selectable, documented "functional/bounded" for finite playlists; rolling-live subtitle windows are not qualified | npm `demuxe@1.1.1` `web/generated/internal/shaka-backend.js:383-386,660-761`, `docs/CAPABILITIES.md:338,349-353`, `docs/COMPATIBILITY-EXPANSION.md:119-122` |
| PR #22 measurement | `scale_vt` retags PQ without tone mapping (mean luma unchanged) | PR #22 `qualification/m4-expansion/README.md:73` |

Conclusions that drive this lane:

- Retagging (`setparams`, `scale_vt`, container tags) does not convert PQ/HLG
  pixels; the existing `tonemap` filter needs linear-light input that only
  `zscale` (or libplacebo) can provide in this build.
- On the current Demuxe/Shaka path, live routes cannot take an external text
  track. Text subtitles on converted/live routes therefore need **burn-in**
  (libass) or a future **HLS WebVTT subtitle rendition**; the latter is not
  qualified for rolling live windows and is not selected here.

## 2. Selected pipelines

**HDR → SDR (software, deterministic):** zimg `zscale` linearization with
explicit input properties, FFmpeg `tonemap=hable`, BT.709 re-encode:

```
zscale=tin=<smpte2084|arib-std-b67>:pin=bt2020:min=bt2020nc:rin=limited:t=linear:npl=100:p=bt2020:m=bt2020nc:r=full,
format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=hable:desat=0:peak=<peak_nits/100>,
zscale=t=bt709:m=bt709:r=limited,format=yuv420p
```

with output tags `-color_primaries bt709 -color_trc bt709 -colorspace bt709
-color_range tv`. Input properties are passed explicitly (the policy decides
them from probed facts) rather than trusting possibly incomplete stream tags.
`peak` must come from mastering metadata/MaxCLL; FFmpeg's default for PQ
without metadata is 100 nits, which crushes highlights.

Rejected alternatives: `scale_vt`/VideoToolbox (measured retag only, hardware
dependent); `libplacebo` (Vulkan/MoltenVK runtime, much larger dependency);
`colorspace` (no PQ/HLG transfer support); `tonemap` alone (requires linear input).

**Text/styled subtitles:** libass through FFmpeg `subtitles=<file>:si=<n>`
(embedded, exact subtitle-stream index) or `subtitles=<sidecar>`; offsets by
`setpts=PTS-<offset>/TB,subtitles=…,setpts=PTS+<offset>/TB`. Fonts come from the
macOS Core Text provider plus the source's attachments (FFmpeg's `subtitles`
filter loads font attachments of the file it reads). No Fontconfig, no bundled fonts.

**Bitmap subtitles:** unchanged native `overlay` (already used by PR #22).

## 3. Tool artifact

| Input | Pin | License | Shipped |
|---|---|---|---|
| FFmpeg | 8.0, `b2751fcc…` (unchanged) | GPL build | yes |
| x264 | `c24e06c`, `090d730e…` (unchanged) | GPL-2.0+ | static |
| zimg | 3.0.6 tag → commit `f819b14`, `4dc9faba…` | WTFPL | static |
| FreeType | 2.14.3, `36bc4f1c…` (GPG: Werner Lemberg) | FTL or GPL-2.0+; BDF/PCF drivers and `fthash` carry MIT-style notices (shipped) | static |
| FriBidi | 1.0.17, `6949dcde…` (= GitHub release digest) | LGPL-2.1+ | static |
| HarfBuzz | 14.6.0, `d07a0073…` (= GitHub release digest) | Old MIT (+ `src/ms-use/COPYING`, ISC-style `src/hb-ucd.cc` notice) | static |
| libass | 0.17.5, `2dca25c0…` (= GitHub release digest) | ISC (+ UC/Sun/Oshmyan notice in `libass/ass_strtod.c`) | static |
| GNU m4 / autoconf / automake / libtool | 1.4.21 / 2.73 / 1.19 / 2.6.2 (GPG-verified) | GPL | build-only (zimg `autogen.sh`); sources in companion |
| CMake (HarfBuzz) | host tool, version recorded in the receipt | — | build-only, not pinned |

Changes: `packaging.lock.json` (new pins, `notices`, `media_build_tools`,
`media_libraries`), `scripts/build_media_tools.py` (receipt schema 3, ordered
build, required-filter gate, build-path scan, `${BUILD}`-normalized receipt
commands), `scripts/package_app.py` (receipt schema 3, all sources in the
companion, per-library notices), `THIRD_PARTY.md`, `README.md`.
**Integration coordination needed:** these are shared packaging manifests.
Changing FFmpeg bytes invalidates prior installed-package/player receipts.

Build result (Kyoto, `python3 scripts/build_media_tools.py --output
artifacts/media-tools-n2 --jobs 4`, source = this branch's working tree):

- Job `4cdc3493` failed at FriBidi: its CLI man page needs `help2man`. Fixed by
  building only FriBidi's `gen.tab lib` subdirectories (the library is all
  that is linked).
- Job `a12173c8` succeeded, exit 0, 3544 s (host under memory-pressure
  throttling). Receipt schema 3; filters `zscale tonemap subtitles ass overlay`
  verified; CMake 4.3.1; Apple clang 21.0.0.
  `ffmpeg` `58ab0089891489a8dcf254504122a2dacb115c36b8e39e672f6b5ed2eb94f711`,
  `ffprobe` `0d945d99f57411652285776ed1af1437179ccdcd933c0ebd1b5dbdfd0fa09664`
  (25 MB each).
- `otool -L`: only `/usr/lib` and `/System` libraries; CoreText is newly
  linked (libass font provider), plus libc++/libiconv. No build-directory or
  user paths in the binaries or the receipt.
- Relocation: the binaries copied to an unrelated temporary directory pass
  the full transform suite.

These bytes are a lane artifact, not a release package. The integration owner
rebuilds them in the package workflow; prior package/player receipts do not
apply to the new FFmpeg bytes.

## 4. Focused checks

| Check | Result |
|---|---|
| `python3 -m unittest test_packaging` (lock pins/notices/order, filter parser, build-path rejection, existing pin/portability tests) | 17/17 pass |
| `MOTION_MEDIA_TOOLS=artifacts/media-tools-n2/install/bin python3 -m unittest test_media_transforms` | 7/7 pass |
| Same suite with relocated copies of the binaries | 7/7 pass |
| Same suite against the previous bundled FFmpeg (integration-release build) — negative control | 6 errors (no `zscale`/`subtitles`), only the retagging control passes, as expected |

`scripts/test_media_transforms.py` asserts real pixels:

- PQ 10-bit neutral bands at 0/10/100/1000 nits (exact ST 2084 code values,
  lossless FFV1 fixture tagged and probed as PQ/BT.2020) map to the Hable +
  display-referred BT.709 (inverse BT.1886, gamma 1/2.4, which is what zimg
  encodes) luma computed independently in the test (±2.5 codes); measured
  16/73/151/235 against 16.0/72.6/150.9/235.0. Chroma stays neutral and the
  output is probed as `yuv420p` bt709/bt709/bt709/tv.
- Negative control: retagging the same PQ fixture as BT.709 leaves the
  1000-nit highlight >40 codes too dark (measured 181 vs 235) and the 100-nit
  diffuse white >15 codes too dark (127.25 vs 150.9); the 10-nit patch is only
  about 9 codes too bright and is not asserted.
- HLG bands at scene light 0.01/0.1/0.5/1 (BT.2100 OETF code values) match an
  independent oracle (inverse OETF, reference OOTF for a 1000-nit display with
  system gamma 1.2, Hable, BT.1886) within ±1.5 codes: measured 55/132/211/235
  against 54.8/131.6/211.4/235.0. HLG is close to SDR by design, so the test also
  asserts that an unconverted pass-through would fail that tolerance (verified:
  an identity graph fails at 135.25 against 131.6). Decoding the same codes as
  PQ must differ by more than 20 codes.
- libass drawing (`{\p1}` rectangle, font-independent) appears only during its
  event, at its position; a +1 s offset moves it by exactly 1 s; `si=1`
  renders the second of two same-language embedded tracks, not the first.
- Text renders glyphs through the Core Text font provider.

## 5. Supported / refused matrix (truthful as of this slice)

| Case | Tool artifact | Production route |
|---|---|---|
| HDR10 (PQ) → SDR BT.709 | supported, pixel-verified | **refused** at this lane; implemented for single-file live conversions by completion item 3A |
| HLG → SDR | supported, property-verified | **refused** at this lane; implemented by item 3A (not pixel-tested through the server) |
| Dolby Vision profile 5 (IPT-PQ-c2, no HDR10 base) | not supported by zscale/tonemap | must stay **refused** |
| DV 7/8.1 (HDR10-compatible base layer) | base layer tone-mappable as PQ | **refused** at this lane; item 3A probes the DV configuration and admits profiles 7/8 with an HDR10 (or 8.4 HLG) base layer |
| HDR10+ dynamic metadata | ignored (static tone map) | n/a |
| HDR preserve (original bytes to HDR client) | unchanged | unchanged |
| Styled ASS/SSA burn-in | supported (libass) | **refused** (`subtitle_format_unsupported`) |
| Text (SRT/WebVTT) burn-in on converted routes | supported (libass) | **refused** (`text_burn_in`) |
| Text as live external track | Shaka 4033 on live presentations | refused; use burn-in or future HLS rendition |
| Bitmap (PGS/VobSub/DVB) burn-in | native `overlay` | supported by PR #22 (needs client `burned`) |
| Font attachments | FFmpeg `subtitles` loads them | untested (no deterministic font fixture) |
| Physical display/colour fidelity | — | release gate, not claimed |

## 6. Handoff to M4 (A06/A07, PR #22 owner)

Do not start until PR #22's shared files settle. Proposed production work:

1. **Observation (adapter):** probe FFmpeg filters once per binary
   (`-filters`, the same parser as `build_media_tools.filters`) and pass
   `{zscale, tonemap, subtitles}` availability into core as explicit facts;
   unknown → refuse. Replace the hardcoded `filters:["scale"]` capability.
2. **Facts (scan, data/catalog owner + schema allocation):** probe and store
   primaries, matrix, range, mastering display / content light level, and DV
   configuration (profile, base-layer compatibility). Current rows have only
   transfer and pixel format.
3. **Core policy:** a `tone_map_to_sdr` route decision from (source transfer,
   primaries, DV profile, peak metadata, client `hdr`, `hdr_policy`, filter
   facts). Refuse DV profile 5, unknown transfer, and missing tools. Default
   peak 1000 nits only when mastering metadata is absent, and record it.
   Styled/text burn-in: extend `burnable_subtitle` and `presents_subtitle` so
   text/ASS burn-in requires a video transform and the libass fact, with exact
   source/stream or sidecar identity and offset carried through.
4. **Execution:** add the section 2 graphs to `src/delivery.rs` arguments.
   Burn-in from embedded streams must use `si=<subtitle-stream ordinal>`
   resolved from the exact selected component. Test bounded processes,
   cancellation, far seek (note the `subtitles` filter reads the whole file;
   seeks need `setpts` offsets equal to the seek origin), stale source/component
   fencing, and probed output colour tags.
5. **Client:** the bridge must advertise `styled`/`burned` and a real `hdr`
   value before these routes can be reached; keep experimental defaults explicit.

Remaining work not owned by this lane: HLS WebVTT subtitle renditions
(Demuxe/Shaka rolling-window qualification), libplacebo/GPU tone mapping,
font-attachment fixtures, physical HDR/SDR display checks.

## 7. Review

GPT-6.1 Sol (high effort) reviewed `eb534df`. It confirmed the artifact hashes
match the receipt and all five required filters are present. Three findings,
all accepted and fixed in the follow-up commit:

1. P1: FreeType's compiled BDF/PCF drivers and `src/base/fthash.c` carry
   separate copyright/permission notices that must ship. These are now in the
   lock's FreeType `notices`, so packaging copies them (and fails if missing).
2. P2: a relative `MOTION_MEDIA_TOOLS` broke the suite because subprocesses run
   in temporary directories. The path is now resolved at import, and the suite
   passes with the relative path.
3. P2: the HLG assertions passed when tone mapping was removed (an identity
   graph gave 16/54/135.25/235). They were replaced by the BT.2100 oracle
   above, a pass-through discrimination assertion and a PQ-decoding negative
   control. Removing the conversion now fails the test.

GPT-6.1 Sol re-reviewed `ce9accf` and verified all three fixes. It raised three
new findings, all accepted and fixed:

1. P1: the ledger contained absolute local paths, which `check_publication`
   rejects, blocking package source capture. They are replaced with portable
   references.
2. P1: libass's compiled `ass_strtod.c` carries separate UC/Sun/Oshmyan
   notices; it is now a pinned notice. A scan of every compiled library source
   for non-standard headers also found HarfBuzz's ISC-style `hb-ucd.cc`, which
   is now pinned as well. FriBidi's `fribidi-config.h` is public domain and
   zimg has no component notices.
3. P3: the PQ negative-control description overstated the dim-detail error;
   it now matches the asserted and measured values.

Notice-list changes alter the lock entries recorded in the build receipt, so
`artifacts/media-tools-n2` no longer matches the lock and `package_app.py`
refuses to reuse it. That is expected: the integration package build rebuilds
the tools. The binaries themselves are unaffected (notices are not compiled in).

GPT-6.1 Sol re-reviewed `2607345` and verified the three fixes. It reported no
actionable findings in the cumulative diff from `c6d6e26`.
