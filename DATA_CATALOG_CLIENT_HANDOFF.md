# PR #17 client interface handoff

Data/catalog owns preparation, processing capabilities, recipes/renditions,
schedule CRUD and storage/backup adapters in addition to its existing catalog
operations. Schemas 0038-0041 belong to this lane: preparation identity, logical schedules,
backup jobs and bounded metadata dispatch, respectively; existing migrations and
history remain intact. This is an implementation handoff, not an acceptance receipt.

Preparation uses the existing `createPreparation` contract:
`POST /api/v2/processing/preparations`, `Idempotency-Key` (16–128 characters), and the usual CSRF
header for cookie authentication. The body contains `source` (`file_id`,
`file_revision`), `timeline_id`, `recipe_id`, and all five `tracks` members
(`audio_component_id`, `subtitle_component_id`, `subtitle_policy`,
`audio_track_id`, `subtitle_track_id`). The initial client acceptance recipe is
`remux_mp4`; send null component/track IDs and `subtitle_policy: off` for the
baseline fixture. Explicit selections must be validated rather than ignored.

For example (replace IDs/revision with the current scoped source and timeline):

```json
{
  "source": {"file_id": "<file-id>", "file_revision": "<current-revision>"},
  "timeline_id": "<timeline-id>",
  "recipe_id": "remux_mp4",
  "tracks": {
    "audio_component_id": null,
    "subtitle_component_id": null,
    "subtitle_policy": "off",
    "audio_track_id": "a1",
    "subtitle_track_id": null
  }
}
```

`a1` selects the second source audio stream; use null for default selection.
The source must have an exact whole-file binding to the requested timeline.
Explicit component/subtitle selections are currently refused.

The response is HTTP 202 and the contract Job, with an ETag. Poll
`GET /api/v2/jobs/{job_id}` until terminal. A completed rendition job supplies
its output file ID in `result_ids`; refresh timeline versions/playback planning
to discover the generated version. Replaying the identical request under the
same principal/key returns its original admission. Changing its body conflicts.

Server-side preparation acceptance passed at `e8c5628` using real HTTP, FFmpeg
and FFprobe. The client/browser acceptance remains a separate lane gate. Capability qualification must reflect receipts,
not merely installed binaries. Updated commit IDs and evidence will be recorded
here as implementations pass review and validation.

## Additional interfaces and limits

- GET `/api/v2/processing/capabilities`: exact FFmpeg executable digest,
  bounded software encode/filter probe, zero interactive slots and one background
  slot. Qualification remains `unqualified` without a qualification receipt.
- GET `/api/v2/processing/recipes`: paginated `audio_aac`, `h264720p`, `remux_mp4`.
  H.264 requires a probed SDR first video stream; audio IDs `a0`, `a1`, etc.
  select the exact revision-local audio stream. Dynamic video profiles are not
  advertised because PreparationInput has no profile options.
- GET `/api/v2/processing/renditions`: paginated preparation identities, including
  pending, failed, expired and stale; ready requires current source, binding and
  publication-pinned output. Refresh its revision when catalog facts change.
- `/api/v2/schedules`: GET page and POST ScheduleInput (201, Idempotency-Key).
  `/api/v2/schedules/{id}`: GET, PUT ScheduleInput with If-Match, DELETE with
  If-Match (204). Permission: sources:manage; metadata refresh also requires
  catalog:write. Tasks `scan` and `metadata_refresh` execute. `preview` and
  `optimize` return `schedule_task_unavailable`; no such existing service was
  found in this checkout. Disabled rows remain editable.
- GET `/api/v2/admin/storage`: UInt64 strings for database, asset, temporary,
  reserved bytes; nullable free bytes. Asset bytes include embedded artwork
  (also present in database bytes) and catalogued generated media. These fields
  are not a disjoint disk-usage accounting sum.
- POST `/api/v2/admin/backups`: BackupInput with include_generated_media, 202
  Backup (not Job), Idempotency-Key. GET returns its page. Both require
  system:admin. Poll its job_id through the jobs API. Destination comes solely
  from server data-dir. Generated media is copied and revision-verified when
  requested; manifest digest hashes the complete manifest bytes. Embedded
  artwork inventory refers to blobs inside the verified database snapshot.
  Retention preserves job/backup history and withdraws ready before removing
  retained snapshot bytes. Existing restore tooling restores the database;
  automatic generated-cache relocation is not claimed.

Implementation through `e8c5628` passed real v2 preparation and generated-media
backup acceptance; `26fe6aa` and `77ebbb8` change only test keys/status expectations. The receipt is
`qualification/data-catalog/preparation-backup-receipt.json`, binary SHA-256
`73fe0e6e0223919ad84e613a206fc0d62101d8753d85729c6e0a57369c4dc3a5`.
The bounded production-policy model exhausted 894 states / 10,856 transitions
with no skipped checks. All 64 focused local tests pass, including the final
17-test HTTP suite at `77ebbb8`; binary hashes and source boundaries are in
`qualification/data-catalog/completion-api-receipt.json`.
