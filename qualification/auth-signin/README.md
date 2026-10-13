# Authentication sign-in implementation ledger

This branch starts `MOTION_SSO_AND_EMAIL_AUTH_PLAN.md`. A1 is **in progress**;
A2–A5 are not implemented here. OIDC/email login and the Authentication & Access
screen are not advertised or enabled by this change.

## A1 checkpoint: explicit caller authority

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
  `Transition::accepted` constructor. The CI rerun and local compile remain
  pending; initial source review did not establish compilation.
- Sol focused review of `558bdee`: no code findings. Its requested ledger
  correction (record the initial CI failures and pending rerun) is included here.
- Kyoto compilation job `c74625e7ab664a35bb00bce08399415b`: submitted; not yet a
  compilation or execution pass.
- Requested targets: `playscale-core --test caller_model` and
  `playscale --test access_api`, `cargo test --locked --no-run --jobs 2`.
- Test binaries have not yet executed. Queue policy requires `--no-run`; record
  their separate executions below before calling this checkpoint tested.
- The new Stateless model checks exact principals/grants, rejection outcomes and
  effect counts after deletion, revocation, generation rotation, policy downgrade
  and expiry, with explicit operator, device, session, ingress and wrong-device
  ingress observations. Declared bounds: one device/parent/session chain, time
  0/99/100, at most 1,000 states, 20,000 transitions and depth 20. Actual explored
  counts are pending execution. It does not model unimplemented SSO transactions.
- New SQLite/API tests cover missing/malformed proof, stale cached principal,
  changed/deleted credential rows, operator continuity, session parent loss,
  ingress revocation and permission changes across transactional writes and SSE.
- GPT-6.1 Sol reviewed implementation commit `36607923ff3aeb235872694ad84c248e89032637`
  and the full `origin/main...HEAD` diff: no actionable findings. Review covered
  caller provenance, core dispatch, write/SSE reauthentication, credential
  replacement/revocation, issuance, durable compatibility and regression tests.
  This was source review; test evidence remains pending.
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
