# Build the paired Motion/Catabolic source snapshot

This is a **work-in-progress source handoff**, not a release, installer, or
full-integration qualification. Go CLI/TUI tests remain unrun pending approval to
install the required Go toolchain. Successful dependency preparation or Rust
compilation does not establish runtime, packaging, playback, migration, or
release readiness. Do not point experimental builds at your live data.

## Prerequisites and scope

- Keep the extracted `motion/` and `catabolic/` directories beside each other.
- Python **3.11+** runs the helper using only the standard library.
- Both `rust-toolchain.toml` files pin **Rust/Cargo 1.99.0**. Have that toolchain
  installed before running Cargo; rustup can otherwise attempt a download.
  The older Rust version in the main README is not the pin for this snapshot.
- Have a host C compiler/linker and the required platform build tools. Bundled
  SQLite and native crypto dependencies still compile native code. Checking the
  entire Catabolic workspace also includes its Python extension crate.
- Offline Cargo commands require all locked registry dependencies and the
  pinned Topcoat Git checkout in the local Cargo cache. Topcoat remains pinned
  to `341f3ff2fe16a73af5469685cf597693af5acb25` in Motion's two UI manifests.
  This handoff does **not** vendor it, verify its remote availability, or make
  a clean machine's dependency cache complete.
- Full playback and application packaging have additional FFmpeg/FFprobe,
  Node/npm/player and platform requirements described in the main README.
  The separate terminal app declares Go 1.26.0 with toolchain **go1.26.9**;
  these Rust commands do not build or test that app.

The source archive retains four original Catabolic Git entries for provenance.
The helper switches exactly those entries to the paired local crates and patched
`fancy-regex`, avoiding reliance on the unpublished Catabolic work being on a
remote. It never runs Cargo, downloads software, changes Catabolic, starts the
application, or accesses/migrates a data directory.

## Prepare and build (macOS/Linux shell)

Stop concurrent edits and Cargo commands in this pair first. From `motion/`:

```sh
python3 scripts/paired_local_build.py status
python3 scripts/paired_local_build.py prepare
cargo +1.99.0 metadata --offline --format-version 1 > /dev/null && \
  python3 scripts/paired_local_build.py verify-lock
python3 scripts/paired_local_build.py status
```

Stop if any command fails. Continue only when status says `localized-verified`.
The metadata step intentionally omits `--locked` once: Cargo must normalize the
original Git-based lock for the paired paths. It must retain all 572 locked
package names/versions, remove exactly four source fields and add only the
current source's `serde` edge on `catabolic-runtime` and `libc`/`rusqlite` edges
on `motion-catalog-backend`. `verify-lock` checks the complete parsed lock,
including every other source, checksum, package and dependency edge, before
recording its exact bytes for rollback. It does not prove a successful build.

An offline failure can mean the required toolchain or cached dependencies are
missing. Preserve the error and the lock; do not delete `Cargo.lock`, run
`cargo update`, change versions, or replace the patched regex with a registry
copy to bypass it. Obtaining missing toolchains/dependencies is a separate setup
step and may require network access. This helper does not do that for you.

After verification, from the common directory containing both repositories:

```sh
(cd catabolic && cargo +1.99.0 check --offline --locked --workspace --all-targets)
(cd motion && cargo +1.99.0 check --offline --locked --workspace --all-targets)
(cd motion && cargo +1.99.0 build --offline --locked -p playscale)
```

Run each command only after the preceding one succeeds. Execute Cargo **inside
each repository**, not merely with `--manifest-path` from their common parent:
the repository `.cargo/config.toml` sets the required SQLite feature flags.
These are compilation commands, not the full acceptance suite. Follow the
project's current qualification plan for runtime coverage; do not treat a
`cargo check` pass as a test pass.

## Safety and rollback

The helper is intentionally specific to these manifests and original lock.
It refuses customized manifests, different source pins, unexpected lock changes,
symlinked managed paths and incomplete backups. There is no `--force` option.
Default invocation is read-only `status`; only explicit `prepare`, `verify-lock`
and `restore` write files. Repeating a successful operation is safe.

Before changing the manifest, `prepare` saves byte-exact originals in
`motion/.paired-local-build/`. It leaves the lock untouched for Cargo to update.
`verify-lock` adds `Cargo.lock.local` only after validating expected normalization.
To restore the original remote-pinned manifest and original lock, from `motion/`:

```sh
python3 scripts/paired_local_build.py restore
```

Restore does not delete backups. It refuses to overwrite a manifest changed
outside the helper or a lock whose current bytes are neither original nor
previously verified. After normalization, run `verify-lock` before rollback only
if the output is the expected Cargo result. You can restore immediately after
`prepare` if Cargo has not changed the lock. Restoring original pins does not
make the unpublished Catabolic source available remotely.

If interrupted or refused, preserve the current files and the complete backup
directory before manual recovery. Inspect/diff them; copy the original backups
back only after saving any custom changes separately. A fresh extraction is the
simplest recovery when uncertain. File replacements are individually atomic,
not one cross-file/power-loss transaction; a partially completed restore can be
retried when all remaining files are still recognized. Incomplete backup sets
are never adopted automatically.

The `.paired-local-build.guard` file prevents simultaneous helper invocations,
but cannot prevent editors or independent Cargo commands from changing files.
If a killed helper leaves this guard, verify no helper is still running before
preserving/removing the stale guard and retrying. Do not publish the backup
folder or commit the local path/lock conversion as a remote-pin update without
separate review.

## Helper-only tests

```sh
# From motion/. Uses temporary copies, not the source manifests or lock.
PYTHONDONTWRITEBYTECODE=1 python3 scripts/test_paired_local_build.py
```

The finite Python tests cover preparation/rollback idempotence, exact edits,
normalization constraints, custom-file refusal, symlinks, incomplete backups,
operation guards and simulated write/restore interruptions. They invoke no
Cargo, Go, network, application process, or migration. Passing these tests is
only evidence about this helper, not the paired application.
