# A06/A07 execution and delivery handoff

Status: A06/A07 implementation is in progress. The earlier macOS/Unix baseline
passed its listed checks, but that baseline is not completion of the assigned
workstreams. Native qualification recorded here is macOS arm64 only. Remaining
implementation and integration gates are listed below.

## Implemented boundaries

- `crates/core/src/work.rs` decides shared admission, priority, reservation
  ownership, cancellation and stuck-worker accounting. `src/execution.rs` binds
  those decisions to native ownership witnesses and confirmed termination.
  Cancelled or timed-out delivery preparation retains capacity and generation
  ownership until the blocking filesystem operation actually exits.
- `crates/core/src/processing.rs` and `jobs.rs` fence publication by source and
  attempt identity, cancellation, validation and previous publication. SQLite
  commits publication and job completion atomically; filesystem and encoder
  observations stay in adapters.
- `crates/core/src/execution_deadline.rs` decides startup, no-progress, total
  and duration-derived deadline expiry from monotonic elapsed time and advancing
  media time. The processing adapter supplies observations, drains bounded pipes
  and executes termination. Optional liveness settings preserve existing defaults.
- `crates/core/src/delivery_admission.rs` decides durable receipt replay and
  request conflicts. The adapter commits the initial state and receipt before
  dispatch, survives lost HTTP waiters, and rechecks transactional authority.
- `crates/core/src/delivery.rs` owns generations, activation, leases, playheads,
  pacing and retention. It also decides live route eligibility
  (`live_route_supported`) and lets a stream-copy generation's segment 0 start
  at the keyframe before the request (`Ready.first_segment_start_ms`). The live adapter validates source descriptors and output,
  shares execution capacity, and persists restart fences. Pending-generation
  heartbeats cannot replace the active playhead. Restart records are diagnostic;
  they cannot revive an old transport.

## Integration dependencies and unfinished scope

| Item | Required next artifact or qualification |
| --- | --- |
| Authenticated v2 plans and delivery admission | A08 principal/profile authorization, authenticated expiring plan tokens, current-permission revalidation, ticket routes and transactional idempotency receipts. The v1 live endpoint is not that contract. |
| Lost create acknowledgment and retired-delivery replay | Durable service/v1 receipts and transactional authority rechecks are implemented and tested, including lost waiters and retired/recovered replay without another encoder. Remaining integration is the v2 plan wire-request identity and A08 authority adapter. |
| Migration allocation | A02 must finalize the delivery-session migration number. This branch has provisional 0015 and admission receipts at 0020; the A08 checkout was observed preparing an uncommitted rename to 0018. Do not apply both. |
| Windows containment | Implement and natively qualify a Job Object adapter, including descendants and parent death. The existing direct-child fallback is not whole-tree qualification; live delivery rejects non-Unix platforms. |
| Broader execution policy | Resource estimates/limits beyond the current scalar capacity ledger remain to be integrated with agreed policy/configuration contracts. Durable processing now has an optional expected-media-duration deadline; live delivery retains its separate pacing policy. |
| Live pipeline breadth | Remux and audio_convert copy routes are implemented behind `processing.experimental_copy_routes` (default off). Before enabling: a signed media-to-timeline mapping in core covering B-frame/AAC-priming shifts and MP4 edit lists; H.264 profile/level, rotation and interlace observations in eligibility; admission against source GOP spacing (12 s target); browser qualification of copied streams (A11). Hardware live encoding, HDR and subtitles remain unimplemented. |
| Client and storage matrix | A11/A12 real browser/desktop open, seek, generation activation and restart; remote/NAS and supported-platform native fault runs remain required. |

The A08 checkout inspected during this pass has v2 identity, content,
organization, events and system adapters, but no playback-plan adapter. Its
migration changes are in progress. This handoff does not edit that checkout or
establish those dependencies as complete.

## Operational and rollback notes

New processing liveness settings are optional and bounded to 1–86400 seconds.
Both omission and JSON null retain the previous total-timeout-only behavior.
No shared/live user configuration is changed by the tests. Native acceptance
uses disposable media, state and cache roots.

Delivery schema installation changes SQLx migration history. Coordinate the
final number before integrating or deploying; an already migrated development
database needs an explicit migration reconciliation, not a silently renamed
historical migration. Downgrades must account for retained diagnostic rows.

No changes in this handoff have been pushed or deployed by this workstream.

## Latest local validation

The resumed admission/execution increment passed 26 server unit tests, six real
FFmpeg delivery tests, and all 20 processing smoke checks. The expanded liveness
model exhausted 1,335 states and 5,730 transitions across seven budget cases.
The mutation harness passed six baselines and detected eleven compiled regressions.
`VALIDATION.md` records exact bounds, the final binary hash and failed preliminary
startup/recipe runs. These results do not close the unfinished scope above.

## Earlier Unix baseline acceptance

On macOS 26.5.2 arm64 with Rust 1.99.0 and FFmpeg 8.1.2:

- `cargo test -p playscale --lib -- --nocapture`: 25 passed.
- `cargo test -p playscale-core --test execution_deadline_model -- --nocapture`:
  241 states, 1,030 transitions, graph exhausted, zero skipped checks.
- `python3 scripts/check_execution_delivery_mutations.py`: three baselines passed;
  seven compiled regressions detected by their expected test failures.
- `cargo test -p playscale --test delivery -- --nocapture`: three real FFmpeg
  tests passed, no tool-availability skips.
- `cargo build -p playscale --bin playscale`: passed.
- After the build/test queue completed and the executable was warmed with
  `--help`, `python3 scripts/processing_smoke.py`: all 18 checks passed, including
  software recipes, both liveness deadlines, reaping/capacity recovery,
  VideoToolbox probe/decode, SIGKILL/restart, DB-only restore and original integrity.
- `cargo fmt --all -- --check` and `git diff --check`: passed.

Earlier startup/readiness timeouts are documented in `VALIDATION.md`; they are
not counted as passing runs. No application deadline was relaxed. Browser,
Linux, Windows, NAS and remote playback qualification is not inferred from these
macOS subprocess/API checks.
