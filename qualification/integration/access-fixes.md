# Access and authentication integration fixes

Scope: PRs #36 and #38, based on authentication head `a69653a`.
The Catabolic lane is excluded. This receipt covers fixes, not a new live SSO
qualification or approval to enable a deployment.

## Review dispositions

- #36 policy editor truncation: fixed. Policy lists use unlimited JSON-list
  inputs, preserving all 200 maximum-size labels and literal commas, quotes and
  whitespace. Invalid JSON is refused. Unrelated name edits preserve restrictions.
- #36 access cache keys: fixed. Users, pending pairings and invitations use their
  own cache/error/loading/truncation slots and empty messages.
- #36 invitation timeout: fixed. Connection and redemption each use the shared
  timeout helper. Time spent entering the code does not consume the redemption
  budget; `--timeout 0` disables it.
- #36 invitation credential persistence: fixed. Before any redemption request,
  a private, synced recovery record binds the server and invitation digest to
  the exact random claim ID and device name. An atomic no-replace publication
  makes simultaneous processes use one complete claim. A retry after a failed
  credential write or process exit reuses that claim. Unsupported/unwritable
  storage fails before redemption. Records contain neither invitation nor token;
  successful records remain to prevent concurrent retries generating new claims.
- #36 revoked device ceilings: fixed. The core treats constraining a permanently
  revoked device as a no-op. The adapter only selects active devices for user
  ceiling updates, so obsolete maximum-size policies cannot prevent user edits.
- #38: no unresolved review threads at inspection. All five inherited #36 issues
  remained in the authentication tip and are repaired here. The existing OIDC
  and email walkthrough receipts are historical evidence, not rerun by this fix.

## Correctness and verification boundaries

Permanent revocation belongs to `crates/core::access_admin::constrain`. The
production-backed administrator model now checks that later user edits leave a
revoked device exactly unchanged. Focused core regression uses 200 old labels,
revision exhaustion, a new restriction and both enabled/disabled owners. A real
router/SQLite regression verifies that user edits succeed without changing the
revoked device. Filesystem claim publication and network timeouts are Go client
adapter responsibilities; tests exercise failed credential writes, restart,
concurrent claim creation, delayed input and disabled timeouts.

Validation so far:

- `go test ./internal/store ./internal/cli ./internal/tui`: passed, including
  final concurrent claim publication and CLI restart regression.
- Rust core/model compile queued through Kyoto with pinned Rust 1.98 and an
  isolated target. Core/model execution and real-router test remain pending;
  update this receipt with completed results before final qualification.
- No full application build; no live data or original worktrees modified.

## Migration reconciliation handoff

Keep access migration 45 and authentication migrations 46/47 unchanged in this
fix branch. They collide with completion history. Integration owns final versions
(access 57, authentication 58/59 are the proposed allocation).

Do not rename SQLx history rows on a running server or silently bypass checksum
validation. Root will provide an explicit offline reconciliation command that
writes a new backed-up database copy, recognizes exact known version/SHA384
lineages, remaps already-applied migrations to canonical versions, and then runs
missing migrations. Main-through-44 databases upgrade normally. Unknown versions,
checksums, partial migrations and contradictory mixed histories must be rejected.
Tests must prove preservation of users, owner associations, credentials, linked
identities, user restrictions and replay/revocation state for each supported
lineage. The source database remains unchanged; activation is a separate step.

## Heavy CI trace metadata follow-up

The access trace replay failure on the integrated branch was a provenance-field
encoding limit, not a failed access invariant. `access.rs` grew to 66,014 bytes;
embedding its whole text in `ModelMetadata.build` exceeded Stateless's default
65,536-byte per-string trace bound. Access and caller models now record a
`sha256:` fingerprint of the complete compile-time source (71 bytes). All model
versions, state/transition limits, inputs, checks and exact-replay expectations
are retained. No trace read/record limits are relaxed. Focused validation pending.
