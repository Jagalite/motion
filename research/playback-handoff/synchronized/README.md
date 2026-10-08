# Frame-phase handoff investigation (2026-10-06)

## Production change

The native H.264 route now waits for matching decoded frame timestamps and
presentation phase before swapping surfaces. It adjusts only the muted incoming
player's speed (at most 0.15 from the requested speed), restores it at the
boundary, and synchronously changes both native mute flags and the visible
surface. Demuxe's mute state is reconciled immediately afterward. The server's
existing acknowledged session transfer and rollback remain in force.

Alignment is bounded to three seconds. Cancellation, changes to transport or
track controls, loss of the admitted native route, or failure to align leave the
outgoing player active and roll session attribution back. Unsupported routes,
paused playback, and different source editions use the existing conservative
handoff. This does not guarantee sample-continuous audio or frame-perfect
compositor output across arbitrary browsers, displays, codecs, and workloads.

## Solutions investigated

1. **Native presentation alignment (implemented).** Audio-backed `currentTime`
   alone is insufficient: two decoders can display different frames at the same
   clock time. Use frame `mediaTime` and `expectedDisplayTime` together. The gate
   requires matching timestamps within 0.1 ms, display times within 3 ms, recent
   callbacks, and media clocks within 50 ms. Those are admission heuristics, not
   an audio synchrony guarantee. See the [frame callback specification](https://wicg.github.io/video-rvfc/).
2. **Shared Web Audio mixer (rejected for production).** The prototype used
   complementary eight-millisecond gain ramps on a shared clock. It captures
   actual production postmix PCM through an AudioWorklet. But claiming Demuxe's
   media element prevents Demuxe from later creating its own gain graph and
   changes output-device ownership. That is a regression, even if one narrow
   fixture looks smooth. See the [Web Audio specification](https://www.w3.org/TR/webaudio/).
3. **One playback pipeline (required next architecture for stronger guarantees).**
   Prepare timestamp-aligned fragmented renditions and switch at an agreed
   boundary within one decoder/presentation and audio timeline. Direct originals
   need compatible segmentation/remux delivery; arbitrary codec changes still
   require explicit admission. This is a server + Demuxe capability, not a safe
   patch to a pair of independent HTML video elements. See [Media Source](https://www.w3.org/TR/media-source-2/).

## Prototype evidence (not the shipped implementation)

`prototype.json.gz` includes decoded pixels, frame callbacks, and worklet audio
statistics from four Original / Convert switches. `prototype-analysis.json`
reports all four consecutive decoded frame transitions (342→343, 554→555,
758→759, 972→973), zero invalid frames or jumps, and maximum frame holds below
35.4 ms (30 fps source). This establishes a bounded decoded-video improvement.

The audio statistics are **not gapless qualification**. They capture postmix PCM
before the device, not physical output. Their one-time wall/audio clock mapping
is insufficient to distinguish the final intentional pause from a late switch
window. They also do not prove sample identity, absence of clicks, or A/V sync.
`mixer-prototype.js` and `audio-probe.js` are research artifacts, not loaded by the
application. Tab recordings have video only.

## Reproduction

Use the fixture and completed real FFmpeg rendition from the parent report.
Start an isolated server containing the reviewed web files. Open Frame-clock,
start collaborative-tab recording (necessary for active frame callbacks in the
hidden preview), install `../probe.js`, and execute `../run_trial.js`.
Export its receipt and run `../analyze.py`. Exactly four surface transitions and
adequate baseline/window sampling are required. For audio research, install
`audio-probe.js` before opening a mixer-enabled prototype; do not claim that
instrumentation measures the production implementation, which owns no audio bus.

Regression commands: `node scripts/test_native_handoff.mjs`,
`node scripts/test_playback.mjs`, `node scripts/test_session.mjs`.

## Final production-safe run

`final.json.gz`, `final-analysis.json`, and `final-provenance.json` bind the
result to the served build. All four surface switches completed; all decoded
frame IDs were consecutive, with no blank/invalid samples. Boundary pairs were
358→358, 562→563, 775→776, and 985→985 (a repeated sample at 60 Hz is normal for
a 30 fps source). The maximum observed hold was **50.015 ms** in switch window
three versus **35.115 ms** in the baseline. The unchanged strict analyzer
therefore reports **FAIL**, despite removing the previously measured rewinds.
Production audio output remains **NOT_MEASURED**. No universal frame-perfect
gapless claim is made.

All three JavaScript suites and 51 Rust tests passed against the isolated
reviewed snapshot. Unrelated working-tree server changes were excluded.
