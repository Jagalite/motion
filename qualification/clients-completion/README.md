# Clients lane consolidation — October 10, 2026

Owner: this `clients-completion` worktree, based on `2344d03`. Independent review:
GPT-6.1 Sol. Owned paths: `packages/ui-bridge`, `apps/terminal`, client evidence
harnesses, `scripts/client.py`, Demuxe installation and client qualification docs.
Root router/manifests/contracts remain with their assigned integration owners.

**Status: implementation and qualification in progress. This is not lane closure,
Topcoat cutover approval, C07 closure, or M2/M4 completion.**

## Implemented changes and boundaries

- Rolling HLS declares Demuxe's explicit `streaming.live` option. The client
  advertises HLS only after loading a `shaka-adaptive` provider and observing
  compatible MSE codec support. Native MIME support alone is insufficient.
  These are client capability observations; production core still decides plan
  admission, source/generation ownership and publication. No server policy is
  duplicated in the bridge.
- The standard-library catalog client uses credentialed v2 cursor pagination,
  rejects redirects and detects repeated cursors. Supply `MOTION_ACCESS_TOKEN`
  in the environment; output is the v2 catalog item schema.
- Playback acceptance uses `createPreparation` with an exact active source
  revision and polls the public job resource. Missing preparation support fails
  acceptance instead of silently provisioning through v1. Candidate setup rejects
  non-v2 requests before sending; browser/Electron request observation also checks
  retired API/media/application paths. Only the old binary in the upgrade harness
  receives the explicit legacy setup exception.
- Demuxe installation can compose explicit provider archives through the official
  upstream composer. It stages extraction/composition before publishing, retains
  all archive/composer hashes and hashes every deployed file. Playback receipts
  verify those hashes and record the provider manifest and source revision/diff.

No new server state rule is introduced in this slice. Existing JavaScript
ownership/interleaving tests exercise the bridge. Server Stateless checks and
adapter acceptance remain required for the playback owner's pacing/recovery work.

## Caller inventory and integration dependencies

Clients owns conversion of every current client/script caller below. Service owners
in the replacement column supply the adapter/interface dependencies; they are not
a reassignment of client work. Package/upgrade execution remains coordinated by
integration/release.

| Current caller / entry point | Replacement / owner | Acceptance and disposition |
|---|---|---|
| `scripts/client.py` | `/api/v2/catalog/items`; Clients | Authenticated cursor tests; converted. |
| `client_playback_e2e.mjs`, `v2_playback_smoke.py` preparation setup | `/api/v2/processing/preparations`, `/api/v2/jobs/{id}`; data/preparation service | Converted; adapter integrated in f9b4763, combined real acceptance pending. |
| `client_playback_upgrade.mjs` old-binary writes | Frozen old-binary v1 setup | Explicit exception; candidate reads use v2. |
| Go CLI/TUI | Existing public v2 catalog/playback and `playback.v2` capability | Retain negotiation. Playback owner must correct server facts; run real-server CLI/TUI afterward. |
| `packages/ui-bridge/player.js` delivery pacing | Playback owner's C06 heartbeat playhead contract | Pending interface integration and playback-only long conversion acceptance. |
| Topcoat `/`, item and player pages; Electron host | Authorized production presentation service | Already opt-in through `--topcoat`; default cutover awaits replacement workflow and absence tests. |
| Legacy `web/app.js`, `web/playback.js`, `web/session.js`, `/media/{id}` | Topcoat/player bridge and v2 media; integration router owner | Keep until cutover gates pass, then remove mounts/assets and prove no business behavior. |
| `operations_smoke.py` | v2 catalog, timeline viewing and diagnostics; Clients | Converted; real-server backup/restore smoke passes all five checks, including rejection of the old operator token after restore. |
| `catalog_smoke.py` | v2 metadata/artwork/relationships; Clients | Converted; six real-process checks pass. Coverage differences below remain explicit. |
| `processing_smoke.py`, `video_profiles_smoke.py` | v2 recipes/preparations/jobs/events; preparation owner | Processing stress migrated to v2, runtime validation pending. Dynamic video profiles still lack a v2 replacement. |
| `viewing_smoke.py`, `reliability_smoke.py`, `smoke.py` | Canonical viewing + v2 storage/catalog; playback/data owners | General smoke (7) and viewing (5) pass over v2. Reliability is migrated; combined runtime validation pending. |
| `upgrade_restore_check.py` | Public v2 catalog, revision-pinned file bytes and canonical viewing; Clients | Candidate migrated; all six real-process upgrade/rollback scenarios pass. Frozen baseline alone retains v1. |
| `test_package.py` | Exact installed v2 candidate; integration/release owner | Candidate migrated to v2; installed-package execution pending combined preparation qualification. |
| Go e2e anonymous v1 request | Negative legacy authorization test | Does not provision business data; revise to 404/410 when retirement lands. |

The remaining legacy scripts are active dependencies, not a historical allowlist.
This inventory does not claim their retirement.

## Qualification

- `a44eb7d`: player bridge tests 15/15; complete JS controller/desktop suites 70/70.
- `590cb10`: Python catalog client tests 3/3.
- `b843d83`: setup guard test and JS/Python syntax checks pass; real preparation
  scenario pending the public adapter.
- `3b9411b`: installer tests 3/3. Actual core/provider composition succeeded.
- `fdcc40b`: setup/provenance tests 2/2.
- GPT-6.1 Sol reviewed `a44eb7d`, `590cb10`, `b843d83`: no actionable findings.
  GPT-6.1 Sol also reviewed `3b9411b`, `fdcc40b` and the cumulative diff through
  `fdcc40b` without actionable implementation findings. The reviewer requested
  the caller-ownership clarification above; it is applied. Final cumulative code
  review `2344d03..abaf4bf`, plus `f6d2f2d` and the qualification prose
  corrections, found no actionable correctness issues. Review accepts this
  bounded source slice only, not C07 or lane closure.

Candidate core `1.1.1`, SHA-256
`4c5ef3bc8ee8413406590545cbbdef6249bd6fb4ce96e6ce8338da1a22b51274`, and Shaka
provider `1.1.1`, SHA-256
`c77762ea95c667bd4b78fc640ace658f6fc17aa48847df50e336d9495c9e1471`, compose with
upstream composer SHA-256
`b86066b90a41eccdc4528c7843043f388448217bb67ff23c8974b781b6e16451`.
Asset integrity passed, but **two installed-package API tests failed**: that core
archive lacks the player's runtime property and the expected borrowed-preview
runtime behavior. Its version label does not establish compatibility. That shared-runtime mode was rejected. The later explicit per-player evaluation
is described below; the original failed probe is retained.

The prior [28-pass/2-blocked engine receipts](../client-playback/README.md) remain
historical evidence. They do not qualify this branch or the candidate provider.

## Reproduction

Non-build checks:

```sh
node --test packages/playback/*.test.mjs packages/ui-bridge/*.test.mjs apps/desktop/test/*.test.mjs scripts/client_playback_setup.test.mjs
python3 -m unittest discover -s scripts -p test_client.py
python3 -m unittest discover -s scripts -p test_install_demuxe.py
```

Explicit provider deployment (fresh output directory):

```sh
python3 scripts/install_demuxe.py <core.tgz> --provider <shaka.tgz> \
  --composer <demuxe-source>/scripts/deploy-providers.py --output <deployment>
DEMUXE_DIR=<deployment> REQUIRE_SHARED_RUNTIME=1 node --test scripts/test_demuxe_bundle.mjs
```

Compilation and build-bearing acceptance commands must run through the shared
Kyoto queue per consolidation plan section 7.1. Server build submitted as
`8c74cd1dfb494ee2a9bbb67e9b62e827` succeeded (exit 0, 138.9 seconds after
queue admission), isolated target directory, unchanged build inputs from bridge
commit `a44eb7d`. Binary SHA-256:
`f878c08082aa5f0903ad3e769500608fecf7c4043224097f697cacac80ef6fab`.

At `4da064c`, `scripts/v2_playback_smoke.py` passed restricted pairing/catalog
reads, original bytes/ranges, ordered events/exact retry and seek activation.
It then **failed** at `POST /api/v2/processing/preparations` with HTTP 404.
The later preparation/live/recovery checks did not run.
A separate [real-server dependency preflight](dependency-preflight.json) also
confirms both `catalog.v2` and `playback.v2` are reported disabled/unimplemented
at this base, so the Go player correctly remains gated. This is concrete evidence
of the missing adapter, not a passing smoke run.

The retained browser-only Demuxe 1.1.0 deployment (core archive
`ddb82eb5dfaa94ca2c806c974bab4e45785e835b0264cd3abab160969f60f66f`)
passed all three installed-package tests with `REQUIRE_SHARED_RUNTIME=1`.
It lacks Shaka and therefore does not close C07.

Go compilation job `76a1df47796b4dc0810075520631a680` failed before compilation
because the multi-package output directory did not exist. After creating it,
replacement job `f5acb77e345341598518fa960489f6e1` passed (exit 0). API, CLI,
store and TUI tests passed; the real-server test initially failed because it
asserted Topcoat authorization while launching the legacy homepage. `3352542`
adds `--topcoat` to the test server without weakening assertions. Narrow rebuild
`a6f2e77b30f64ff08681a70ca83b97ff` passed, followed by all three root tests,
including real pairing, profile/browser authorization, events, conditional rename
and revocation. Across the five compiled Go test binaries: 55 passes, no skips.
This does not qualify the disabled Go playback feature.

GPT-6.1 Sol reviewed every implementation commit, the evidence commit `25030b3`
and cumulative diff through `3352542`, with no actionable findings. The final
PR records the final reviewed documentation SHA separately. Browser/Electron
live playback and default cutover remain blocked; passing focused tests does not
close this lane.

The operations smoke now provisions catalog items and editions through v2 with
idempotency keys, uses returned ETags for timeline viewing writes, and verifies
restored identities, watched state and cursor exhaustion. All five real-process
checks pass against the Kyoto-built server above. Public diagnostics now checks
v2 health/epoch and counters; the retired v1 ffprobe/Demuxe diagnostic fields are
not represented as v2 claims. Backup/restore still uses the operator filesystem
tool, preserving its snapshot-integrity and non-overwrite checks. No production
state rule changed.

The upgrade/restore caller now rejects candidate JSON calls outside `/api/v2/`;
only explicit frozen-baseline instances permit legacy calls. Candidate observation
compares v2 item/library identities, revision-pinned original bytes and canonical
timeline viewing position against the baseline's observations. All six scenarios
passed: baseline backup, in-place migration 9 to 33, restart idempotence, old-binary
refusal, rollback through both pre-upgrade backups, and candidate backup/restore
with epoch rotation. The [receipt](upgrade-v2-observer.json) binds both binaries.
The existing stamped baseline build was reused read-only; no compilation ran.
This is a debug-server migration gate, not installed-package qualification.

## Follow-up cache publication repair

Linux fast CI at `f3f10df` failed two offline-cache event publication tests with
HTTP409. Integration commit `28f6332` identified fsync on cap-std's O_PATH handle
as the cause. `f31d031` ports its implementation and regression test: a readable
directory handle is verified against the capability directory's device/inode,
then synced after file sync and rename and before memory publication/acknowledgement.
The directory-replacement regression proves writes remain with the opened cache.
Core event admission, ordering and retry rules are unchanged; actual filesystem
acknowledgement remains an adapter responsibility and is covered by integration
tests, including failed publication and concurrent exact retries.

Focused Kyoto compilation `39a3ececcf9a498a944ec83f6b5e769f` passed at clean source
`f31d031` (exit0, 93.2seconds). All six compiled cache tests then passed locally
on macOS. This is not Linux qualification; refreshed Linux CI is tracked on PR15.
GPT-6.1 Sol reviewed `0b8e82f`, `e9fae9f` and `f31d031` without actionable findings.
Publication privacy and diff checks pass. The service/deployment dependencies
above still prevent lane closure and default cutover.

## Combined-tree continuation

Clients now includes the integration owner's composed `a518832` tree. Catalog
smoke `0f09b3a` passes six real-process v2 checks. It does not replace the former
external-identity lookup, file-to-edition reassignment, edition rename, or full
season/episode hierarchy regressions. Explicit relationship revision conflicts,
metadata attribution, edition/timeline identity and artwork restart are covered;
the missing replacements remain acceptance gaps, not passes.

Sol reviewed `0f09b3a` and `2812de3`: no implementation defect; the coverage gap
above is recorded. CSP now permits blob scripts required by Demuxe's Shaka
loader. CSP permits all blob scripts; provider-byte integrity is enforced by
Demuxe and the installed deployment manifest, not by CSP. Browser execution
reached the Shaka live-source contract after this change, but full playback
qualification is still pending.

The 1.1.1 archive is being evaluated in explicit per-player runtime ownership
mode. The original shared-runtime test failures remain valid; this mode does
not claim borrowed-preview compatibility. Its package probe passes two checks
and skips the two inapplicable shared-ownership checks.

### Continued migration and review

- `4e617e1`: general smoke 7/7 and viewing smoke 5/5 real-process checks pass
  against the combined schema-37 application. Exact duplicate events, changed
  duplicate rejection, restart outbox drain, supersession, automatic completion,
  manual revision fences, original bytes/ranges, metadata and owner lock remain
  asserted. Positive Next is explicitly not covered by the no-next check.
- `a371793`: installed-package candidate uses v2; only syntax checked so far.
- `d00207d`: processing stress uses preparations, jobs, rendition references,
  public event cursors and logical schedules. Syntax checked; real execution
  awaits the new combined application. Existing isolated-database clock/progress
  fault injection makes this adapter integration evidence, not exclusively HTTP
  evidence. Hardware backend selection has no v2 contract replacement yet.
- `f883f19`: reliability uses v2 scans and durable backup requests. Concurrent
  writes now exercise canonical manual-watched mutations, not playback progress
  on fake media. Unavailable-root readiness remains; the former structured
  storage-error assertion lacks a v2 replacement. Retention checks both public
  readiness withdrawal and retained request snapshot directories.
- `f9b4763` composes catalog service commits through `5f15820` (schemas 38–41).
  Sol reviewed the merge/client boundaries without findings; combined build and
  real acceptance remain pending. Service ownership and publication authority
  stay in catalog/core, not client helpers.
- Sol reviewed every client commit through `f883f19`, including the live seek
  policy and failed-Electron evidence retention, without remaining implementation
  findings. The removed legacy-write rejection assertion remains a separate
  negative-boundary gate while v1 is still served.

Browser observations so far (T3 collaborative Chromium 152 / Electron 44.4.2
host, not standalone Chrome): original playback advances; explicit seek replaces
one player; reload resumes at the persisted 145473 ms. Live Shaka loads but its
moving seek window still rejects initial positioning. These are partial probes,
not a completed workflow receipt. Product Electron 44.7.0 reached the original
plan but failed media requests with 409; failed-run diagnostics are being retained
for investigation. No exact provider deployment is qualified for release yet.

Release-order provisioning and dynamic video-profile recipe selection are missing
public interfaces. Integration handoffs are on PR #14 and #17. Default cutover,
legacy removal, positive Next and provider release handoff remain gated.

### Schema-41 and playback-contract composition

The current tree composes catalog `ee842e4`, playback `0621762` and the combined
origin-fixture correction at `782c00f`. GPT-6.1 Sol reviewed the merges and
`df7d9e4` without actionable findings. All 76 JavaScript playback/bridge/desktop
tests and formatting checks pass. This does not replace real-client acceptance.

The `f9b4763` application built successfully through Kyoto
`bad3fce311c347ed8b77247e56f04cef`; frozen binary SHA-256 is
`f740686f28baa41e5dedb7fef0fd40d3687716827d384d42b61e103d7108ec94`.
Its dedicated preparation smoke passes five real HTTP/FFmpeg checks, including
second-audio selection and generated-media backup byte equality. Processing
stress completed 13 independent checks before a harness source-selection error;
that error is corrected in `f8ea0f6`. The run also found an extra generated-store
logical library, which remains a product failure. A later run timed out in its
first encode; neither run qualifies the complete stress suite. Reliability timed
out awaiting a requested backup and remains failed; `df7d9e4` adds last-job and
server-log diagnostics for the next run.

The live-window intersection build `38db68c` passed Kyoto
`3daeb868b7364cb68441ec34b1dd3f95`; its frozen binary SHA-256 is
`365e728b296d5f34e856508eba16dc2eb1da3bf1b9ad48744f5a988e20391a45`.
The updated combined build is tracked separately as
`facb0c99c69a4e63ab3f4f3e4d49bcaa` and is not yet qualified.

Electron's retained 409 response was `source_revision_changed`: source bytes
still had the same SHA-256, but the fixture ctime changed after scanning. The
server correctly fenced that fingerprint. The process responsible for the
metadata change is not established. New browser/Electron runs use isolated
system-temp fixtures without weakening source validation. Release assets remain
unqualified; archive hashes alone are not a release receipt.

### Retained runtime receipts

[Electron](electron-live-intersection.json) records 30 passing assertions and one
plain-video diagnostic against frozen `38db68c`, including audio/live-HLS and
prepared-version switching, durable outage retry, resume and restart. The raw
receipt's source field identifies the harness worktree `782c00f`, not the frozen
server source; the scoped receipt makes both identities explicit. Positive Next,
installed-package acceptance and the later combined server are not qualified by
that run.

[Browser observations](browser-live-intersection-partial.json) remain partial.
Original playback, seek and exact persisted reload resume passed. Audio switching
hit a 0.770-second live-window start; the fixed 0.250-second margin exceeded the
one-second limit. `f283ca1` instead caps the margin inside the remaining allowance
and range, with tests at the narrow boundaries. Delayed Play on an already-open
paused live conversion also jumped from 1.475 to 22.345 seconds in 2.5 seconds;
this separate provider/pacing compatibility issue remains open on PR #16.

[Reliability](reliability-v2.json) now passes all seven checks on `38db68c`,
including physical backup retention and restore. This uses fake probe data for
inventory and does not qualify codecs. Processing reached 18 checks before its
cache-expiry harness confused previously stale renditions with completed physical
cleanup; `d8d23c1` now awaits that effect independently. The generated logical
library exposure is still a failing product assertion coordinated on PR #17.

The combined `782c00f` server built successfully through Kyoto
`facb0c99c69a4e63ab3f4f3e4d49bcaa`, SHA-256
`6f678c7e9af9dc2b8bb9bb7f0d99b01ad80e11f1de6b56726920f3817ae65d9e`.
The margin update's build is tracked by `fbe3fcdcdd6248788224cb67d6ef2551`.
All 76 JavaScript checks, Rust formatting, OpenAPI structure (99 paths/150
operations/148 schemas) and JSON parity pass at this source checkpoint. Sol
reviewed the margin fix, composition and receipt boundaries without findings.
