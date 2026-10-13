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

Review of implementation commits and the final PR diff is pending.

## Verification status

- Browser bridge: 18 tests passed, including retained invitation claim identity,
  no code in session storage, and terminal denial feedback.
- OpenAPI: YAML/JSON parity and validation passed (110 paths, 166 operations,
  173 schemas).
- Kyoto Go test compilation job `335abcd85e904c699511841a5d24cb52` completed.
  API, CLI, store and terminal tests passed; TUI regression exposed a fixture
  initialization issue, now fixed with a bounded selection check, rerun pending.
- Core model: 648 states / 9,072 transitions, graph exhausted; exact issue count,
  replay identity, recipient binding, denial finality, stale revisions, user ceiling
  and last usable administrator checked. Two tests passed. Six existing access
  regression tests passed; the older four-million-state-budget graph was not rerun.
- API/SQLite compilation, strict lint, real TUI
  and browser walkthrough, final review and PR remain pending.
- `upgrade.py` runs the actual migration SQL against the preceding SQLite schema,
  passed: two legacy devices retained unchanged IDs/grants/credentials and separate
  explicit owners; zero foreign-key violations.

Release packaging, installed-app upgrades, native credential-store qualification,
public ingress abuse testing and cross-platform UI qualification are separate.
