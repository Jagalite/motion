# Authentication sign-in implementation ledger

Implementation now covers ownership, browser OIDC/email authentication,
Authentication & Access administration, self-service methods, freshness proofs,
and restore fencing. **Runtime qualification is in progress; this is not a
completed feature or release qualification.** Methods default to disabled.

## Current implementation and qualification

The branch integrates the committed shared access API/TUI primitives at `e804b87`
through merge `a70be30`. Migration 0045 remains unchanged; forward migrations
0046 and 0047 reconcile legacy ownership and add sign-in persistence. Existing
viewing and playback ownership remains device-scoped.

Production core policy owns grant intersection, linking ceilings, transaction
purpose/revision/browser/epoch checks, one-time consumption and freshness.
Adapters own verified OIDC observations, encrypted secret storage, bounded mail
transport and SQLite atomic issuance. Reserving an OIDC exchange commits before
network I/O; final identity/session issuance consumes that reservation atomically.
An interrupted exchange fails closed and requires a new login.

Completed checks on the implementation draft:

- Browser bridge tests: 16 passed, including secret handling, stale revisions,
  self-service pagination and explicit proof resubmission.
- API contract validation: 142 paths, 201 operations, 213 schemas.
- Isolated SQLite ownership-upgrade probe: fresh and managed legacy cases passed,
  preserving credentials, invitations and effective grants with no foreign-key errors.
- Kyoto dependency resolution and isolated local OIDC fixture installation passed.
- First native compile (`a6e6a8cf968a4071826749cf03a6d3dd`, source snapshot 01)
  failed on a UI route macro parameter name; corrected in source snapshot 02.
- Kyoto snapshot-02 compile `da715b6363544f1ba6aff33c5e374173` was deferred
  four times under memory/swap/disk pressure, then cancelled before completion.
  Snapshot-03 compile `daed411bbc5443e1ba34b2bcba217e72` targets core models,
  server/UI libraries, access/sign-in/protocol APIs, contracts and playback.
  TUI/API Go regression job `9ad5620dc3f54d89b235171c7bf435d7` passed both
  `internal/api` and `internal/tui` packages on snapshot 03. Those Go sources
  are unchanged in implementation commit `04b1a8f`.
  Compilation and test execution remain distinct gates; no native runtime pass
  is claimed. Snapshot 03 was deferred under pressure and superseded after review
  fixes; a new final snapshot is required.
- Dependency notices regenerated and verified; six notice-generator tests passed.
  The defmt-parser archive omits licenses, so exact upstream-revision texts and
  their provenance are preserved under `third_party/licenses`.
- Local OIDC discovery was reached in the collaborative browser; the loopback
  SMTP sink started. This is fixture readiness, not Motion login qualification.
  Controlled HTTP/JWT claim/key-rotation tests and API mail-outage, network-limit
  and secret-retention tests are added but have not executed.

GPT-6.1 Sol source review found four issues, all corrected and accepted in focused
follow-up review: restricted-device linking could inherit the owner's broader
rights; device-policy admin elevation lacked fresh proof; pending enrollment
could outlive its initiating fresh proof; and re-enrollment of a disabled owned
email identity was rejected. Linking now intersects initiating-device, user and
existing-method ceilings; admin elevation requires action-bound freshness;
transactions capture actor revisions and the original proof deadline; same-owner
email re-enrollment is an explicit core exception. Adapter regressions cover
these boundaries. GPT-6.1 Sol reviewed implementation commit `04b1a8f` and
`origin/main...HEAD`. Three additional findings were resolved and accepted in
focused review: the returned approval code's contract annotation, exact encryption
key/AAD documentation, and session expiry measured from verified authentication
rather than later local issuance. The latter now uses production core policy and
one deadline for device, parent, session, family and cookie. Final qualification
evidence review remains pending.

The local fixtures use a genuine `oidc-provider` service and a loopback SMTP sink.
No external provider, real mailbox or live database is used. Remaining gates are
native execution, protocol failure/key-rotation tests, browser administration and
sign-in/playback walkthroughs, mail retry/outage, cross-client checks and final
review. The sections below retain the earlier caller-only checkpoint evidence;
their limitations describe that historical checkpoint, not the current source.

## Historical A1 checkpoint: explicit caller authority

Base: `origin/main` at `28f4b32c0df0c31d636488fdb524c23549c89bdc`.
Worktree: `motion-auth-signin` under the seed1 volume's `Projects` directory.

Request callers now carry one private, explicit source: proven local operator,
stored credential, or verified ingress device. Missing credential/ingress fields
can no longer select operator authority. Transactional writes and SSE disclosures
use the same reauthentication method. It reloads the current device/parent rows
and rejects replacement of a credential's immutable admitted identity.

The production core receives explicit `Authentication` observations and decides
credential/parent validity, ingress-device identity, grants and revocation.
Protocol/header proof, secret lookup and SQLite reads stay in the adapter. The
source type is neither client-deserializable nor constructible outside the auth
module. No global state model or new identity store is introduced.

The HTTP contract and database schema are unchanged. Existing credentials, cookie
flags, CSRF and origin rules remain compatible. No migration is required for this
checkpoint. Durable jobs/tickets keep their existing encoding: their explicit
requester/principal ID is still required to identify operator work when no
credential/ingress reference exists. Existing content-ticket issuance now also
uses the explicit caller source for its operator exemption.

## Verification

Builds use Kyoto with `RUSTC` pinned to the 1.98.0 toolchain and isolated target
`motion-auth-signin-target` under the same seed1 `Projects` directory. Receipts and raw logs belong in
`motion-auth-signin-artifacts` alongside that target; all database fixtures are
temporary and isolated from live state.

- Formatting and `git diff --check`: passed.
- Initial CI on `3660792` failed publication privacy because this ledger used
  absolute machine paths; portable locations fixed that in `a838d25` and the
  local publication check passed.
- The same initial CI failed to compile `caller_model`: its `Transition` struct
  initializer omitted `disposition`. Commit `558bdee` uses the existing
  `Transition::accepted` constructor. It compiled in the CI run recorded below;
  local compilation remains pending. Source review did not establish compilation.
- Sol focused review of `558bdee`: no code findings. Its requested ledger
  correction (record the initial CI failures and pending rerun) is included here.
- Kyoto compilation job `c74625e7ab664a35bb00bce08399415b`: submitted; not yet a
  compilation or execution pass. Last observation: still queued; shared Kyoto
  admission reported OS memory pressure. Do not bypass the queue.
- Requested targets: `playscale-core --test caller_model` and
  `playscale --test access_api`, `cargo test --locked --no-run --jobs 2`.
- Local test binaries have not yet executed. Queue policy requires `--no-run`.
- CI run `38341814710` on `558bdee` passed formatting, Clippy, the new caller
  Stateless model (asserted 192 states and graph exhaustion), and 40/41 access
  API tests. The remaining new operator SSE test used invalid fixture event kind
  `updated`; SQLite requires `changed` or `deleted`. This checkpoint corrects the
  fixture to `changed` in `3a7452a`; the complete access API rerun remains pending.
- CI contract, client-controller and publication checks passed on `558bdee`.
  These results are separate from the still-queued local Kyoto build.
- The new Stateless model checks exact principals/grants, rejection outcomes and
  effect counts after deletion, revocation, generation rotation, policy downgrade
  and expiry, with explicit operator, device, session, ingress and wrong-device
  ingress observations. Declared bounds: one device/parent/session chain, time
  0/99/100, at most 1,000 states, 20,000 transitions and depth 20. Actual explored
  transition count was not printed by CI; the passing test asserts 192 states,
  graph exhaustion and no skipped checks. It does not model unimplemented SSO
  transactions.
- New SQLite/API tests cover missing/malformed proof, stale cached principal,
  changed/deleted credential rows, operator continuity, session parent loss,
  ingress revocation and permission changes across transactional writes and SSE.
- GPT-6.1 Sol reviewed implementation commit `36607923ff3aeb235872694ad84c248e89032637`
  and the full `origin/main...HEAD` diff: no actionable findings. Review covered
  caller provenance, core dispatch, write/SSE reauthentication, credential
  replacement/revocation, issuance, durable compatibility and regression tests.
  This was source review, separate from the partial CI execution recorded above.
- Sol final implementation/PR diff review at `3a7452a`: no code findings. Its
  requested status wording correction is included here; no builds were run by
  the reviewer.
- Draft PR: https://github.com/Jagalite/motion/pull/38 (linked to this thread).

No live browser, native/TUI walkthrough, provider, mail delivery, restore flow,
active playback qualification or production migration has been performed here.
The existing access API suite includes pairing/cookies/tickets and ingress checks;
those must not be described as complete SSO or playback qualification.

## Shared TUI seam and next checkpoint

Read-only inventory of `motion-tui-access` at
`6980c6007a58f0654e7bbd0518c212500c56ad14` found an existing `access_admin::User`,
`access_users`, `devices.user_id`, invitation lifecycle, user/pairing/invitation
APIs and migration `0045_access_management.sql`. Reuse and reconcile those
primitives; do not independently allocate migration 0045 or create another users
or invitations system. Recheck the branch/main state before integration.

Two differences require deliberate reconciliation against the sign-in plan:

1. TUI migration 0045 assigns a synthetic user to every legacy device; its loaders
   expect ownership. This plan requires legacy devices to remain visibly
   unassigned. Keep legacy authority intact and use a forward migration if 0045
   has landed/applied; never rewrite an applied migration.
2. TUI `constrain` represents disjoint rating allowlists by clearing libraries and
   replaces an admin device's policy with a restricted user's policy. The sign-in
   plan needs explicit unrestricted/empty rated eligibility and independent
   restrictions on both sides of the effective grant.

Its invitation adapter also needs the new `Caller::credential()` accessor when
integrated. Existing durable worker readers already guard absent authority with
an explicit operator requester ID. Do not infer users from viewing profiles.

Next A1 work: reconcile shared ownership/effective grants, add method/session
lineage and restore fencing, then update contracts/migrations and verify API,
SSE and delivery user revocation. A2's dedicated administrator screen remains a
required feature phase. This checkpoint must not be represented as completed A1.
