# Motion: single-agent completion handoff

Baseline: merged `main` at `28f4b32` (PRs #22 and #28–#33). Refresh remote
state before starting. This is the execution backlog following the October 12,
2026 plan audit, not a claim that M3–M6 are complete.

## Assignment

One implementation agent owns the remaining work below, in order, across small
reviewable commits and PRs. Reuse the existing engine, API, UI and verification
models. Do not start another architecture rewrite or repeat completed lanes.
Use GPT-6.1 Sol as the independent reviewer, not as a second implementation owner.
Do not spawn parallel implementation agents.

Read these first:

- `AGENTS.md`
- `Motion_Final_Architecture_and_Implementation_Plan.md`, especially sections
  16, 17, 20 and 21
- `MOTION_CONSOLIDATION_AND_COMPLETION_PLAN.md`
- `qualification/next-lanes/m3-workflows.md`
- `qualification/m4-expansion/README.md`
- `qualification/next-lanes/native-media-tools.md`
- `qualification/next-lanes/downloads-offline.md`
- `contracts/served_operations.txt` and the contract YAML/JSON

Keep a single progress ledger at `qualification/completion/README.md`. For every
work item record source SHA, implemented behavior, unsupported cases, focused
test evidence, review disposition, and remaining release checks. Distinguish
implemented, tested, merged and release-qualified. Missing code is not a test
deferral. Give every unfinished item an owner, dependency and concrete next step.

## Workspace, builds and review

- Start from current `origin/main` in your own worktree, named
  `motion-plan-completion`, under the seed1 volume's `Projects` directory.
  Use a dedicated branch and target/artifact directories. Inspect existing
  worktrees before creating anything; never overwrite another agent's work.
- The shared checkout has uncommitted completion-plan edits, including the
  missing section 11. Preserve and inspect that diff. Do not reset, stash or
  overwrite unrelated changes. Copy only the relevant reviewed document changes
  into your worktree when reconciling the plan.
  *(Done in step 0: section 11 is committed in the plan with a superseded
  banner; see plan section 12 and `qualification/completion/README.md`.)*
- Use the shared Kyoto installation for builds. The following constructs its
  required host path without putting a private absolute path in published docs:

  ```sh
  motion_volume_root="$(printf '/%s/%s' Volumes seed1)"
  export KYOTO_SCRIPT="$motion_volume_root/Projects/kyoto/kyoto.py"
  python3 "$KYOTO_SCRIPT" --help
  ```

  Read current queue policy. Build the smallest affected crate/test target;
  avoid full app/package builds unless the change actually requires one.
  If the queue compiles tests only, execute the produced test binaries separately.
  Record completed results, not merely submissions.
- Follow `AGENTS.md`: state-dependent identity, revision, authorization,
  cancellation, publication and recovery rules belong in deterministic production
  core where practical. Adapters supply observations and enforce IO/transactions.
  Extend production-backed Stateless checks and real adapter tests as appropriate.
- Obtain GPT-6.1 Sol review of implementation commits and each final PR diff.
  Fix concrete findings and verify the fixes. After the full review, keep
  subsequent review focused on changed behavior and unresolved findings; avoid
  endless unrelated scope expansion. Record accepted deferrals explicitly.
  If the reviewer is unavailable, continue independent work and mark review
  pending; never claim that another model satisfies this requirement.
- Coordinate schema/API changes yourself: main has migrations through 0044.
  Recheck before allocating the next version; never rewrite applied migrations.
  Keep contract YAML/JSON, served-operation inventory and clients consistent.
- Use disposable databases and fixtures. Do not alter the user's migrated
  catalog, original media, credentials, live preview or running services.
  Source/library deletion tests must never operate on the user's actual library.
- Commit scoped batches, push and open reviewable PRs. Do not merge, release or
  change experimental defaults without a subsequent user instruction.

## 0. Reconcile the plan and establish the baseline

1. Verify the current main SHA, CI and open PRs; avoid duplicating newer work.
2. Bring the relevant local section-11 worklane history into the committed plan,
   updating it to reflect that N1, N2 and N3 have merged. Preserve history while
   clearly marking superseded instructions and current status.
3. Correct stale claims: native FFmpeg tools now contain zimg/libass, while
   production HDR and styled-subtitle conversion remain incomplete; live HLS
   text and bounded software ABR are implemented. Do not erase historical receipts.
4. Publish a milestone/parity matrix with each remaining item below mapped to
   an original-plan requirement, implementation location and acceptance check.
5. Make the plan, lane ledgers and this handoff mutually consistent. Keep
   published docs free of machine paths, credentials and private hostnames.

Done: a new agent can identify the current backlog from committed documentation
without reading this conversation. No application rebuild is needed for this step.

## 1. Download correctness and bounded storage

Address N3 review findings S30 and S31 first:

- Reconcile retained offline events independently of download-list permission.
  Losing `downloads:manage` must not block sync still authorized by
  `viewing:write`; revoking viewing authority must still deny writes. Preserve
  unsynchronized events, restore-epoch fencing and exact acknowledgement identity.
- Collect crash-orphaned blobs after withdrawal, under existing cache ownership
  and locking rules. Never collect a published/pinned blob or another scope's data.
- Define bounded server-side offline-event retention/compaction. Preserve sequence
  identity, retry behavior, causal lineage and stale-device refusal after pruning;
  deleting rows by age without a durable deduplication strategy is insufficient.
- Add focused permission-transition, lost-response, crash/restart and quota tests.

Done: both known bugs are fixed with regression evidence, and retention has an
explicit durable protocol and tested bounds rather than unlimited growth.

## 2. Library scalability and query support

The current UI's safety bounds expose missing query capabilities. Fix those
capabilities rather than merely increasing limits:

- Add stable recent ordering/pagination for scan history and a safe retention
  policy. Preserve active scans and any records needed for correctness/audit.
- Add scoped `timeline_id` filtering/pagination for renditions.
- Replace per-title organization reads with scoped batched reads or equivalent
  bounded queries. Avoid leaking inaccessible titles through counts or errors.
- Update the facade/UI to use these queries. Preserve useful partial navigation
  instead of making an entire section unavailable at 1,000 rows or roughly 300
  distinct organization titles.

Done: fixtures beyond the old limits remain usable; pagination is stable under
concurrent changes, authorization is retained, and reads remain bounded. Broader
10,000-item/100,000-file performance qualification is recorded separately.

## 3. Finish production playback and preparation

Use the merged N2 tools and M4 routes; do not rebuild their architecture.

- Observe configured FFmpeg filter capabilities and source color facts, including
  primaries, matrix/range, peak metadata and Dolby Vision compatibility. Feed
  explicit facts into core policy; unknown/unsupported inputs must be refused.
- Wire HDR-to-SDR tone mapping into production planning/execution and relevant
  preparation. Preserve truthful HDR capability reporting. Keep unsupported
  Dolby Vision cases explicit. Verify pixels and output tags, not tags alone.
- Wire text/styled burn-in, fonts/attachments and offsets through exact component
  identity. Retain live HLS WebVTT delivery where appropriate.
- Complete per-part subtitles for multipart timelines and bitmap cues that begin
  earlier than the existing 20-second lookback. Test seeks and part boundaries.
- Complete subtitle-inclusive preparation and audio-component selection, with
  exact source/output mappings and stale-source fencing. Do not silently choose
  the first same-language track.
- Add a contract-backed user bitrate/quality preference and UI control; pass it
  through the bridge to the implemented server cap. Cover invalid limits and
  interactions with route selection and adaptive delivery.
- Expose custom video-profile preparation through validated UI controls.

Done: representative real routes work from the product UI with exact selected
components, focused core/adapter/client tests and truthful capabilities. Keep
copy/hardware/ABR defaults unchanged until the required qualification is complete.
Wider adaptive ladders remain an explicit later increment, not an implied promise.

## 4. Complete library-management UI workflows

Reuse the existing public API for:

- Smart-collection filters and saved filters.
- Alternate episode order groups and timeline placement.
- Explicit merge/split preview, confirmation and commit, preserving aliases/history.
- Library/source deletion with clear impact preview/confirmation and existing
  revision/authorization checks; deleting a catalog source must not delete media.
- Backdrop/banner/thumbnail artwork selection in addition to posters.
- More readable match proposals, adding a scoped query if necessary instead of
  exposing unscoped file information.

Done: each workflow has a discoverable UI, usable empty/error states, stale-edit
protection and a focused representative workflow check. Online provider matching
and provider artwork remain the previously approved deferral, not an accidental
new external-service commitment.

## 5. Complete the remaining offline experience

- Support prepared downloads and explicitly selected supported tracks/subtitles,
  artwork and metadata, with manifests pinning every required artifact.
- Expose device transfer state/progress through a reviewed ownership contract;
  the server must not invent verified-local status from server-side readiness.
- Add offline cache selection for multiple profiles/servers and offline removal,
  with strict namespace separation and active-playback pins.
- Implement an explicit recovery flow after restore/replacement. Preserve the
  existing epoch fence; never automatically overwrite newer viewing history or
  discard unsynchronized events without an explicit user decision.
- Keep unsupported containers and multipart/interval download cases visible until
  implemented; do not label the supported original-file slice full offline parity.

Done: download through the actual desktop agent, stop the remote server,
cold-start the offline host, play/seek, save progress, restart, reconnect and
reconcile. Include the real Electron/Demuxe path with a downloaded cache, not a
manually seeded fixture. Exhaustive package/large-file/device checks remain below.

## 6. Remaining M5 convenience and receiver work

- Inventory existing chapter/preview primitives before coding. Complete the v2
  marker API, correction/authoring UI and player exposure for the selected marker
  types. Bind markers to the correct timeline/revision and show uncertainty.
- Complete server-generated previews and their player integration, with bounded
  processing, publication validation, cancellation, retention and access control.
- Enable preview/optimization schedules only when their underlying tasks and
  durable resource policies exist; prevent overlapping duplicate work.
- Address desktop workflow/focus/accessibility gaps found while exercising these
  flows, without starting another general visual redesign.
- For TV/cast, define the reviewed device/control contract and select one physical
  receiver proof before promising broad support. Cover receiver network access,
  pairing, permissions, queues, handoff and reconnect. Controller reachability
  does not prove receiver reachability. If hardware is unavailable, name the
  exact blocked check and continue independent work; never fabricate qualification.

Done: each delivered slice has an explicit supported matrix. Casting cannot be
declared complete from a loaded webpage, simulator or screen-mirroring demo alone.

## 7. M6 and wider parity backlog

These are subsequent implementation phases, not prerequisites for the fixes above.
Keep them visible and address them sequentially after M3/M4/M5 closure:

- Dedicated music identity, album/disc/track organization, tags/art, queues,
  shuffle/repeat, listening history and loudness policy. Qualify gapless,
  background/offline listening and output fidelity independently.
- Photos/personal videos: dates/orientation, albums/slideshows, thumbnails and
  EXIF/GPS privacy; explicit edited/RAW relationships and bounded decoding.
- Previewable Catabolic/Plex/Jellyfin interchange with provenance and ID mappings,
  not production access to another application's internal SQLite schema.
- Supported platform expansion and upgrade/restore parity against an explicit
  target matrix. Do not claim Windows support for the Unix-only offline transport.
- Reconcile remaining chapter/extras/recommendation/watchlist, subtitle-alignment,
  dialog/loudness and richer-optimization rows against the original parity ledger.

Optional external/commercial/social services remain separately approved scope.
Do not create accounts, accept terms or promise those integrations as part of this
handoff. Hardware, credentials or product choices that block a phase must be named,
while unrelated authorized implementation continues.

## 8. Release qualification and final closure

Keep exhaustive release qualification after feature implementation, as requested.
Run focused correctness tests throughout. Before claiming release readiness:

- Freeze the actual server, UI, Demuxe, FFmpeg and package identities.
- Exercise browser/Electron original, prepared and live playback; subtitles,
  far seeks, audio switches, multipart boundaries, adaptive switching, revocation,
  restart and cancellation. WebKit stays explicitly deferred unless scope changes.
- Exercise real offline playback, long/interrupted transfers, full disk, multiple
  namespaces and restore recovery. Check both known N3 follow-ups are closed.
- Verify clean install, relocation, upgrade/rollback/restore and client/server
  compatibility, including retained user artwork and migrated state.
- Measure the plan's performance targets using its frozen workload and recorded
  hardware/network/cache state. A passing unit suite is not a performance receipt.
- Obtain physical receiver/audio/HDR evidence on the promised devices.
- Audit verification coverage: M4 subtitles, caps and ABR were not all covered
  by the route-selection model. Add relevant production-model properties where
  warranted and retain explicit bounds/gaps alongside real adapter tests.

Completion report: list what merged, what is usable, which exact artifacts were
qualified, and every remaining exception. No full M3/M4/M5/M6 completion claim
while a required implementation row or acceptance gate is still missing.

## Suggested delivery sequence

Use separate scoped PRs for documentation, download fixes, library query scaling,
HDR/subtitle production, preparation/quality controls, library UI, offline breadth,
markers/previews, and receiver work. Split further when it improves reviewability.
Only stack PRs with real dependencies; refresh the next branch from merged main.
Each PR description states the user-visible result, core/adapter boundary,
verification performed, Sol review disposition and release-only gaps.

Do not stop after creating the ledger or a plan. Proceed through the authorized
implementation phases, reporting concrete progress and genuine blockers without
repeatedly asking permission for routine reversible development work.
