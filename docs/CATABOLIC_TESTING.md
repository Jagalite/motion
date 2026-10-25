# Reproducible native integration tests

The native observer registers the **actual application executable** used by the
integration test, not the Rust test harness. Its 128 MiB registration limit is an
intentional bound. Do not raise the limit, register an arbitrary replacement
binary, or silently skip the native process fixtures to make a debug build pass.

## Complete workspace run

Install the repository's pinned Rust toolchain, approved FFmpeg/ffprobe tools,
and Ruby with its standard YAML and JSON libraries. The reviewed contract parity
test invokes Ruby; the full runner checks these prerequisites before compiling.
Use an otherwise immutable checkout, with the published native dependency commit
and matching Cargo.lock. From the repository root:

```sh
python3 scripts/ci/test_workspace.py
```

The runner:

1. Builds every workspace test with `cargo test --locked --profile native-qualification
   --workspace --no-fail-fast --no-run --message-format=json-render-diagnostics`.
   This uses the same workspace selection and unified features as execution,
   not a separate `cargo build --bin` that could produce a different binary.
2. Obtains the actual non-harness `playscale` executable path from Cargo's artifact output (including custom
   target directories), verifies it is a nonempty regular file at most 128 MiB,
   and records its size and SHA-256.
3. Runs `cargo test --locked --profile native-qualification --workspace --no-fail-fast`
   without target/name filters. This covers workspace library, binary,
   integration and documentation tests, both legacy and native.

`python3 scripts/ci/test_workspace.py --print` prints the planned commands without
building. `python3 scripts/ci/test_workspace_runner.py` exercises the runner's
bound/digest and full-suite command policy without invoking Cargo.

The opt-in Cargo profile inherits development semantics, keeps debug assertions
and overflow checks, disables debug info, and strips symbols at link time. It
does not change normal `cargo build` or `cargo test` defaults. No manual mutation
of an already-built executable is needed. Profile overrides that produce an
oversized helper fail the explicit check rather than weaken registration.

## Existing tiered CI

`scripts/ci/test_tier.py fast` and `full` both select the same explicit profile.
Together they cover the workspace; the full-workspace runner is the unfiltered
qualification invocation. CI keeps long-running model/delivery tests separate
for scheduling, not as an exemption from eventual qualification.

A full run may take substantially longer than the focused native fixtures.
Record all failures, ignored tests and platform skips. Classify intended API
changes separately from regressions; do not silently rewrite legacy fixtures or
claim a selected native subset is the full suite. Native external-processing
proofs remain separately gated and are not established by this Motion runner.

## Current source handoff limitation

The integration worktree currently depends on unpublished native changes; its
upstream git revision/lock still require an authorized publication and pin
update. Until then, validation uses immutable paired worktrees with local
path overrides. The runner/profile are implemented, but a clean public checkout
is not reproducible merely because a paired local validation succeeds. Record
both source identities and do not describe the dependency publication as done.

## Exclusive full-suite target and model runtime

A full unfiltered run must exclusively own its Cargo target directory through
all runtime targets and doctests. Do not compile another source snapshot into
that target while emitted tests are still running: library metadata can change
under the later doctest phase even when previously emitted test binaries remain
intact. Use a separate target directory for concurrent focused validation.

The runner defaults `RUST_TEST_THREADS` to `1` and logs the effective value.
This avoids concurrent expensive catalog transactions and model explorations
inside each test binary without skipping tests, increasing native operation
budgets, or reducing model bounds. An explicit environment override is preserved
and must be recorded with results. Exclusive target ownership does not by itself
prevent resource contention from other targets or unrelated compilation; avoid
those while qualifying timing-sensitive native transactions.

The explicit qualification profile optimizes `playscale-core`, `serde_json`,
`serde`, and `serde_core` at level 2, with debug assertions and overflow checks
explicitly enabled for each package. The core override applies to its integration
test targets as well as its library: test-local codec and explorer generic
instantiations need optimization alongside out-of-line serialization code.
Ordinary development defaults and the rest of the server/native dependency graph
are unchanged. No model source, state space, transition order, bound, check, or
mandatory independent successor roundtrip is removed by this profile.

Two measured runs of the same four compact-model proof tests passed with exactly
10,408 prefix states, 39,248 edges, 39,249 independent roundtrips, and at most two
cached decoded states. Cached prefix exploration took 2.678 and 2.459 seconds,
versus the earlier 6.027-second level-1 result. These are prefix measurements,
not a full-graph completion claim or an estimated completion time. Full acceptance
requires the unchanged 12,000,000-state / 200,000,000-transition / depth-100
exploration to report `GraphExhausted`, with elapsed time and peak RSS recorded.
Interrupted or OS-killed exploration is inconclusive, never a passing model or
permission to reduce its bounds.

### Full bounded model result (2026-10-21)

The original opt2 compact adapter with its two-entry decoded-state cache and
Value-based decoder completed the unchanged full exploration with
`GraphExhausted`: **4,765,164 states and 76,242,624 transitions**. All ten tests in
that standalone artifact passed in **9,979.62 seconds** (wrapper wall time
9,984.0005 seconds), with peak child RSS **3,288,612 KiB** (about 3.14 GiB).
The limits remained 12,000,000 states, 200,000,000 transitions and depth 100;
all model checks and independent successor roundtrips remained enabled.
The qualified artifact SHA-256 is
`1a69de0aa92a2d45cbdd2cc0bf74e37878b7f3d32a3421be40610c01cd55e827`.
The recorded evidence is `motion-v27p7-opt2-downloads.log` and
`motion-v27p7-opt2-downloads-status.json` (exit code 0).

This result belongs to the original Value-decoder artifact, not the newer
streaming decoder candidate. The newer candidate has twice passed its five
prefix/malformed-input proofs: exactly 10,408 states, 39,248 edges, 39,249
independent roundtrips, cache maximum two and 15,072 malformed-input parity
cases. Its same-run prefix performance improved, but it has **not** completed
full-graph qualification. An independent standalone full streaming-candidate run
started only after the Value-decoder run completed; its result is pending.
Do not transfer the original artifact's full result
to the newer decoder or describe earlier interrupted runs as passing.

## In-crate native observer fixtures

After the identical workspace preflight identifies and verifies the actual
non-harness `playscale` binary, the runner sets `MOTION_TEST_OBSERVER` to that
canonical artifact for the test phase. An inherited value is replaced, never
trusted instead of Cargo's emitted artifact. In-crate native recovery tests use
this explicit verified helper because they do not receive Cargo's integration
`CARGO_BIN_EXE_playscale` binding. Missing helper configuration is an explicit
failure, not a skipped test or fallback to the test harness. Direct focused
invocations must first verify the same real packaged helper and supply this
variable; preserve the recorded byte count/hash and exclusive target ownership.
