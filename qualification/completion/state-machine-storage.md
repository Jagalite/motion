# Download, cache and maintenance state boundaries

This records the implemented boundaries and local evidence, not a blanket claim
that every audit item or application integration suite is closed.

## Production contracts

- `downloads`: device/owner admission, effective pinned duration, first committed
  subtitle identity, serialized subtitle-work selection and complete-copy byte
  admission. SQL observes the immutable download/file/track pin and takes the
  writer transaction before publishing an artifact. The per-download mutex and
  spawned conversion task own I/O; cancelling a waiter does not release that
  task's ownership. Subtitle source identity remains the immutable pinned row,
  not a second mutable lifecycle in the core model.
- `offline`: restored causal pins and cache-entry identity checks; exact
  content/sidecar publication identity and post-manifest shared-reference release
  effects; local opening/open/closed ticket transitions. The host's exclusive
  cache lock and media lock serialize openings. Independent ticket generations
  are modeled even for reopening the same download. Private snapshot/reader
  lifetime, byte hashing and durable filesystem operations remain adapters.
- Cache release never globally sweeps unrelated orphan blobs. The desktop's
  production `download-reconciliation.mjs` reducer owns whole-pass ordering:
  complete scoped inventory, exact transfer outcomes, then `Collect` only if all
  expected identities succeeded. `transfer()` attempts recovery before transfer.
  The low-level `Store::collect`/CLI remains an explicitly destructive primitive
  with this caller precondition, not a general safe collector for unknown live
  demands; it has no durable server-demand inventory of its own. A process crash
  before collection stops the pass; the next pass reconstructs its inventory.
- `maintenance`: backup ready/withdrawn/reclaimed transitions; automatic backup
  ownership/age/rank eligibility; reference-aware history pruning; revision-bound
  schedule batches, exact queued candidate identities and cursor decisions.
  Schedule jobs/cursor/time commit together under one writer transaction; the
  shipped revision-reset trigger remains enforcement. SQL enumeration is ordered
  and unique; the batch planner consumes those observations, not database reads.
- Migration `0069` persists the requested-backup cleanup cursor. Failure to remove
  one withdrawn artifact does not repeatedly consume the front of every bounded
  500-row pass. Withdrawal and cursor progress commit before physical deletion;
  reclaimed is recorded only after durable absence. Unknown/unowned paths are
  still not deleted.
- Historical migration `0058` and representation invalidation migrations `0037`
  and `0054` remain unchanged. Pure reference policies and actual-migration
  fixtures describe ownership and dependency behavior; triggers keep atomicity.
- Rust and installed Python restore adapters consume one authority-withdrawal
  inventory, `scripts/restore-authority.json`, including `signin_mail_tests` and expiring `catalog_review_plans` (not immutable
  catalog receipts).
  Rust restore publication requires snapshot/rotation/withdrawal/baseline/durable
  acknowledgements. Python intentionally leaves older-schema baseline capture to
  server startup. The macOS installer copies the inventory beside `backup.py`.

## Executed evidence (2026-11-02)

Pinned Rust 1.98, shared low-debug target, no incremental builds:

- `cargo test -p motion-ui-host --test store --test cache -j2`: 26 + 14 passed
  after the final typed publication/reference changes.
- `cargo build -p motion-ui-host --bin motion-ui-host -j2`: passed.
- Core `offline_maintenance_boundaries`: 13 passed, including exact sidecar
  identity/shared-release and restored causal-pin negative cases.
- Core `cache_publication_model`: exhausted 1,908 states / 41,976 transitions.
  It checks exact publication decisions and shared release identities with failed
  manifest commits, corrupt leftovers and removal. Filesystem acknowledgements
  are simulated observations; real host tests supply separate conformance.
- Core `schedule_dispatch_model`: exhausted 332 / 3,652, checking exact queued
  identities and atomic cursor outcomes across queue pressure, revision change,
  crash/rollback and retries.
- Core `offline_lifecycle_model`: exhausted 21 / 273 plus a same-download
  reopen/new-ticket/stale-close regression.
- Core `subtitle_transfer_model`: exhausted 120 / 6,240, first committed artifact
  A/B/failure, cancellation, changing revision, duplicate/reordered sequences and
  media-plus-subtitle totals. All these graphs have zero skipped checks.
- `python3 scripts/test_backup.py`: 4 passed, including executing a copied,
  installed-layout backup/restore tool with its JSON inventory and a legacy
  snapshot without the new catalog proposal table. Rust stages through migrations
  before executing the inventory, so its current-schema table is guaranteed.
- `python3 scripts/test_legacy_ownership_contract.py`: 48 combinations passed.
- `python3 scripts/test_representation_dependency_contract.py`: 35 cases passed,
  including binding old/new timelines, versions, source/output files, source
  enabled/no-op, timeline edition and owned/external rendition changes.

- `cargo test --test v2_metadata_processing -j2 -- --test-threads=2`: 28 passed,
  including the new 501-row failed-removal fairness regression, interrupted
  retention, backup publication/restore history, resumable schedule fairness,
  preview per-revision queue bounds, atomic scan scheduling and preparation
  connection-pool/dependency regressions.

- Final completion-owner Node run with `MOTION_UI_HOST_BINARY` set: 203 passed,
  zero skipped, including all three real-cache download cases (corrupt-poster
  recovery, metadata and subtitles); log `motion-node-final.log` in
  the shared workspace was inspected.

- `cargo test --test v2_downloads -j2 -- --test-threads=2`: 13 passed,
  including real FFmpeg subtitle conversion/pinned WebVTT, device report order,
  ownership/ticket fencing, restored baseline/reanchoring and effective duration.

The catalog envelope-history and restore regressions subsequently passed in the
clean canonical fast tier (899 tests); real-process integration passed 16 checks
and exact released-baseline upgrade/rollback/restore passed. These are separate
receipts, not inferences from this lane's tests. The inventory-length test initially retained the pre-catalog count
of 12; it was updated to 13 with explicit proposal-deletion/receipt-retention
assertions after the shared inventory gained that table.
