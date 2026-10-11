# Historical local Demuxe core input

Current builds fetch the published npm archive pinned in `packaging.lock.json`.
The files here are retained as provenance for older qualification receipts; they
are no longer inputs to `scripts/build_motion_provider.py`.

The following describes that historical candidate.

This is the Apache-2.0, provider-independent core candidate assembled by upstream
Demuxe on 2026-10-08. It is **not** the npm or GitHub release named 1.1.1. Its
archive SHA-256 is `4c5ef3bc8ee8413406590545cbbdef6249bd6fb4ce96e6ce8338da1a22b51274`.
The published release has different bytes and is not interchangeable.

The 444 KiB archive is vendored so Motion builds do not depend on an unpublished
archive on a developer's machine. `build-inventory.json` is its original per-file
and TypeScript source provenance. `corresponding-sources.tar.gz` contains all 236
recorded source/build inputs, each checked against that inventory. Six older
inputs were recovered by content hash from upstream Git history; no current
upstream worktree was changed. This is a matching-source companion, not a claim
that these selected files alone form a complete upstream development checkout.
The archive also includes its readable JavaScript runtime and Apache license.

Motion uses explicit per-player ownership for this candidate. It does not expose
the later component runtime-injection contract. Shared-runtime tests must not be
marked passed or skipped without recording this selected ownership policy.

The previous `scripts/build_motion_provider.py` extracted this exact core, compiled the patched
Shaka source, and records a new asset closure/runtime identity. It modifies the
Shaka backend configuration and provider inventory explicitly. The generated
receipt remains `candidate` until separate product qualification succeeds.
