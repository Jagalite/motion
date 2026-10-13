# Completion review integration

Status: in progress; no release qualification claimed. Source base: completion accessibility plus offline end-to-end; original lane branches unchanged.

Core boundaries: HDR classification, offline history supersession, and cache eligibility are deterministic policies. Database reauthorization and durable demand holds are observed inside adapter transactions. Filesystem/codec/HTTP effects remain adapters.

## Finding ledger

- PR #35: [Preserve events for clients that lack the new refusal code](https://github.com/Jagalite/motion/pull/35#discussion_r4253707716) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #39: [Re-evaluate plain text for burn-in on live routes](https://github.com/Jagalite/motion/pull/39#discussion_r4254983078) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #39: [Refuse detected Dolby Vision before the SDR early return](https://github.com/Jagalite/motion/pull/39#discussion_r4254983084) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #39: [Re-probe cached files that lack attachment metadata](https://github.com/Jagalite/motion/pull/39#discussion_r4254983089) — inherited fix: Track::lacks_probe_facts forces reprobe for legacy attachment facts.
- PR #39: [Replay durable changes before probing FFmpeg](https://github.com/Jagalite/motion/pull/39#discussion_r4254983093) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #39: [Queue burned-subtitle extraction when slots are busy](https://github.com/Jagalite/motion/pull/39#discussion_r4254983097) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #40: [Include descendant timelines in play-order controls](https://github.com/Jagalite/motion/pull/40#discussion_r4255070102) — addressed by dedicated UI/marker follow-up35ddbff; see its ledger and combined verification.
- PR #40: [Extract Windows basenames with platform-aware path handling](https://github.com/Jagalite/motion/pull/40#discussion_r4255070106) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #40: [Reauthorize the subject on idempotent replay](https://github.com/Jagalite/motion/pull/40#discussion_r4255070112) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #40: [Account for hidden library references before offering removal](https://github.com/Jagalite/motion/pull/40#discussion_r4255070120) — addressed by dedicated UI/marker follow-up35ddbff; see its ledger and combined verification.
- PR #41: [Let later manual corrections supersede earlier ones](https://github.com/Jagalite/motion/pull/41#discussion_r4255145510) — addressed by dedicated UI/marker follow-up35ddbff; see its ledger and combined verification.
- PR #41: [Enforce the marker cap on every writer](https://github.com/Jagalite/motion/pull/41#discussion_r4255145517) — addressed by dedicated UI/marker follow-up35ddbff; see its ledger and combined verification.
- PR #41: [Seek to the final valid millisecond instead of rewinding](https://github.com/Jagalite/motion/pull/41#discussion_r4255145523) — addressed by dedicated UI/marker follow-up35ddbff; see its ledger and combined verification.
- PR #41: [Require nullable marker fields to be present](https://github.com/Jagalite/motion/pull/41#discussion_r4255145528) — addressed by dedicated UI/marker follow-up35ddbff; see its ledger and combined verification.
- PR #41: [Count label characters rather than UTF-8 bytes](https://github.com/Jagalite/motion/pull/41#discussion_r4255145531) — addressed by dedicated UI/marker follow-up35ddbff; see its ledger and combined verification.
- PR #42: [Pass the sole saved root when the chooser is hidden](https://github.com/Jagalite/motion/pull/42#discussion_r4255152053) — inherited fix: chrome always passes the selected root even with hidden chooser.
- PR #42: [Include account identity in each library label](https://github.com/Jagalite/motion/pull/42#discussion_r4255152057) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #42: [Keep the online view until the chosen root is validated](https://github.com/Jagalite/motion/pull/42#discussion_r4255186648) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #44: [Decode the selected subtitle before publication](https://github.com/Jagalite/motion/pull/44#discussion_r4255429488) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #44: [Resolve automatic subtitles before testing prepared candidates](https://github.com/Jagalite/motion/pull/44#discussion_r4255476351) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #45: [Update the playback capability inventory](https://github.com/Jagalite/motion/pull/45#discussion_r4255464843) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #45: [Reject caps that the planner cannot encode](https://github.com/Jagalite/motion/pull/45#discussion_r4255464851) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #45: [Do not advertise resolutions capped playback cannot deliver](https://github.com/Jagalite/motion/pull/45#discussion_r4255464856) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #45: [Prevent Original-only profiles from saving a bitrate cap](https://github.com/Jagalite/motion/pull/45#discussion_r4255464859) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #48: [Validate the re-anchor against the submitted batch](https://github.com/Jagalite/motion/pull/48#discussion_r4255782096) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #48: [Verify that the source history was actually issued](https://github.com/Jagalite/motion/pull/48#discussion_r4255782105) — root integration owns issued-history proof (migration0061); pending combined verification.
- PR #48: [Backfill the epoch for existing compacted logs](https://github.com/Jagalite/motion/pull/48#discussion_r4255782109) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #49: [Fence reapplication against another restore](https://github.com/Jagalite/motion/pull/49#discussion_r4255979317) — inherited fix: restored pins must match currentHistory before rewrite.
- PR #49: [Serialize restore-choice listing with the download pass](https://github.com/Jagalite/motion/pull/49#discussion_r4255979329) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #49: [Preserve server progress when reapplying old observations](https://github.com/Jagalite/motion/pull/49#discussion_r4255979344) — inherited fix: reapply uses captured restore baselines, not current viewing revisions.
- PR #49: [Refuse recovery when the old history identity is unknown](https://github.com/Jagalite/motion/pull/49#discussion_r4255979354) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #49: [Resize the content view when the restore panel toggles](https://github.com/Jagalite/motion/pull/49#discussion_r4255979362) — inherited fix: relayout IPC runs on restore panel changes.
- PR #49: [Refuse Keep when the restored baseline is unknown](https://github.com/Jagalite/motion/pull/49#discussion_r4256188668) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #49: [Suppress older progress by timeline, not download ID](https://github.com/Jagalite/motion/pull/49#discussion_r4256188677) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #50: [Report person-initiated removals](https://github.com/Jagalite/motion/pull/50#discussion_r4256391987) — inherited fix: pass reconciles pendingRemoved state.
- PR #50: [Reject sequences that do not fit SQLite](https://github.com/Jagalite/motion/pull/50#discussion_r4256391993) — inherited fix: report_transfer rejects integers above i64::MAX.
- PR #50: [Reconcile reports for already-saved copies](https://github.com/Jagalite/motion/pull/50#discussion_r4256392005) — inherited fix: each pass reconciles verified and removed copies before skipping local media.
- PR #50: [Change the ETag when device_transfer changes](https://github.com/Jagalite/motion/pull/50#discussion_r4256392014) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #51: [Reject renditions whose source is unavailable](https://github.com/Jagalite/motion/pull/51#discussion_r4256639035) — inherited fix: prepared availability checks source facts and output_rendition_status.
- PR #51: [Pin the prepared version selected by the form](https://github.com/Jagalite/motion/pull/51#discussion_r4256639044) — inherited fix: prepared download form carries selected version_id.
- PR #51: [Validate subtitle-policy combinations before matching](https://github.com/Jagalite/motion/pull/51#discussion_r4256639056) — inherited fix: kept_subtitle_problem runs before prepared candidate matching.
- PR #51: [Page through all prepared renditions shown on the item](https://github.com/Jagalite/motion/pull/51#discussion_r4256639066) — inherited fix: list_up_to retains ready rows and explicitly reports prepared_incomplete.
- PR #51: [Recheck the rendition's original timeline binding](https://github.com/Jagalite/motion/pull/51#discussion_r4256639077) — inherited fix: output_rendition_status checks current source whole original binding.
- PR #51: [Keep downloaded renditions out of cache cleanup](https://github.com/Jagalite/motion/pull/51#discussion_r4256639084) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #51: [Return only a downloadable generated binding](https://github.com/Jagalite/motion/pull/51#discussion_r4256773499) — inherited fix: advertised version requires current whole single-part output binding.
- PR #51: [Continue paging until matching candidates are exhausted](https://github.com/Jagalite/motion/pull/51#discussion_r4256773507) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #51: [Continue paging until matching candidates are exhausted](https://github.com/Jagalite/motion/pull/51#discussion_r4256773517) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #51: [Scope expiry checks to the prepared rendition](https://github.com/Jagalite/motion/pull/51#discussion_r4256773527) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #51: [Cache component resolution outside the candidate loop](https://github.com/Jagalite/motion/pull/51#discussion_r4256898577) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #52: [Resolve automatic poster choices for the manifest](https://github.com/Jagalite/motion/pull/52#discussion_r4256828048) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #52: [Admit artwork bytes before writing them](https://github.com/Jagalite/motion/pull/52#discussion_r4256828050) — inherited fix: poster bytes included in admission before savePoster.
- PR #52: [Include artwork-only staging in lifecycle inventory](https://github.com/Jagalite/motion/pull/52#discussion_r4256828057) — inherited fix: staged inventory includes artwork and subtitles.
- PR #52: [Apply the poster timeout when a connection signal exists](https://github.com/Jagalite/motion/pull/52#discussion_r4256828065) — inherited fix: AbortSignal.any combines connection and deadline.
- PR #52: [Pass artwork metadata into crash recovery](https://github.com/Jagalite/motion/pull/52#discussion_r4256978811) — inherited fix: recover receives posterIdentity.
- PR #52: [Do not charge an already-staged poster twice](https://github.com/Jagalite/motion/pull/52#discussion_r4257057948) — inherited fix: staged artwork is discarded before quota admission; no double charge.
- PR #52: [Constrain the poster dimensions inside the card](https://github.com/Jagalite/motion/pull/52#discussion_r4257057961) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #52: [Retry the valid artwork blob after discarding staging](https://github.com/Jagalite/motion/pull/52#discussion_r4257190818) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #53: [Restrict embedded chapters to the caller's catalog scope](https://github.com/Jagalite/motion/pull/53#discussion_r4257205675) — addressed by dedicated UI/marker follow-up35ddbff; see its ledger and combined verification.
- PR #53: [Clear stale chapters when changed content cannot be probed](https://github.com/Jagalite/motion/pull/53#discussion_r4257205681) — addressed by dedicated UI/marker follow-up35ddbff; see its ledger and combined verification.
- PR #53: [Make the backfill chapter probe cancellation-aware](https://github.com/Jagalite/motion/pull/53#discussion_r4257205696) — addressed by dedicated UI/marker follow-up35ddbff; see its ledger and combined verification.
- PR #53: [Include derived chapters in the management page bound](https://github.com/Jagalite/motion/pull/53#discussion_r4257205706) — addressed by dedicated UI/marker follow-up35ddbff; see its ledger and combined verification.
- PR #53: [Include derived chapter facts in the page snapshot](https://github.com/Jagalite/motion/pull/53#discussion_r4257318285) — addressed by dedicated UI/marker follow-up35ddbff; see its ledger and combined verification.
- PR #53: [Isolate chapter output from the main media probe](https://github.com/Jagalite/motion/pull/53#discussion_r4257318293) — addressed by dedicated UI/marker follow-up35ddbff; see its ledger and combined verification.
- PR #53: [Select the playback source before filtering empty chapters](https://github.com/Jagalite/motion/pull/53#discussion_r4257529659) — addressed by dedicated UI/marker follow-up35ddbff; see its ledger and combined verification.
- PR #54: [Bound metadata to the cache manifest budget](https://github.com/Jagalite/motion/pull/54#discussion_r4257439801) — inherited fix: Store::publish trims optional metadata at METADATA_BUDGET.
- PR #55: [Revalidate the download after subtitle conversion](https://github.com/Jagalite/motion/pull/55#discussion_r4257753043) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #55: [Release subtitle artifacts when deleting profiles](https://github.com/Jagalite/motion/pull/55#discussion_r4257753053) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #55: [Stop retrying terminal subtitle conversion failures](https://github.com/Jagalite/motion/pull/55#discussion_r4257753065) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #55: [Include subtitle bytes in transfer verification](https://github.com/Jagalite/motion/pull/55#discussion_r4257753078) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #55: [Page originals before reporting a subtitle unavailable](https://github.com/Jagalite/motion/pull/55#discussion_r4257837442) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #55: [Evict production subtitle conversion counters](https://github.com/Jagalite/motion/pull/55#discussion_r4257837451) — inherited fix: conversion instrumentation is one atomic aggregate counter, no retained ID map.
- PR #55: [Reserve storage before persisting subtitle artifacts](https://github.com/Jagalite/motion/pull/55#discussion_r4257837457) — fixed in this integration lane; Rust/runtime verification pending combined CI.
- PR #55: [Bound memory used by cached subtitle reads](https://github.com/Jagalite/motion/pull/55#discussion_r4257837463) — fixed in this integration lane; Rust/runtime verification pending combined CI.

## Validation

- Desktop Node tests: 59 passed, 3 skipped (62 total), latest source rerun passed.
- Rust core compile and server/test typecheck were submitted through Kyoto, stayed queued under memory pressure, then cancelled as redundant when the integration owner moved validation to exact combined CI. No local Rust pass claimed.
- GPT-6.1 Sol reviewed exact checkpoint `a4cd66a` with no outstanding actionable findings after bounded-cache and test-status repairs. Follow-up `505ee18` corrects prepared automatic text selection and includes its router regression; combined source review and runtime CI remain authoritative.
- Focused desktop download tests after the byte-proof regression: 35 passed, 3 skipped.

## Follow-up checkpoint

- Required subtitle artifact state, digest and byte count are durable in migration0063. First preparation changes the download revision; the manifest supplies that revision and the desktop reports media plus required subtitle bytes.
- Terminal codec/output failures remain terminal across restart; contention, process launch and I/O failures remain retryable. Cached reads have a global four-reader bound and a 16MiB file bound, with permits retained through response bodies.
- Candidate enumeration pages until an eligible result or exhaustion, retaining only representative refusal facts; component resolution is cached per source revision.
- Text burn fallback replans through the production playback policy. Prepared selection uses the profile's automatic subtitle choice before rendition matching.
- Profile deletion schedules subtitle cleanup after commit, including the legacy adapter.
- No full application build, live browser or Electron playback qualification was run in this lane.
