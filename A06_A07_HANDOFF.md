# A06/A07 execution and delivery handoff

Status: the scoped macOS/Unix execution and experimental v1 H.264/AAC SDR
delivery implementation and handoff are complete. Native qualification in this
pass is macOS arm64 only. Full Motion A06/A07 integration and release gates remain
open below; `VALIDATION.md` records exact checks and bounds.

## Implemented boundaries

- `crates/core/src/work.rs` decides shared admission, priority, reservation
  ownership, cancellation and stuck-worker accounting. `src/execution.rs` binds
  those decisions to native ownership witnesses and confirmed termination.
- `crates/core/src/processing.rs` and `jobs.rs` fence publication by source and
  attempt identity, cancellation, validation and previous publication. SQLite
  commits publication and job completion atomically; filesystem and encoder
  observations stay in adapters.
- `crates/core/src/execution_deadline.rs` decides startup, no-progress and total
  deadline expiry from monotonic elapsed time and advancing media time. The
  processing adapter supplies observations, drains bounded pipes and executes
  termination. Optional liveness settings preserve existing defaults.
- `crates/core/src/delivery.rs` owns generations, activation, leases, playheads,
  pacing and retention. The live adapter validates source descriptors and output,
  shares execution capacity, and persists restart fences. Pending-generation
  heartbeats cannot replace the active playhead. Restart records are diagnostic;
  they cannot revive an old transport.

## Integration dependencies and unfinished scope

| Item | Required next artifact or qualification |
| --- | --- |
| Authenticated v2 plans and delivery admission | A08 principal/profile authorization, authenticated expiring plan tokens, current-permission revalidation, ticket routes and transactional idempotency receipts. The v1 live endpoint is not that contract. |
| Lost create acknowledgment and retired-delivery replay | Integrate admission with A08's durable principal-scoped idempotency transaction. Replaying a retired receipt must never start another encoder. Creation is not currently idempotent. |
| Migration allocation | A02 must finalize the delivery-session migration number. This branch has provisional 0015; the A08 checkout was observed preparing an uncommitted rename to 0018. Do not apply both. |
| Windows containment | Implement and natively qualify a Job Object adapter, including descendants and parent death. The existing direct-child fallback is not whole-tree qualification; live delivery rejects non-Unix platforms. |
| Broader execution policy | Resource estimates/limits beyond the current scalar capacity ledger and expected-media-duration execution policy remain to be integrated with agreed policy/configuration contracts. |
| Live pipeline breadth | Remux, audio-only conversion, hardware live encoding, HDR/subtitles and broader exact-track combinations need implementation/qualification beyond the current H.264/AAC SDR live recipe. |
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

## Final local acceptance

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
