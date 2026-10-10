# On-demand file encodes and Demuxe integration

Playscale can create multiple independent MP4 encodes of one source. No HLS/DASH
manifest is required. Demuxe selects a revision-bound URL; its own capabilities
determine whether/how it can switch without interruption. Different frame rates
share source provenance, not one-to-one frame identity or a gapless guarantee.

## Built-in FFmpeg

Submit an admin-authorized job to `POST /api/v1/processing-jobs`:

```json
{
  "source_file_id": "SOURCE_FILE_ID",
  "source_revision": "SOURCE_SHA256",
  "recipe": "video_profile",
  "backend": "software",
  "idempotency_key": "unique-request-key",
  "video_profile": {
    "version": 1,
    "codec": "h264",
    "max_width": 1280,
    "max_height": 720,
    "video_bitrate": 2000000,
    "audio_bitrate": 128000,
    "frame_rate": {"numerator": 30000, "denominator": 1001}
  }
}
```

`codec` is `h264` or `hevc`. Output is MP4, 8-bit YUV420 video and AAC audio,
using the first video/audio streams; subtitles are omitted. Dimensions form an
even bounding box, preserve aspect ratio, and do not upscale. Set `frame_rate`
to null (or omit it) to preserve input cadence; a rational requests CFR and may
drop/duplicate frames. Conversion uses nearest-frame rounding at EOF; clips
shorter than half a requested frame interval are rejected before encoding. Tagged PQ/HLG HDR inputs are rejected by this recipe;
untagged color/HDR, tone mapping, interlacing, and exotic source behavior are not
qualified. Bitrates are encoder targets, not measured network requirements.

Version 1 limits: even width 128–3840, height 72–2160; video 64,000–80,000,000 bps;
audio 32,000–320,000 bps; explicit frame rate 1–120 fps. The schema accepts only
bounded fields, never arbitrary executable arguments. `software` selects
libx264/libx265 in the configured FFmpeg executable. On macOS, `videotoolbox`
selects the corresponding hardware encoder with software fallback disabled;
this change's live qualification covers software only. Advertised backends are
platform options, not proof that a configured FFmpeg build has every encoder.

Profiles are persisted in migration 0009. Idempotent retries must preserve all
parameters. A different profile with the same key returns 409. New keys permit
independent jobs; callers should use the planner to reuse available output or
an already-active matching job. Retry/cancel use the existing job control API.
The existing single-worker concurrency, queue, snapshot identity checks,
output/cache budgets, timeout, decode validation and atomic publication apply.
A queued/running job has no playable output URL. This is on-demand *file*
transcoding, not playback of an encoder's growing output.

## Playback planning and discovery

`POST /api/v1/profiles/{profile}/items/{item}/playback-plan` accepts
`recipe: "video_profile"` and the same `video_profile` object. In Convert mode,
only a completed local job with that exact profile satisfies the recipe. A
preparation proposal includes the complete profile and the matching active job
ID, if any. Planning is read-only and does not grant encoding permission.
Existing profile preferences and legacy recipes remain unchanged; custom
profiles are explicit per-request overrides. The existing frontend controls do
not yet expose a custom-profile editor.

`GET /api/v1/items/{item}/playback-options` supplies originals and renditions.
Renditions now include `bytes`, `duration_seconds`, `average_bitrate`, and
probed `tracks`. Both original and rendition tracks can include `width`,
`height`, rational `average_frame_rate`, stream `bitrate`, `pixel_format`,
`color_transfer`, and `start_time_seconds`. Unknown fields are null. Whole-file
average bitrate includes container/audio overhead; stream bitrate may be absent.
Measured metadata, rather than the producer's claimed recipe, describes files.
Old cached track JSON remains readable; rescan/reprobe the library to populate new probe fields.
Video-profile admission rejects video sources with unknown dimensions.

Demuxe should group alternatives by `source_file_id` + `source_revision`, bind
its selection to `file_id` + `file_revision`, and use `media_url` unchanged.
Source identity is not proof of timeline equivalence for external renditions.
Do not infer frame/sample alignment from equal duration, labels or frame rate.

## Catabolic or another API producer

No Catabolic execution adapter is added here. An external producer can encode
into a configured library, scan it, and register the completed file using:

`PUT /api/v1/items/{item}/renditions/{producer}/{external_id}`

Provide `file_id`, `file_revision`, `source_file_id`, `source_revision`, `label`,
and informational `recipe` JSON. This existing authenticated API verifies
catalog identities and does not execute commands or trust claimed codec/quality
metadata. External files can be selected explicitly or considered in Auto mode;
they cannot impersonate a validated local `video_profile` recipe in Convert mode.
A client can alternatively call the built-in processing API to run FFmpeg on the
server. Remote upload/remote worker leasing is outside this contract.

## Keeping the frontend synchronized

Use `GET /api/v1/events` (SSE). Events are committed invalidation hints:

- `change`, topic `processing`, resource ID = job ID: refetch that job; render
  phase, attempt and `progress_seconds` (encoded source seconds, not a percentage).
- `change`, topic `catalog`: refetch current playback options and, if necessary,
  re-plan. Completion publishes the file, rendition and job status transactionally.
- `reset`: refetch all relevant state. This is emitted for initial subscription
  and an expired/out-of-range cursor. The feed checks for changes every 500 ms
  and sends keepalives every 15 seconds; it is not a per-frame transport clock.

Subscribe before fetching the initial snapshot. Buffer/coalesce invalidations
while fetching and refetch if one arrives during a request. Use one in-flight
refresh per resource and discard responses belonging to an old source/viewer/
selection generation. Persist or let EventSource carry `Last-Event-ID`; a newly
constructed connection can use `?after=N`. Replays can duplicate hints; GET is
idempotent. On a reset, clear assumptions based on the previous cursor. Reconnect
with bounded backoff and use a slow polling fallback if SSE is unavailable.
Show reconnecting/stale status; loss of metadata connectivity should not stop
already-buffered playback. A completion only authorizes offering the rendition;
Demuxe must still validate compatibility and own the actual playback switch.

## Validation

`cargo test -p playscale -p playscale-core` covers profile validation,
idempotent conflicts, active-job matching, probed rendition metadata and existing
job/session/event invariants. `scripts/video_profiles_smoke.py --binary PATH`
uses a disposable real server, a 4.7-second 640×360/60 fps H.264/AAC fixture and five real
FFmpeg jobs: H.264 320×180/30, HEVC 320×180/preserved 60, and H.264
640×360/30000:1001, plus H.264 at 1 and 2 fps. The low-fps cases must
complete within the existing 0.5-second duration tolerance. It verifies decoded files, hashes, ranges, planner matching,
SSE replay and persistence across restart. It does not qualify client ABR,
seamless switching, physical audio output, Catabolic execution or hardware encoding.

### Duration-derived execution deadline

Processing can optionally set `expected_duration` alongside the existing absolute,
startup and no-progress timeouts:

```json
{"processing":{"expected_duration":{"allowance_seconds":30,"media_duration_multiplier":4}}}
```

Each conversion and validation-decode command gets an elapsed-time budget of
`allowance_seconds + source_duration_seconds * media_duration_multiplier`. The
absolute `timeout_seconds` remains an upper bound. The allowance must be 1–86400
seconds and the integer multiplier 1–100. Omission or null disables this optional
policy. Source duration comes from the revalidated catalog revision and rounds
up to milliseconds. Progress never extends this budget; expiration requests tree
termination and capacity remains reserved until termination is confirmed. Output
duration/track validation is still required before publication. This setting
applies to durable processing, not intentionally paced live delivery.
