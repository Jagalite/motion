# Catabolic backend qualification

Integration branch: `feat/catabolic-main-backend`, based on Motion `d2d091a`.
Upstream: Catabolic main `6969f08c5d9b207afe8eec819555759be136644d`.
Host: macOS arm64; Rust 1.99.0. Application cutover: **not implemented**.
No existing catalog/media database was migrated or modified.

The original foundation receipt below predates import-staging changes. Its source
and binary hashes describe that historical checkpoint, not the current worktree.
Import-staging qualification is recorded separately at the end of this file.

## Dependency boundary

One bundled `libsqlite3-sys 0.38.2` is shared by SQLx 0.8.6 and rusqlite 0.40.2.
The SQLx crate archive SHA-256 is recorded in its vendor patch note. All 56
upstream files were compared to the official cached archive; only the documented
manifest dependency line differs. Catabolic's patched fancy-regex uses the same
pinned Git source as the native crates. SQLite build flags match upstream.

The combined lock additionally aligns chrono, libc, serde_json and uuid with
Catabolic's exact pins. Their application-wide behavior requires regression
qualification; graph resolution alone does not establish it.

## Completed non-runtime checks

- Core admission tests: 2 passed, including 729 identity comparisons.
  Kyoto compilation job `8f0eeeed64ff484693eaf64875bebb92`; emitted executable
  `catalog_backend-f5638b50eba702ad` ran separately with exit 0.
- Dependency notice checks: 6 tests passed; target-specific notices regenerated
  and checked. Missing upstream archive license files were retained at the exact
  source revisions recorded beside them.
- Packaging helper unit tests: 17 passed. No package was built or installed.
- Formatting and whitespace checks passed.
- Pre-extraction Clippy: `bb69d5f2b8fa4b7698f477789546ed26` passed for the
  application library, operator binary and backend tests. A later final-source
  check is required after moving the same adapter into its own crate.

## Build history and remaining evidence

All Rust builds/checks were submitted to Kyoto's default shared queue. Its policy
adds `--no-run` to test submissions. Test compilation and execution are separate.

Initial resolution failed on locked serde_json; a targeted lock update aligned
the shared exact pins. An initial update request was rejected for an ambiguous
sha2 package selector; the corrected update succeeded. These are preparation
failures, not passing build evidence.

The default parallel check and two-job test build were repeatedly deferred for
memory/swap pressure. A one-job, debug-info-disabled build compiled the native
store/runtime but full Motion code generation was again interrupted. These jobs
were explicitly cancelled before extracting the adapter crate. Their logs remain
in Kyoto's state directory; no interrupted run is counted as a pass:

- `0a199fc9d8c6482c991277cf02b633d7` (check)
- `ef26f0a3bba0453eb104d4edc2a1577f` (two-job test compilation)
- `29170c41ff7e42b48d5d605d7359f370` (application integration/regression compilation)
- `c5cd0cdb994d4002a5345da72dff4d5a` (workspace library test compilation)

Focused native crate result: **8 passed, 0 failed, 0 ignored** (one worker-lifetime
unit test and seven integration tests). Kyoto final compilation job:
`fd79c398ce4d4f11a7b51e862c3efe6e`. Both emitted binaries were executed separately
with `--nocapture --test-threads=2`. Initial focused compilation/execution also
passed under job `bbaa90dc435f41a1aee7f9d0ca179ca1` before a test-layout-only fix.
Final execution logs are retained in `artifacts/catabolic-integration/` locally.

The fixture opened native schema 39. SQLx and Catabolic reported the same SQLite
source ID: `2026-06-03 19:12:13 d6e03d8c777cfa2d35e3b60d8ec3e0187f3e9f99d8e2ee9cac695fd6fcdf1a24`.
Math functions were enabled and soundex absent. Lost-acknowledgment replay returned
an identical result/generation with exactly one native item and one receipt.

Final-source Clippy/typecheck: **passed**, Kyoto job
`1f31d1b8a0a24957ae10fc497df339f0`, covering both packages' libraries, binaries and
all test targets with warnings denied (the repository's existing two lint
exceptions retained). Prior job `9ce3bdd01c864517b09985765be1a7ba` found one
`items_after_test_module` layout lint; moving the helper functions fixed it.
This check does not execute the full-application tests.

A final application regression compilation after extraction is tracked as
`4fac146e427545c9810491ab589c0299`; compilation passed. Executed application
suites: catalog persistence/restore **16 passed**, catalog workflows **27 passed**,
search **3 passed**, v2 catalog **21 passed**. No ignored tests in these suites.

The new combined-database test initially failed a test-only assumption that the
number of migrations equals the latest version. There are 65 embedded migrations,
ending at version 66. The assertion now checks the actual embedded count and
latest version separately. Kyoto job `6add461a564b4019b6772aea7c3296b3` compiled
that correction; its focused rerun passed **1/1**. The corrected test also passed
Clippy under job `53d23e518d2540a7b0439d92138c3574`.
The initial failed aggregate log/JSON remains in the local artifact directory.

Operator CLI smoke passed against disposable databases: native initialization,
inspection, refusal to overwrite, Motion binding/migration, durable item mutation,
receipt replay and conflicting operation-key refusal. Both stores were removed
with the disposable fixture; no live application service was launched.

The native crate's fixture applies only the real command-journal migration to a
disposable SQLx database. It does **not** simulate Motion's complete upgrade
runner. `tests/catabolic_backend.rs` separately composes the actual `db::connect`
with a native catalog. That test and the existing catalog persistence/workflow,
search and v2 catalog suites are recorded separately from the native-crate result.

No full suite, real source/helper scan, migration/import activation, application
user-scope integration, browser playback, package, other-platform or production
readiness claim is made by this checkpoint.

## Retained receipt

`qualification/catabolic-native-backend.json` records the changed-source manifest,
upstream pin, final artifact/log hashes and the distinction between the failed
initial aggregate and the successful focused correction. Across the core, native
adapter and selected application suites there are 78 distinct passing Rust tests
at this checkpoint; repeated executions are not counted again. This is a focused
integration checkpoint, not a complete repository or production cutover gate.

## Import staging checkpoint

Current source receipt: `qualification/catabolic-import-staging.json`. The native
adapter now applies migrations 0066 and 0067 in its integration fixtures. The
full Motion migration test checks the actual embedded count and latest version 67.

- Native adapter: **14 passed** (13 integration tests and one worker-lifetime test).
  Compilation job `443002922c414b4db70293a73f3f28ac`; tests executed separately.
- Full Motion migrations/reopen: **1 passed**. Compilation job
  `0ae98a1fcad7436ba441ff6e8835ddc3` succeeded after two memory-pressure deferrals.
- Changed package libraries, CLI and test targets passed Clippy with warnings
  denied and the repository's two existing lint exceptions, job
  `f75972888b3f42db8949abad05227b18` (one pressure deferral).
- CLI smoke passed against disposable databases: prepare, remove input file,
  stage, restart/replay and validate. Exactly three native receipts, three host
  commands, one retained snapshot and zero published items remained.
- Formatting and whitespace checks passed.

Import tests assert immutable input, descriptor matching, receipt counts,
interrupted-batch recovery, cross-batch duplicate rejection, independently granted
typed metadata rehearsal and preserved source revision, and unresolved-evidence
refusal. These use actual native owners and SQLite; they are not a separate model
of import behavior. No new Motion cutover state machine or activation entrypoint
has been added. The earlier application regression suites were not repeated for
this staging checkpoint and their historical receipt remains separate.

No Motion source export, real catalog migration, activation, server-route switch,
application-user authorization, source scan, playback or package qualification is
claimed by this checkpoint. The overall main-backend integration remains active.

## Motion export checkpoint

Current source receipt: `qualification/catabolic-motion-export.json`; earlier
receipts are historical snapshots. Motion schema now ends at 68.

Final focused runtime results: **23 passed, 0 failed, 0 ignored**: core policy 3,
native integration 13, worker lifetime 1, source export 5, actual Motion migration
and reopen 1. Core provenance policy enumerates 432 combinations in addition to
the original 729 binding comparisons. The exporter tests apply the actual embedded
SQL migrations; full `db::connect` remains a separate application test.

Kyoto compilation jobs `d8e88d83cba04152b9699350e8142859` (core/adapter) and
`0ce721b7fc8b4f12ae08e62943bb5c72` (application/CLI) passed. Emitted tests ran
separately. Changed-target Clippy passed under job
`5764e20834c449ee846cfa36400d47bb`. An intermediate compile observed an older core
dependency while edits were in progress; final combined compilation passed.
Clippy also found a redundant borrow in the extracted persistence helper, which
was corrected. Historical failed jobs are identified in the new receipt.

Separate-process CLI smoke passed: capture a Motion database, edit its source
metadata, capture again and get the original snapshot, stage it, refuse generic
validation without owner grants, then validate using captured-source provenance.
The returned typed mapping retained source revision 7 after the live source row
advanced to 8. There were three native receipts and zero published items.

No persistent freeze or completed reference audit is claimed. Unsupported source
facts and the missing audit remain unresolved native evidence; this output cannot
activate. Complete typed migration, large-catalog partitioning, service routing,
source/helper lifecycle, playback and package qualification remain open.

## Durable source-freeze checkpoint

Current source receipt: `qualification/catabolic-source-freeze.json`. Host schema
ends at 69. Source capture retains the same catalog-data format while optionally
binding the export to the current durable freeze epoch.

**99 distinct Rust tests passed, none failed or ignored**: binding/provenance core
3, freeze model/boundaries 2, native integration 13, source export/freeze 10, worker
lifetime 1, full Motion database/startup 3, catalog persistence 16, catalog workflows
27, search 3, v2 catalog 21. Tests ran from emitted binaries after Kyoto compilation.
The application job `2704a0ce415e407d821a5e4d5470e6b2` passed after one pressure
deferral. Core/native compilation `076e24b9d52b4c34a9b0b95b2536b331` and Clippy
`3e933b6ce90a4e71a90ba0a442e3d7ce` passed. A final test-only addition checks partial
DDL rollback; its compile/Clippy jobs are recorded separately in the receipt and
the entire 10-test export/freeze suite passed afterward.

The Stateless model runs production transitions and exhausts its declared 13
states/390 transitions with no skipped checks. Bounds are two migration IDs,
invalid empty IDs, revisions/epochs 0..5, and stale/duplicate events. Separate
boundary tests cover malformed state and counter overflow. This does not prove
unbounded histories, OS worker cancellation, SQLite durability or native cutover.

Real SQLite tests check competing ownership, one event on replay, write refusal,
rebind persistence, old-epoch release refusal, irreversible seal, changed guard
inventory, and rollback when a trigger-name collision interrupts guard installation.
Application tests check frozen startup refusal, operator-only abandonment/reopen,
and malformed core-state rejection even when its stored live flag is true.

The CLI smoke passed across separate processes against disposable databases.
A separate SQLite writer was refused while frozen. Capture/stage/validation stayed
available through operator control access; stale abandonment and reused migration
identities failed. Matching abandon restored legacy writes. Native publication
remained zero items. No live catalog or running user service was frozen.

The initial model compilation lacked a required Hash implementation and static
check-name lifetime; those interface issues were corrected. The failed job is
retained separately from final evidence. Formatting and whitespace checks passed.

The primary-backend goal remains incomplete: native mappings, application reference
audit, worker drain, native activation/restart, production routes, source observation,
playback, larger catalogs, packaging and other-platform qualification remain.

First-party staged whitespace checks passed. Staging the previously untracked
vendor archive exposed three upstream trailing-whitespace lines in
`vendor/sqlx-sqlite-0.8.6/src/connection/serialize.rs`. That complete file was
recompared byte-for-byte to the verified crate archive; it is intentionally
unchanged to preserve the recorded one-line dependency patch boundary.

## Manual identity pin mapping

The v2 Motion exporter maps live manual identities to the native pin owner.
The production core reuses Motion's identity-withdrawal policy; SQLite supplies
match state, identity, revision and alias status from the same capture transaction.
The core test exhausts 16 combinations of four match states, retirement and
identity presence. This is a finite policy check, not a concurrency model.
Native integration exercises two accepted identities through batch packing,
retained snapshot replay, restart and typed rehearsal. Matched/unmatched/ambiguous
identities and retired aliases remain evidence; releasing one manual pin affects
only a subsequent capture. Source document revision 7 maps to native pin revision
1 explicitly. Missing original pin history is not reconstructed.

Kyoto compile job: `c82bd091cfe44e9e8c537de218cfb67e` (shared policy: no-run).
Kyoto Clippy job: `74d8070dd9584010a2171a755116130a` (`-D warnings`).
Source and executable receipts are in `qualification/catabolic-motion-pins.json`.
This does not qualify activation, application routing, tags, physical/structural
mapping, real libraries, or playback with Catabolic as the primary backend.
