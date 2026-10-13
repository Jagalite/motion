# Completion review integration

Status: in progress; no release qualification claimed. Source base: completion accessibility plus offline end-to-end; original lane branches unchanged.

Core boundaries: HDR classification, offline history supersession, and cache eligibility are deterministic policies. Database reauthorization and durable demand holds are observed inside adapter transactions. Filesystem/codec/HTTP effects remain adapters.

## Finding ledger

- PR #35: [Preserve events for clients that lack the new refusal code](https://github.com/Jagalite/motion/pull/35#discussion_r4253707716) — pending source disposition.
- PR #39: [Re-evaluate plain text for burn-in on live routes](https://github.com/Jagalite/motion/pull/39#discussion_r4254983078) — pending source disposition.
- PR #39: [Refuse detected Dolby Vision before the SDR early return](https://github.com/Jagalite/motion/pull/39#discussion_r4254983084) — pending source disposition.
- PR #39: [Re-probe cached files that lack attachment metadata](https://github.com/Jagalite/motion/pull/39#discussion_r4254983089) — pending source disposition (older diff).
- PR #39: [Replay durable changes before probing FFmpeg](https://github.com/Jagalite/motion/pull/39#discussion_r4254983093) — pending source disposition.
- PR #39: [Queue burned-subtitle extraction when slots are busy](https://github.com/Jagalite/motion/pull/39#discussion_r4254983097) — pending source disposition.
- PR #40: [Include descendant timelines in play-order controls](https://github.com/Jagalite/motion/pull/40#discussion_r4255070102) — pending source disposition.
- PR #40: [Extract Windows basenames with platform-aware path handling](https://github.com/Jagalite/motion/pull/40#discussion_r4255070106) — pending source disposition.
- PR #40: [Reauthorize the subject on idempotent replay](https://github.com/Jagalite/motion/pull/40#discussion_r4255070112) — pending source disposition.
- PR #40: [Account for hidden library references before offering removal](https://github.com/Jagalite/motion/pull/40#discussion_r4255070120) — pending source disposition.
- PR #41: [Let later manual corrections supersede earlier ones](https://github.com/Jagalite/motion/pull/41#discussion_r4255145510) — pending source disposition.
- PR #41: [Enforce the marker cap on every writer](https://github.com/Jagalite/motion/pull/41#discussion_r4255145517) — pending source disposition.
- PR #41: [Seek to the final valid millisecond instead of rewinding](https://github.com/Jagalite/motion/pull/41#discussion_r4255145523) — pending source disposition.
- PR #41: [Require nullable marker fields to be present](https://github.com/Jagalite/motion/pull/41#discussion_r4255145528) — pending source disposition.
- PR #41: [Count label characters rather than UTF-8 bytes](https://github.com/Jagalite/motion/pull/41#discussion_r4255145531) — pending source disposition.
- PR #42: [Pass the sole saved root when the chooser is hidden](https://github.com/Jagalite/motion/pull/42#discussion_r4255152053) — pending source disposition (older diff).
- PR #42: [Include account identity in each library label](https://github.com/Jagalite/motion/pull/42#discussion_r4255152057) — pending source disposition.
- PR #42: [Keep the online view until the chosen root is validated](https://github.com/Jagalite/motion/pull/42#discussion_r4255186648) — pending source disposition.
- PR #44: [Decode the selected subtitle before publication](https://github.com/Jagalite/motion/pull/44#discussion_r4255429488) — pending source disposition.
- PR #44: [Resolve automatic subtitles before testing prepared candidates](https://github.com/Jagalite/motion/pull/44#discussion_r4255476351) — pending source disposition.
- PR #45: [Update the playback capability inventory](https://github.com/Jagalite/motion/pull/45#discussion_r4255464843) — pending source disposition.
- PR #45: [Reject caps that the planner cannot encode](https://github.com/Jagalite/motion/pull/45#discussion_r4255464851) — pending source disposition.
- PR #45: [Do not advertise resolutions capped playback cannot deliver](https://github.com/Jagalite/motion/pull/45#discussion_r4255464856) — pending source disposition.
- PR #45: [Prevent Original-only profiles from saving a bitrate cap](https://github.com/Jagalite/motion/pull/45#discussion_r4255464859) — pending source disposition.
- PR #48: [Validate the re-anchor against the submitted batch](https://github.com/Jagalite/motion/pull/48#discussion_r4255782096) — pending source disposition.
- PR #48: [Verify that the source history was actually issued](https://github.com/Jagalite/motion/pull/48#discussion_r4255782105) — pending source disposition.
- PR #48: [Backfill the epoch for existing compacted logs](https://github.com/Jagalite/motion/pull/48#discussion_r4255782109) — pending source disposition.
- PR #49: [Fence reapplication against another restore](https://github.com/Jagalite/motion/pull/49#discussion_r4255979317) — pending source disposition (older diff).
- PR #49: [Serialize restore-choice listing with the download pass](https://github.com/Jagalite/motion/pull/49#discussion_r4255979329) — pending source disposition.
- PR #49: [Preserve server progress when reapplying old observations](https://github.com/Jagalite/motion/pull/49#discussion_r4255979344) — pending source disposition.
- PR #49: [Refuse recovery when the old history identity is unknown](https://github.com/Jagalite/motion/pull/49#discussion_r4255979354) — pending source disposition.
- PR #49: [Resize the content view when the restore panel toggles](https://github.com/Jagalite/motion/pull/49#discussion_r4255979362) — pending source disposition.
- PR #49: [Refuse Keep when the restored baseline is unknown](https://github.com/Jagalite/motion/pull/49#discussion_r4256188668) — pending source disposition.
- PR #49: [Suppress older progress by timeline, not download ID](https://github.com/Jagalite/motion/pull/49#discussion_r4256188677) — pending source disposition.
- PR #50: [Report person-initiated removals](https://github.com/Jagalite/motion/pull/50#discussion_r4256391987) — pending source disposition (older diff).
- PR #50: [Reject sequences that do not fit SQLite](https://github.com/Jagalite/motion/pull/50#discussion_r4256391993) — pending source disposition.
- PR #50: [Reconcile reports for already-saved copies](https://github.com/Jagalite/motion/pull/50#discussion_r4256392005) — pending source disposition.
- PR #50: [Change the ETag when device_transfer changes](https://github.com/Jagalite/motion/pull/50#discussion_r4256392014) — pending source disposition.
- PR #51: [Reject renditions whose source is unavailable](https://github.com/Jagalite/motion/pull/51#discussion_r4256639035) — pending source disposition (older diff).
- PR #51: [Pin the prepared version selected by the form](https://github.com/Jagalite/motion/pull/51#discussion_r4256639044) — pending source disposition (older diff).
- PR #51: [Validate subtitle-policy combinations before matching](https://github.com/Jagalite/motion/pull/51#discussion_r4256639056) — pending source disposition.
- PR #51: [Page through all prepared renditions shown on the item](https://github.com/Jagalite/motion/pull/51#discussion_r4256639066) — pending source disposition (older diff).
- PR #51: [Recheck the rendition's original timeline binding](https://github.com/Jagalite/motion/pull/51#discussion_r4256639077) — pending source disposition (older diff).
- PR #51: [Keep downloaded renditions out of cache cleanup](https://github.com/Jagalite/motion/pull/51#discussion_r4256639084) — pending source disposition.
- PR #51: [Return only a downloadable generated binding](https://github.com/Jagalite/motion/pull/51#discussion_r4256773499) — pending source disposition.
- PR #51: [Continue paging until matching candidates are exhausted](https://github.com/Jagalite/motion/pull/51#discussion_r4256773507) — pending source disposition.
- PR #51: [Continue paging until matching candidates are exhausted](https://github.com/Jagalite/motion/pull/51#discussion_r4256773517) — pending source disposition.
- PR #51: [Scope expiry checks to the prepared rendition](https://github.com/Jagalite/motion/pull/51#discussion_r4256773527) — pending source disposition.
- PR #51: [Cache component resolution outside the candidate loop](https://github.com/Jagalite/motion/pull/51#discussion_r4256898577) — pending source disposition.
- PR #52: [Resolve automatic poster choices for the manifest](https://github.com/Jagalite/motion/pull/52#discussion_r4256828048) — pending source disposition.
- PR #52: [Admit artwork bytes before writing them](https://github.com/Jagalite/motion/pull/52#discussion_r4256828050) — pending source disposition.
- PR #52: [Include artwork-only staging in lifecycle inventory](https://github.com/Jagalite/motion/pull/52#discussion_r4256828057) — pending source disposition.
- PR #52: [Apply the poster timeout when a connection signal exists](https://github.com/Jagalite/motion/pull/52#discussion_r4256828065) — pending source disposition (older diff).
- PR #52: [Pass artwork metadata into crash recovery](https://github.com/Jagalite/motion/pull/52#discussion_r4256978811) — pending source disposition (older diff).
- PR #52: [Do not charge an already-staged poster twice](https://github.com/Jagalite/motion/pull/52#discussion_r4257057948) — pending source disposition.
- PR #52: [Constrain the poster dimensions inside the card](https://github.com/Jagalite/motion/pull/52#discussion_r4257057961) — pending source disposition.
- PR #52: [Retry the valid artwork blob after discarding staging](https://github.com/Jagalite/motion/pull/52#discussion_r4257190818) — pending source disposition.
- PR #53: [Restrict embedded chapters to the caller's catalog scope](https://github.com/Jagalite/motion/pull/53#discussion_r4257205675) — pending source disposition (older diff).
- PR #53: [Clear stale chapters when changed content cannot be probed](https://github.com/Jagalite/motion/pull/53#discussion_r4257205681) — pending source disposition (older diff).
- PR #53: [Make the backfill chapter probe cancellation-aware](https://github.com/Jagalite/motion/pull/53#discussion_r4257205696) — pending source disposition (older diff).
- PR #53: [Include derived chapters in the management page bound](https://github.com/Jagalite/motion/pull/53#discussion_r4257205706) — pending source disposition.
- PR #53: [Include derived chapter facts in the page snapshot](https://github.com/Jagalite/motion/pull/53#discussion_r4257318285) — pending source disposition.
- PR #53: [Isolate chapter output from the main media probe](https://github.com/Jagalite/motion/pull/53#discussion_r4257318293) — pending source disposition.
- PR #53: [Select the playback source before filtering empty chapters](https://github.com/Jagalite/motion/pull/53#discussion_r4257529659) — pending source disposition.
- PR #54: [Bound metadata to the cache manifest budget](https://github.com/Jagalite/motion/pull/54#discussion_r4257439801) — pending source disposition.
- PR #55: [Revalidate the download after subtitle conversion](https://github.com/Jagalite/motion/pull/55#discussion_r4257753043) — pending source disposition.
- PR #55: [Release subtitle artifacts when deleting profiles](https://github.com/Jagalite/motion/pull/55#discussion_r4257753053) — pending source disposition (older diff).
- PR #55: [Stop retrying terminal subtitle conversion failures](https://github.com/Jagalite/motion/pull/55#discussion_r4257753065) — pending source disposition.
- PR #55: [Include subtitle bytes in transfer verification](https://github.com/Jagalite/motion/pull/55#discussion_r4257753078) — pending source disposition.
- PR #55: [Page originals before reporting a subtitle unavailable](https://github.com/Jagalite/motion/pull/55#discussion_r4257837442) — pending source disposition.
- PR #55: [Evict production subtitle conversion counters](https://github.com/Jagalite/motion/pull/55#discussion_r4257837451) — pending source disposition (older diff).
- PR #55: [Reserve storage before persisting subtitle artifacts](https://github.com/Jagalite/motion/pull/55#discussion_r4257837457) — pending source disposition.
- PR #55: [Bound memory used by cached subtitle reads](https://github.com/Jagalite/motion/pull/55#discussion_r4257837463) — pending source disposition.

## Validation

- Desktop Node tests: 59 passed, 3 skipped (62 total), before subsequent small shell changes; rerun pending.
- Rust core compile and server/test typecheck submitted through Kyoto; queued, not passed.
- Independent Sol review pending.
