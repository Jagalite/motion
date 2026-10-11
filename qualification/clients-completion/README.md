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
| `catalog_smoke.py`, `operations_smoke.py` | v2 metadata/artwork/scan/storage/backup; data/catalog owner | Current-server v1 callers; pending adapter integration and migration. |
| `processing_smoke.py`, `video_profiles_smoke.py` | v2 recipes/preparations/jobs/events; preparation owner | Current-server v1 callers; preserve engine regressions while migrating. |
| `viewing_smoke.py`, `reliability_smoke.py`, `smoke.py` | Canonical viewing + v2 storage/catalog; playback/data owners | Current-server v1 callers; pending authority/cutover integration. |
| `test_package.py`, `upgrade_restore_check.py` | Exact installed v2 candidate; integration/release owner | Candidate traffic still needs migration; distinguish old-binary writes from candidate reads. |
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
`8c74cd1dfb494ee2a9bbb67e9b62e827`, isolated target directory, source with bridge
commit `a44eb7d`; initially deferred by host memory pressure. A queued job is not
build or runtime evidence.
