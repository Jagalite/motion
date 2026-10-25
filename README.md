# Motion

> **Combined-source WIP snapshot:** Start with [LOCAL_BUILD.md](LOCAL_BUILD.md).
> For this snapshot it supersedes the older dependency/toolchain notes below:
> Motion builds against the paired native Catabolic source. This is not a
> fully qualified release.

A private Rust media server using Axum, Tokio, SQLite, and Demuxe. The first core
supports local libraries, durable scans, a documented public API, original-file
streaming, ordered viewing sessions, source-attributed metadata/tags, generated
renditions, live events, scheduled scans, automatic snapshots, and library administration. Catabolic is an optional future producer of API data, not a dependency.

## Build a complete application package

The first package target is **macOS Apple Silicon (arm64)**. Building requires
Rust 1.98 or newer, Python 3.12 or newer, Node/npm, Xcode command-line tools,
`make`, and `pkg-config`. These tools are not needed to run the finished package.

```sh
python3 scripts/package_app.py --output artifacts/release
python3 scripts/test_package.py artifacts/release/Motion-macos-arm64.tar.gz \
  --work artifacts/package-test
```

The builder gets **Demuxe 1.1.1 from npm**, verifies its pinned integrity/hash,
builds FFmpeg 8.0 with x264, zimg and libass (see `THIRD_PARTY.md`) from
checksum-pinned sources, and packages FFmpeg,
FFprobe, the Rust server, web UI, player assets, and license materials. Inputs
are recorded in `packaging.lock.json`; application input hashes and an installed
file inventory are in `motion-package.json`. Successful native tool builds can
be reused after their hashes and source pins are checked. Use a fresh output path
for each build. The separate source companion includes upstream Demuxe sources,
FFmpeg and its library/build-tool sources, Motion sources, and the native tool
build recipe. Rebuilding the native tools needs CMake (for HarfBuzz).

Build artifacts:

- `Motion-macos-arm64.tar.gz`: extract the whole folder, then double-click
  `Motion.command` or run `./motion --library /absolute/path/to/media`.
- `Motion-sources.tar`: matching source companion; distribute alongside the app.
- `SHA256SUMS`: archive hashes.

The executable finds bundled tools and assets relative to its own location.
Default packaged data lives in `~/Library/Application Support/Motion`; moving
or replacing the application does not move that data. `--data-dir`, `--ffmpeg`,
`--ffprobe`, `--demuxe-dir`, and JSON configuration provide explicit overrides.
`--check-config` prints the effective paths without starting the server.

The local package is not Developer ID signed or notarized. Linux, Windows,
Intel Mac, and older macOS versions have not been qualified. The manual
**Build Motion package** GitHub workflow builds and tests the same archive;
it does not publish a release. Browser playback still needs a separate browser
qualification against the resulting package.

## Run from source

For development, install FFmpeg/FFprobe on PATH and obtain the pinned npm player:

```sh
python3 scripts/build_motion_provider.py --cache artifacts/npm \
  --source-build artifacts/shaka-npm-1.1.1 --output web/vendor/demuxe-npm-1.1.1
cargo run --locked -- --demuxe-dir web/vendor/demuxe-npm-1.1.1 --library /path/to/media
```

The provider builder requires new output and source-build directories. Existing
development deployments are preserved. Source builds retain their original local `data/` default.

Open **http://127.0.0.1:8787**. Registration and an initial scan run for each
`--library`; repeat the option for more roots. Pair the browser from a paired
administrator terminal under **Pairing requests**, or redeem an invitation.
On a new server with no users, choose **Pair this browser**, then on the server
computer run `motion --operator-token-file <data-dir>/admin-token tui`: press
**s** on **Pairing requests** to create the Owner, then compare the browser's
code and approve it (see [First run](ACCESS_MANAGEMENT.md#first-run-single-owner)).
No email, password or SSO is needed. See [Administrator access](ACCESS_MANAGEMENT.md)
for users, grants, invitations, device revocation and explicit server-local recovery.
On a trusted home network, `--access-mode trusted_household` lets everyone browse
and play without signing in
(see [Household mode](ACCESS_MANAGEMENT.md#household-mode-no-sign-in)).
Use the exact configured hostname. Local data defaults to `data/`; put it on a local
disk outside media roots. One process may own a data directory.

Demuxe 1.1.1 is the pinned npm distribution. The builder verifies both archive
checksums, preserves upstream licensing materials, and adds Motion's provider
manifest and paced-HLS adaptations. Build and asset hashes are recorded in
`playscale-package.json`. The Shaka build requires Node, Python, and Java.

`packages/ui-bridge/player.js` is the production playback bridge. The legacy
`fixtures/legacy-web/demuxe.js` remains as a compatibility fixture. If a package provides
both `DemuxeRuntime` and `demuxe-player.runtime`, it uses one document-owned runtime
and retries failed initialization. Motion packaging retains per-player ownership
and applies its paced-HLS adaptations to the integrity-pinned npm release. No cross-player Wasm
compilation reuse is claimed for that deployment.
Run `DEMUXE_DIR=/path/to/installed/player DEMUXE_RUNTIME_OWNERSHIP=per-player node --test scripts/test_demuxe_bundle.mjs`
to verify installed files and exports. Set `REQUIRE_SHARED_RUNTIME=1` when qualifying
a package that must provide that API.

For Tailscale Serve, set `--public-origin https://your-server.your-tailnet.ts.net`
and configure Serve to forward to the loopback listener. The configured Host must
reach Axum unchanged. See [validation](VALIDATION.md) for the tested deployment.

## Managed deployment

See [OPERATIONS.md](OPERATIONS.md) for JSON configuration, macOS login-service
installation, readiness/diagnostics, bounded logs, retention, and SQLite backup/restore.

## Public API

Topcoat, Electron and the Go CLI/TUI use `/api/v2`. The served contract is at
`/api/v2/openapi.json`; the reviewed source is
[Motion_Server_API_v2.yaml](contracts/Motion_Server_API_v2.yaml). Protected
operations require a scoped credential or the local operator token. Profiles
identify viewing history; they do not replace authentication. Browser mutations
must pass the origin and CSRF policies.

Old clients and media URLs must update. See [the cutover note](MOTION_V2_CUTOVER.md)
for retirement, data migration and rollback requirements. [API.md](API.md) is
historical documentation of the retired API.

```sh
MOTION_ACCESS_TOKEN=your-credential python3 scripts/client.py http://127.0.0.1:8787
```

## Architecture and API v2

The [consolidation and milestone completion plan](MOTION_CONSOLIDATION_AND_COMPLETION_PLAN.md)
is the current execution addendum: retire v1 client compatibility, preserve data
upgrades, repair integration gaps, and close M0–M4 with explicit acceptance evidence.
It supersedes the older plan's requirement to keep serving v1; the remaining
architecture and later milestones still apply. This is planned work, not a claim
that the cutover has shipped.

The [Motion architecture and implementation plan](Motion_Final_Architecture_and_Implementation_Plan.md)
(design version 1.1.0, Topcoat revision) is the main design document. It and the
[API v2 contract](contracts/Motion_Server_API_v2.yaml) describe the next
implementation target; [Topcoat research](research/TOPCOAT_RESEARCH.md) records
the frontend selection evidence. Version 1.1.0 replaces the React/Vite frontend
decision with Topcoat; the Motion-owned catalog and native desktop host are
unchanged. The [implementation readiness review](MOTION_IMPLEMENTATION_READINESS.md)
was written against version 1.0.0 and lists checks performed, missing handoff
artifacts, and the recommended first work. The supplied documents are preserved
unchanged. Their referenced companion bundle (`CHANGELOG_TOPCOAT.md`,
`contracts/TOPCOAT_PRESENTATION_CONTRACT.md`, `qualification/TOPCOAT_ACCEPTANCE.md`,
`agents/`, `contracts/API_ENDPOINTS.md`, `qualification/PARITY_LEDGER.md`) was
not included in this import.

For the new implementation, use these documents for target architecture decisions;
`DESIGN.md`, `IMPLEMENTATION_PLAN.md`, and `ROADMAP.md` retain earlier decisions and
history, including proposals superseded by the new plan. The current served-operation
inventory and capability responses describe implemented endpoints; contract presence
alone does not establish implementation or runtime qualification.

## Verification

```sh
python3 scripts/third_party_notices.py --check
cargo test --locked
cargo clippy -p playscale -p playscale-core --all-targets -- -D warnings
cargo fmt -p playscale -p playscale-core --check
cargo build --locked -p playscale
python3 scripts/smoke.py
python3 scripts/catalog_smoke.py
python3 scripts/viewing_smoke.py
python3 scripts/operations_smoke.py
python3 scripts/reliability_smoke.py
python3 scripts/processing_smoke.py
node scripts/test_session.mjs
```

Stateless uses the MIT-licensed `statelessness` 0.1.1 package, pinned in
`Cargo.lock`, to test the production reducers. Its upstream experiments are not
vendored. The smoke
script creates synthetic media and isolated databases, starts the actual server,
uses only HTTP for application operations, restarts it, and cleans up its processes.
See [VALIDATION.md](VALIDATION.md) for evidence and qualification limits.

## Current boundaries

- Discovery creates unclassified video/audio items and an Original edition. The API
  supports explicit movie/series/season/episode structure, edition management, and
  source-attributed artwork uploads/selections. It does not infer structure or
  contact metadata providers; the bundled UI still uses the file-oriented catalog.
- Scans hash full files for exact revision identity, skip symlinks, and are capped
  at 100,000 recognized media files. A traversal failure leaves previous catalog
  observations intact. Root replacement requires explicit operator intervention.
- Viewing APIs include watched overrides, continue watching, next-episode lookup,
  ordered playback sessions, and profile preferences. The bundled UI uses these
  APIs, applies language/subtitle preferences, and offers optional episode autoplay.
- Metadata contributions and existing rendition registration are implemented.
  Importers map their files using the admin file API; arbitrary remote URLs and
  external file paths are not accepted for playback.
- The [playback planner](API.md#playback-planning) supports Auto, strict Original
  only, and Convert, with profile defaults and per-view overrides. Preparation
  requires an explicit admin-authorized job and completes before playback.
- Registered renditions can be played without generating them again. Local FFmpeg
  processing supports explicit MP4 remux, AAC audio conversion, and H.264 720p
  recipes, with software or macOS VideoToolbox encoding. Delegated Catabolic jobs,
  callback delivery, adaptive HLS, HDR tone mapping, and subtitle burn-in remain future work.
- Processing configuration accepts optional `startup_timeout_seconds` and
  `no_progress_timeout_seconds` (1–86400 seconds each). Startup requires advancing
  media time; repeated or regressing progress and log output do not renew either
  deadline. The no-progress clock begins at the first advancement. Omitted/null
  values disable these additional limits and preserve the existing
  `timeout_seconds` total limit. Each encoder and FFmpeg decode-validation
  invocation has its own clocks; timed-out attempts cannot publish and retain capacity until reaped.
- A blocked filesystem syscall cannot be forcibly interrupted. NAS performance,
  large-library scale, Windows filesystem identity, relayed Tailscale paths, and broad codec/
  browser coverage require further qualification.

[Design](DESIGN.md) · [Implementation plan](IMPLEMENTATION_PLAN.md) · [Roadmap](ROADMAP.md)

## License

Playscale's original code is licensed under [MIT](LICENSE). External dependencies
retain their own licenses; see [THIRD_PARTY.md](THIRD_PARTY.md) for the dependency
and distribution boundaries. Rust dependency notices are generated in
[THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt). After dependency updates, run
`python3 scripts/third_party_notices.py`; CI checks that the file stays current.

## Local configuration

Copy `config.example.json` to `config.local.json`, add your library paths, and run
`motion --config config.local.json` (or `cargo run -- --config config.local.json`
from source). Validate it first with `--check-config`. Relative paths resolve
against the configuration file. The example preserves the package's bundled
Demuxe, FFmpeg and FFprobe defaults.

Real configuration, `.env` files and generated admin-token files are ignored by
Git. Motion reads JSON configuration; it does not load `.env` automatically.
Keep the protected admin token in Motion's data directory for server-local recovery;
ordinary browser pairing and invitations never require copying it.
Commit only example configuration with placeholders. `--check-config` output
contains your local paths; keep that output private.

Release packaging removes archive owner metadata, remaps Rust compiler paths,
and rejects private machine paths or tailnet hostnames in the assembled app.
Run `python3 scripts/check_publication.py` before publishing source; the CI notice
workflow also runs this check. Upstream license and author notices are retained.
