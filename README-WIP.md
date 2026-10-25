# Recovered Motion + Catabolic checkpoint — WORK IN PROGRESS

Recovered source124 / Library v97 (archive checkpoint 20261024T0513), published
2026-10-25 to `recovery/source124-wip-20261025` in both Jagalite/motion and Jagalite/catabolic.
This is a source checkpoint, NOT a completed integration, release, installation,
or runtime qualification. No merge, deployment, or live migration is performed.

## Verification and unfinished work

The archive's historical record reports two Rust workspace all-target checks and
53 focused capture/paired-image/cache/custody/cancellation/expiry/regression tests
passing for source124. Those historical results are not new checks of this branch.
In the replacement workspace, 28 Python paired-local-build helper tests pass.
Rust is absent, so Rust builds/tests have not been rerun here; full CI is not
qualified. Go CLI/TUI and end-to-end acceptance are not claimed passing.
Durable paired95 reservation/exact-plan readiness/effects, image installation,
runtime activation/restart validation, and complete integration acceptance remain
unfinished. Do not use experimental builds against live data.

## Paired build and provenance

Keep the two repository checkouts side by side, both on this recovery branch.
Follow Motion's LOCAL_BUILD.md and scripts/paired_local_build.py for local source
overrides. Motion's existing Catabolic dependency pin remains
`6969f08c5d9b207afe8eec819555759be136644d`; it does NOT select this recovered
Catabolic work. No dependency pins or lockfiles were changed for publication.
Source presence and successful helper checks do not establish runtime readiness.

Existing main history is preserved as the parent; no existing branch is moved.
Generated caches, local helper backups, credentials, and build artifacts are not
added. Two Motion documentation home-directory prefixes are replaced with $HOME
for publication privacy; the original recovered archive remains unchanged.

## CI workflow preservation

All active .github/workflows files are retained byte-for-byte from remote main.
Motion's four divergent archived workflow files are preserved inertly under
`docs/recovery/source124-workflows/*.yml.txt`, not activated. They contain Rust
1.99 toolchain updates, runner-policy checking and additional source path filters.
The retained active Motion workflows still install Rust 1.98 whereas this source
requires Rust 1.99; CI/build success on the recovery branch is NOT asserted.
Workflow reconciliation and qualification are separate, unfinished work.
