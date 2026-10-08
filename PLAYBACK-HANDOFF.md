# Playback handoff

Changing Auto / Original / Convert, conversion recipe, or file version keeps the
active player running while the replacement opens in a muted staging element.
The browser transfers the current position, play/pause intent, volume, mute,
speed, and matching language tracks before replacing the visible player. The
existing playback session stays authoritative; changing files does not start a
new viewing session or reset progress.

Preparation still requires an explicit admin-authorized processing request.
The current file plays while the complete replacement is encoded and validated.
The browser watches the job and switches automatically after publication. This
is not HLS/DASH transcoding or playback of an encoder's growing output.

A newer selection, title, viewer, or Close cancels pending handoff work. It does
not cancel a shared processing job. Opening or validating a replacement can fail
without destroying the current player. Auto can attempt another version after a
runtime player error; it does not infer network bandwidth or silently create a
processing job.

Changing viewers retains the player and current position, stops progress updates
for the old viewer, and starts a session for the new viewer. The new viewer's
preferences apply without resuming their previously saved position.

## Session API

`PUT /api/v1/profiles/{profile}/playback-sessions/{session}` accepts an optional
file identity alongside an ordered playing/paused event:

```json
{
  "sequence": 12,
  "position_seconds": 42.5,
  "status": "playing",
  "file": {"file_id": "output-file-id", "file_revision": "output-revision"}
}
```

The target must be a current, available original or revision-valid rendition of
the session's item. File identity, duration, event, and viewing progress update
in one transaction. Retrying an acknowledged event must retain the same file
identity. Stale sequences, replaced sources, and superseded sessions cannot take
ownership. Existing clients can continue sending events without `file`.

## Validation and limits

Run `node scripts/test_session.mjs`, `node scripts/test_playback.mjs`, and
`cargo test -p playscale -p playscale-core`. Tests cover exact retries, invalid
revisions, retained session identity, cancelled/stale handoffs, a pause/seek while
acknowledgement or the final seek is pending, persistence of the final paused
position, immediate cancellation cleanup, and rollback when activation or audio
cleanup fails. A rejected final position on a shorter rendition still permits
restoring the original file. Repeated planning of an already completed conversion
is bounded instead of polling in a tight loop.

A local collaborative-browser test used a 180-second H.264/AAC source and real
FFmpeg H.264 conversion. With encoding held, the original advanced from 18 to 28
seconds. Releasing encoding caused automatic handoff while retaining the session
ID, volume 0.25, and speed 1.25. Paused switching retained 62 seconds; a pause/seek
during delayed opening retained 91 seconds. Viewer switching kept the same
player and attributed 62-second progress to the new viewer. Injected replacement
failure and a superseding selection retained the active player.

A review regression check injected a pause/seek to 73 seconds during the
replacement's final seek. The updated browser build switched paused at 73 seconds
and the server stored 73 seconds, confirming the final position is acknowledged.
A superseding selection removed a delayed hidden player before its opening
promise was released, leaving exactly one active player at the same position.

The initial frame-perfect qualification **did not pass**. The native clock
fix reduced the 2–7-frame decoded rewinds to one frame in three of four tested
handoffs, but did not synchronize presentation. Tab recordings have no audio
track, so gapless audio remains unmeasured. See the [qualification report and
reproducible evidence](research/playback-handoff/README.md).

The subsequent [frame-phase investigation](research/playback-handoff/synchronized/README.md)
adds a bounded native presentation gate. Failed alignment retains the original.
Audio remains owned by Demuxe; sample-gapless audio is not established.
Run `node scripts/test_native_handoff.mjs` for alignment regressions.

Fullscreen/PiP transfer, external subtitle attachments, different edition
timelines, and HDR transitions remain unqualified. Both players briefly coexist,
so the handoff needs resources for two decoders.
