# Playscale core API

Versioned JSON routes live under `/api/v1`; `/api/v1/openapi.json` is generated with
Utoipa. Errors use `{ "code": "…", "message": "…" }`. No local root or relative
file path appears in ordinary viewer catalog responses. `/health` reports HTTP
liveness; it does not establish library availability or processing readiness.

## Resource identity

Content items, editions, media files, profiles, and jobs have distinct opaque IDs.
An exact SHA-256 content revision pins file bytes; a filesystem fingerprint guards
against replacement since scanning and detects in-place changes while streaming.
Catalog availability is the last observation; serving rechecks the actual source.
Unavailable files are retained, not deleted. A unique exact-content move within
one library preserves identity; ambiguous moves are not guessed.

`GET /items?limit=50&offset=0&q=term&library_id=…` returns `{items,total,limit,offset}`.
The initial browse projection is one entry per discovered file, containing distinct
`id` (content), `file_id`, `edition_id`, technical tracks, revision, and `media_url`.
Search is a case-insensitive SQLite substring match (ASCII case folding), not fuzzy
or full-text search. Limit is 1–200; ordering is title, content ID, file ID. Offset
pages can shift during catalog edits. `GET /items/{id}` returns a preferred available
file projection; `/items/{id}/playback-options` exposes all originals and renditions.

`GET /libraries` returns IDs/names. Admin `POST /libraries` accepts `name` and an
absolute `root`; the server canonicalizes the directory. `POST /libraries/{id}/scans`
returns 202 and a durable job. A repeated request joins that library's active scan;
this is not a guarantee of a newer traversal. Poll `GET /jobs/{id}`. Admin
`POST /jobs/{id}/cancel` is idempotent; cancelling is distinct from cancelled. After
a terminal job, submit another scan to retry. No scan starts from a GET.

## Metadata and tags

Catabolic and other importers may contribute through the same API as local tools.
Use admin `GET /admin/files?library_id=…&limit=100&offset=0` to map configured
library-relative paths to Playscale file/content IDs and revisions. Stop paging on
a short page. This is an admin snapshot-by-page interface, not a stable bulk-export
cursor. A real importer must account for concurrent scans and stale revisions.

`GET /items/{id}/metadata` returns effective values, tags, their contributing sources,
conflicts, and individual source documents/revisions. Admin writes replace one
source's complete contribution with optimistic concurrency:

```http
PUT /api/v1/items/ITEM_ID/metadata/catabolic
Authorization: Bearer ADMIN_TOKEN
Content-Type: application/json

{
  "expected_revision": 0,
  "external_id": "catabolic-item-123",
  "values": {"title": "Signal Garden", "release_year": 2026, "description": "A generated test film."},
  "tags": ["Test collection", "Short"]
}
```

Zero creates the contribution; subsequent writes supply its current revision.
Stale writes return 409. `(source, external_id)` is unique when supplied. Field
values retain arbitrary JSON, subject to the 16 KiB request limit and bounded
field/tag counts. Do not put secrets or internal filesystem paths in metadata:
these contributions are intended for viewers. `title` has explicit string validation.
Null is rejected: omit a field in the replacement document to remove that source's
contribution. It does not remove another source's contribution.

`local` fields override providers; providers override the `scan` filename fallback.
If multiple providers disagree and no local override exists, the field is reported
in `conflicts` rather than choosing a provider silently. Browse uses the original
filename title when title is unresolved. `scan` is reserved and cannot be written
through the metadata API. Rescanning cannot overwrite local/imported curation.

Tags are whitespace-normalized and lowercased, then merged with source attribution.
A local document can include `excluded_tags` to suppress imported tags. Removing
an imported tag does not remove an independently supplied local tag. Source-document
timestamps describe receipt at Playscale, not when an external provider observed data.

## Existing renditions

Scan the output into a configured library first. An importer registers its identity
and provenance, using **Playscale's** source/output revisions, not Catabolic's own
revision encoding:

```http
PUT /api/v1/items/ITEM_ID/renditions/catabolic/output-456
Authorization: Bearer ADMIN_TOKEN
Content-Type: application/json

{
  "file_id": "OUTPUT_FILE_ID",
  "file_revision": "OUTPUT_SHA256",
  "source_file_id": "ORIGINAL_FILE_ID",
  "source_revision": "ORIGINAL_SHA256",
  "label": "Small H.264 version",
  "recipe": {"external_job_id": "job-456", "video_codec": "h264", "audio_codec": "aac"}
}
```

No processing is started. Existing identities are retry-safe; a different source,
output revision, or recipe requires a new external identity. Labels may be updated.
Playback options mark a rendition unavailable if its output is missing/replaced or
its original's catalog revision has changed. An offline original alone does not
invalidate an unchanged rendition. Actual media requests recheck output bytes.
Recipe data is informational provenance, never an executable command. Imported
outputs remain independent discovered catalog entries as well; automatic grouping
or hiding those entries is a later curation feature.

Local FFmpeg processing is available through the processing contract below. A future
Catabolic adapter will use authenticated completion by job, attempt, source revision,
and recipe identity. External dispatch, output transfer, and callbacks remain planned.

## Profiles and progress

`GET /profiles` lists selectable profiles; admin `POST /profiles` accepts `{name}`.
The initial profile ID is `default`. `GET /profiles/{profile}/progress/{item}` returns
404 until a position exists. `PUT` accepts `{ "position_seconds": 7.0 }` and returns
the saved record. Progress belongs to content identity, so selecting a rendition
does not create separate history. This is a legacy interface: last accepted write
wins until the profile/item uses playback sessions. From then on, legacy writes
return 409 `session_required`, including after a session closes. Reads remain
available. This prevents delayed unsequenced updates from overwriting protected
progress. Profiles still provide no identity verification or parental restriction.

## Original and rendition bytes

Use the returned `/media/{file_id}?revision=SHA256` URL. An old explicit revision
or a file changed since scanning returns 409. A missing/unreadable file returns 404.
GET supports one byte range, suffix ranges, end clamping, empty-file 416, ETags,
If-Match/If-None-Match and date preconditions. Stale/weak/date If-Range falls back
to full delivery; only a matching strong ETag admits the requested range. HEAD
ignores Range. Malformed or multiple ranges are deliberately ignored with 200.
Media is not dynamically compressed. There are 16 media I/O permits, acquired before filesystem access (including HEAD
and conditional requests); exhaustion returns 503. A response retains its permit
until its body is dropped and any outstanding blocking I/O finishes. Stalled OS
operations cannot be cancelled, but cannot exceed this admission limit.

## Access and browser policy

Viewing requires reachability through the intended Tailscale deployment, with no
application login. Only a loopback listener is allowed. Configure the exact public
origin for Host and mutation-Origin validation. Configuration normalizes DNS case,
IPv6 spelling, and default HTTP(S) ports to browser origin syntax. No cross-origin browser API policy
is enabled yet; same-origin browser clients and non-browser HTTP clients are supported.
Library registration, scans/cancellation, profile creation, metadata imports,
rendition registration, catalog/edition changes, artwork imports/selections, and
admin file mapping require the protected local token.
Integrations currently use that admin credential; scoped integration credentials
are future work. No Catabolic credential belongs in browser code.

## Logical catalog and editions

The logical catalog is separate from the existing file-oriented `/items` projection.
`GET /catalog/items` includes discovered items and fileless logical containers. It
accepts `media_type`, `parent_id`, `limit` (1–200), and `offset`. Supply both
`source` and `external_id` to look up an imported metadata identity. Results include a
stable item ID, effective title, structure revision, parent, and number. Fetch one
with `GET /catalog/items/{id}`. The bundled browser continues using the file API.

Admin `POST /catalog/items` creates a logical item:

```json
{"title":"Example Show","media_type":"series"}
```

Create a season with `media_type: "season"`, `parent_id: SERIES_ID`, and `number: 1`.
Season zero represents specials. Episodes require a season parent and nonnegative
number; numbers are unique within their parent. Roots (`movie`, `series`, and
`unclassified`) have neither parent nor number. Structure is explicit, never
inferred from filenames. Manual creation returns 201 and revision 1; this POST is
not idempotent. Persist its returned ID before subsequent importer operations.

Classify an already discovered item with admin `PUT /items/{id}/structure`:

```json
{"expected_revision":0,"media_type":"episode","parent_id":"SEASON_ID","number":1}
```

Existing discovered items start with structure revision zero. Updates replace the
whole structure and require the current revision; stale revisions or duplicate
sibling numbers return 409. Invalid relationships, self-parenting, reclassification
that breaks children, and series/seasons with editions are rejected. Type rules
prevent cycles. Scans retain structure, metadata, IDs, editions, and progress.
Fileless items support the same metadata/artwork APIs; `/items/{id}` remains a file
projection and can return 404 for a known logical container without files.

`GET /items/{id}/editions` includes empty editions. Admin `POST` to that path accepts
`{"label":"Director's cut"}`. Admin `PUT /editions/{id}` renames an edition with
`{"expected_revision":1,"label":"Restored cut"}`. Admin `PUT /files/{id}/edition`
accepts `{"expected_edition_id":"OLD_ID","edition_id":"NEW_ID"}` and moves the
file only between editions of its existing item. File IDs, revision URLs, metadata,
registered renditions, and progress remain stable. Old empty editions are retained.
Cross-item merging, attaching a discovered item to a separately created movie,
bulk import transactions, episode ranges, alternate episode ordering, and edition-
specific progress timelines require a later explicit reconciliation workflow.
For now, classify the discovered item rather than creating a duplicate movie/episode.

Conventional metadata fields are `title` (nonempty, ≤500 bytes), `description`
(string, ≤8,000 bytes), `release_year` (integer 1–9999), `release_date` (valid
`YYYY-MM-DD`), and `cast` (at most 100 objects with `name` and optional `role`, each
nonempty and ≤200 bytes). Other non-null JSON fields remain supported as extensions.
The legacy `scan` source represents the initial baseline title, including manually
created logical items. Local and imported contributions override it as before.

## Artwork imports and selection

Admin uploads use raw PNG, JPEG, or WebP bytes. No remote URLs are fetched. For
example, with `ADMIN_TOKEN` supplied locally:

```sh
curl -X PUT \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H 'Content-Type: application/octet-stream' \
  --data-binary @poster.png \
  'http://127.0.0.1:8787/api/v1/items/ITEM_ID/artwork/poster/catabolic?expected_revision=0'
```

Roles are `poster`, `backdrop`, and `thumbnail`. Sources use lowercase letters,
digits, and hyphens. Each `(item, role, source)` contribution has its own optimistic
revision: zero creates it, otherwise supply its current revision. Stale writes
return 409. JSON APIs keep their 16 KiB limit; this binary endpoint permits 8 MiB.
Images must decode within 8192×8192 dimensions and the decoder's 64 MiB allocation
limit. Two decoder tasks may run concurrently; excess requests return 503. Validation
checks a decoded image, not all animation frames. Assets retain the uploaded bytes,
including embedded metadata, and use their SHA-256 as immutable identity.

`GET /items/{id}/artwork` returns contributions and role selections. Selection uses
an explicit pin first, then the `local` contribution, then a single distinct provider
asset. Different provider assets without an override produce an explicit conflict
and no selected asset. Identical bytes from different providers do not conflict.
Admin `PUT /items/{id}/artwork-selection/{role}` accepts:

```json
{"expected_revision":0,"asset_id":"SHA256_FROM_CONTRIBUTION"}
```

The chosen asset must currently be contributed to that item and role. A pin remains
on those immutable bytes even if its provider later replaces its contribution.
Set `asset_id` to null with the current selection revision to restore automatic
selection. Selection revisions are independent from source contribution revisions.

`GET /artwork/{asset_id}/content` serves the detected image MIME type with an ETag;
conditional `If-None-Match` returns 304. Artwork blobs are stored in SQLite, so DB
backups include them. Old assets/pins are retained; garbage collection, image
resizing, and provider fetching are not part of this API milestone.

## Viewing state and playback sessions

These are viewer operations: no admin token is required. Profile IDs separate
history, not authorization. The existing Host and same-origin mutation policies
still apply. All examples below are relative to `/api/v1`.

`GET /profiles/{profile}/viewing/{item}` returns a state even before viewing:
position zero, revision zero, `automatic_watched: false`, `manual_watched: null`,
`watched: false`, and `session_id: null`. Unknown profile/item IDs return 404.
The revision changes with accepted progress, session admission, and manual changes.

`PUT` to the same path accepts `{"expected_revision":N,"watched":true}` or false.
This changes the manual override, preserves the saved position, and invalidates
the current session. Null restores the automatic watched value. Local overrides
remain effective across subsequent playback until explicitly cleared; a false
override does not erase the automatic completion history. Changes affect only
that item; marking a series watched does not cascade to episodes. Stale revisions
return 409. To rewatch from the beginning, a client can start a fresh session and
send a position-zero event.

New clients should use sessions for every playback:

1. Read the viewing state and choose an available original or registered rendition
   from `/items/{item}/playback-options`.
2. `POST /profiles/{profile}/playback-sessions` with the current viewing revision:

```json
{
  "item_id":"ITEM_ID",
  "file_id":"SELECTED_FILE_ID",
  "file_revision":"SHA256",
  "expected_revision":0
}
```

The response is 201 with a session ID, pinned file/revision, duration if known,
initial position, sequence zero, and status `paused`. Initial position is clamped
to the selected file duration if known. Only a cataloged original belonging to the
item or a registered rendition with matching revisions is eligible. The source of
a rendition may be offline. Admission supersedes the previous session for that
profile/item, but not another profile or title. Concurrent admissions using the
same revision cannot both succeed. This POST is not idempotent: a lost response
can be reconciled by rereading viewing state and fetching its `session_id`.

3. `PUT /profiles/{profile}/playback-sessions/{id}` for progress/lifecycle events:

```json
{"sequence":1,"position_seconds":12.5,"status":"playing"}
```

Statuses are `playing`, `paused`, `stopped`, and `ended`. Sequences are positive
integers below the signed 64-bit maximum; increase them for each new event.
Gaps are allowed. Backward seeks are valid. Retrying the last accepted sequence
with exactly the same position/status is a no-op returning 200; older sequences or
a reused sequence with changed content return 409. Superseded/invalidated sessions
cannot write, including when retrying an old event. A changed cataloged file or
rendition source revision rejects new events with 409. File availability can change
while playback uses buffered bytes; it does not by itself invalidate an admitted
session. Media serving still performs its own source checks.

Positions must be finite, nonnegative, and no greater than the known duration plus
one second (or ten years when duration is unknown). `ended` requires a position
within one second of the known end. It marks automatic completion; playing near
the end or stopping does not. Automatic completion is sticky across rewatches;
manual overrides take precedence. Completion is a client-reported fact, not proof
that the viewer watched every frame. `stopped` and `ended` close the session;
resuming requires a new admission. Exact retries of the terminal event are allowed
while that session remains authoritative. Sessions and ordering survive restarts.

`GET /profiles/{profile}/playback-sessions/{id}` returns current durable state,
including `superseded` or `invalidated` status where applicable. A session under
a different profile returns 404. The implementation retains session history;
expiration/pruning remain future work; the event stream supplies playback change hints.

The bundled webpage still uses legacy `/progress` writes. Once a profile/item is
admitted through the session API, those writes return 409 permanently for that pair;
clients must migrate to session writes to preserve ordering guarantees. This API
milestone does not change the page or automatically apply stored preferences.

## Continue watching and episode order

`GET /profiles/{profile}/continue-watching?limit=50&offset=0` returns
`{items,total,limit,offset}`. It includes positive saved positions whose effective
watched state is false, ordered by server update time descending with item-ID ties.
Each entry has item ID, effective title, position, update time, and observed
availability (including valid registered renditions). Unavailable titles remain
visible for clients to explain missing media. Limit is 1–200. These are offset
pages, not stable cursors during concurrent updates. Existing legacy progress is
included after migration without inferring completion from its position.

`GET /profiles/{profile}/next-episode/{item}` returns `{next: EPISODE_OR_NULL}`.
The input must be an explicitly classified episode; other known items return 400.
Ordering is by season number, then episode number within the same series. Watched
candidates and season-zero specials are skipped by default; use
`skip_watched=false` and/or `include_specials=true` to change those rules. The search
is strictly after the current episode, never wraps, and returns null at the end.
It does not skip an unavailable next episode: availability is returned separately.
Alternate episode orderings, automatic playback, and inferred filename order are
not implemented.

## Playback preferences

`GET /profiles/{profile}/playback-preferences` returns `{revision,preferences}`.
Defaults have revision zero, empty language lists (client/source default),
`subtitle_mode: "foreign_audio"`, `quality: "auto"`, and
`conversion_recipe: "h264720p"`. Replace them with a PUT:

```json
{
  "expected_revision":0,
  "preferences":{
    "audio_languages":["ja","en-US"],
    "subtitle_languages":["en"],
    "subtitle_mode":"foreign_audio",
    "quality":"original",
    "conversion_recipe":"h264720p"
  }
}
```

Language lists are ordered preferences, at most ten unique entries each. Tags use
bounded ASCII language/script/region-style segments and are normalized to lowercase;
this is syntax validation, not an IANA registry lookup. Subtitle modes are `off`,
`always`, or `foreign_audio`. Quality is `auto`, strict `original`, or `convert`;
none starts processing. The [playback planner](#playback-planning) applies these
choices. Language preferences remain client hints, not guarantees about tracks
or formats.
Stale revisions return 409. Preferences are isolated by profile and persist across
restart; supported clients decide how to apply them to Demuxe.

## Operational endpoints

`GET /ready` returns `{ready,database,worker,shutting_down}` with 200 when the
migrated DB responds and the worker is running outside shutdown, otherwise 503.
It uses the configured Host boundary, just like `/health`. This readiness contract
does not promise media availability, free disk capacity, or codec compatibility.

Admin `GET /admin/diagnostics` returns uptime, package version, readiness, startup
Demuxe availability, a bounded current FFprobe check, catalog/job counts, available
stream slots, and SQLite page metrics. It contains no credentials or paths.
[OPERATIONS.md](OPERATIONS.md) documents configuration, service management, backup,
restore, and the distinction between liveness, readiness, and playback validation.


## Processing and generated media

Processing is explicit. Viewing, playback preferences, and media GETs never start
FFmpeg. All mutations below require the admin token. Listing jobs, capabilities,
and individual job status are available to viewers.

| Method and path | Behavior |
| --- | --- |
| `GET /processing-capabilities` | Recipe/backend choices and recent validated H.264 job evidence |
| `POST /processing-jobs` | Admit a fixed recipe; 201 new or 200 exact idempotent replay |
| `GET /processing-jobs` | Latest 100 jobs, including progress and terminal errors |
| `GET /processing-jobs/{id}` | Durable state, attempt, output file ID, cache expiration |
| `POST /processing-jobs/{id}/control` | `{"action":"cancel"}` or `{"action":"retry"}` |

```json
{
  "source_file_id": "catalog-file-id",
  "source_revision": "64-character-sha256",
  "recipe": "h264720p",
  "backend": "software",
  "idempotency_key": "client-generated-unique-request-key"
}
```

Version 1 recipes select the first video and first audio stream, if present, and
omit subtitles/data and copied metadata:

- `remux_mp4`: copy selected codecs into MP4. Incompatible codecs fail explicitly.
- `audio_aac`: copy video and encode AAC audio at 192 kbit/s.
- `h264720p`: fit video within 1280×720 without upscaling, encode 8-bit H.264 and
  AAC audio. Software uses libx264 CRF 23; macOS `videotoolbox` uses the hardware
  H.264 encoder at 2.5 Mbit/s with software fallback disabled. These are SDR presets;
  HDR preservation/tone mapping and subtitle burn-in are not qualified.

`software` works with every recipe; `videotoolbox` is admitted only for H.264 on
macOS. A configured backend choice is not proof that an encoder works. Successful
jobs are probed, checked for duration/required tracks/codecs and fully decoded;
`validated_jobs` records that evidence for this installation and time. It does not
claim all codecs, devices, drivers, or a fully GPU-resident processing pipeline.

The queue admits at most 100 active requests and runs one conversion at a time.
Sources must be available originals with a positive probed duration. Each attempt
hashes a bounded snapshot, pins the catalog revision, checks the physical source
again before publication, and stores recipe/backend/job/attempt provenance.
Output bytes are synced before one transaction registers the generated file,
rendition, and completed state. GET playback options exposes the result. Generated
outputs do not become duplicate original cards or scannable libraries.

A reused idempotency key with different input returns 409. Exact replay returns the
original job even after failure or expiration, while its history record is retained. Retry is explicit for failed or
cancelled jobs; use a new key to regenerate an expired completed result. Cancelled
or stale attempts cannot publish. A running cancellation remains `cancelling`
until its work stops. A blocked OS call may delay that transition while retaining
its admission slot. Startup requeues interrupted work with a new attempt and
finishes interrupted cancellation. An encoder supervisor detects server death,
including SIGKILL, and kills/reaps its child before exiting.

Progress is the latest encoded timestamp, not a guaranteed percentage. Internal
source paths and bounded encoder diagnostics stay in server logs. Job errors are
sanitized. Local recipes have no arbitrary argument, URL, shell-command, or output
path input. Catabolic dispatch/callback authentication and transfer are future work.

## Live events

`GET /events` uses Server-Sent Events. Up to 32 subscribers are admitted; excess
connections return 503. Events have persistent integer IDs and JSON data:

```text
id: 42
event: change
data: {"topic":"processing","resource_id":"job-id"}
```

Topics cover catalog/files/editions/renditions, metadata, artwork, libraries,
profiles, viewing/progress, playback sessions, preferences, scans, processing, and
schedules. IDs identify the changed resource (profile ID for viewer state), never
private paths. These are invalidation hints: refetch the relevant public endpoint.
They can repeat and do not represent complete state snapshots.

Supply `Last-Event-ID` on reconnect, or `?after=N`. The header takes precedence.
The last 10,000 changes are retained transactionally; rolled-back writes emit none.
A fresh subscription emits `reset` at the current cursor. An expired or future cursor
also emits `reset`; refetch state and continue from its supplied ID. Establish the
subscription before fetching a snapshot to avoid a read/subscribe gap. Heartbeats
arrive every 15 seconds; changes are polled in batches of 100 every 500 ms. Viewers
share the existing reachability policy; profiles do not imply event access isolation.

## Scheduled scans and cache maintenance

Admin `GET /admin/scan-schedules` lists persisted schedules. Admin
`PUT /admin/scan-schedules/{library_id}` accepts `{"interval_seconds":3600}`.
Intervals range from 60 seconds to one year; zero disables a schedule. The scheduler
checks every 30 seconds, coalesces with an already-active scan, and schedules the
next run from the current time instead of replaying every missed interval.

Admin `GET /admin/cache` reports usage and configured limits. `POST /admin/cache`
removes failed/cancelled attempts and expired completed outputs. Automatic cleanup
runs every 30 seconds. Cleanup defers while processing is active and preserves
outputs referenced by playing/paused sessions updated within the last hour.
Readers with already-open files may finish after expiration; new requests see the
output as unavailable. Only generated job directories are removed; originals,
external imported renditions, and unknown cache entries are never deleted.

Capacity admission reserves space for a source snapshot, the maximum output, and
1 MiB of overhead. FFmpeg gets an output-size cap; near-cap/truncated outputs are
rejected. This is an application byte budget, not a filesystem quota against
external writers. Unexpired results are not evicted to admit new work. Failed jobs
may be retried after space is freed. A database-only restore invalidates generated
entries belonging to another installation's cache; regenerate them with new requests.

## Playback planning

`POST /api/v1/profiles/{profile}/items/{id}/playback-plan` returns a read-only
plan. It requires no admin token, creates neither jobs nor sessions, and does
not check that an encoder or browser can actually play arbitrary media.

```json
{
  "mode": "auto",
  "recipe": "h264720p",
  "backend": "software",
  "client_support": [],
  "failed_versions": []
}
```

Omit `mode` and `recipe` to use the profile's `quality` and `conversion_recipe`.
`quality` now accepts `auto`, `original`, and `convert`; `original` is strict.
`conversion_recipe` accepts `remux_mp4`, `audio_aac`, or `h264720p` and defaults to
`h264720p` for existing preference documents and clients that omit it.

- **Auto:** prefer available originals, then existing renditions. Within each
  group, reported supported versions precede versions with unknown support.
  If none qualify, propose the chosen fixed recipe for an available original.
- **Original:** only select an original; otherwise return `blocked`. Never
  propose processing or silently select a rendition.
- **Convert:** reuse a current rendition backed by a completed local job for the
  chosen recipe, otherwise propose preparation. Informational recipe metadata
  supplied during external registration does not establish recipe compliance.
  Auto can still play externally registered renditions.

Optional `selected: {"file_id":"...","revision":"..."}` pins a version.
A version outside this item or at a different revision returns 409; a version
excluded by the mode, evidence, availability, or budget returns a blocked plan.
There is no fallback from an explicit selection.

Optional `source: {"file_id":"...","revision":"..."}` restricts planning to
that original and its renditions. A stale source or a source outside this item
returns 409. Use this to preserve a chosen edition when changing modes or
recipes. The bundled UI pins the title's displayed original, preserves that
source across mode changes, and changes it only when a different version is
explicitly selected. A ready selection includes `source_file_id` and
`source_revision` for clients to retain. Convert may choose a rendition only
when a completed job's published rendition matches its exact output revision.


`client_support` contains at most 64 `{file_id, revision, support}` entries,
where support is `supported`, `unsupported`, or `unknown`. This is client-supplied,
full-file evidence, not a server-inferred codec guarantee. Omitted support is
unknown and permits an attempt. `failed_versions` contains at most 64
`{file_id, revision}` entries to skip for this request without asserting a codec
incompatibility. Evidence for an older revision has no effect on its replacement.
The bundled UI uses actual open failures, with at most eight attempts, rather
than inferring Demuxe support from native HTML video support.

Optional positive `max_average_bitrate` is a bits-per-second ceiling calculated
from file size and duration. Unknown duration fails this filter. This is an
average, not a peak or a bandwidth measurement. The bundled UI does not infer a
bandwidth limit. Fixed processing recipes do not guarantee meeting that limit;
re-plan after conversion to evaluate the resulting file. If a current output for
the chosen recipe is unusable, the planner blocks rather than proposing the same
recipe repeatedly; choose another recipe or adjust the constraint.

Responses contain `mode`, `status`, `reason`, `selection`, `preparation`, and
`warnings`. `status` is one of:

| Status | Next client action |
| --- | --- |
| `ready` | Attempt `selection.media_url`, then start the normal revision-bound playback session. `client_check_required` preserves unknown compatibility. |
| `preparation_required` | Reuse `preparation.job_id` if present. Otherwise explicitly submit its source, revision, recipe, and backend to `POST /processing-jobs`, with an idempotency key and admin authorization. |
| `blocked` | Show the reason and let the viewer change mode/version. |

`selection.delivery` distinguishes `original` and `existing_rendition`.
`operation` distinguishes `original`, `remux`, `audio_conversion`,
`video_transcode`, and `unknown` (external provenance). Original delivery does
not imply native browser decoding. Server backend admission is checked for
preparation; capability evidence is not a guarantee that a future job succeeds.

Preparation completes and validates the whole file before it becomes playable.
The UI provides **Check prepared version** to re-plan after completion. This
contract does not implement streaming transcoding, viewer processing grants,
automatic track-specific recipes, subtitle burn-in, or adaptive bitrate delivery.

## Storage and catalog administration

All routes below require the admin bearer token and are included in OpenAPI.

| Method and path | Contract |
| --- | --- |
| `GET /admin/storage` | Free bytes, disk reserve, backup/history settings and maintenance health. |
| `POST /admin/storage` | Create and validate a local database snapshot; 201 manifest, 503 if busy or failed. A caller disconnect does not cancel an admitted snapshot. |
| `GET /admin/libraries` | Roots, revisions, enabled state, available/missing file counts, including detached libraries. |
| `PUT /admin/libraries/{id}` | Rename with `{ "expected_revision": 0, "name": "Movies" }`. |
| `POST /admin/libraries/{id}/detach` | Disable a root with `{ "expected_revision": 0 }`; no source deletion. |
| `POST /admin/libraries/{id}/relocate` | Verify and switch/reactivate a root with `{ "expected_revision": 0, "root": "/new/path" }`; no physical move. |
| `PUT /admin/profiles/{id}` | Rename with expected revision; retain viewing state. |
| `DELETE /admin/profiles/{id}?expected_revision=0` | Remove a nondefault profile and its viewing data; 204. The default profile is protected. |

Stale revisions and active library jobs return 409. Relocation rejects changed or
missing candidate files and roots overlapping server data. Reads are not identity
or access controls: selectable profiles still share the tailnet reachability policy.
`GET /admin/files` now accepts `available=false` alongside `library_id`, `limit`,
and `offset` to inspect missing entries. Detach retains those entries and metadata;
there is deliberately no bulk deletion of original files.

Scan requests accept `?full=true`. Job responses include `full_scan`,
`inspected_files`, and `reused_files`; counts describe a successfully published
scan. A full request cannot silently coalesce into an active incremental scan
(409). The default Unix incremental mode reuses unchanged file fingerprints;
see [operations](OPERATIONS.md) for the filesystem assumptions and full-scan escape.

Processing request replay is retained for the configured history horizon (90 days
by default), after terminal cache cleanup. A key whose record has been pruned is a
new request. Current viewing state and its authoritative session are preserved;
older pruned session IDs return 404. Clients should retain their own long-term
request audit history if needed.


## Experimental live delivery admission

`POST /api/v1/deliveries` accepts an optional `Idempotency-Key` header of 16–128
bytes. Clients retrying admission must retain the key and the same file revision,
start position and audio selection. Exact retries replay the original `201`
acknowledgement; different content with the same key returns `409
idempotency_conflict`. The legacy endpoint has one admin principal. Requests
without a key retain the original create-a-new-delivery behavior.

The initial delivery state and receipt commit atomically before encoder dispatch.
Losing the HTTP waiter does not cancel an admission already in progress. Replaying
a closed, interrupted or retired delivery returns its saved acknowledgement and
never starts another encoder. Fetch the delivery to learn its current state.
Receipts currently remain durable without automatic expiry; diagnostic-record
retention does not remove them.

The internal `delivery::AdmissionAuthority` port rechecks current permission and
source visibility within the admission transaction, including on replay. A08's
v2 adapter must implement that authority and bind the authenticated plan contract;
this experimental v1 endpoint does not provide v2 plan tokens.
