# Motion consolidation and milestone completion

- **Date:** October 10, 2026
- **Plan version:** 1.0
- **Audited source:** `f115d25e11e2e5795930bed924f28eeb35dcfa74`

**Status:** Implementation plan. This document does not itself retire endpoints,
repair stored data, qualify a release, or launch implementation agents.

## 1. Decision and document authority

Motion will support **one public business API, `/api/v2`, and one authoritative
state model**. Retire v1 client compatibility and the old web application after
their required functionality and current callers have moved to v2. Preserve
existing libraries, identities, curation, viewing history, and supported database
upgrade paths. Original media remains read-only.

This is the execution addendum to the
[architecture plan, design 1.1.0](Motion_Final_Architecture_and_Implementation_Plan.md).
It supersedes D11 and the compatibility obligations in sections 18.2–18.5
**only where they require continuing to serve v1, maintaining a v1 adapter,
or supporting legacy runtime URLs and API clients**.
Old clients must update; a permanent v1 compatibility layer is no longer a goal.
The underlying data-preservation, backup, recovery, authorization, and migration
requirements remain binding. The [v2 contract](contracts/Motion_Server_API_v2.yaml)
remains the wire baseline; changes to it require coordinated fixtures and clients.

Retain Rust/Axum/Tokio/SQLite, production policies in `crates/core`, Topcoat SSR,
Demuxe, the Go terminal client, and the Electron host. Reuse current engine services
and tests. Crate renaming and a wholesale module rewrite are not prerequisites.
Keep the recorded online-provider deferral from architecture section 8.1; local
metadata/NFO and the provider-neutral correction workflow remain in scope.

For current cleanup order and milestone closure, use this document ahead of the
historical [readiness review](MOTION_IMPLEMENTATION_READINESS.md) and old PR
handoffs. Keep those records as dated evidence. This plan does not replace the
missing original G01–G45 parity ledger or claim that its mapping was recovered.

## 2. Baseline: what is implemented and what is not complete

PRs #1–#13 are merged at the audited commit. Catalog identity, timelines/versions,
guarded scanning, shared scan demands, local metadata, search/organization,
execution supervision, v2 playback/viewing, Topcoat clients, and CI have substantial
implementations. All worktrees were clean during the audit. Old worktree branches
are checkpoints, not a queue of changes to merge again.

The served-operation inventory lists **107 of 146** contract operations:

| Contract milestone | Listed as served | Total planned | Meaning |
|---|---:|---:|---|
| M1 | 65 | 82 | Required foundation operations are still missing. |
| M3 | 32 | 44 | Library/preparation services are not fully exposed. |
| M4 | 10 | 10 | Route presence does not prove complete semantics or playback. |
| M5 | 0 | 8 | Some underlying components exist; these public operations do not. |
| M6 | 0 | 2 | Interchange services do not yet establish public API completion. |

These counts come from [served_operations.txt](contracts/served_operations.txt),
not a percentage of product completion. The audit reran structural OpenAPI and
YAML/JSON parity validation. Runtime body conformance and the complete application
were not requalified by that audit.

The production client records report 28 passing and two blocked checks per
engine. Live HLS remains blocked in the deployed product player. The combined
receipt also preserves an initial WebKit setup failure; a separate rerun supplies
the later WebKit result. Do not erase that history or count blocked checks as
passes. See [client playback evidence](qualification/client-playback/README.md).

## 3. Immediate defects and integration gaps

All rows start **open**. An owner closes a row with an implementation commit,
regression evidence, and any remaining qualification limit.

| ID | Work | Required outcome and proof |
|---|---|---|
| C01 | Repair legacy-to-timeline progress migration | Run attribution before dependent data publication or add an idempotent repair afterward. Real startup/upgrade tests cover schema 9 and already-upgraded databases, multiple editions including empty cuts, exact and ambiguous history, reopen, and preservation of newer v2 progress/manual actions. Legacy data remains recoverable. |
| C02 | Reconcile capability negotiation | Replace stale `catalog.v2`/`playback.v2` false flags with accurate implementation, enablement and qualification reporting under the contract. Test Go CLI/TUI against the real server; do not enable unsupported routes merely to pass a client gate. |
| C03 | Repair package source inventory and provenance | Include every workspace/build input, embedded contract, UI asset and player bridge in source capture and change detection. Extract the generated source companion and prove it can rebuild; test that changing an embedded input changes the inventory. |
| C04 | Complete the first-workflow v2 adapters | Expose required scan, structure, relocation, preparation, metadata/artwork and operations services through the reviewed API. Update the served-operation inventory and add authorization, body, precondition, retry and failure tests. |
| C05 | Establish one viewing authority | Move all current viewing reads/writes and watched-dependent queries to the timeline authority. Define work-level watched aggregation in core before wiring smart collections. Preserve ambiguous legacy history separately; do not create a new permanent dual-write bridge. |
| C06 | Separate delivery pacing from history writes | An authorized viewer without `viewing:write` can play a long live conversion. Review the contract for a delivery playhead observation; fence it by owner/generation and test stale/duplicate reports, seeks and revocation without granting history-write permission. |
| C07 | Close real player live-HLS acceptance | Select a reproducible Demuxe/provider deployment that supports the admitted rolling transport and qualify it on the supported browser/Electron matrix. Prove conversion-only open and in-player switching in the actual product path. The product's missing live provider and diagnostic native-video parsing failures are distinct; repair native HLS only if it remains an admitted path. A plain video element or static playlist copy is diagnostic evidence only. |
| C08 | Reconcile recovery and revocation semantics | Record whether viewing authority is interrupted or recoverable across ordinary restart; distinguish restart from restored/replaced server state. Align core, contract, outbox and tests. Verify that revocation denies new access and stops owned delivery work within a measured bound. |
| C09 | Repair verification coverage and status records | Add new client suites and relevant heavy-test triggers to CI; qualify an integrated commit; update contradictory handoffs and receipt claims. Pending CI is not acceptance. |

C01 concerns [startup ordering](src/db.rs), [legacy attribution](src/upgrade.rs),
and [migration 0032](migrations/0032_timeline_viewing.sql). C02 concerns
[server capabilities](src/v2/system.rs) and the [terminal player](apps/terminal/internal/cli/play.go).
C03 concerns [package_app.py](scripts/package_app.py). C06/C08 cross
[v2 playback](src/v2/playback.rs), [viewing](src/v2/viewing.rs), and the production
delivery/viewing core; use the observations described in [V2_PLAYBACK.md](V2_PLAYBACK.md)
as the starting evidence, not as permission to leave acknowledged defects open.

## 4. v1 retirement sequence

### 4.1 Inventory callers and preserve useful behavior

Create a checked inventory of each current route/caller with: current operation,
required v2 replacement, owning service, client/script, acceptance test, and
disposition. Inventory includes the original UI, Go tools, startup/administration,
smoke scripts, package tests, recovery tools, media URLs, and fixture provisioning.

Search `/api/v1` references, then inspect route composition and generated URLs.
Retirement also covers the unversioned `/media/{id}` route, v1 stream/sidecar
routes, legacy root-page fallback, and old application assets such as `/app.js`,
`/playback.js`, and `/session.js`. Preserve shared player assets under
`/assets/demuxe`; an asset name alone does not make a file obsolete.

Classify `/health`, static assets, Topcoat HTML and native bootstrap/lifecycle
channels separately. Keep `/health` as a minimal liveness alias if tooling needs
it; it must expose no alternate business behavior. Topcoat reads continue through
the same authorized application services. No replacement private business API is
permitted.

### 4.2 Complete the v2 functionality needed for cutover

Use existing services before introducing new ones. At the audited baseline,
these unserved groups are required to close the relevant foundation/library gates:

| Gate | Missing operation groups |
|---|---|
| M1 | Scan admission/list/get/cancel; source relocation preview/commit; relationship create/read/list/delete; timeline/version creation; item/admin file inventory; storage and backup operations. |
| M3 | Processing capabilities, recipes, preparation and renditions; schedule CRUD; metadata refresh; artwork upload/selection. |

For each group, verify exact contract IDs in the YAML and register actual methods
in `served_operations.txt`. Add payload/response conformance, permission scope,
ETag/precondition, idempotency, stale-source and recovery cases as applicable.
If an existing required v1 behavior has no v2 equivalent, approve its wire shape
and fixtures before updating callers. Do not invent an endpoint in client code.

Structure, component/marker, organization and interchange services need an explicit
public-versus-later disposition. Connect smart-collection evaluation to authorized
reads and define its watched predicate against canonical timeline state. Test the
real episode-order writer through `/next` and the client Next action; injected SQL
alone does not prove the composed workflow.

### 4.3 Migrate data independently of API compatibility

- Repair C01 with forward, repeatable data-upgrade logic. Never edit or renumber an
  applied migration to make the new branch look simpler. A02 allocates new numbers
  through [MIGRATIONS.md](MIGRATIONS.md).
- Preserve IDs, curation, metadata provenance, manual watched state, exact resume
  attribution, aliases and receipts. Retain unresolved history without guessing
  timeline equivalence or copying a position across cuts.
- Quiesce current writers during migration/cutover. When both old and new viewing
  records exist, preserve newer canonical v2 state; repair must not resurrect a
  superseded session or overwrite a manual action. Define this policy in core and
  apply it under one reserved-writer transaction.
- Cover databases already at schema 33 as well as direct schema-9 upgrades. Test
  retry/reopen, failure/recovery, backup restoration, and a supported previous-main
  database. Distinguish documented disposable pre-merge branch databases from
  supported main/release upgrade inputs.
- Keep old table readers and migration fixtures for as long as supported upgrades
  need them. Removing live v1 writers does not require immediately dropping legacy
  tables. Destructive schema cleanup is a separate forward migration after retained
  history and upgrade readers no longer depend on those tables.

### 4.4 Move clients, harnesses and product entry points

Make Topcoat the product entry point once its declared cutover checks pass. Update
the web bridge, Electron host, Go CLI/TUI, Python operator scripts, current-server
smokes and package acceptance to use v2 with proper credentials.

Include test **setup** traffic: the current client playback harness and v2 playback
smoke still provision some fixtures through v1. Capture/assert requests so a passing
v2 scenario cannot conceal a v1-only preparation path.

The old-binary side of an upgrade test may use v1 to create authentic legacy data.
After the candidate starts, assertions use v2. Maintain an explicit allowlist for
historical documentation, frozen fixtures and old-binary migration tests; a text
search is not sufficient proof of endpoint retirement.

### 4.5 Remove live v1 support and verify absence

Remove v1 router mounts, old OpenAPI publication, legacy UI fallback/assets and
obsolete adapters only after the required replacements and caller tests pass.
Keep shared storage, scanning, media, encoding and validation services used by v2.
Move or retain regression tests for their behavior rather than deleting coverage
with the old handlers.

Retired endpoints return a non-business response such as 404 or 410, with no
legacy writes, media delivery or HTML success fallback. Test anonymous and
authenticated calls, unsafe methods, old media URLs and absence of legacy state
mutation. Old credentials/URLs must not create an alternate authorization path.

Publish a breaking-change note: old clients and media URLs must update; library
data is migrated. Rollback uses the verified pre-upgrade backup with its matching
older binary/assets, not an old binary opening the new schema.

## 5. Milestone closure ledger

Milestones retain their original IDs. None is declared complete by this document.
For every gate, record owner, exact acceptance commit/artifact, commands, receipts,
open defects and declared supported matrix. A narrower delivery claim must be
named as a slice; it does not silently close the full milestone.

| Gate | Current assessment | Remaining completion gate |
|---|---|---|
| M0 — Contracts and proof | Strong foundation; source/receipt and decision records need reconciliation | Record this v2-only decision; reconcile restart/pacing contracts; fix capability semantics; restore a current endpoint/evidence index; resolve the plan's Stateless source-pin versus packaged 0.1.1 choice explicitly; bind Topcoat/Demuxe/Electron pins to actual proofs. Retain strict-CSP/no-eval runtime gates, anonymous HTML/fragment authorization checks and JSON/media streaming isolation. |
| M1 — Rust catalog foundation | Substantial implementation; data upgrade and API gaps remain | C01 plus supported upgrade/restore matrix; all required M1 v2 operations; source→scan through public API; partial/unavailable/stale/cancel/recovery tests; v1 retirement with one writer and restricted media authorization. |
| M2 — One-app vertical slice | Production client evidence exists; installation/client negotiation incomplete | C02/C03; exact package containing server, Go/TUI, Topcoat/bridge, Electron and media dependencies; clean install and strict-CSP local/remote attachment; original playback, cross-client resume, ownership, player disposal and restart; no development runtime/setup required by the user. |
| M3 — Library and preparation | Services ahead of public workflows | Complete v2 matching/correction/NFO/artwork/search/organization/schedule/preparation flows; smart watched collections and real next ordering; source/track identity and stale-result tests; corrections survive refresh/rescan. Online providers remain the explicit section 8.1 deferral, not an automatic matching claim. |
| M4 — Playback service | Server HLS implemented; product player remains blocked | C06–C08; real uncached conversion before completion, far seek and generation switching; authorized tickets where headers are unavailable; selected component/subtitle paths; resource/cancellation/revocation/restart tests; a declared HDR and quality policy with tested refusals and no silent substitutions. |
| M5 — Offline/client breadth | Partial helper and marker infrastructure only | Retain full download/cache publishing, transfer integrity/resume/quotas, causal reconciliation, device and receiver acceptance. Prove cold-start offline without a remote renderer; the presentation host has no server DB/scanner access. No new M5 expansion in this consolidation pass unless needed to avoid regressing existing behavior. |
| M6 — Broader media/platforms | Later milestone | Retain music/photos/personal-media, integrations, platform expansion and parity requirements. Complete/reconstruct provenance for missing scope ledgers before claiming broad parity. |

Multipart/interval playback, v2 hardware/copy routes, sidecar/bitmap/styled
subtitles, HDR conversion and adaptive bitrate support retain their individual
open gates. Decide their required milestone/matrix explicitly; refusing an
unsupported format is necessary correctness, not proof that support is complete.

## 6. Acceptance workflow and evidence

Run the shared workflow on a disposable installation of the exact candidate:

1. Install and start through the product entry point; pair a restricted client.
2. Add source/library through a normal client and scan through v2. Browse the same
   logical item from web, Electron and Go/TUI.
3. Play an original, seek, stop, resume on another surface and verify the same
   timeline authority. Different cuts retain separate history.
4. Correct metadata, rescan, prepare a rendition and observe the authorized result
   on each applicable surface. Verify smart collections and explicit episode order.
5. Play an uncached conversion before it completes; change a supported track, seek
   outside the produced window, activate a replacement and retire its predecessor.
6. Repeat with playback permission but no viewing-write permission. Playback
   advances without writing history or stalling at the pacing window.
7. Exercise lost acknowledgments, duplicate/out-of-order events, manual overrides,
   revocation, source replacement, ordinary restart and backup restore. Verify exact
   retry identity, accepted progress, no stale publication and no leaked execution.
8. Confirm retired APIs cannot perform operations and all candidate setup/test
   traffic uses v2. Upgrade fixtures are the explicit old-binary exception.

Model state-dependent decisions in production core: attribution and precedence,
attempt/generation/session ownership, epoch transitions, retries, cancellation,
revocation eligibility and effect ordering. Stateless checks those same decisions,
including rejected inputs and effect identities/counts. Record bounds and fairness
assumptions; preserve failing traces. Adapters own SQL/filesystem/process/network
and clock observations; real integration tests prove their conformance separately.

Receipts identify commit and dirty-state status, schema, contract digest, server
binary, complete application/source archive hashes, Demuxe archive/provider hashes,
Topcoat build, native tools, browser/Electron/OS/hardware, fixture/network identity,
commands and results. Distinguish unit/model, real HTTP, browser, native host,
installed package, physical A/V, and performance evidence. Requalify affected layers
after source or asset changes. A successful provider switch requires its own proof.

Fix the package source companion before trusting its inventory as provenance.
Test reconstruction from extracted source and package relocation/upgrade/restore.
Align the packaged Demuxe pin with the qualified deployment; the audited package
pins 1.0.0 while the newer client receipts use 1.1.0. Select and qualify the actual
artifact, rather than merely changing its version label.

## 7. CI and reproducible checks

### 7.1 Required local build workflow for every agent

**Run local builds through the shared Kyoto installation (`$KYOTO_SCRIPT`). Build the
smallest affected target first; do not build the full application by default.**
This applies to every lane and includes compilation triggered by test, client,
packaging or other helper scripts.

- Start with source inspection, formatting and relevant non-build checks. For
  documentation-only changes, check links and the diff; no application build is
  needed.
- For implementation changes, select the affected crate/package, library or test
  target and relevant test filter. Prefer a targeted `cargo check`, focused test
  compilation, or the equivalent client-package check. A compile check alone does
  not satisfy a behavior regression gate.
- Use the shared Kyoto queue, with an explicit working directory pointing to your
  own worktree and an isolated build target directory. Keep a stable agent UUID
  through `--user-id` or `KYOTO_USER_ID`. Do not create a private Kyoto queue/state
  directory, change shared scheduling policy, or bypass the queue with direct
  builds or shell wrappers.
- Inspect the current command policy. If it permits only test compilation, queue
  the narrow `cargo test ... --no-run` target, then execute the relevant compiled
  test binary separately. Coordinate expensive test execution with the integration
  owner; do not run a broad suite merely because its binary exists.
- Save the job ID and check its final status, exit code and logs. Submission is not
  a successful build. Kyoto uses the live worktree, so keep build inputs stable
  while the job is queued/running and record the source revision and dirty state.
- Expand to workspace tests, a complete application build or packaging only when
  a concrete dependency, unresolved failure, cross-component change or assigned
  acceptance gate requires it. Record the reason before submitting. The
  integration/release owner coordinates the necessary combined-tree and installed
  product runs; individual lanes do not repeat them for every commit.

Example from the agent's own worktree, for a core-library change:

```sh
python3 "$KYOTO_SCRIPT" status
python3 "$KYOTO_SCRIPT" submit --cwd "$PWD" -- \
  cargo check --locked -p playscale-core --lib --target-dir "$PWD/target"
```

Set `KYOTO_SCRIPT` to the existing shared Kyoto executable on the build host;
do not install a private queue. Set the agent identity before submission. Adapt the target to the actual change;
do not copy a core check as validation for another lane. Inspect the returned job
with `python3 "$KYOTO_SCRIPT" show <job-id>` and retrieve
output with `logs <job-id>`.

### 7.2 Integration gates and check inventory

Keep fast and full tiers, with source-aware dependencies. Changes to production
models, playback/viewing adapters, delivery, migrations or their tests must trigger
the relevant heavy checks before integration. Run Go tests and the new JavaScript
controller/host suites in CI. Require the declared checks for the merge candidate;
use a branch rule or documented integration gate instead of assuming a workflow
name enforces merge policy. Obtain a successful full run of the combined tree.

The following commands are a check inventory, not a checklist every agent runs
after every change. Select the relevant checks and install their documented
dependencies. Locally, build-bearing entries must follow the Kyoto workflow above;
they are not instructions to launch compilation directly from the agent shell.

```sh
python3 scripts/check_contract.py
ruby scripts/contract_json.rb --check
cargo fmt --all --check
python3 scripts/ci/test_tier.py fast
python3 scripts/ci/test_tier.py full
(cd apps/terminal && go test ./...)
node --test packages/playback/*.test.mjs packages/ui-bridge/*.test.mjs apps/desktop/test/*.test.mjs
```

Use pinned Rust from `rust-toolchain.toml` and the isolated Python dependencies in
`scripts/ci/contract-requirements.txt`. Adapt existing integration, upgrade/restore,
package and client-playback harnesses as described above; their current legacy
setup must not be mistaken for completed v2-only checks. The new cutover, capability,
source-rebuild and migration-order regressions are **tests to add**, not tests
claimed to exist or pass today. Keep real encoder/browser checks on suitable hosts;
tool unavailability and hardware skips never count as media qualification.

## 8. Work ownership and integration order

For the post-consolidation M4 continuation, use [section 10](#10-m4-playback-expansion-work-lane).
The four lanes below describe the original consolidation assignments.

Use four bounded lanes. These are assignments to make, not agents already running.

| Lane | Owns | Boundary |
|---|---|---|
| Data/catalog API (A01–A04/A09 with A02) | C01, scan/structure/metadata/organization/preparation/schedule/storage/backup adapters, legacy history readers, migration registry | Sole allocator of schema numbers; playback owns its domain policy while this lane coordinates persistence changes. |
| Playback/viewing (A06/A07/A08) | C02 playback service facts, C05/C06/C08, planning, tickets, generation and authority policies | Propose contract changes through A00; coordinate capability transport edits with the API owner and package/player decisions with clients/release. |
| Clients (A10–A12) | All current client callers, Topcoat cutover, Go capability use, C07 player integration, real-client acceptance | No independent catalog or viewing authority; fixtures remain explicitly labeled. |
| Integration/release (A00/A05/A14) | Contract/evidence ledger, C03/C09, CI, manifests, source/package reconstruction, final integration | Owns root manifests/lockfiles and shared-file integration; implementation lanes still own their regression tests. |

Playback/viewing handoff for PR #16: the combined implementation and acceptance
receipt is [qualification/playback-viewing/COMBINED.md](qualification/playback-viewing/COMBINED.md).
It composes catalog `ee842e4` and clients `f883f19`, preserving authorization-scoped
watched facts. Generation tickets, exact component pins, selected original-file
text subtitles, pacing without history-write permission, restore identity and
revocation have explicit policies and acceptance checks. Unsupported media cases
remain named refusals; this is not a claim of full M4 format/platform coverage.
[The caller contract](qualification/playback-viewing/CALLERS.md) coordinates epoch
headers, heartbeats and legacy-writer retirement. Clients/integration still owns
product-entry cutover, installed/native acceptance and public positive release-order
provisioning before the global M2/M4 and one-writer gates can close.

Before parallel edits, record named owners, owned paths, base commits and the
interface/receipt each lane supplies. Assign one editor for shared router modules,
`src/lib.rs`, root manifests and contract files. Coordinate small patches or
stacked changes instead of merging incompatible independent versions. Use dedicated
worktrees/build targets and the shared Kyoto queue from section 7.1. Every lane
starts with minimal builds; the integration owner coordinates broader qualification
when needed.

Sequence:

1. **Inventory and contracts:** name owners, freeze first-workflow contracts,
   classify callers, resolve authority/recovery choices, establish the gate ledger.
2. **Repairs in parallel:** data migration; capability/pacing/revocation; package
   inventory/CI; player deployment investigation. API-dependent client work uses
   frozen contracts, not invented private routes.
3. **Compose and cut over:** land missing v2 adapters, switch callers/harnesses,
   integrate data repairs, then remove live v1 support with absence tests.
4. **Qualify the product:** close M1/M2 on the exact installation, then M3/M4
   workflows. M4 research may proceed earlier, but cannot substitute for M1/M2 gates.
5. **Close documentation:** reconcile README, API/operations, V2_PLAYBACK, execution
   and desktop handoffs, Topcoat acceptance and client receipts. Preserve historical
   failures; replace stale current-status claims with links to dated evidence.

Use small reviewable commits. Have GPT-6.1-Sol independently review each commit
change and the final PR diff as requested for this effort; record the reviewed SHA,
findings/disposition and affected reruns. Review approval is not runtime evidence.
Register PRs with the coordinating thread where supported. Completion means the
owned gate has implementation and evidence, not just that its PR merged.

Current lane-4 implementation and integration gates are recorded in
[the consolidation release ledger](qualification/consolidation/README.md).

## 9. Completion checklist

- [ ] C01–C09 have owners, reviewed changes, regression receipts and dispositions.
- [ ] All required M1–M4 API/caller gaps are closed or an explicit, named partial
      milestone claim records the remaining scope.
- [ ] Current clients and candidate test setup use v2; retired routes perform no
      business operations; legacy data upgrades and backups remain supported.
- [ ] One canonical viewing authority serves all current clients and dependent
      queries; no runtime v1 writer remains.
- [ ] The actual packaged player passes the declared original/prepared/live media
      matrix, including playback without viewing writes and revocation/restart.
- [ ] Complete source inventory and rebuildable source companion match the package.
- [ ] Browser, Electron, CLI/TUI, package, model and adapter gates pass at the exact
      integrated revision; blocked/skipped/deferred cases remain visible.
- [ ] Status documentation reflects the merged implementation and exact evidence.
- [ ] M5/M6 requirements and explicit provider/platform deferrals remain tracked.

### Data/catalog completion ownership update (PR #17)

Data/catalog owns the missing `getProcessingCapabilities`, `listRecipes`,
`createPreparation`, `listRenditions`, all five schedule operations,
`getStorage`, `createBackup`, and `listBackups`. These are no longer unassigned
cross-lane dependencies. Migrations 0038-0041 are allocated in MIGRATIONS.md;
coordinate subsequent schema numbers with this lane and preserve prior files.

The exact client interface and supported values are recorded in
DATA_CATALOG_CLIENT_HANDOFF.md. Preparation and backup adapters reuse existing
services, with durable request identities and publication/recovery checks.
Scan and metadata-refresh schedules execute; preview/optimize scheduling and
automated generated-cache restore remain explicit service gaps. Route inventory
alone does not close M1/M3. Real HTTP preparation/backup acceptance,
schema 9→41 upgrade repairs and scoped-smart regression checks pass; exact
receipts are in qualification/data-catalog. Browser/client and combined-package
acceptance remain separate gates; prior receipts bind only their recorded commits.

### Clients continuation status (PR #15)

The Clients tree composes catalog `69e0ff4` and playback `0621762`. Current
callers migrated include catalog/playback preparation, general/catalog/viewing/
operations smoke, processing stress, inventory/backup reliability, installed
package setup and candidate upgrade observers. Frozen old-binary writes remain
an explicit exception. `video_profiles_smoke.py` still needs a public v2 dynamic
profile/backend interface; no full caller-migration claim is made.

Exact scoped receipts are in [the Clients ledger](qualification/clients-completion/README.md).
The earlier `38db68c` Electron workflow passed 30 assertions. The newer combined
`ad67fa0` binary passes original/audio/prepared switching, resume and restart but
fails conversion-only live startup. Collaborative-browser live-window and delayed
Play behavior also remain failed. Seven reliability checks, 20 real processing checks, 11 library regressions and
79 JavaScript/setup checks pass within their recorded scopes. Managed generated-store
visibility is corrected with focused adapter regression coverage.

Clients is **in progress**, not complete. Positive Next needs public release-order
provisioning; dynamic profiles need a v2 contract; provider/pacing compatibility,
latest combined acceptance and installed-package gates remain open. Topcoat
default cutover, legacy UI/media/API retirement and a qualified provider release
handoff are withheld until those gates pass. Candidate archive hashes identify
inputs, not qualified release assets. Integration coordination is tracked on
PRs #14, #16 and #17; old-binary upgrade fixtures are retained.

### Final consolidation ownership (integration/release)

Integration/release owns completion across PRs #14 → #17 → #19 → #18.
PR #19 is the main-bound carrier of #16; #18 preserves #15, which remains
closed as superseded with user approval. Integration ownership includes
shared-file reconciliation, the one-app distribution, qualified player
deployment, final legacy-route removal and absence tests, combined CI, installed
acceptance and milestone evidence. See
[the final gate ledger](qualification/consolidation/FINAL.md). Merging the lane
PRs does not close a milestone. Historical lane receipts keep their exact scope.

## 10. M4 playback expansion work lane

**Status:** Ready to start alongside UI/UX design. This is an assignment, not a
claim that an agent is running or that the remaining features are implemented.
Baseline: merged main `8d6dd7f` (PRs #18/#19 integrated). Start from current main
and record the actual input SHA; do not resume an obsolete pre-merge lane branch.

### 10.1 Objective and existing baseline

Extend the existing v2 playback service to the remaining media routes and formats
from architecture sections 10.5–10.6. Preserve the working original/prepared/live
SDR paths, exact component selection, generation tickets, timeline history,
no-history-write pacing, restart/restore behavior and revocation cleanup.

Read [the final consolidation ledger](qualification/consolidation/FINAL.md) first.
It records passing installed package, Electron/Chromium player and playable Next
checks for their exact artifacts. Earlier live-startup failures remain historical
evidence; do not automatically treat them as unresolved on the qualified deployment.
[V2_PLAYBACK.md](V2_PLAYBACK.md) is a source map, but some historical status prose
is stale; verify each gap against current code and the newer receipts.

### 10.2 Ownership and parallel design boundary

- One M4 implementation owner covers A06/A07 with A08: core planning/selection,
  execution and delivery adapters, playback HTTP contracts and focused regressions.
  Likely paths include `crates/core/src/`, `src/delivery.rs`, `src/processing.rs`,
  `src/v2/playback.rs`, media/subtitle adapters and their tests. Inspect actual
  modules before recording exact owned paths; this is not blanket ownership of core.
- Coordinate shared contract YAML/JSON, routers and capability fields with the
  API/integration owner. Data/catalog retains schema-number allocation, version
  bindings and preparation publication interfaces. Never edit applied migrations.
- UI design can proceed independently on layouts, typography and browsing screens.
  Give its owner a concise contract for available routes, track/subtitle choices,
  quality options, progress and refusal/error states. Agree on one editor for
  `packages/playback`, `packages/ui-bridge/player.js` and production player views.
  M4 supplies the functional wiring and tests; visual redesign stays with UI.
- Provider assets, native-tool pins, root lockfiles and package manifests require
  integration/release coordination. Changing provider bytes invalidates affected
  prior player receipts. Keep a dedicated worktree and isolated build target.
- Downloads/offline reconciliation, cast receivers, previews/marker authoring,
  music/photos and general platform expansion remain M5/M6 work. M4 must not
  silently absorb them or claim that they are complete.

### 10.3 Work packages and implementation order

| ID | Deliverable | Required implementation boundary and focused proof |
|---|---|---|
| M4-00 | Current capability and parity inventory | Record original, remux/copy, audio-only conversion, software/hardware conversion, prepared, multipart/interval, subtitles, HDR and ABR as implemented, qualified, unsupported or missing. Map existing engine behavior to v2 planning and client exposure. Record fixtures, dependencies, owners and intended support; do not rebuild features already present. |
| M4-01 | Route selection, remux and hardware | Expose eligible copy/remux and audio-only routes; integrate supported hardware encoding with explicit availability observations and bounded resource admission. Core decides compatibility, exact selected tracks, bitrate/HDR constraints and permitted fallback. Test unavailable/failed hardware, cancellation, stale completion, far seek and capacity release; advertised support must match the actual route. |
| M4-02 | Component continuity and subtitle delivery | Carry selected components through prepared outputs and generation changes using exact source/output mappings. Extend sidecars, converted playback, styled/bitmap subtitles and burn-in by declared route. Preserve off/forced/SDH/language roles, offsets and attachment/font ownership. Test same-language alternatives, missing/stale occurrences, source replacement and authorization; never substitute the first track. Burn-in requires a video transform. |
| M4-03 | Multipart and interval timelines | Plan known ordered parts and explicit episode intervals. Model logical duration, part boundaries, transport time origins, clipping, seeks and resume against one selected timeline. Test boundary seeks, missing/replaced parts, cancellation and restart. Refuse unknown split points; never infer episode boundaries from episode count or transfer progress between unrelated cuts. |
| M4-04 | HDR and bounded quality policy | Define preserve-HDR versus tone-map-to-SDR routes from observed source/client/tool capabilities. Implement the chosen color/tone-map path and real bitrate constraints before advertising them. Test unknown/unsupported capabilities and explicit refusals. Keep physical display/color fidelity as a release gate; successful encoding does not prove faithful HDR output. |
| M4-05 | Adaptive bitrate increment | Implement genuine multi-rendition delivery only after single-route timing, selection and resource policies are stable. Define aligned variants, component identity, switching, aggregate encoder/storage budgets and generation retirement. Test rendition switches under bandwidth changes and stale/revoked access. Manual prepared-file switching is not ABR. Architecture section 10.5 treats this as a later increment; record an explicit release deferral if it is outside the first supported matrix. |

Start with M4-00, then agree the route/selection interfaces before M4-01/M4-02.
M4-03 can progress against those frozen interfaces in disjoint files; M4-04 depends
on route and component policy. Keep M4-05 last. Work packages do not automatically
mean five concurrent agents: the M4 owner must allocate paths and integration
order before parallel implementation. Do not wait for finished visual designs.

### 10.4 Core rules and validation policy

Keep route eligibility, component/part identity, timeline mapping, generation and
attempt ownership, resource limits, cancellation and publication validity in the
production core wherever practical. Adapters report codec/tool/device/file facts,
execute I/O and enforce transaction ordering. Extend Stateless against those same
production decisions; cover stale/duplicate events, replacement, rollback and
recovery with explicit effect identities and counts. Real SQL/process/media tests
remain necessary for adapter behavior.

Follow section 7.1: builds use the shared Kyoto scheduler through the host-configured
`KYOTO_SCRIPT`; use the smallest affected crate/test target first. No routine full
application or package rebuild per edit. Run focused correctness and regression
checks while implementing and have GPT-6.1-Sol review commits and the final PR diff.

Per the user's development/release split, defer the exhaustive browser/native,
hardware/physical A/V, performance and final installed-package matrix to release
qualification. This does not waive focused checks of new rules or permit a known
authorization, migration or data-loss defect to be labeled a qualification task.
Record every deferred check with its owner, required host/fixture and exact expected
outcome. Keep existing evidence and failures; do not promote old receipts to new
provider or executable bytes.

### 10.5 Agent handoff and completion gates

Create `qualification/m4-expansion/README.md` when implementation starts. Maintain
one row per work package with source SHA, API/client changes, supported/refused
matrix, focused checks, review findings and release-only checks. Link it from the
consolidation ledger. This path is a deliverable to create, not existing evidence.

- **Implementation complete for a declared slice:** production core/adapters and
  public API/client wiring exist; focused regressions and review pass; capability
  reporting is truthful; prior supported playback is preserved; remaining work
  has explicit owners. Name the slice and every deferred work package.
- **Release qualified:** the exact delivered server/provider/tools/package passes
  the declared original/prepared/live, track/subtitle, multipart/interval, seek,
  recovery, revocation and resource matrix. Repeat affected Chromium/Electron and
  installed checks after changes; WebKit remains explicitly deferred unless scope
  changes. Qualify hardware/physical HDR only on suitable devices, and measure
  performance against architecture section 21.3's budgets with a frozen workload.
- **Full M4 closure:** reconcile the complete milestone against these receipts and
  the parity inventory. A narrow format slice, explicit refusal, merged PR or
  deferred ABR increment must not be presented as complete broad playback parity.

## 11. Next parallel worklanes after the M4 backend slice

> **Status (2026-10-12, main `28f4b32`): historical assignment, superseded.**
> Lanes N1 (PR #30), N2 (PR #33) and N3 (PR #31) have merged, together with
> PR #22 (M4 backend slice), #28 (TUI), #29 (admin metrics) and #32 (playback
> monitoring). The text below is the 2026-10-11 assignment, kept unchanged
> except that machine paths were replaced with neutral names. Its
> "may start", "PR #22 owns" and "PR #29 proposes migration 0042" sentences
> describe that checkpoint, not current work. Current status per lane:
>
> - **N1 merged** as a UI workflow slice. Remaining M3 UI gaps (smart filters,
>   order groups, merge/split, deletion, non-poster artwork, readable matches,
>   bounded list queries) are completion items 2 and 4 in section 12.
> - **N2 merged** as a *tool-enablement slice*: the bundled FFmpeg build now
>   contains zimg (`zscale`) and libass (`subtitles`/`ass`) with pixel-verified
>   transforms. **No production route tone-maps HDR or burns text/styled
>   subtitles yet**; that is completion item 3.
> - **N3 merged** with two open Sol findings (S30, S31) and unbounded server
>   event retention; these are completion item 1.
> - M4 (PR #22) delivered live HLS WebVTT text renditions and a bounded
>   two-rung software ABR ladder (off by default) after this checkpoint.
>
> Section 12 and [the completion ledger](qualification/completion/README.md)
> are the current backlog.

**Assignment checkpoint: 2026-10-11.** These are implementation assignments,
not completion claims. Refresh `origin/main` and record the actual starting SHA.
At this checkpoint PR #22 owns M4 fixes and playback wiring, PR #28 owns the TUI,
and PR #29 owns admin metrics and its CI repair. Do not duplicate those changes
or assume an open PR has merged. Older missing-operation lists in this document
are historical inventories: inspect merged routes and clients before adding work.

The next three lanes may start in parallel. None requires a Catabolic backend
replacement, Tauri migration, or a new online metadata provider.

### 11.1 Common execution and ownership rules

- Use an individual Git worktree under the build host's shared worktree root, with a dedicated
  branch and build/artifact directory. Suggested worktree names appear below;
  they are assignments, not directories already created. Never build or edit in
  another agent's worktree, and do not check out branches in the shared checkout.
- Use **`python3 "$KYOTO_SCRIPT"`**, set to the host's shared Kyoto installation,
  for builds under section 7.1. Queue only
  the smallest affected target. No full application/package build unless the
  integration change actually requires one. Read the queue's current policy and
  record completed job results; test compilation alone is not a passing test.
- Inventory existing behavior first. Keep a per-lane ledger under
  `qualification/next-lanes/` with implementation, missing wiring, unsupported
  cases, focused checks, review findings and deferred release checks separated.
  Those ledger files are deliverables to create when the lane starts.
- Get GPT-6.1 Sol review of implementation commits and the final PR diff; resolve
  findings and record dispositions. Follow AGENTS.md for production-core policy,
  deterministic verification and real adapter tests. Do not manufacture a model
  for ordinary rendering or read-only measurements.
- Integration coordinates contract YAML/JSON, generated clients, schema-number
  allocation, root dependencies, native-tool manifests and shared player files.
  PR #29 proposes migration 0042: recheck the merged registry before allocating
  any migration. Never edit an applied migration or reuse an occupied number.
- Keep the exhaustive browser/native, physical A/V, performance and final
  installed-package matrix for release as agreed. Each lane still runs focused
  correctness tests and a representative real workflow. Missing implementation,
  wrong authorization or data-loss defects are not qualification deferrals.
- Use disposable state for tests. The user's migrated preview and original
  library databases are not fixtures. Coordinate preview restarts with its owner.

### 11.2 Lane N1 — Complete M3 library workflows

**Worktree:** `motion-m3-workflows` (shared worktree root).
**Owners:** A01–A04/A09 with A11 presentation; coordinate with A02 for persistence.
**Ledger:** `qualification/next-lanes/m3-workflows.md`.

**Objective:** complete ordinary library management through the real Motion UI
and public API, reusing the already implemented catalog/preparation services.
This is functional workflow completion, not another visual redesign.

1. Map each workflow to its current API route, UI entry, authorization and known
   evidence: source registration/relocation, scan start/progress/cancel, browse
   and search, manual identify/match correction, NFO/local metadata, artwork
   upload/selection, collections/playlists/order, preparation and schedules.
2. Implement missing calls/screens/actions and useful pending/error/conflict
   states. A route existing is not proof that the ordinary user can reach it.
   Conversely, do not rebuild an already completed endpoint based on stale prose.
3. Demonstrate an isolated library's complete path: add source, scan, browse,
   correct metadata/match/artwork, rescan without losing corrections, organize,
   prepare a supported rendition and inspect its result and schedule. Test
   restricted access and stale-revision rejection at changed boundaries.
4. Preserve source/timeline identity and preparation component selection. If a
   requested component cannot be prepared yet, make that explicit and coordinate
   the implementation with M4 rather than silently selecting another track.

**Likely paths:** `crates/motion-ui`, relevant `src/v2` catalog/metadata/organization/
scan/schedule adapters and focused tests. Negotiate edits to shared UI layout and
bridge files before starting. Leave `apps/terminal` to PR #28 and playback route
selection/delivery to PR #22. Online provider matching and provider artwork remain
explicitly deferred; local/manual workflows must remain useful without them.

**Done for this lane:** a workflow matrix identifies every required M3 operation
as usable or explicitly still missing, implemented gaps have focused tests and
real UI/API evidence, and the remaining full-M3 release gate is accurately named.
An inventory alone does not complete the lane.

### 11.3 Lane N2 — Native-tool support and remaining playback transforms

**Worktree:** `motion-native-media-tools` (shared worktree root).
**Owners:** A14 integration/release for native tools; A06/A07 for playback policy
and execution, handed off after PR #22's shared files settle.
**Ledger:** `qualification/next-lanes/native-media-tools.md`.

**Objective:** unblock a supported HDR-to-SDR route and text/styled subtitle
fallbacks without misreporting what the current FFmpeg/player can do.

1. Inspect exact host and bundled tool/provider capabilities. Select one explicit
   supported tone-mapping pipeline and a libass-backed subtitle pipeline, pin
   required sources/dependencies and record license/notices changes. Do not assume
   the presence of `zscale` alone performs tone mapping, or that retagging color
   metadata converts HDR pixels to SDR.
2. Build the smallest native-tool artifact needed and verify real filter output,
   relocation and dependency closure. This lane may require a native-tool rebuild;
   it does not justify repeatedly rebuilding the complete Motion application.
3. Coordinate with M4 to add capability-gated production policies and execution
   for the selected transforms. Report unknown/unavailable tools explicitly.
   Test bounded processes, cancellation, source/component fencing, output color
   metadata and a fixture with meaningful pixel/color expectations. Keep physical
   display fidelity as a separate release gate.
4. Inspect Demuxe's actual supported subtitle paths before requiring burn-in.
   Distinguish external text-track addition on a live Shaka presentation from
   supported HLS subtitle renditions. Carry offsets, exact subtitle identity,
   forced/SDH roles and attachment/font ownership through the chosen route.
5. Coordinate missing client capability/quality wiring with the PR #22 owner.
   Existing text-only capability advertisement cannot exercise bitmap burn-in;
   adding tools cannot itself fix that. Preserve original playback and keep
   unqualified experimental defaults explicit.

**Likely paths:** `scripts/build_media_tools.py`, native-tool/package lock and
notices, packaging checks; later coordinated changes in core, delivery and the
player bridge. Do not independently modify M4's active files or replace Demuxe.

**Done for this lane:** exact tool artifacts support the chosen transforms,
production routes and client wiring exist, focused media/negative tests and
review pass, and the delivered supported/refused matrix is truthful. If only
the tool artifact is ready, call it a tool-enablement slice and name the pending
playback implementation owner. Full HDR/subtitle parity is not implied.

### 11.4 Lane N3 — Managed downloads and offline reconciliation (M5)

**Worktree:** `motion-m5-downloads` (shared worktree root).
**Owners:** A13 with A11/A12; A00 reviews contract additions.
**Ledger:** `qualification/next-lanes/downloads-offline.md`.

**Objective:** turn the existing cache-only `motion-ui-host` and offline core
into an end-to-end download feature. Reuse their verified manifest, scoped
media access, durable event log and desktop lifecycle; do not create a second
catalog server or count manually seeded cache playback as download completion.

1. Freeze the download identity/state contract and ownership split: the server
   authorizes exact timeline/source revisions and optional preparation; the
   device owns transfer staging, verified publication, resume, quotas and removal.
   Coordinate any new schema/API allocation after PR #29 and other active work.
2. Implement request/status/cancel and resumable transfer into the managed cache.
   Publish manifests and blobs atomically only after size/checksum verification.
   Cover interrupted transfer, stale source, disk full, duplicate completion and
   competing cancellation/deletion. Keep active playback pins safe from eviction.
3. Wire the Downloads UI and explicit offline launch to the actual completed
   cache. Start with a named supported media/component slice; surface unsupported
   subtitles/artwork/multipart cases instead of silently dropping requested data.
4. Reconcile offline viewing through existing canonical authority and causal
   revision/manual-epoch rules. Preserve device sequence identity and duplicate
   handling. Define acknowledged-event compaction before removing local events;
   the current bounded log is not an unlimited sync queue.
5. Demonstrate download online, stop the remote server, cold-start offline,
   play/seek/save progress, restart offline, reconnect and reconcile without
   overwriting newer manual state or creating duplicate history. Test account/
   profile/server isolation; disconnected cache cannot promise remote erasure.

**Likely paths:** `crates/core/src/offline.rs`, `crates/motion-ui-host`, new/reused
download services and contract entries, coordinated desktop lifecycle and
Downloads views. N1 owns general library views; N2/M4 own codecs and transforms.
Do not add cast, music, browser-offline storage or a second native host here.

**Done for this lane:** the real download-to-offline-to-reconciliation workflow
works without manual cache seeding, focused state/SQLite/filesystem/client tests
and independent review pass, and final installed/long-duration checks remain
listed with exact required fixtures and owners.

### 11.5 Parallel schedule, integration and later backlog

| Work | Can start now | Must coordinate or wait |
|---|---|---|
| N1 library workflow inventory and focused UI/API fixes | Yes | Shared UI files, preparation selection with M4, schema/API edits with integration |
| N2 native-tool inventory, dependency pins and isolated filter proof | Yes | Production playback changes after agreement with PR #22; shared manifests with integration |
| N3 download contract, cache/transfer and offline policy work | Yes | Contract/schema allocation; coordinated Downloads UI and desktop changes |
| Existing M4, TUI and metrics fixes | Continue with current owners | Reconcile current main and shared contracts before merge |
| Final release qualification | Prepare fixtures/ledger now | Run acceptance against the integrated server/player/tools/package bytes |

Merge independently reviewed foundations first; dependent PRs name their exact
base and handoff. There is no requirement to merge an unfinished N1 before N2 or
N3. One integration owner reconciles shared contracts, migrations and packages.

After these assignments, explicitly scope M5 chapters/previews/markers against
existing server and Demuxe functionality. Physical TV/cast receiver support,
dedicated music/photos, broader OS support and M6 interchange remain separate
future assignments. ABR remains M4's declared later increment. Tauri or Catabolic
engine replacement requires a separate architecture decision, not silent scope
expansion in these lanes.

The one-time old-library transfer is operational migration work with its own
backup and validation receipt; it does not prove all historical schema upgrade
paths or complete M1/M3 qualification. Final release also reconciles the full
parity ledger, supported upgrade/restore paths, package provenance, performance,
and the exact combined browser/Electron/client acceptance matrix.

## 12. Single-agent completion backlog

**Status (2026-10-12):** current execution backlog after the plan audit of main
`28f4b32`. One implementation owner works the items below in order, in small
reviewable PRs; GPT-6.1 Sol is the independent reviewer. This is a backlog,
not a claim that M3–M6 are complete. Progress, evidence and review dispositions
are recorded per item in [the completion ledger](qualification/completion/README.md).
The full assignment text is
[MOTION_SINGLE_AGENT_COMPLETION_HANDOFF.md](MOTION_SINGLE_AGENT_COMPLETION_HANDOFF.md).

### 12.1 Corrected current-status claims

These replace stale current-status prose elsewhere; historical receipts stay.

- **Native tools:** the bundled FFmpeg build recipe now includes zimg and libass
  (N2, PR #33), pixel-verified in `scripts/test_media_transforms.py`. Statements
  that "no available FFmpeg has the filters" (M4 ledger) describe the pre-N2
  state. Production HDR-to-SDR conversion and text/styled burn-in are **still
  not implemented**: they remain refused by core policy.
- **Live subtitles:** text subtitles on live HLS routes are implemented as an
  in-manifest WebVTT rendition for clients listing `hls_text` (PR #22).
  External text tracks on live presentations remain unsupported by the player.
- **ABR:** a bounded two-rung software ladder exists behind
  `processing.experimental_adaptive_bitrate` (off by default). Wider ladders are
  a later increment, not an implied promise.
- **Served operations:** 152 of 156 contract operations are served. Unserved:
  `listMarkers`, `createMarker` (M5) and `previewImport`, `commitImport` (M6).
- **Schema:** migrations run through 0044. The next allocation is 0045,
  rechecked against `migrations/` and [MIGRATIONS.md](MIGRATIONS.md) at use.

### 12.2 Milestone and parity matrix

"Requirement" cites the architecture plan
([Motion_Final_Architecture_and_Implementation_Plan.md](Motion_Final_Architecture_and_Implementation_Plan.md))
section or the ledger row it closes. Location is where the work is expected to
land; the ledger records the actual commits.

| Item | Work | Requirement | Expected location | Acceptance check |
|---|---|---|---|---|
| 1.1 | Offline sync independent of `downloads:manage` (S30) | §16.1 reconciliation; N3 S30 | `apps/desktop/src/downloads.mjs` | Agent test: losing `downloads:manage` still syncs with `viewing:write`; revoked `viewing:write` denies; events retained, exact acknowledgement |
| 1.2 | Collect crash-orphaned blobs after withdrawal (S31) | §16.1 quotas; N3 S31 | desktop agent, `motion-ui-host` store | Store/agent test: orphan collected after withdraw, published/pinned and foreign blobs kept |
| 1.3 | Bounded server `offline_events` retention | §16.1; N3 deferred row | `crates/core/src/downloads.rs`, `src/v2/downloads.rs`, migration ≥ 0045 | Core + model + SQLite tests: retry identity, duplicates, lineage and stale-device refusal survive pruning; bound enforced |
| 2.1 | Recent-ordered, paginated scan history; retention | §20 M1/M3; N1 §2 bounds | `src/v2/scans.rs`, scan persistence | > 1,000 scans usable; active scans retained; stable pages under inserts |
| 2.2 | `timeline_id` filter on `listRenditions` | §20 M3; N1 §2 | `src/v2/processing.rs`, contract | Scoped filter, pagination, no inaccessible rows |
| 2.3 | Batched organization title reads | §20 M3; N1 §2 | `src/presentation.rs`, `crates/motion-ui` | > 300 titles usable; no leak via counts/errors |
| 3.1 | Filter and color-fact observation | §10.5–10.6; N2 handoff 1–2 | `src/processing.rs`, scan probe, migration | Unknown → refused; facts persisted per revision |
| 3.2 | Production HDR→SDR tone mapping | §20 M4; M4-04 | `crates/core/src/delivery.rs`, `playback_session`, `src/delivery.rs` | Pixel and tag checks on output; DV profile 5 refused; capability truthful |
| 3.3 | Text/styled burn-in with fonts and offsets | M4-02 | core subtitle policy, `src/delivery.rs` | Exact stream identity (`si`), offset, far seek, attachments |
| 3.4 | Per-part subtitles; bitmap cues before 20 s lookback | M4-02/M4-03 deferrals | core `parts`, `src/delivery.rs` | Boundary seeks, early-cue restore |
| 3.5 | Subtitle-inclusive preparation, audio-component selection | §20 M3 "selected tracks retained"; N1 §2 | `src/v2/processing.rs`, core `processing` | Exact mapping, stale-source fencing, no first-track substitution |
| 3.6 | User bitrate/quality preference and control | M4-04 caps "API-only" | contract, bridge, `crates/motion-ui` | Invalid limits refused; interaction with routes and ABR |
| 3.7 | Custom video-profile preparation UI | N1 §2 `video_profile` | `crates/motion-ui` | Validated controls, server refusal shown |
| 4.x | Smart filters, order groups, merge/split, deletion, artwork roles, readable matches | §20 M3; N1 §2 | `crates/motion-ui`, scoped reads | Per-workflow representative check, stale-edit protection |
| 5.x | Prepared/track downloads, transfer-state contract, multi-root picker, offline removal, restore recovery | §16.1; N3 §3/§8 | contract, server, agent, host | Real agent download → offline cold start → reconnect, Electron/Demuxe path |
| 6.x | Markers v2, previews, schedules, receiver proof | §16.2, §16.5, §20 M5 | `src/markers.rs`, core `markers` | Per-slice supported matrix; physical receiver evidence or named block |
| 7.x | Music, photos, interchange, platforms, parity reconciliation | §16.3–16.5, §20 M6 | new services | Media-specific fidelity/privacy gates |
| 8 | Release qualification | §21 | release ledger | Frozen identities, matrices, performance with recorded workload |

### 12.3 Rules carried forward

Sections 7.1, 10.4 and 11.1 still apply: Kyoto builds of the smallest target,
production-core decisions with Stateless coverage, disposable fixtures,
contract/served-operation consistency, no edits to applied migrations, no
merges/releases or experimental-default changes without user instruction.
