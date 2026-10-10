# Core validation

## Motion macOS arm64 package — 2026-10-08

Built `artifacts/motion-release/Motion-macos-arm64.tar.gz`, SHA-256
`9fb8dda4f9dd6496856c69966157a1b2e8189405a30525cfe3792eb9ea0b4ae5`.
Matching source companion: `Motion-sources.tar`, SHA-256
`c73123b06a283bc8d649a504aea469e300d5ba684017a4f014a42c86e93ec2d5`.
The app includes a native Rust server, embedded web UI, npm Demuxe 1.0.0,
FFmpeg 8.0 with statically linked x264, FFprobe, and license/notice materials.
FFmpeg and the Rust binary were checked for non-system dynamic-library dependencies;
no Homebrew libraries are needed at runtime. npm integrity and source archive hashes
are pinned in packaging.lock.json. The package manifest records dirty-tree source
hashes, compiler details, tool build commands, and every installed file hash.

Eight package smoke groups passed using the extracted archive after relocation to
`Relocated Motion With Spaces`, with PATH pointing to a nonexistent directory:
archive inventory; executable and double-click launcher defaults; fixture encoding
and probing; server scanning with bundled FFprobe; byte delivery and ranges;
serving the pinned npm player; completed server H.264 conversion using bundled
FFmpeg; and saved progress after process restart. The server was started outside
the source checkout. No user library or managed service was used.

Against that same extracted package in collaborative Chromium 152 / Electron 44:

- The actual library UI resumed the generated H.264/AAC original at 2.5 seconds.
- Playback advanced to 3.72 seconds on the native browser route, with 99 decoded
  frames, width 320, and 248 distinct canvas pixel-channel values.
- Seeking to four seconds succeeded. Switching through the UI to the generated
  H.264 rendition preserved four seconds; playback advanced to 4.96 seconds and
  126 decoded frames. Closing/reopening restored approximately 4.99 seconds.
- The player reported no error. Demuxe package version was 1.0.0 and the shared
  runtime property was absent, as expected for that npm release.

Evidence: `artifacts/motion-package-test/receipt.json`, `browser.json`, server logs,
`artifacts/motion-package-build.log`, and the native build receipt. T3 screenshot
capture failed; browser evidence uses the real UI, public player state, decoded
frames, and canvas readback. Audible fidelity, other browsers, hardware encoding,
broad codec coverage, other OS versions and platforms were not qualified.

Three Rust configuration tests, Clippy with warnings denied, Rust formatting,
four Demuxe integration adapter tests, four packaging helper tests, five notice
generator tests, and Python compilation passed. The installed npm package's
integrity/export test passed; two shared-runtime tests were explicitly skipped.
Set REQUIRE_SHARED_RUNTIME=1 to require that capability for a future package.
The manual GitHub packaging workflow is added but has not run remotely. These
local packages are not Developer ID signed or notarized and are not published.

Correctness boundary: packaged paths and configuration precedence are filesystem/
CLI adapter policy; tests cover relocation, partial configuration, explicit
supplied paths, and incomplete bundles. No catalog/job core transitions changed.
Browser runtime selection follows observed upstream API availability; tests run
the production integration module and cover shared initialization, replacement,
failure retirement, and retry. Cache correctness remains owned by Demuxe. Package
assembly verifies source/archive identities and publishes a staged directory only
after assembly succeeds; this build-time policy is outside the application core.


## Automated dependency notices — 2026-10-08

`scripts/third_party_notices.py` generates a deterministic, target-specific
`THIRD_PARTY_NOTICES.txt` from locked Cargo metadata and the downloaded packages'
license/notice/attribution files, including LICENSES directories and explicitly
declared license-file paths. It retains license expressions and distinct
attribution texts while deduplicating identical text. The checked-in file covers
`aarch64-apple-darwin` with default features and includes build/test dependencies.
Demuxe and FFmpeg remain separately installed and outside this Rust inventory.

Validation: five generator tests passed (nested licenses, attribution preservation,
deterministic ordering/deduplication, custom license paths, missing/empty files,
path escape rejection, and missing declarations). Generation and `--offline
--check` passed against the current lockfile. A CLI check with deliberately stale
output failed without overwriting it. `git diff --check` passed.

`.github/workflows/licenses.yml` checks notices on pushes and pull requests with
Rust 1.95.0. This workflow has not run on GitHub. Local validation used Rust 1.99.0
and cached package sources. Collection checks packaged notice materials, not
upstream legal completeness or source obligations of optional binary providers.
No application behavior or production state transition changed.

## MIT licensing and external dependency setup — 2026-10-08

Playscale's two packages declare MIT and the root LICENSE carries the standard
MIT text with project attribution. The vendored Stateless snapshot, including
its GPL Superseedr experiments, was removed from the current tree and replaced
with the registry package `statelessness = 0.1.1` (MIT), checksum
`62c04a035e8d120597440c45dc296f6bc8394145a4a6661e15642949bdf0471c`.
The inspected registry archive contains 73 entries and no experiments directory.
The public initial snapshot excludes the former vendored source.

Validation for this change:

- `cargo test --offline -p playscale-core`: all 26 tests passed with the new
  dependency, including job and processing exploration/replay, viewing fuzz,
  and focused production policy tests.
- Job exploration bounds remain attempts <=3 and completion identities 0..4.
  Processing bounds remain attempts <=3, three source states, valid/invalid
  output and commit/rollback. Viewing fuzz is configured for 1,000 cases, up to
  100 steps each / 100,000 transitions, two session identities and sequences 0..3.
  No expansion of the models or their completeness claims is implied.
- `cargo clippy --offline --locked -p playscale-core --all-targets -- -D warnings`
  and `cargo fmt -p playscale -p playscale-core --check` passed.
- macOS-targeted `cargo metadata --offline --locked` resolved 249 packages with
  declared licenses and no GPL/AGPL declarations. This checks package metadata,
  not every source file, historical revision, or optional provider distribution.
  Full cross-platform metadata was unavailable offline because `etcetera` 0.8.0
  was not cached. Evidence: `artifacts/mit-dependency-metadata.json`.
- All three installed Demuxe package tests and the existing session, playback,
  and native-handoff JavaScript scripts passed against the existing local patched
  deployment. The asset receipt check now verifies that package's recorded files
  without depending on a Playscale-held patch or source pin.

Demuxe is now supplied as an external upstream archive. The downstream build
helper, runtime patch and candidate pin were removed; setup documents the required
shared runtime and preview capabilities. The current installed deployment was
preserved. No new upstream package or browser run was qualified in this change;
the historical results below apply only to their stated candidate.

Correctness boundary: production state and transitions did not change. Stateless
remains a development adapter exercising the same production reducers. Demuxe owns
cache correctness; Playscale retains its document-scoped runtime adapter. This
change updates licensing and dependency delivery, not domain decisions. Server
integration and full application smoke tests were not rerun for this scope.

## Historical Demuxe shared session bundle — 2026-10-08

The local default `web/vendor/demuxe` points to `demuxe-dedb3c63-session`.
Its predecessor is preserved as `web/vendor/demuxe-before-dedb3c63`.
This is a local candidate built from committed upstream source
`dedb3c63a53951efb17aaab9c3afab9ba8adf7c5` plus
`scripts/demuxe-runtime.patch`, not an upstream release qualification.
The Demuxe working tree and managed Playscale service were not modified.

The former `demuxe-bundle.json` recorded the source, patch, archive and build inventory hashes.
The final archive SHA-256 is
`ddb82eb5dfaa94ca2c806c974bab4e45785e835b0264cd3abab160969f60f66f`.
Two isolated source builds produced identical archives and inventories. The
package tool compiled TypeScript 5.9.3 from source and audited all 265 archive
files against its source and license inventory. Candidate assembly was explicit;
historical provider qualification receipts were not reused for the changed code.

Passed on the final bundle:

- 44 Node tests: 41 upstream shared-runtime, provider-promotion and state tests,
  plus installed-file provenance, the eight combinations of component runtime
  lock/connection/terminal state, and preview ownership checks.
- All three existing Playscale session, playback and native-handoff test scripts.
- `cargo build --locked` and `cargo test --locked --test server` (45 passed).
- Collaborative Chromium 152/Electron browser: concurrent component creation,
  failed manifest initialization followed by retry, independent native playback,
  player destruction/replacement, failed and cancelled opens, and rejection of
  runtime reassignment while connected.
- Three real component players prepared the same valid eight-byte Wasm module:
  exactly **one HTTP asset request and one WebAssembly compilation**, including
  replacement after the first two players were destroyed. HTTP caching was disabled
  by the fixture server, so these counts demonstrate runtime cache reuse.
- Playscale's actual Rust server and UI opened a generated H.264 MP4, switched
  through the version selector to its server-produced MP4 remux, and closed and
  reopened the title. Playback advanced after both operations, the runtime object
  identity stayed the same, and the provider manifest was fetched once across the
  page session, including background thumbnail attempts.

The first UI pass exposed software previews creating independent provider
runtimes. The final patch passes the shared owner into that path; all final-bundle
Node and browser checks above were rerun after that correction.

Correctness boundary: asset identity, reservation, acquisition, publication,
cancellation and retirement remain in Demuxe's production deterministic models.
The component patch adds its configuration lock to Demuxe's existing pure
configuration transition. Playscale's browser adapter owns the shared runtime
reference and a retryable initialization promise; it supplies that reference
before connecting each component. No server domain decision or durable state
changed, so duplicating the browser resource lifecycle in `crates/core` or
Stateless would create a second model rather than exercise the production rule.
Real browser tests verify the adapter's connection and disposal behavior.

This remains a browser-only deployment. The minimal Wasm fixture proves caching
and compilation reuse, not optional codec execution. Native playback was tested
in the collaborative Chromium browser; Safari, Firefox, physical devices and the
managed service were not retested. Playback-time advancement does not measure
frame-perfect continuity during the version switch.

Evidence from this historical patched build is retained under
`artifacts/demuxe-bundle-final/`. The downstream build helper, patch, and pin file
were subsequently removed when Playscale adopted an external upstream package
contract. Their local backup is `artifacts/licensing-before-mit.tar.gz`; neither
artifact directory is part of the Git distribution. These results do not qualify
a future upstream release.

To qualify an upstream candidate with the required shared runtime APIs, install
its archive into a new directory and run:

```sh
python3 scripts/install_demuxe.py /path/to/demuxe-compatible.tgz \
  --output web/vendor/demuxe-retest
DEMUXE_DIR=web/vendor/demuxe-retest node --test scripts/test_demuxe_bundle.mjs
DEMUXE_DIR=web/vendor/demuxe-retest node scripts/demuxe_session_server.mjs
```

Open a fresh browser page at `http://127.0.0.1:4189` and evaluate
`import('/session-tests.js').then(m => m.runSessionTests())`. Restart the fixture
server before repeating the browser suite: its first manifest request deliberately
fails and request counts are per server run. The fixture server generates its own
video and serves only test assets. Both test servers from this run were stopped.

## Earlier core validation

Validated locally on macOS on 2026-10-06 using Rust 1.95.0. This qualifies the
first core described in IMPLEMENTATION_PLAN.md, not the entire Plex-style roadmap.

## Automated checks

- `cargo test --locked`: 14 Playscale tests passed (9 server integration tests,
  2 core unit tests, 3 Stateless model/replay tests).
- The production job reducer passed bounded exploration of 20 states and 274
  transitions, with attempt identities bounded to three and completion identities
  0–4. A seeded campaign completed 1,000 cases and 100,000 transitions. These
  establish the declared lifecycle properties within those bounds, not arbitrary
  OS scheduling or filesystem behavior.
- Integration tests cover scan publication, cancellation and child-process exit,
  restart recovery, missing/replaced/moved files, root identity and symlink escape,
  metadata provenance/optimistic concurrency, immutable rendition registration,
  unavailable originals with available renditions, profile isolation, HTTP ranges,
  validators, mid-stream mutation, body lifetime limits, and API access controls.
- `cargo clippy -p playscale -p playscale-core --all-targets -- -D warnings`,
  formatting, Python compilation, and JavaScript syntax checks passed.
- `cargo build --locked -p playscale` and `python3 scripts/smoke.py` passed.
  The smoke test starts the actual binary against generated H.264/AAC media and
  an isolated database. Seven reported groups cover FFprobe cataloging, complete
  bytes by SHA-256, wire ranges/validators, authenticated metadata import, public
  OpenAPI, persisted progress/curation after restart, and exclusive data ownership.
  It also asserts rejection of unauthorized and cross-origin mutations.
- The final served OpenAPI document contained 14 paths; all 124 internal
  references resolved. This is a structural check, not full specification linting.

## Browser and package evidence

The T3 collaborative browser uses Chromium 152 / Electron 44 on macOS. A synthetic
20-second H.264/AAC MP4 and a separately generated smaller rendition were used;
no personal media was needed. The exercised path is Playscale HTTP → Demuxe's
browser-native provider → browser video rendering.

- Original playback reached the end at 640×360 with decoded frames. A canvas read
  of a rendered frame contained nonuniform pixel values.
- Seeking to seven seconds, closing, and reopening restored saved progress.
- Selecting the registered 320×180 rendition retained the title's progress and
  played successfully. Playback is a GET of registered bytes; it starts no
  transcode job.
- The page was cross-origin isolated. Browser playback is paused initially.
- After restarting with the final binary and installer output, reload/resume at
  seven seconds and rendition selection passed again. The rendition rendered at
  320×180 with 401 decoded frames at the sampled playing position and 239 distinct
  pixel byte values in a canvas readback; source controls and file drop were disabled.

Demuxe input archive: `demuxe-1.1.0-rc.2.tgz`, SHA-256
`78559c5dfb556426893ef44271c2d7723d5db72e78e75798b41ad8279f95096d`.
The installer's browser-only provider manifest was byte-compared with Demuxe's
own no-provider deployment output; SHA-256
`dd49a84db6fd412e97b5526fc0f597bafd02742b860b3b8b0ff5bb28c1cc572c`.
Each installation records the archive and installed file hashes in
`playscale-package.json`. These assets are not committed to this repository.

Original fixture SHA-256:
`61441408c10fb0b743df776ead26bbc03e05a6bf3a027d9c849d81860c5600a5`.
Smaller rendition SHA-256:
`f2ae5378ca7deb81a623c80759ff60e51a2348ee481ea2708bb239dd0040874c`.

## Limits

No qualification is claimed for Safari, Firefox, audible output fidelity, optional
Demuxe codec providers, Tailscale Serve deployment, NAS failures/performance,
large libraries, Windows filesystem identity, hardware transcoding, or external
Catabolic processing. Local FFmpeg workers, callback/polling adapters, and HLS
preparation are planned, not implemented. No remote CI or release qualification
was performed. Historical vendored Stateless provenance and its subsequent
registry replacement are recorded in vendor/STATELESS.md.

## Catalog and artwork API milestone (2026-10-06)

The later API-only milestone passed `cargo test --locked`: 21 tests comprising
14 server integration tests, four pure-core unit tests, and three Stateless tests.
Clippy with warnings denied, formatting, the binary build, Python syntax, and
`git diff --check` passed. The seven existing real-HTTP smoke groups passed again.
The browser receipts above describe the earlier playback build; no new UI or broad
browser qualification is implied by these API changes.

New integration coverage includes upgrading a database with migrations 1–3,
series/season/episode relationship constraints and numbered specials, sibling and
optimistic revision conflicts, concurrent edits, metadata external-ID lookup,
conventional metadata validation, edition reassignment/renaming, and rescan
preservation of IDs, structure, curated metadata, editions, and viewing progress.
Artwork tests cover PNG/JPEG/WebP decoding, malformed/truncated images, body and
dimension limits, an upload above the default JSON body limit, source conflicts,
local precedence, selection revision checks, pinned old bytes after a provider
update, exact byte delivery, and conditional ETags.

`python3 scripts/catalog_smoke.py` passed six real-process HTTP groups:

1. Hierarchy creation/classification and revision conflicts.
2. Metadata import and lookup by external source identity.
3. Edition assignment and rename.
4. Artwork upload, explicit pin, exact bytes, and ETag handling.
5. OpenAPI internal reference resolution, unique operation IDs, and binary upload schema.
6. Structure, edition, and artwork persistence across process restart.

The script generates its own video and PNG, uses a temporary database and free
loopback port, and shuts down its processes. Image decoding validates a decoded
image, not every animation frame. External Catabolic calls, remote artwork fetching,
provider matching, cross-item merging, artwork garbage collection, and richer
library UI remain outside this milestone.

## Viewing-state API milestone (2026-10-06)

The viewing API build passed 29 Rust tests: 20 server integration tests, six core
unit tests, and three Stateless tests. Clippy with warnings denied, formatting,
binary build, Python syntax, and diff whitespace checks passed. All 18 real-HTTP
groups passed: seven original core groups, six catalog/artwork groups, and five
new groups from `python3 scripts/viewing_smoke.py`.

New coverage exercises event ordering, exact duplicate handling, backward seeks,
concurrent competing updates, superseded sessions, legacy-write rejection after
session admission, terminal events, near-end validation, manual watched overrides,
profile isolation, preference validation/revision conflicts, next-episode ordering,
season-zero policy, and end-of-series results. Migration tests preserve old progress
without inventing completion. Rendition sessions work with an offline original and
reject later updates after its cataloged source revision changes.

The real-process script generates a short H.264 video, admits a playback session,
writes and retries events, checks continue watching, restarts the process, resumes
ordered updates, supersedes the session, verifies completion/manual overrides,
restarts again to verify watched state, and exercises next-episode lookup. Persisted
preferences and session state are checked through HTTP. The catalog smoke test also
checks the expanded OpenAPI's internal references and unique operation IDs.

This milestone changes APIs only. It does not qualify UI integration, automatic
preference application, notification delivery, session expiration, or concurrent
clients on real devices/Tailscale. Completion is client-reported; it is not an
independent observation of rendered frames. Earlier browser evidence remains
limited to the playback builds and routes recorded above.

## Operations and managed macOS service milestone (2026-10-06)

The operational build passed 32 Rust tests: one configuration test, one blocked-I/O
runtime-shutdown test, 21 server integration tests, six core tests, and three
Stateless tests. Clippy with warnings denied, formatting, build, Python syntax,
and diff checks passed. All 23 HTTP smoke groups passed (7 core, 6 catalog,
5 viewing, and 5 operations).

The operations smoke test validates configuration without creating data; checks
readiness and authenticated diagnostics; snapshots a running SQLite server;
verifies that a post-snapshot write is absent from the restore; rejects corruption
and existing restore destinations; checks graceful SIGTERM; and boots a restored
server with preserved watched state and a newly generated admin credential.
Readiness tests cover stopped workers, shutdown, and a closed database. The runtime
regression test verifies that an unresponsive blocking task cannot indefinitely
prevent process-runtime shutdown.

The actual per-user LaunchAgent was installed and then upgraded using `--replace`.
Duplicate installation without that flag was rejected without changing configuration.
A forced SIGKILL was followed by a new launchd-managed PID and healthy readiness.
Through the unchanged private Tailscale HTTPS origin, all three generated videos
were fetched and matched their complete SHA-256 hashes; the copied Demuxe entry
point returned 200. No reboot or login/logout cycle was performed: startup at login
is configured, while automatic restart was exercised directly.

The executable, assets, configuration, database, and generated test videos are now
under `~/Library/Application Support/Playscale`. The first managed process stalled
while reading a video from the external volume. The fixture files were copied and
hash-verified onto the internal disk; the library root was deliberately relocated
while its data lock was exclusively held, then rescanned. Item/file IDs and content
revisions were preserved. Original fixtures/data remain in the checkout. This does
not qualify external-volume/NAS access by launchd or provide a general relocation API.

Pre-service, pre-relocation, and pre-runtime-upgrade snapshots were retained in the
managed `backups` directory. The existing admin credential was explicitly preserved
for this controlled deployment move; the general restore tool intentionally excludes
credentials and creates none itself.

Installed executable SHA-256:
`d0b6355cfc67035472694447ea6f5faa69c4901c642fb63f9bf4f404308339b8`.
Installed Demuxe tree receipt:
`9753948835d6d76f417841c59ba1d6db7ce6b67b7dcdf0a1d9ca95fe4347cd37`.
Local service receipts are in `artifacts/video-test-20261006/managed-service-validation.json`
and `service-install-final.json` (ignored generated artifacts).

Limitations: per-user login service only, development binary rather than a signed
release, no scheduled backup retention/off-device copies, no automatic log rotation,
no pre-login system daemon, and no new browser playback qualification. Readiness
covers DB/worker availability, not disk capacity, media readability, or codec support.

## Review fixes — 2026-10-06

All 37 Rust tests and 23 real-process HTTP smoke groups passed; Clippy with
`--all-targets -- -D warnings` and formatting checks passed. New regressions cover
admission before filesystem access, cancellation retaining a blocked operation's
permit, consistent legacy item pages under concurrent title updates, and canonical
browser origins with foreign-host/origin rejection.

The real-process FIFO reproduction admitted 16 stalled opens and rejected eight
additional requests with 503; diagnostics correctly reported zero free slots.
Unblocking the FIFO completed all 16 admitted requests with 404. The concurrent
HTTP title-update/page reproduction reported zero inconsistent pages. An origin
configured as `https://EXAMPLE.com:443/` was printed canonically by `--check-config`,
accepted the browser Host `example.com`, and rejected a foreign Host.

A fresh backup preceded the managed-service replacement. Through Tailscale HTTPS,
readiness, authenticated diagnostics (16 free slots), the Demuxe entry point, and
complete SHA-256 hashes of all three synthetic videos passed after the upgrade.
Installed executable SHA-256:
`7b168ea75ccbaab53fedcb96a834107f194880051d1e21f204e85822868aa310`.
Receipts: `artifacts/video-test-20261006/service-install-review-fixes.json` and
`review-fixes-validation.json`. This is HTTP/media-byte qualification, with no new
browser playback or external-volume/NAS qualification. Stalled filesystem work is
bounded, not cancellable; its permit remains occupied until the OS call returns.

## Processing, live events, maintenance, and viewing UI — 2026-10-06

42 Rust tests pass, including the production job reducer/Stateless model, generated
same-item source-revision guards, transactional event emission/replay/retention,
subscriber capacity, processing idempotency/recovery, and scan/cache maintenance.
All 23 existing HTTP smoke groups pass. The new `scripts/processing_smoke.py` adds
13 real-process groups using actual FFmpeg/FFprobe:

- MP4 remux, AAC conversion, software H.264, and explicit VideoToolbox H.264 outputs
  were probed and fully decoded; VideoToolbox software fallback was disabled.
- Exact request replay and conflicting keys, durable SSE replay, cancellation/retry,
  SIGTERM recovery, and SIGKILL encoder reaping/recovery passed.
- Source replacement, cache-budget exhaustion, missing encoder, and exit-zero corrupt
  output produced terminal failures without publishing a rendition.
- Scheduled scans/disable, DB-only restore cache isolation, and cache expiration
  passed while original fixture hashes were preserved.

Clippy with `--all-targets -- -D warnings`, formatting, Python syntax, and JavaScript
syntax checks pass. `node scripts/test_session.mjs` verifies ordered updates, exact
network retry, stopping after supersession, and language selection.

The collaborative browser exercised the bundled session UI against synthetic
fixtures: save at 3.5 seconds, reload/continue at 3.5 seconds, preference persistence,
subtitle visibility, watched override, and automatic transition between explicitly
modeled episodes. Browser checks observed a native Demuxe mode and nonzero player
layout. These are runtime/state checks; the preview screenshot tool failed, so no
new screenshot-based visual qualification or audible-output claim is made. Multiple
language tracks were not independently qualified with a multilingual fixture.

A fresh snapshot preceded the managed upgrade. Existing credentials and the three
original media hashes were preserved. The live Tailscale service completed a
VideoToolbox H.264 job, served its 2,690,493-byte output with the recorded SHA-256,
and passed an independent full FFmpeg decode. In the browser, the generated version
played in native mode over private HTTPS with cross-origin isolation, then saved
4.246239 seconds under a separate `Server validation` profile. Readiness, SSE,
capabilities, diagnostics, Demuxe assets, and the new session module passed.

Installed executable SHA-256:
`0481d11881ebdb434108849c820d66542fbb91917061dc8e218b57f501444088`.
The unchanged Demuxe tree SHA-256 is
`9753948835d6d76f417841c59ba1d6db7ce6b67b7dcdf0a1d9ca95fe4347cd37`.
Receipts and the generated output are under `artifacts/processing-ui-20261006/`.
Hardware evidence covers this Mac and H.264 preset, not HDR, other hardware APIs,
all codecs, GPU-only scaling/decoding, or relayed Tailscale performance. Catabolic
submission/callback/transfer, adaptive streaming, and broad device qualification
remain separate work.


## Playback planner (2026-10-06)

Validated from an isolated source snapshot because concurrent work was changing
the shared checkout and target directory. The planner, preferences, browser
client, and planner-test files matched the workspace by SHA-256 at completion.

- `cargo test --locked`: 51 tests passed, including five pure planner tests and
  two planner API integration tests. The isolated build used debug information
  disabled for dev/test profiles.
- `node scripts/test_playback.mjs` and `node scripts/test_session.mjs`: passed.
- `cargo fmt -p playscale -p playscale-core --check`: passed.
- Strict Clippy remained blocked by two unrelated `collapsible_if` diagnostics
  in the in-progress `src/storage.rs` (lines 318 and 361 in the snapshot).
- T3 preview, macOS Chromium/Electron: an isolated eight-second 320×180 H.264/AAC
  fixture played as an original. Convert created no job until explicitly
  requested; software H.264 preparation completed. The final isolated build
  reused that output with one total job, advanced playback, and reported 20
  decoded frames at 320×180. Saved Original only preferences persisted and
  disabled rendition selection. Served client modules matched workspace bytes.

This validates planning and completed-file playback on that fixture/browser. It
does not qualify streaming transcoding, other codecs/devices, remote bandwidth,
or audible output. Preview snapshot/click helpers failed; browser inspection
and interaction used the same T3 preview's JavaScript evaluation tool.

## Core storage, recovery, and administration (2026-10-06)

This milestone adds automatic database snapshots, history retention, bounded logs,
free-space admission, incremental inspection, and revision-checked library/profile
administration. It was developed in `codex/core-server-v1` to keep concurrent
playback-planning work out of its qualification scope.

- 50 Rust tests pass: 6 server-library units, 1 bounded-runtime-shutdown test,
  34 HTTP/DB integrations, 6 core units, and 3 Stateless lifecycle tests.
- New regressions cover incremental reuse and full inspection, unchanged-catalog
  event suppression, library relocation hash mismatch and identity/history
  preservation, detach during active work, stale revisions, protected default
  profile deletion, validated snapshots and owned retention, injected disk-reserve
  rejection, and preservation of the authoritative session/legacy-write barrier.
- A controlled blocking cache-deletion test proves a writer can acquire its lock
  and commit while deletion is stalled. Aborting the maintenance task retains its
  filesystem admission slot until the blocked closure actually exits. This is a
  concurrency fault injection, not a real stalled NAS certification.
- The existing 36 HTTP/process groups pass: 7 media, 6 catalog, 5 viewing,
  5 operations, and 13 processing checks. The processing suite generates real
  media and validates remux, audio conversion, software H.264, explicit
  VideoToolbox H.264, cancellation, graceful restart, SIGKILL/child reaping,
  stale-source rejection, corrupt output rejection, cache admission/expiration,
  and database-only restore isolation.
- `scripts/reliability_smoke.py` adds 7 real-process groups: unavailable configured
  root/readiness, 1,000-file inventory/incremental reuse, full scan with concurrent
  viewing writes, scheduled snapshots with the existing restore tool, backup
  retention, disconnected-root catalog preservation, and disk-reserve failure
  while HTTP/viewing writes remain operational.
- The inventory fixture uses tiny unique files and a synthetic probe executable.
  All 1,000 unchanged files were reused; all 1,000 were inspected in full mode.
  It is not codec, multi-terabyte throughput, network-mount, or production-scale
  qualification. Disk pressure is injected through an impossible reserve;
  the host disk is never filled. Actual SQLite `ENOSPC` is not qualified here.
- Session JavaScript checks, strict Clippy, formatting, and diff checks pass.
  Browser/player source is unchanged by this milestone; earlier browser receipts
  are historical evidence and are not a new browser qualification.

Snapshots remain database-only, local to the server disk. Cache cleanup covers
owned generated jobs; broader artwork/preview eviction, remote backups, signed
packaging, larger real-library loads, and cross-platform filesystem failure
qualification remain open.

The managed Tailscale service was upgraded after a pre-upgrade database snapshot.
The deployed binary SHA-256 is
`e8889f3c0783268ede25e0e031f03482b5e8462cc1e785d247b868e1c61b861b`;
matching Demuxe assets were preserved. HTTPS readiness, all three original-file
hashes, catalog/library/profile/progress preservation, the new admin routes, a
schema-8 manual snapshot, and rolling-log creation passed. This deployment contains
the isolated core-operations build. Concurrent playback-planner work was preserved
in the shared checkout and its combined 57-test Rust suite plus both JavaScript
suites passed; that combined build is not this deployment receipt.

Local receipts are in `artifacts/core-v1-validation/`; generated artifacts are
excluded from Git. The 1,000-file final fixture took 3.572 seconds for initial
inspection, 0.292 seconds for incremental reuse, and 3.164 seconds for full
inspection. The maximum of 30 concurrent viewing writes was 0.003 seconds. These
are observations for tiny local synthetic files, not a throughput guarantee.

## Core operations review fixes (2026-10-06)

Fixed two reliability issues found after the operations milestone:

- Library registration previously released filesystem admission and then resolved
  the root again. It now retains the inspected path/identity and admission through
  serialized registration, avoiding a second lookup and races with relocation.
  A regression replaces the path after inspection and verifies that a later scan
  rejects the replacement instead of silently adopting it.
- Stale partial snapshots previously needed a successful new backup before they
  could be reclaimed. Cleanup now runs before disk-space admission. A regression
  verifies cleanup under injected disk pressure while preserving young partials,
  unowned directories, and complete backups beyond the normal retention count.

The isolated core build passed 52 Rust tests, strict Clippy, formatting/diff checks,
and 19 HTTP/process groups (media, operations/restore, and the 1,000-file reliability
fixture). macOS initially stalled executable startup at `_dyld_start`, and the
cancellation fixture hit its readiness timeout. The final serial suite passed;
the helper now uses shell/exec and the 30-second probe startup budget, while actual
cancellation and PID reaping retain their separate five-second limits. Review
receipts are in `artifacts/review-fixes-validation/`. Concurrent playback work is
preserved separately in the shared checkout.

### Playback planner review fixes (2026-10-06)

Review added source/revision pinning across mode changes, tied recipe reuse to
exact published output revisions, corrected preparation retry-key lifetime,
returned the final plan after exhausting open attempts, and serialized close,
viewer-change, and watched actions. Pending playback now captures its viewer
before a playable session exists.

An isolated snapshot passed 60 Rust tests, strict Clippy, formatting, and the
client/session scripts. New regressions cover source scoping, stale source IDs,
replaced output bytes, lost-response retries versus intentional new attempts,
and the final failed-open preparation proposal. The earlier storage lint
failures were no longer present in this snapshot.

The final macOS T3 browser build passed two UI regressions: marking a title
watched while preparing it updated only the selected viewer, and deliberately
delaying player destruction kept competing playback actions disabled until
cleanup completed, without leaving a stray player. Served client modules matched
the reviewed workspace files. The fixture server was stopped afterward.

Scoped commit validation on the committed baseline, excluding unrelated operations
work: all 50 Rust tests, strict Clippy, formatting, and both client scripts passed.

## State and correctness boundary refactor (2026-10-07)

Production core policies now cover processing completion/publication and output
requirements, complete-scan admission and identity reconciliation, viewing
ownership/file selection/completion, rendition registration, optimistic revisions,
catalog administration, artwork resolution, event cursor reset, and maintenance
admission/retention/scheduling. See DESIGN.md for adapter responsibilities.

Validation used Rust 1.95.0 and an isolated local source copy to avoid slow
external-volume Cargo traversal. SHA-256 comparison of 102 source, manifest,
migration, vendored Rust, and embedded web files found no differences from the
working checkout. This validates the combined working tree, not a new commit.

- `cargo test --offline --locked --no-fail-fast`: 78 tests passed, including 44
  HTTP/DB integration tests and three SQLite fault-injection cases for processing
  publication, scan publication, and scheduled admission/advancement. Stale and
  duplicate processing results also preserve timestamps and diagnostic fields.
- Stateless job exploration exhausted 20 states / 274 transitions. Properties now
  check effect identity/count and recovery outcomes explicitly.
- Stateless processing exploration exhausted 60 states / 1,602 transitions:
  attempts through three, completion attempt identities 0..4, three source states,
  validated/invalid output, and commit/rollback. A retained sequence replays exactly.
- Job, processing, and viewing seeded fuzzing each completed 1,000 cases / 100,000
  transitions (300,000 total). Viewing uses two abstract session identities and
  event sequences 0..3. This is bounded verification of declared properties.
- Policy regressions cover counter exhaustion, ambiguous moves, incomplete and
  stale scans, rendition/source identity, output requirements, viewing barriers,
  and retention/deletion races.
- Strict Clippy for Playscale/core with all targets, package formatting checks,
  build, and `git diff --check` passed.

The processing model treats atomic commit/rollback as an adapter contract; the
SQLite fault injections test that contract independently. It does not model OS
thread scheduling, file-handle semantics, or SQLite internals. The suite does not
constitute exhaustive verification of every core policy or every possible input.

The real-process smoke initially reproduced `SQLITE_BUSY_SNAPSHOT` during
scheduled admission: a background maintenance-status write invalidated a deferred
transaction's read snapshot before its first write. Mutating state transactions
now use `BEGIN IMMEDIATE` before observing transaction-local state. A deterministic
second-connection test proves that the writer is reserved until commit; read-only
projections retain deferred transactions.

After that fix, `scripts/processing_smoke.py` passed all 13 real-process groups
against the rebuilt matching source: three FFmpeg recipes, explicit VideoToolbox
H.264 with decoded output, SSE replay, cancellation/retry, graceful restart,
SIGKILL recovery/child reaping, changed-source rejection, cache-budget admission,
encoder failure, corrupt-output rejection, scheduled scans, database-only restore
isolation, and cache expiration preserving original bytes. Fixtures and server
state were isolated; no existing user library or running service was changed.

### Review fixes (2026-10-07)

- Exhausted attempt counters now become terminal in the production reducer on
  start/recovery. Workers execute only an explicit Run effect, and exhausted
  processing jobs reject retry. A real-worker regression checks that both queues
  continue past an exhausted job and that an unadmitted processing attempt's
  directory is untouched. The prior no-overflow checks did not establish these
  worker behaviors.
- Viewing properties now require acceptance and the intended state/revision
  changes for valid updates, plus complete no-ops for exact retries and rejection.
  Fault injection demonstrates detection of reject-all behavior and lost automatic
  completion. These supplement the prior safety-only properties.
- Model build identities include the production reducers' direct core source
  dependencies, so changing the job or revision reducer changes replay provenance.
- The legacy progress adapter reconstructs actual persisted viewing state instead
  of supplying placeholder watched/position fields to the core.

The full Rust suite passed 81 tests (including 45 HTTP/DB integrations) in an
isolated copy; all 102 source/manifest/migration/embedded-asset files matched the
checkout. After the replay-provenance adjustment, core/model tests were rerun and
passed. Strict all-target Clippy, package formatting, and diff whitespace checks
passed. Bounded exploration remains 20 job states / 274 transitions and 60
processing states / 1,602 transitions, with 300,000 seeded fuzz transitions total.
The 13-group real-process smoke above was not repeated for this review follow-up.

## Publication privacy and local configuration — 2026-10-08

The public repository starts from the current source snapshot. Developer history
and other linked checkouts are kept separately from the publication repository.
Private hostnames and local attachment paths were replaced with portable examples
and evidence labels. Required upstream attribution remains unchanged.

`config.example.json` validates with existing JSON configuration support. Local
config, `.env` and generated token/database files are ignored. Runtime state and
core transition rules are unchanged; these changes belong to build/configuration
adapters. No new Stateless model is needed for archive ownership or compiler
source-path mapping. Existing core/model and real-server tests remain applicable.

Validation on an isolated, byte-matched source copy:

- Full locked offline Rust suite: 82 passed, no failures or ignored tests.
- Packaging tests: seven passed, including binary-path rejection and archive
  owner/extended-metadata normalization.
- Notice generator tests: five passed; locked offline notices check passed.
- Demuxe integration adapter tests: four passed.
- Gitleaks 8.30.1 candidate source scan: no detections.
- Source publication privacy check, formatting and diff checks passed.

Package assembly rejects private build paths and tailnet hostnames before atomic
publication. It requires the new portable media-tools receipt, remaps Rust paths,
and removes local archive owner metadata. The extracted-package smoke test also
checks archive and binary privacy before exercising bundled runtime dependencies.
Release build receipts and smoke results are kept outside tracked source.

## Delivery restart records and generation playheads (A06/A07, 2026-10-09)

Delivery creation and close acknowledge only after their diagnostic/recovery
snapshot is saved. Terminal sessions are evicted from memory only after a final
snapshot is saved; a failed write leaves them available for retry. Intermediate
snapshots are asynchronous and may lag. Restart applies the production core's
`Interrupt` transition to recorded live sessions: it does not resume encoders or
serve the previous process's manifests. Records expire after seven days at
startup. Migration `0015` is provisional pending A02's numbering coordination.

The production core owns generation identity, active-playhead acceptance,
lease renewal, and lifecycle fencing. A pending generation's heartbeat renews
the lease without replacing the active generation's playhead. SQLite remains
an adapter: its conditional upsert rejects older revisions and any write over
an interrupted record, including a late higher-revision snapshot from the old
runtime. Recovery reads and fences snapshots in one write transaction, preventing
an old-runtime write between those steps. Creation failure stops the admitted worker; persistence is not an
atomic transaction with process launch.

Regression coverage includes exact core playhead/lease/effect assertions,
bounded Stateless exploration of the production reducer, SQLite snapshot
ordering and restart fencing, and HTTP/FFmpeg integration for restart reads,
command refusal, old-worker capacity accounting, and readable closed records
after memory eviction. This does not qualify multi-server database sharing,
power-loss durability, browser playback, or remote/NAS behavior. Seeded delivery
fuzzing remains explicitly ignored as previously deferred.

`cargo test -p playscale-core --test delivery_model -- --nocapture` passed
four tests with one explicitly ignored fuzz test. All three enumerations reached
`GraphExhausted` with zero skipped checks (configured ceilings: 3,000,000 states,
300,000,000 transitions, depth 200):

| Model bounds | States | Transitions |
| --- | ---: | ---: |
| Playhead: two generations, report at 6 s, pause/resume at 8/4 s | 2,611 | 107,505 |
| Window: one generation, eight segment indices, 1/6 s segments, clock through 48 s | 329,040 | 9,193,372 |
| Lifecycle: up to three generations, byte/HLS routes, clock through lease expiry | 133,948 | 6,732,222 |

These are finite model bounds, not exhaustive real-world scheduling coverage.

Native results: `cargo test --lib delivery -- --nocapture` passed two adapter
and eight core unit tests. On the final recovery-transaction source,
`cargo test -p playscale --test delivery -- --nocapture` passed all three real
FFmpeg/FFprobe tests with no skips, and
`cargo test -p playscale --test server snapshots_are_validated_retained_and_disk_pressure_is_reported -- --nocapture`
passed. Tests used temporary databases/media/cache roots. Fresh executable
linking and macOS loader startup were slow; superseded native runs were stopped
and the affected integration checks were rerun on the final source.

## A06/A07 seeded and mutation verification (2026-10-09)

This closes the seeded-fuzz deferral above. Both seeded tests now run by default
and require `CasesCompleted`, their exact case/transition counts, and zero
skipped checks. Delivery's former 400,000-transition cap could stop its
3,000-by-150 run early; it now permits and verifies all 450,000 transitions.
Seed `20261009` passed 3,000 delivery cases / 450,000 transitions and 2,000 work
cases / 200,000 transitions. These generated sequences found no violation;
they do not exhaust the wider state space.

The deterministic heartbeat boundary test passed for active and pending
identities, positions 0, 60,000, 61,000, 61,001 and `u64::MAX` against a 60-second
duration, exact lease expiry, and generation/lease rejection precedence. It
asserts accepted lease/revision/playhead values and exact absence of effects.
The production reducer is unchanged by this verification increment.

`python3 scripts/check_execution_delivery_mutations.py` passed two baseline
checks and detected four compiled regressions through the expected test failures:
pending heartbeats replacing the active playhead, accepting out-of-range
positions, renewing an expired lease, and releasing capacity on cancellation
before worker exit. The script uses a temporary core-only workspace and isolated
Cargo target; it rejects compile failures, missing tests, and timeouts as mutation
evidence. This is a four-mutation sensitivity check, not a general mutation score.

## A06/A07 follow-up audit and bounded progress decoding (2026-10-09)

The execution audit found `BufReader::lines()` retaining unbounded encoder stdout
until a newline. Processing now reads at most 4 KiB per select iteration and
keeps a fixed 128-byte line prefix. Oversized lines are discarded through their
newline; subsequent valid progress, CRLF, fragmented fields and final EOF fields
are supported. Malformed/non-finite/negative advisory progress is ignored.
Cancellation and the existing total processing deadline remain selectable while
a line is incomplete. No core publication/admission decision changes: this
parser reports advisory progress only, and media validation still gates output
acceptance. Memory/parse-work bounds belong in this I/O adapter rather than a
new domain lifecycle model.

The plan audit does **not** establish complete A06/A07 delivery. Remaining items
include separate startup/no-progress deadlines (processing currently has a total
deadline), Windows Job Object containment/qualification, v2 authenticated plan
and ticket integration with A08, delivery-creation idempotency, and A02 migration
number coordination. The current live adapter is the experimental v1 H.264/AAC
SDR path; browser/player, remote/NAS and additional live pipelines still need
qualification. The four source mutations above cover selected rules, not every
fault or property in the plan.

Validation on the final parser source: `cargo test -p playscale --lib processing::
-- --nocapture` passed all eight tests (four decoder, three supervisor and one
atomic-publication regression). `cargo build -p playscale --bin playscale` passed.
After warming the fresh executable with `--help`,
`python3 scripts/processing_smoke.py` passed all 15 checks in disposable roots.
The added fixtures write 16 MiB without newlines to each pipe: one proceeds to
real encoding/validation, and another holds an incomplete stdout line while
readiness and cancellation remain responsive. Existing checks also passed for
three real recipes, cancellation/retry, restart and SIGKILL recovery, source
replacement, cache pressure, corrupt output rejection, restore, and preservation
of original bytes. VideoToolbox completed and its output was probed/decoded on
this Mac; this is not qualification of other hardware or live delivery pipelines.

## A06/A07 execution liveness and ownership ordering (2026-10-09)

Processing now accepts optional `startup_timeout_seconds` and
`no_progress_timeout_seconds`, each 1–86400 seconds. Omitted/null settings retain
existing behavior. The production core's `execution_deadline::Deadline` owns
expiry decisions; adapters provide monotonic elapsed milliseconds and decoded
media progress. Positive advancement ends startup, duplicate/regressing positions
cannot renew the stall budget, and expiry cannot be revived by later progress.
The absolute total limit remains independent. These clocks apply separately to
encoding and FFmpeg decode validation, not copying, FFprobe, live-delivery pacing
or an expected-media-duration resource policy.

The bounded progress decoder now reports the highest valid observation in each
read: a regressing field coalesced into the same pipe read cannot hide genuine
advancement. Test coverage varies every read chunk size. Validation/publication
still requires the existing core completion decision and atomic adapter commit;
progress does not establish output validity.

Both processing and live delivery register the ownership witness before spawn.
Previously a failed post-spawn witness open could leave a live process without
its lease's drop guard. Registration failure now prevents spawn. The adapter
regression verifies that deleting a registered witness path cannot free capacity
while its open description remains held, and that closing the last holder releases
the reservation. Core reservation/termination decisions are unchanged.

`cargo test -p playscale --lib -- --nocapture` passed all 25 tests on final
production source, including configuration bounds, five parser tests, Unix
supervisor cleanup, witness retention, atomic publication and restart fencing.
`cargo test -p playscale-core --test execution_deadline_model -- --nocapture`
exhausted 241 states and 1,030 transitions with zero skipped checks. Its finite
domain has elapsed times 0–9, positions 0/1/2/3/u64::MAX, total limit 8, startup
limit 3 and stall limit 2. Properties check irreversible expiry, the absolute
cap, zero progress, duplicate/regressing observations, exact renewed stall windows
and single expiry reasons. This does not prove native timer scheduling or OS
termination.

`python3 scripts/check_execution_delivery_mutations.py` passed three baselines
and detected seven compiled regressions through the expected named test failures.
The three new regressions accept duplicate progress, revive expired execution
and remove the total deadline; the four existing delivery/capacity mutations
remain covered. Compilation errors, absent tests and timeouts remain failures
of the harness, not evidence that a mutation was detected.

Native validation encountered startup delays before Rust test code: sampling a
waiting test executable showed `_dyld_start` and a 112 KiB footprint. The smoke
runner is executed after building and warming the final server executable; no
application timeout was enlarged to hide this delay. An initial smoke assertion
also read the wrong log file; it now checks the server's bounded diagnostic log.
The restart integration fixture was made deterministic with real-time FFmpeg
input: an ultrafast encoder could previously finish before the test asserted
that recovery still retained its reservation. The exact reservation assertion
is preserved.

The remaining full-plan scope and integration dependencies are listed in
`A06_A07_HANDOFF.md`. In particular, these changes do not implement authenticated
v2 plan admission, delivery-create idempotency, Windows Job Objects or additional
live pipelines, and do not finalize A02's migration numbering.

Final native results on macOS 26.5.2 arm64, Rust 1.99.0 and FFmpeg 8.1.2:
`cargo test -p playscale --test delivery -- --nocapture` passed all three tests
with real tools and no skips. `cargo build -p playscale --bin playscale` passed.
Once the build/test queue finished and the final executable was warmed with
`--help`, `python3 scripts/processing_smoke.py` passed all 18 checks. The added
startup and no-progress cases verify distinct diagnostic reasons, no published
output, actual encoder reaping and successful subsequent work. The existing
recipes, VideoToolbox probe/decode, pipe floods, cancel/retry, restart/SIGKILL,
source replacement, cache/output rejection, scheduling, DB-only restore and
original integrity checks also passed. Earlier runs timed out at recipe or
server startup, including restored-root startup; they are not counted as passes.
Failed-boot cleanup now reaps the attempted server and reports the relevant data
root's log. No readiness or application deadline was enlarged. Formatting and
diff checks passed.


## Durable delivery admission and lost acknowledgements

The production core now decides create, exact replay, key conflict and foreign
scope rejection. The SQLite adapter revalidates authority inside the same write
transaction as receipt lookup and publication. Initial delivery state and its
acknowledgement receipt commit together before execution dispatch. A bounded
owned admission task survives loss of the HTTP waiter; shutdown serializes with
admission. Retiring or recovering a transport never deletes its receipt or
restarts execution for an exact retry. Receipts currently have no expiry.

The v1 endpoint accepts an optional 16–128 byte Idempotency-Key in its existing
legacy-admin scope. The service exposes a transactional authority callback for
principal-specific integration. This is not the authenticated v2 plan endpoint:
plan-token validation, wire-request identity and profile authorization remain
integration work. Migration 0020 is a provisional A02 allocation request.

The admission model exhausted 77 states and 1,078 transitions with zero skipped
checks. Its finite domain includes two principals, two request digests, one key
per principal, rollback, commit followed by lost dispatch/acknowledgement, normal
dispatch, retirement and restart. Properties assert exact start/ack/conflict
effects, at most one start per receipt, receipt preservation, and unchanged
state on rollback/replay. This proves the modeled decisions within those bounds;
it does not itself prove SQLite atomicity or native process behavior.

`cargo check -p playscale --tests`, formatting and diff checks passed.
`python3 scripts/check_execution_delivery_mutations.py` passed five baselines
and detected ten compiled mutations through the expected named test failures.
The three added regressions recreate a delivery on retry, accept changed request
content and accept a foreign principal receipt.

`cargo test -p playscale --test delivery -- --nocapture` passed all six native
tests with real FFmpeg/FFprobe and no skips. The three new tests verify concurrent
exact replay, conflicting keys, retired/recovered replay without execution,
transaction rollback on receipt failure, continuation after a lost HTTP waiter,
current-authority rejection and principal isolation. Existing before-completion
HLS, generation switching, disk rejection and restart fencing also passed.
The focused core admission unit test and both server delivery unit tests passed.
These results qualify the admission increment, before the subsequent optional
expected-duration execution policy.
