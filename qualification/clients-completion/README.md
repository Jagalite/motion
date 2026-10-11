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
| `client_playback_e2e.mjs`, `v2_playback_smoke.py` preparation setup | `/api/v2/processing/preparations`, `/api/v2/jobs/{id}`; data/preparation service | Converted to contract; needs served preparation API before real acceptance. |
| `client_playback_upgrade.mjs` old-binary writes | Frozen old-binary v1 setup | Explicit exception; candidate reads use v2. |
| Go CLI/TUI | Existing public v2 catalog/playback and `playback.v2` capability | Retain negotiation. Playback owner must correct server facts; run real-server CLI/TUI afterward. |
| `packages/ui-bridge/player.js` delivery pacing | Playback owner's C06 heartbeat playhead contract | Pending interface integration and playback-only long conversion acceptance. |
| Topcoat `/`, item and player pages; Electron host | Authorized production presentation service | Already opt-in through `--topcoat`; default cutover awaits replacement workflow and absence tests. |
| Legacy `web/app.js`, `web/playback.js`, `web/session.js`, `/media/{id}` | Topcoat/player bridge and v2 media; integration router owner | Keep until cutover gates pass, then remove mounts/assets and prove no business behavior. |
| `operations_smoke.py` | v2 catalog, timeline viewing and diagnostics; Clients | Converted; real-server backup/restore smoke passes all five checks, including rejection of the old operator token after restore. |
| `catalog_smoke.py` | v2 metadata/artwork/relationships; Clients | Converted; six real-process checks pass. Coverage differences below remain explicit. |
| `processing_smoke.py`, `video_profiles_smoke.py` | v2 recipes/preparations/jobs/events; preparation owner | Current-server v1 callers; preserve engine regressions while migrating. |
| `viewing_smoke.py`, `reliability_smoke.py`, `smoke.py` | Canonical viewing + v2 storage/catalog; playback/data owners | Current-server v1 callers; pending authority/cutover integration. |
| `upgrade_restore_check.py` | Public v2 catalog, revision-pinned file bytes and canonical viewing; Clients | Candidate migrated; all six real-process upgrade/rollback scenarios pass. Frozen baseline alone retains v1. |
| `test_package.py` | Exact installed v2 candidate; integration/release owner | Candidate traffic still needs migration after preparation adapter integration. |
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
runtime behavior. Its version label does not establish compatibility. It is not
selected for product qualification; the failed probe is retained here.

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
