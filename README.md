# Motion

A private Rust media server using Axum, Tokio, SQLite, and Demuxe. The first core
supports local libraries, durable scans, a documented public API, original-file
streaming, ordered viewing sessions, source-attributed metadata/tags, generated
renditions, live events, scheduled scans, automatic snapshots, and library administration. Catabolic is an optional future producer of API data, not a dependency.

## Build a complete application package

The first package target is **macOS Apple Silicon (arm64)**. Building requires
Rust 1.95 or newer, Python 3.12 or newer, Node/npm, Xcode command-line tools,
`make`, and `pkg-config`. These tools are not needed to run the finished package.

```sh
python3 scripts/package_app.py --output artifacts/release
python3 scripts/test_package.py artifacts/release/Motion-macos-arm64.tar.gz \
  --work artifacts/package-test
```

The builder gets **Demuxe 1.0.0 from npm**, verifies its pinned integrity/hash,
builds FFmpeg 8.0 and x264 from checksum-pinned sources, and packages FFmpeg,
FFprobe, the Rust server, web UI, player assets, and license materials. Inputs
are recorded in `packaging.lock.json`; application input hashes and an installed
file inventory are in `motion-package.json`. Successful native tool builds can
be reused after their hashes and source pins are checked. Use a fresh output path
for each build. The separate source companion includes upstream Demuxe sources,
FFmpeg/x264 sources, Motion sources, and the native tool build recipe.

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
mkdir -p artifacts/npm
npm pack demuxe@1.0.0 --ignore-scripts --pack-destination artifacts/npm
python3 scripts/install_demuxe.py artifacts/npm/demuxe-1.0.0.tgz \
  --output web/vendor/demuxe-npm-1.0.0
cargo run --locked -- --demuxe-dir web/vendor/demuxe-npm-1.0.0 --library /path/to/media
```

The installer requires a new output directory. Existing development deployments
are preserved. Source builds retain their original local `data/` default.

Open **http://127.0.0.1:8787**. Registration and an initial scan run for each
`--library`; repeat the option for more roots. The webpage includes folder/scan
administration using the token in `data/admin-token` (created with mode 0600 on Unix).
Use the exact configured hostname. Local data defaults to `data/`; put it on a local
disk outside media roots. One process may own a data directory.

Demuxe 1.0.0 is the currently pinned npm distribution and includes its upstream
engine assets. The installer preserves that deployment and its licensing materials.
Modular Demuxe packages instead receive an explicit browser-only provider manifest.
Each install records archive and installed-file hashes in `playscale-package.json`.

`web/demuxe.js` supports the published per-player API. If a future package provides
both `DemuxeRuntime` and `demuxe-player.runtime`, it uses one document-owned runtime
and retries failed initialization. **The npm 1.0.0 package has no shared-runtime
cache API**; no cross-player Wasm compilation reuse is claimed for this package.
Pin and qualify a newer npm package before enabling that release capability.
Run `DEMUXE_DIR=/path/to/installed/player node --test scripts/test_demuxe_bundle.mjs`
to verify installed files and exports. Set `REQUIRE_SHARED_RUNTIME=1` when qualifying
a package that must provide that API.

For Tailscale Serve, set `--public-origin https://your-server.your-tailnet.ts.net`
and configure Serve to forward to the loopback listener. The configured Host must
reach Axum unchanged. See [validation](VALIDATION.md) for the tested deployment.

## Managed deployment

See [OPERATIONS.md](OPERATIONS.md) for JSON configuration, macOS login-service
installation, readiness/diagnostics, bounded logs, retention, and SQLite backup/restore.

## Public API

The webpage uses the same API as other applications. The generated specification
is at `/api/v1/openapi.json`; [API.md](API.md) explains identity, import, and media
contracts with examples. Viewing needs no application login. Admin operations
require `Authorization: Bearer <local token>`. Profiles are selectable viewing
identities, not security boundaries. Browser mutations must be same-origin.

```sh
curl http://127.0.0.1:8787/api/v1/items
python3 scripts/client.py http://127.0.0.1:8787
```

## Next architecture and API v2

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
history, including proposals superseded by the new plan. `API.md` and the running
`/api/v1/openapi.json` describe the existing API. Importing the v2 contract does not
enable v2 endpoints or change application behavior.

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
Keep your admin token in Motion's data directory and enter it in the local UI.
Commit only example configuration with placeholders. `--check-config` output
contains your local paths; keep that output private.

Release packaging removes archive owner metadata, remaps Rust compiler paths,
and rejects private machine paths or tailnet hostnames in the assembled app.
Run `python3 scripts/check_publication.py` before publishing source; the CI notice
workflow also runs this check. Upstream license and author notices are retained.
