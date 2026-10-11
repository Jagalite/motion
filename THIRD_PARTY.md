# Third-party software

The root MIT license covers Playscale's original code. It does not replace the
licenses of dependencies, installed player assets, external tools, or their source distributions.

## Rust dependencies

Cargo.toml and Cargo.lock declare the dependencies and exact resolved versions.
`THIRD_PARTY_NOTICES.txt` collects their packaged license and attribution texts
for `aarch64-apple-darwin`, including build and test dependencies conservatively.
Identical texts are included once with references from each package. Include this
file with a matching macOS distribution along with the root LICENSE.

After changing Cargo dependencies, regenerate and check the file:

```sh
python3 scripts/third_party_notices.py
python3 scripts/third_party_notices.py --check
```

Use `--offline` when the required packages are already cached. GitHub Actions runs
the check on pushes and pull requests. Missing license declarations, missing or
empty license files, and a stale generated file fail the check. Cargo runs with
`--locked`; the generator records the lockfile hash and never updates dependencies.

For another target, generate notices alongside that target's release artifacts:

```sh
python3 scripts/third_party_notices.py --target x86_64-unknown-linux-gnu \
  --output artifacts/linux/THIRD_PARTY_NOTICES.txt
```

The generator uses Cargo's default feature selection. Changes to release features
require matching notice collection. It preserves discovered packaged materials;
it does not certify that an upstream author included every required notice or
fulfill source-offer obligations for separately bundled tools/providers.

The model tests use `statelessness` 0.2.0 (imported as `stateless`), licensed under
MIT. Its registry archive includes LICENSE and excludes the upstream experiments.
The upstream copyright notice is retained in THIRD_PARTY_NOTICES.txt.
See vendor/STATELESS.md.

## Demuxe

The one-app package uses the exact Apache-2.0 provider-independent core candidate
vendored under `vendor/demuxe`. It is distinct from the published npm 1.1.1 bytes.
Its archive, original build inventory, all recorded corresponding source inputs,
and license are retained. See that directory's README for provenance limits.

Motion compiles an Apache-2.0 Shaka 5.2.11 derivative from checksum-pinned upstream
source, applying `vendor/shaka/motion-paced.patch`. The application retains the
license, modification notice, patch, new asset identity and deployment receipt.
The source companion includes the upstream archive and complete Motion recipe.
Per-player ownership is selected explicitly; shared component runtime injection
is not supplied by this core candidate. Product qualification is separate from
source licensing and is recorded in the consolidation evidence ledger.

## Electron and Go/TUI

Electron 44.7.0 is checksum-pinned in `packaging.lock.json`. Its MIT license and
Chromium third-party notices are included inside the app. The Go runtime license
and license/notice files for every linked Go module are also installed there,
with module versions recorded. The terminal source, go.mod and go.sum are in the
Motion source archive. Local ad-hoc signing is not Developer ID notarization.

## FFmpeg and FFprobe

Motion invokes FFmpeg/FFprobe as separate executables. The macOS package includes
FFmpeg 8.0 built with statically linked x264 and only system dynamic libraries.
That build enables GPL components; the binaries retain FFmpeg/x264's GPL terms.
Motion's original Rust code remains MIT. Source-mode users may instead supply
separately installed executables.

The package contains media-tool license files; its source companion contains the
unmodified source archives, exact configure/build receipt, and build script.
Distribute `Motion-sources.tar` alongside `Motion-macos-arm64.tar.gz`. Source
and binary hashes are recorded in the package manifest and SHA256SUMS. Replacing
these binaries or changing build options requires regenerating and reviewing those
materials. FFmpeg's upstream guidance: https://ffmpeg.org/legal.html.

## License references

- MIT: https://opensource.org/license/mit
- Apache-2.0: https://www.apache.org/licenses/LICENSE-2.0
- Stateless: https://crates.io/crates/statelessness/0.2.0

This document identifies the integration boundaries. It is not qualification of
every future binary, codec bundle, or historical Git revision.
