# Frame-perfect / gapless qualification — 2026-10-06

**Result: NOT QUALIFIED.** The current implementation preserves viewing state,
but fails the decoded-surface continuity criterion. Audio output was not captured,
so no sample-gapless audio claim is possible. A better native-clock measurement
reduced the error; it did not establish synchronized presentation.

Scope: macOS, T3 Code's Chromium 152.0.7977.130 / Electron 44.4.2 preview,
Demuxe `native-direct`, local H.264/AAC MP4, software `h264720p` conversion,
30 fps, 48 kHz mono, 1× playback. This is a finite-file rendition handoff test,
not a streaming encoder-output test. Other browsers, backends, hardware encoding,
playback rates, fullscreen/PiP, HDR, remote I/O, and physical audio output are
not qualified.

The 120-second source carries a 12-bit frame counter in its pixels and a
continuous chirp in its audio. FFmpeg decoded and checked **all 3,600 frame IDs
and timestamps in both files**. All IDs match their frame index; maximum PTS
rounding error is below one microsecond. The actual server processing job
created the rendition; this was not a manually substituted output.

The probe samples decoded video pixels on animation frames and independently
records `requestVideoFrameCallback` metadata. An unchanged baseline must show
consecutive frame IDs, normal frame holds, and sufficient sampling coverage.
Each trial must retain that continuity across a mode change. Repeated screen
samples within one normal video-frame interval are expected, not failures.
The analyzer does not treat missing callbacks as a pass. A preview run without
recording delivered only one sample and is retained as **INCONCLUSIVE**.

| Switch | Before: last old → first new frame | After correction | After decoded result |
|---|---:|---:|---|
| Original → transcode, trial 1 | 1166 → 1159 (−7) | 338 → 337 (−1) | Fail |
| Transcode → original, trial 2 | 1368 → 1365 (−3) | 546 → 546 (0) | No discontinuity detected |
| Original → transcode, trial 3 | 1575 → 1571 (−4) | 756 → 755 (−1) | Fail |
| Transcode → original, trial 4 | 1780 → 1778 (−2) | 966 → 965 (−1) | Fail |

Both steady baselines had zero nonconsecutive frame IDs and zero invalid pixel
samples. The before/after baselines held frames for at most 35.21/35.29 ms.
After correction, all four switch windows had sufficient coverage; their largest
sampling intervals were below 18.67 ms. The before run's native clock regressed
by 103–241 ms at the handoffs. After correction, sampled clock deltas were
+8.46, −0.26, −28.05, and −29.76 ms. Clock proximity does not imply frame alignment.

The production fix uses the native backend's live source clock instead of its
periodically published UI position, with a fallback for other routes or missing
diagnostics. The outgoing surface is also removed from layout synchronously
before asynchronous destruction, so both players do not occupy the layout during
cleanup. Tests cover the native-clock choice and safe fallback.

Decoded-surface samples are **not interchangeable with physical display output**.
The after tab recording did not resolve the one-frame backwards steps. It did
record frame 338 at 6.381567 s and again at 6.432200 s: a 50.633 ms separation,
with frame 339 first recorded at 6.465567 s. This shows an extended hold or replay
around that transition. Recorder sampling cannot distinguish the two or prove
absence of a shorter glitch. Video-frame callback receipts also retain the old
and new surfaces' distinct presentation timestamps. No claim of a frame-perfect
screen transition is made from matching media clocks.

Both tab recordings contain **only H.264 video, no audio stream**. The chirp's
presence in the source, player mute flags, and advancing media clocks are not
output-audio measurements. No audio route was replaced with a synthetic mixer
for this test. Audio needs a real tab-output/loopback PCM capture and sample
continuity analysis before it can pass.

The remaining work is synchronized video presentation and sample-timed audio
switching, followed by another qualification run. The present implementation
seeks two independent players, acknowledges server state, then mutes/unmutes
and swaps surfaces; it has no shared presentation boundary. Further timing
heuristics alone must not be labelled frame-perfect or gapless.

Evidence is bound to dirty source snapshots by `before-provenance.json` and
`after-provenance.json`, including served-JavaScript matches, server executable
hashes, Demuxe route implementation hashes, fixture hashes, and browser receipts.
`evidence-sha256.json` identifies the retained inputs and results. Raw receipts
are gzip-compressed JSON; the summary files are readable JSON.

The recordings remain outside Git:

- Before: `private-recordings/before.mp4`
- After: `private-recordings/after.mp4`

To reproduce, use an isolated server and data directory:

1. Run `python3 research/playback-handoff/generate_fixture.py /tmp/frame-fixture`.
   This requires NumPy and FFmpeg. Register only its MP4 in the test library.
2. Start Playscale, scan the MP4, and submit a software `h264720p` processing job
   through the normal admin API. Wait for validated completion.
3. Run `verify_fixture.py SOURCE.mp4 OUTPUT.mp4`; both files must pass.
4. Open the original in the collaborative preview. Start tab recording to keep
   its offscreen compositor producing frames. Evaluate `probe.js`, then
   `run_trial.js`. Wait for `window.trialDone`; require `window.trialError` null.
5. Export `window.handoffProbe.receipt` and stop recording. For large receipts,
   gzip with `CompressionStream`, then transfer base64 in chunks. Keep the raw
   receipt; do not substitute a summary or player state samples for decoded IDs.
6. Run `analyze.py RECEIPT.json[.gz]`. Exit 1 means decoded continuity failed;
   exit 2 means insufficient evidence. Exit 0 covers only observed decoded-video
   trials, never audio or a universal gapless claim.
7. Inspect recording tracks with FFprobe. `analyze_recording.py RECORDING.mp4`
   decodes screen counters at the measured crop for this exact viewport; remeasure
   that crop if the viewport or layout changes. It cannot establish a positive
   qualification because recorder sampling can omit frames.

`python3 research/playback-handoff/check_evidence.py` verifies evidence hashes
and confirms that failed or incomplete receipts cannot be reported as passes.

Recording references are portable labels; the original recordings remain private.
