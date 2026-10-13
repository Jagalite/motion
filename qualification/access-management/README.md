# Administrator access evidence

Implementation branch: `feat/tui-access`, based on `origin/main` `28f4b32`.
One implementation agent, with GPT-6.1 Sol read-only review. No live database or
credentials were used. Application source head: `b183d54`. Core/API source is unchanged from the fully
passing `6980c60` CI run. Interactive testing found and fixed terminal focus and
browser feedback defects; refreshed presentation artifacts are pending below. No merge or deployment is authorized by this task.

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
Sol final diff review at `e476bcd` found no new issue; all earlier source
findings were resolved. The overflow regression subsequently passed in CI.
Self-review additionally pinned operator-created invitations to the current
operator token hash. Sol confirmed the adapter observation and existing core
issuer rule align; token rotation invalidates unused invitations and claim
retries while leaving already issued device credentials independent. The regression
passed at `6980c60`.

CI at `057c55a` exposed a Topcoat boolean-attribute syntax error and a model lint;
`9ba2e03` fixed both. That head compiled past the UI and exposed a pairing-row
Clippy type-complexity warning; the row is now a named adapter struct. `e476bcd`
fixed five temporary-header lifetime errors in concurrent request tests. Earlier
failed lanes stopped at compilation and are not runtime evidence.

The first Rust execution at `e476bcd` passed 40 access API tests and failed 11:
ten new tests used invalid short/whitespace idempotency keys, and a textual YAML
path count missed quoted keys. `add746d` fixes these fixtures without relaxing
production validation and retains full Ruby-parsed YAML/JSON parity. Focused Sol
review found no issue. The rerun at `add746d` passed 50 of 51 access API tests.
The remaining test exposed a production acknowledgement bug: legacy approval
persisted a generated user but serialized the pre-insert row with `user_id: null`.
`6980c60` reloads that device inside the same transaction before publishing the
approval and durable receipt. The regression compares approval, exact retry and
fresh device GET. This is an adapter acknowledgement correction; core grant and
lifecycle decisions are unchanged. Focused Sol review found no issue; the final
runtime rerun passed all 51 access API tests at `6980c60`.

PR: https://github.com/Jagalite/motion/pull/36 (draft, not merged).

## Verification status

- Browser bridge: 19 tests passed, including retained invitation claim identity,
  no code in session storage, and terminal denial feedback.
- OpenAPI: YAML/JSON parity and validation passed (110 paths, 166 operations,
  173 schemas).
- Kyoto Go build `ecf86a2aa16b4e678a4ff88c87bd420b` and test compilation
  `b034431c6b5f4a5d877f5a72053f6bdb` completed. Separately executed API 23,
  store 2 and TUI 70 tests passed. CLI retry test compilation
  `466263e91c984ba9b62e98102d1f058b` completed; 14 CLI tests passed, including lost-response claim identity and private storage.
- Pre-walkthrough terminal refresh `1ee33c0ab0354b4182ff1d0592de1bc8` completed
  through Kyoto. Its SHA-256 is
  `d9acba9809fdc85756b1a4e326e0e84e83e9a251492fa24008e4d2ed0f662ba6`.
- Core model compilation `6efc6961a9a942d8aa14ee5b9f19c3d1` completed.
  Three separately executed tests passed; CI at `e476bcd` passed all four final
  tests, including the added restriction-overflow boundary. The finite graph exhausted 2,808 states
  and 50,544 transitions: two administrators and user disable/demotion/revocation,
  one pairing, one invitation, two claim identities, one target-user revision
  change, issue/replay/revoke/expiry/issuer loss. Independent expected outcomes
  check successful actions as well as refusals; exact issuance count/identity,
  replay identity, recipient binding, decision finality, stale revisions, user
  ceiling and last usable administrator are checked. Separate boundary tables
  exercise each grant dimension, credential presence/expiry and rate limits.
  Six existing access regression tests passed; the older four-million-state-budget
  graph was not rerun locally. This model does not exhaust arbitrary users/devices or
  SQLite/worker scheduling; the adapter tests cover those separately.
- Viewing writer 14 tests passed, including exact pending event identity retained
  in its original scoped archive on 401/403 before active state is cleared.
  Bridge/player/desktop outbox/download/recovery suites: 61 passed.
- `upgrade.py` runs the actual migration SQL against the preceding SQLite schema:
  five legacy devices retained unchanged IDs/grants/credentials and separate
  explicit owners; names meet the Unicode bound; zero foreign-key violations.
- CI at `e476bcd` passed workspace formatting/strict Clippy, all four administrator
  core tests, all five download tests, and real-process integration/upgrade/restore.
  The access API target failed as recorded above. Client CI passed 104 JavaScript
  regressions and all five Go packages. These results are bound to that head;
  final-head fast CI at `6980c60` passed as recorded below.
- Kyoto's local multi-target Rust compilation was pressure-deferred six times.
  Its duplicate lint/model/API jobs were cancelled after CI supplied compilation,
  lint/model/download evidence. Server-only job `f6e3d44847a54102b8d5c36a257e20a0`
  was pressure-deferred and cancelled; replacement job
  `28646ba89c4641f5b80ac3a1e67568be` was pressure-deferred four times and
  cancelled after the verified CI server became available. It did not pass.
  No shared Kyoto policy was changed and no other owner's jobs were cancelled.
- Fast CI for the core/API source [38338373941](https://github.com/Jagalite/motion/actions/runs/38338373941)
  at `6980c60` passed: workspace formatting/strict Clippy, 51/51 access API tests
  (all 13 new administrator tests), 4/4 administrator core tests, and 5/5 download
  tests, with none ignored in these targets. Contract and client lanes also passed.
- Real-process integration/upgrade/rollback/restore passed at `6980c60` in full CI
  [38338374318](https://github.com/Jagalite/motion/actions/runs/38338374318).
  Heavy Stateless/live-FFmpeg tests and packaging also passed: 59 heavy tests
  (including both new live-conversion user-change tests), none ignored.
  Original and reconstructed packages each passed their 13 runtime checks.
  These package receipts explicitly exclude browser qualification.
- Live-conversion user changes passed in the 28-test `v2_playback` CI target.
  The local real TUI/browser walkthrough is recorded below. A rebuilt server
  embedding the final feedback text is queued as Kyoto job
  `970b895cd7f6468e8e122a0d4e14aa86`. A terminal refresh with corrected list
  titles is queued as `08fdacd250e54ac19732cb34319ed0ad`.

## Interactive walkthrough and follow-up fixes

The server came from CI artifact `11787381501` at clean source `6980c60`.
Its archive SHA-256 is
`821562602a0383a308fbdadba453efda1eb5a673dbfab85bd0ffca62c5ca7887`;
the extracted server binary is
`c1b25a94076ba0f2aab1eb8b3695142ed8dc7572ddf5c4b6f6a32a1a97791560`.
The fixture uses restricted mode, a new database and private configuration under
`/Volumes/seed1/Projects/motion-tui-access-artifacts`, with no live settings.
A normal paired administrator performs all access changes through the actual
terminal in a PTY. The protected operator file was used only to bootstrap that
fixture administrator. Browser actions use the T3 collaborative browser.

The first real form navigation panicked when a checkbox's zero-value text input
was focused. `e804b87` limits text input focus/update to text fields and returns
cursor commands. This is presentation state in the terminal adapter; the access
core and API decisions did not change. Sol reviewed the fix with no finding.
Kyoto jobs `ddd6949f8c5c49449778c19d059d2483`,
`97b4a7f1c7de4ce993cbc34a8200113e` and
`5344ad8c04b74cadbeda788355aa000d` completed the terminal and test builds.
All 71 TUI tests passed, including constructed user/approval/invitation forms,
text entry, checkboxes, user selection, forward/reverse navigation and cancel.
All four terminal-root tests passed with a real server, including both E2E tests.
The latter initially exposed a stale baseline v1 expectation; the test now
requires the retired route's 404 and separately checks anonymous v2 refusal (401).

The corrected interactive terminal at `e804b87` has SHA-256
`8d30b939057bd8049eeb44136260c91408b72a9ef8d270ba3206b01a838c4248`.
Observed through its real keyboard flow and browser UI:

1. Created a named household user, selected Everyone and ordinary permissions,
   with administration unchecked.
2. Compared the browser's displayed code with the pending TUI request and
   approved it for that named user. Browser reached Home and `/me` returned 200.
3. Issued an invitation, stopped and restarted the real server, then redeemed it
   using the terminal CLI. The sibling credential file was mode 0600; `/me` was 200.
4. Removed `events:read` from the user. Both browser and terminal reported exactly
   catalog/read, viewing/write and playback/request permissions.
5. Revoked only the original browser device. Browser returned to sign-in (401);
   the sibling terminal remained authorized (200).
6. Denied a second browser request. Polling stopped with a refusal.
7. Issued and revoked an unused invitation; browser redemption was refused.
   A separate fresh invitation successfully paired the browser with the narrowed
   permissions.
8. Disabled the user after a confirmation naming all three owned devices and
   their current statuses. Both active recipients returned 401; device history
   remained visible with revoked status.
9. Attempted to disable the last usable administrator. The TUI showed the
   `last_administrator` refusal and the administrator remained authorized (200).

This walkthrough exposed generic conflict text for denial and unavailable
invitations, plus three incorrectly named list panels. `b183d54` corrects these
presentation issues. Bridge tests now assert the actual denial, expiry and
unavailable-invitation text as well as polling termination and secret-storage
exclusion (19/19 passed). Sol found no correctness issue in these fixes.
Fast CI [38343775889](https://github.com/Jagalite/motion/actions/runs/38343775889)
at `b183d54` passed: formatting/strict Clippy, 51 access API tests, 4 administrator
core tests, 5 download tests, 105 JavaScript tests, Go client suites and contract.
The refreshed compiled presentation check remains pending; full CI
[38343776222](https://github.com/Jagalite/motion/actions/runs/38343776222) is running. Server/API/core source is unchanged, while the bridge is embedded into
server binaries and therefore requires a new artifact for that final check.

Release packaging, installed-app upgrades, native credential-store qualification,
public ingress abuse testing and cross-platform UI qualification are separate.

The [walkthrough receipt](walkthrough/receipt.json), [final API state](walkthrough/final-state.json),
terminal snapshots and browser screenshots are retained in `walkthrough/`.
They contain disposable fixture identities, not credentials or invitation codes.
