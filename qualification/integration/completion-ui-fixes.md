# Completion UI and marker review fixes

Based on completion checkpoint `9e6e42f`; handles PR40 UI and PR41/53 marker
findings. Does not modify Catabolic or perform browser qualification.

## Dispositions

- PR40 descendant timeline placement: management now walks readable descendants
  through scoped catalog APIs, collecting their timelines as play-order choices.
  Work titles identify episode candidates. A 1,000-work traversal bound reports
  the section unavailable rather than presenting an incomplete list.
- PR40 hidden source usage: when no visible library uses a source, removal is
  offered only to system administrators whose library view is complete. Scoped
  managers receive an explanation to ask an administrator; hidden library names
  are not disclosed. Server transactional removal checks remain authoritative.
- PR40 metadata replay and Windows basename findings were already repaired in
  the parent completion checkpoint, outside this follow-up's ownership.
- PR41 manual corrections: migration 0062 adds explicit persisted creation order.
  Both production writers allocate the next timeline-local value under the write
  transaction. Core effective selection gives the latest manual intro/recap/
  credits correction precedence. Existing rows use zero and retain deterministic
  earliest-start/ID ties. Prior provenance records remain intact; no rowid or
  clock ordering is assumed. Chapters retain their existing set semantics.
- PR41 writer cap: already fixed in descendants. Both writers call the shared
  transactional admission adapter backed by core `admits_another`; existing
  `v2_catalog` regression checks the internal writer at the bound.
- PR41 terminal skip: uses duration minus one millisecond, avoiding the previous
  final-second rewind. Player unit suite passes with the corrected expectation.
- PR41 nullable contract members: omission of end_ms or label is rejected;
  explicit null remains valid.
- PR41 Unicode labels: validation counts Unicode scalar characters, matching
  the advertised 200-character bound instead of UTF-8 bytes.
- PR53 scope: descendant fix already checks every candidate with production
  file facts and playback readability; only whole single-file originals qualify.
  Existing playback regression covers hidden sources and 100 earlier hidden copies.
- PR53 stale chapters: descendant fix already clears chapters when changed
  content cannot be probed and preserves them only on unchanged revision.
- PR53 cancellation: descendant backfill path already selects the chapter probe
  against shutdown and 100 ms job-state polling; dropping kills the child.
- PR53 combined bound: both effective-player and management reads now allow the
  complete 1,000 stored plus 200 derived chapter markers (six 200-row pages).
- PR53 snapshot: hashes full marker facts and persisted ordering, so same-count
  chapter title/start changes invalidate an old effective-page cursor.
- PR53 optional probe overflow: base duration/track probe excludes chapters;
  chapters use a separate bounded optional probe. Their failure leaves base
  observations intact. A fake native probe regression exercises output overflow.
- PR53 selected original: choose the first readable original before examining
  chapters, so a chapterless selected file cannot inherit another cut's markers.

## Validation and boundaries

- `node --test packages/ui-bridge/player.test.mjs`: 27 passed.
- All branch SQL migrations applied to an empty SQLite database successfully.
- Rust formatting and whitespace checks passed.
- Added core correction-order/Unicode tests, required-nullable/snapshot unit
  tests, real API repeated-correction test, readable-original chapter regression,
  and optional-probe overflow test. Rust compilation and execution are reserved
  for the root's combined integration build; they are not yet claimed passing.
- Independent review and browser walkthrough remain pending.

Correction precedence belongs to deterministic core policy. SQL allocation,
transaction serialization, filesystem reads, native process cancellation and
scoped HTTP reads remain adapters. Existing marker count and timeline-revision
policies are reused rather than duplicated.
