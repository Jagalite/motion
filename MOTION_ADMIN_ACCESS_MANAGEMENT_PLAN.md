# Motion TUI administrator access plan

Standalone task: TUI administration and the server/API support it requires.
No dependency on the broader milestone-completion work. Web administration pages
are outside this task; minimal browser invitation redemption is in scope.

User-requested addition: ordinary administrators must be able to approve or deny
pairing, manage users and revoke access without shell commands, database reads,
or an assistant.

## TUI screens

- **Pairing requests:** list pending requests with device/client name, creation
  time, expiry and status. Names are untrusted display text, not proof of identity.
  Select a request, compare its code with the requesting device, select user,
  allowed profiles/libraries and permissions, then explicitly approve or deny.
  Expired, denied, already claimed and concurrently handled requests must have
  clear outcomes. Do not require administrators to discover an internal UUID.
- **Users and access:** show named access identities, associated devices, allowed
  viewing profiles, library/rating restrictions and effective permissions. Allow
  creating/managing identities and changing access, with explicit confirmation
  for disabling/revoking a user's access across all associated devices.
- **Devices:** show owner, client, status and available activity information;
  allow individual-device revocation without disabling the user's other devices.
  Never invent online/last-seen data if the server does not observe it.
- **Access invitations:** allow an administrator to generate a short-lived,
  single-use access code for a new device, with an explicit intended user and
  grant. Show expiry, unused/claimed/revoked state and a revoke action. A recipient
  enters this code in the browser or terminal pairing flow to receive only the
  preapproved access. This is distinct from a device-generated pairing request:
  the invitation itself represents the administrator's approval.

## Identity and API work

Inventory the existing identity model before adding a users table. Viewing
profiles are not authenticated users. If current principals are only device
identities, add the smallest explicit user-to-device ownership model needed;
do not relabel profiles as accounts or infer ownership from matching names.
Preserve existing devices and profile grants through any migration, assigning
unowned legacy devices explicitly rather than guessing which person owns them.
No password/account-provider system is implied by this feature.

The current contract supports pairing creation/approval/claim and device
revocation, but lacks the complete pending-list/deny/invitation workflow. Add
reviewed public API operations and capability advertisements for the missing
pieces. Implement through the shared API, never TUI direct database access.
Use the existing Charm libraries and Marquee TUI style.

## Correctness and acceptance

- Core models request/invitation lifecycle, grant bounds, expiry, one-time claim,
  deny/revoke races, user/device ownership and revision checks. SQLite atomically
  consumes invitations with credential issuance; retries cannot create another
  independent credential. Adapters supply time and execute effects.
- Invitation codes are scoped bearer secrets, not reusable admin passwords.
  Use unpredictable codes, hashed storage, expiry and bounded redemption attempts.
  Exclude them from logs and URLs. Do not grant administration by default or let
  clients widen the invitation's grant. Decide and document whether outstanding
  invitations are revoked when their issuing administrator loses authority.
- Revocation must fence existing sessions, tickets and playback under the same
  rules as other access changes. A disconnected client cannot promise immediate
  erasure of downloaded bytes. Preserve authorized unsynchronized viewing events.
- Guard against accidental loss of the last usable administrator and provide an
  explicit server-local recovery path without giving ordinary loopback clients
  administrative privileges.
- Test approve versus deny, duplicate approval/claim, expiry, brute-force limits,
  invite revoke versus claim, user-wide versus device-only revocation, stale UI
  responses and restart recovery. Require real API/SQLite tests and core model
  checks of the lifecycle rules, plus a real TUI and browser pairing walkthrough.

Done: an administrator can pair a browser from the TUI, deny another request,
manage a user's grants, revoke one device or the user, and issue/revoke/redeem an
invitation code. No UUID lookup, copied operator secret or database intervention
is required. Update the user-facing instructions and record Sol review evidence.

## Standalone execution instructions

- Read `AGENTS.md` and the identity/access sections of
  `Motion_Final_Architecture_and_Implementation_Plan.md` before implementing.
- Use one implementation agent in its own branch/worktree, based on current
  `origin/main`. Preserve unrelated work and the live database and credentials.
- Put the worktree and isolated build/artifact directories under the seed1
  volume's `Projects` directory; suggested worktree: `motion-tui-access`.
- Run builds through the shared Kyoto installation. Construct its host path as:

  ```sh
  motion_volume_root="$(printf '/%s/%s' Volumes seed1)"
  export KYOTO_SCRIPT="$motion_volume_root/Projects/kyoto/kyoto.py"
  python3 "$KYOTO_SCRIPT" --help
  ```

  Read the current queue policy. Build only affected Rust targets and the Go
  terminal; avoid full app builds unless required by the integration. Record
  completed jobs and separately executed tests when the queue only compiles.
- Obtain GPT-6.1 Sol review of implementation commits and the final PR diff.
  Resolve findings and record dispositions; focus follow-up review on fixes.
- Follow `AGENTS.md` for production core decisions, Stateless verification and
  adapter tests. Keep contract YAML/JSON, clients and migrations consistent.
  Recheck migration allocation on current main; never edit applied migrations.
- Work independently of the broader completion task. Inspect concurrent edits
  before touching shared files; coordinate only actual overlap, not a mandatory
  handoff or approval from another agent.
- Deliver scoped commits and reviewable PRs; do not merge or deploy without a
  subsequent instruction. Record implementation, focused verification, review
  findings and deferred release checks separately.
- Suggested order: identity/API lifecycle, pending-request TUI, user/device
  management and revocation, invitations and focused end-to-end walkthroughs.

This document defines work to implement; it does not claim these screens or API
additions already exist.
