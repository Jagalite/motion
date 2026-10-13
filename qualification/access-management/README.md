# Administrator access evidence

Implementation branch: `feat/tui-access`, based on `origin/main` `28f4b32`.
One implementation agent, with GPT-6.1 Sol read-only review. No live database or
credentials were used. No merge or deployment is authorized by this task.

## Correctness boundary

`crates/core/src/access_admin.rs` decides denial, grant ceilings, user-to-device
constraint changes, invitation claim/replay/revoke, usable administrators and
last-administrator preservation, and redemption admission. The existing access
core still decides authentication, credential generation, pairing and revisions.
Device principal IDs remain stable for playback, downloads and viewing outboxes.

SQLite adapters reserve the writer before observing authorization and current
revisions. User changes, all device constraints, credential deletion and dependent
ticket deletion commit atomically. Invitation consumption, device creation and
credential issuance commit together. A separate committed admission counter means
failed claims cannot roll back their attempt charge. The adapter supplies clock,
credential hashes, unpredictable secrets, known IDs and durable acknowledgements.
User ceilings are materialized monotonically to devices, with a fail-closed owner
check on credential and active-delivery observations. Ordinary reads remain local
queries; no global application state machine was added.

## Review dispositions

Early GPT-6.1 Sol source review identified:

- P1 legacy sibling ceiling bypass: explicit association now ends legacy mirroring;
  authentication also validates the current owner ceiling. Regression added.
- P2 invalid invitation revoke ETag: use the contract's strong revision spelling.
  TUI regression added.
- P2 administration selected by default: approval/invitation forms start with
  `system:admin` unchecked. Explicit-selection regression added.
- P2 truncated invitation codes on narrow terminals: wrap without dropping bytes.
  Width 20/40/60/100 regression added.

Sol review of `a2119ac` added two P2 findings, fixed in `9405c42`:

- Authorization withdrawal retained an access dialog: sign-out, verified admin
  loss and refused access now clear modal/caches and advance the epoch. Late
  issuance results cannot reveal a code. Regression passed.
- Generated legacy owner names exceeded 100 characters: truncate by Unicode
  character with an ellipsis; preserve the original device name. Migration probe
  includes long ASCII, Unicode and whitespace-boundary names.

Follow-up review confirmed these fixes and requested stronger positive model
outcomes, affected-device details, and user-change integration coverage. These
are added. It also caught an incorrect test expectation (download creation is
202, ticket creation 201), corrected before execution. Final diff review at `42b3eaa` found one P2: repeated user restriction changes
could accumulate more than 200 device blocked labels. Core now rejects that
proposal before any user/device update, with a clear API error. It never truncates
security restrictions. Boundary and transaction rollback tests are added;
focused Sol review at `057c55a` confirmed the fix and found no new issue.
There are no unresolved source findings. Execution of the new regression remains
pending with the Rust queue.
PR: https://github.com/Jagalite/motion/pull/36 (draft, not merged).

## Verification status

- Browser bridge: 18 tests passed, including retained invitation claim identity,
  no code in session storage, and terminal denial feedback.
- OpenAPI: YAML/JSON parity and validation passed (110 paths, 166 operations,
  173 schemas).
- Kyoto Go build `ecf86a2aa16b4e678a4ff88c87bd420b` and test compilation
  `b034431c6b5f4a5d877f5a72053f6bdb` completed. Separately executed API 23,
  store 2 and TUI 70 tests passed. CLI retry test compilation
  `466263e91c984ba9b62e98102d1f058b` completed; 14 CLI tests passed, including lost-response claim identity and private storage.
- Core model compilation `6efc6961a9a942d8aa14ee5b9f19c3d1` completed.
  Three separately executed tests passed. The finite graph exhausted 2,808 states
  and 50,544 transitions: two administrators and user disable/demotion/revocation,
  one pairing, one invitation, two claim identities, one target-user revision
  change, issue/replay/revoke/expiry/issuer loss. Independent expected outcomes
  check successful actions as well as refusals; exact issuance count/identity,
  replay identity, recipient binding, decision finality, stale revisions, user
  ceiling and last usable administrator are checked. Separate boundary tables
  exercise each grant dimension, credential presence/expiry and rate limits.
  Six existing access regression tests passed; the older four-million-state-budget
  graph was not rerun. This model does not exhaust arbitrary users/devices or
  SQLite/worker scheduling; the adapter tests cover those separately.
- Viewing writer 14 tests passed, including exact pending event identity retained
  in its original scoped archive on 401/403 before active state is cleared.
  Bridge/player/desktop outbox/download/recovery suites: 61 passed.
- `upgrade.py` runs the actual migration SQL against the preceding SQLite schema:
  five legacy devices retained unchanged IDs/grants/credentials and separate
  explicit owners; names meet the Unicode bound; zero foreign-key violations.
- Rust API/SQLite test compilation, strict lint, real TUI/browser walkthrough and
  final review are pending. Submitted work is not a passed gate. The initial
  Rust compile was pressure-deferred three times and cancelled in favor of a
  single-compiler job. No shared Kyoto policy was changed.

Release packaging, installed-app upgrades, native credential-store qualification,
public ingress abuse testing and cross-platform UI qualification are separate.
