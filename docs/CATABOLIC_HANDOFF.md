> Active continuation (2026-10-20): the historical handoff below describes the
> published starting point. Current implementation and remaining qualification
> are tracked in [CATABOLIC_INTEGRATION_STATUS.md](CATABOLIC_INTEGRATION_STATUS.md).
> Do not infer completion from either source presence or historical test claims.

# Catabolic primary backend handoff — 2026-10-19

## Resume here

- Branch: `handoff/catabolic-main-backend`.
- Worktree: `$HOME/Projects/motion-catabolic-main-backend`.
- Original checkout: `$HOME/Projects/playscale`; preserve its unrelated work,
  including the untracked integration proposal. This handoff does not switch it.
- Baseline checkpoint: `1ec14d5`; earlier commits `4594ef7` and `a6319cf` contain
  identity pins and the initial native migration/source-freeze implementation.
- Catabolic main is pinned to `6969f08c5d9b207afe8eec819555759be136644d`, schema 39.
  Read-only upstream checkout: `$HOME/Projects/catabolic-main-motion`.
- No push, PR, merge, deployment or live catalog migration has been performed.

**The objective is unfinished: make Catabolic the main catalog backend.**
The running server still uses Motion's legacy catalog and scanner. Do not mark
this objective complete based on the adapter's tests or migration preparation.

## Implemented before this handoff

`crates/catalog-backend` embeds the native Rust core/store/runtime. It provides
catalog/incarnation admission, immutable command intents and receipt replay,
durable bounded import staging, consistent Motion snapshot capture, and trusted
metadata/pin grants. Native workers retain their semaphore permits and actual
host authorization leases after async caller cancellation.

Motion migrations 66–69 retain native bindings, command journals, immutable
snapshots/provenance, and source-freeze state/events/guards. The production core
models freeze, abandon and irreversible seal with revision checks and migration
ID reuse refusal. SQL triggers stop legacy writers while frozen; normal startup
refuses frozen sources, and the operator control connection supports recovery.
Source freeze is not proof of native worker drain or completed reference audit.

Exporter v3 includes metadata contributions, manual identity pins, edition item
owners (`motion.edition:<id>`, kind `custom:motion_edition`) and provisional
timelines. Work/timeline IDs remain stable. Native revisions explicitly start at
1 while original source revisions remain recorded. Original labels, structural
facts, durations, ordering and relational references remain unresolved evidence.
The exporter always retains a missing-reference-audit marker; activation is
therefore intentionally unsupported. Saved v1/v2 snapshots remain supported.

The operator CLI is `src/bin/motion-catalog.rs`. It supports preparation,
staging, validation and source-freeze control, but no activation command.
See `CATABOLIC_BACKEND.md`, `CATABOLIC_BACKEND_QUALIFICATION.md`,
`adr/0002-catabolic-native-backend.md`, and `qualification/catabolic-*.json`.

## Latest work committed for handoff

- `crates/catalog-backend/src/bin/motion-catalog-observer.rs`: a narrow companion
  dispatching only Catabolic's closed v2 and shared v1 observation workers.
  It validates invocation size/shape, rejects failpoint fields through the native
  owner, and registers its own executable before calling the pinned runtime.
- `crates/catalog-backend/tests/observation.rs`: intended qualification using a
  real temporary source directory, native source binding, two scans separated by
  host restart/file replacement, exact occurrence capture, and worker-inventory
  checks. A second test rejects legacy protocols and unfenced invocations.

**These two tests have compiled but have NOT been executed.** The prior turn was
interrupted immediately after compilation. No companion Clippy or release/package
qualification has been performed. The companion is not wired into the server or
operator scan commands. Do not treat its test's packaging-grant construction as
a production API that accepts arbitrary user-supplied paths/hashes.

Verified at handoff: Kyoto job `5e4e03d975ab4d14923216cda4c7f232` is terminal
`succeeded`, effective command includes `--no-run`. It emitted:

```
target/debug/deps/observation-71081437ec4c3080
```

The first compile, `ed5bf3d4c8db4695b76f4a3729b28533`, failed because the test
referenced private native component functions. The test now uses the public
`catalog_import_typed::capture_existing_occurrence` with explicit physical grants.
There is no known running build to resume from this work.

Earlier verified checkpoint `1ec14d5`: 27 backend tests passed; Kyoto compile
`b9ef54604aa14d3890fe3b23895a2c1f` and Clippy
`9da481a3de2943d79ad28371f321fd49` passed. These results predate the companion.
Earlier broader evidence is retained in the qualification documents; do not
represent it as a full current-tree regression run.

## Next actions

1. Execute the emitted observation test binary and inspect actual results. Repair
   any native protocol/admission issues, then run focused Clippy through Kyoto.
   Record source and executable hashes with the results. If rebuilding, obtain
   the emitted binary name from that build's logs rather than assuming the name.
2. Integrate source registration and retained `HostSession` observation ownership
   into the adapter. Native `observation::execute(...,"bind",...)` handles source
   binding; `HostSession::start_scan/poll_scan` owns supervised scans. Avoid silent
   rebinds: native binding can replace a source and erase observations. Represent
   Motion's required binding identity/revision decisions explicitly in core.
3. Map source occurrences using actual Catabolic observations and exact native
   revisions, not Motion fingerprints or digest-based guesses. Native
   `capture_existing_occurrence` requires explicit physical read grants.
   Add version associations/bindings and qualified equivalence evidence through
   the native identity owner. Preserve application references and source revisions.
4. Finish tag assertions/suppressions, work/edition relationships, aliases,
   artwork references, embedded JSON reference audit and large-catalog partitioning.
   Native import is bounded to 16 batches; each is at most 64 KiB, 64 items and
   separately bounded typed/evidence records. Never truncate or drop records.
5. Implement durable cutover checkpoints, real native lease drain, activation
   grants and restart reconciliation. Use native `TrustedImportActivation`,
   `reconcile_persisted_activation` and `HostSession::open_after_import_cutover`.
   Current `NativeCatalog::drain` does not retain the revocation witness needed
   for activation. Keep admission closed until all reference and owner work settles.
6. Route server catalog reads/writes and scanning through Catabolic; preserve
   Motion-owned playback/viewing/download/access data. Frozen/sealed guards
   currently block all ordinary Motion tables, so active-native ownership rules
   must allow application writes without reviving legacy catalog authority.
7. Qualify migration/recovery, playback, permissions, browser behavior, packaging,
   real libraries and primary cutover. No such qualification is complete today.

## Build and correctness constraints

All Rust builds/checks/Clippy/test compilation must go through Kyoto. Read
`$HOME/.codex/skills/rust-builds-kyoto/SKILL.md`. Example from this worktree:

```sh
CARGO_INCREMENTAL=0 python3 $HOME/Projects/kyoto/kyoto.py submit --resource $HOME/Projects/motion-catabolic-main-backend/target -- cargo test -p motion-catalog-backend --test observation -j 1 --config profile.dev.debug=0 --config profile.test.debug=0
```

Shared policy adds `--no-run`; run the emitted test binary separately. Inspect
Kyoto `show` and `logs`. Never relax shared queue settings. Do not change compiled
sources while their build is running. Preserve unrelated checkouts and live data.

Follow Playscale's AGENTS.md: state-dependent correctness belongs in production
core/native transitions, adapters supply observations and effects, and actual
SQLite/filesystem atomicity needs integration evidence. Native unresolved import
evidence and metadata JSON cannot manufacture trusted activation or owner grants.

Dependencies use Rust 1.99 and a provenance-preserved vendored SQLx SQLite driver
to share libsqlite3-sys 0.38.2 with native rusqlite. Preserve the upstream vendor
bytes and `MOTION_PATCH.md`; three upstream trailing-whitespace lines are retained
intentionally. `.cargo/config.toml` supplies the required SQLite build flags.
