# PR #17 client interface handoff

Data/catalog owns preparation, processing capabilities, recipes/renditions,
schedule CRUD and storage/backup adapters in addition to its existing catalog
operations. Schema 0038 is reserved for these adapters; existing migrations and
history remain intact. This is an implementation handoff, not an acceptance receipt.

Preparation uses the existing `createPreparation` contract:
`POST /api/v2/processing/preparations`, `Idempotency-Key`, and the usual CSRF
header for cookie authentication. The body contains `source` (`file_id`,
`file_revision`), `timeline_id`, `recipe_id`, and all five `tracks` members
(`audio_component_id`, `subtitle_component_id`, `subtitle_policy`,
`audio_track_id`, `subtitle_track_id`). The initial client acceptance recipe is
`remux_mp4`; send null component/track IDs and `subtitle_policy: off` for the
baseline fixture. Explicit selections must be validated rather than ignored.

The response is HTTP 202 and the contract Job, with an ETag. Poll
`GET /api/v2/jobs/{job_id}` until terminal. A completed rendition job supplies
its output file ID in `result_ids`; refresh timeline versions/playback planning
to discover the generated version. Replaying the identical request under the
same principal/key returns its original admission. Changing its body conflicts.

Preparation acceptance remains pending until the endpoint, publication fencing,
and real FFmpeg fixture pass. Capability qualification must reflect receipts,
not merely installed binaries. Updated commit IDs and evidence will be recorded
here as implementations pass review and validation.
