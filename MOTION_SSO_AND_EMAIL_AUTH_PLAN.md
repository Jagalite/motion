# Motion SSO, email sign-in and administrator access screen

Status (2026-10-13): A1–A5 implemented, reviewed and qualified within the
isolated local OIDC/mail scope in [PR #38](https://github.com/Jagalite/motion/pull/38),
stacked on [PR #36](https://github.com/Jagalite/motion/pull/36). This is not a
shipped or fully qualified production release. Final-head CI is tracked on the PR.

See [the qualification ledger](qualification/auth-signin/README.md) for exact
source/artifact receipts: 348 distinct native tests across checkpoints, 98 affected
tests rerun on final application source, 61 bridge tests, genuine provider/mail
browser flows and 27 final headless viewport/state audits. The user-approved
headless browser resolved the earlier T3 screenshot/resize qualification gap.
Keyboard, labels, narrow layouts and targeted accessibility checks passed; a
manual screen-reader walkthrough and physical mobile hardware remain unqualified.
The ledger retains the CI timeout diagnosis/fix and isolated-server SQLite-busy
interruption, including recovery evidence and the limits of each claim.
Original source-review baseline: local main `28f4b32`, 2026-10-12.

## Outcome and scope

Add optional OpenID Connect (OIDC) sign-in and passwordless email sign-in alongside
device pairing. Add a dedicated **Authentication & Access** screen in the web UI
so administrators can configure these methods and manage the people and devices
allowed to use Motion. Preserve private, pairing-only deployments.

The screen belongs in Settings, with a directly addressable route such as
`/settings/access`. Use the existing Motion visual language and components.
An administrator signs in through an authorized Motion device/session; there is
no separate administrator password. The server checks `SystemAdmin` on every
administrative read and mutation. Hiding navigation is only a presentation rule.

This is a standalone feature plan. The separate
[TUI administrator plan](MOTION_ADMIN_ACCESS_MANAGEMENT_PLAN.md) remains standalone
and owns its terminal screens. Both clients must use the same user, pairing,
invitation and revocation APIs. Reuse whichever shared primitives land first;
neither task requires completion of the other's UI. Do not create parallel user
tables, competing invitation lifecycles or a second authorization system.

Local passwords, SAML, SCIM, automatic group-to-admin mapping, unrestricted public
registration and a new general-purpose administration console are outside this
plan. Email through an OIDC provider is supported by the OIDC phase; direct Motion
email codes are a separate phase of this plan and remain opt-in per deployment.

## Dedicated administrator screen

Build one coherent Authentication & Access area with the following sections.

| Section | Administrator workflow |
| --- | --- |
| Overview | See enabled sign-in methods, pending access requests, usable administrators and configuration errors. Explain whether new arrivals need invitations or approval. |
| Sign-in methods | Add, edit, test, enable or disable an OIDC provider; configure direct email delivery; copy the exact callback URL; see the last actual configuration test and its time. Pairing remains available for enrollment and recovery. |
| Users | Create a local access identity; inspect its devices and linked methods; set permissions, allowed viewing profiles and library restrictions; disable or re-enable access; revoke all credentials. Distinguish disabling access from deleting viewing data. |
| Sign-in access requests | Approve or deny an unknown verified SSO identity, showing issuer/subject, selected local user, bounded grant and expiry. Keep these distinct from device pairing; no email-match linking or authority before approval. |
| Pairing and invitations | List pending requests, compare the requesting device's code, approve or deny with an explicit user and grant; generate, inspect and revoke expiring single-use invitations. Distinguish access enrollment from linking an identity to an existing user. |
| Linked identities | Show a user's provider and subject or enrolled email channel; start explicit linking/recovery; remove a method with a clear consequence preview. No arbitrary reassignment by typing an email or provider subject. |
| Devices and sessions | Show owner, method, issue/expiry time and observed activity; revoke one session family, one device or all of a user's access. Mark unavailable activity data as unknown. |
| Security activity | Show bounded, paginated records of sign-in outcomes and administrative changes, including actor, target, outcome and time. Never display tokens, codes or provider secrets. |

Use lists with detail panels or subpages rather than one large settings form.
Include empty, loading, error, expired, denied and stale-revision states. Confirm
destructive access changes by naming the affected user/devices and their scope.
Update stale data after conflicts; never silently replay an old approval.
Provide keyboard navigation, accessible labels and usable narrow layouts.

Provider setup is a guided flow: enter public origin and issuer/client settings,
show the callback URL, save a disabled draft, test discovery/configuration, perform
a browser sign-in test, then explicitly enable it. A connectivity test alone is
not proof that login works. Disabled drafts can run administrator-bound tests,
which must not grant ordinary application access. A test invitation or email
send must be an explicit action with its destination visible.

Secrets have **retain / replace / clear** semantics. Never return a saved secret
to the browser, render it into page state or treat a redacted placeholder as a
replacement. Store secret references or protected server-side secret material;
document the storage/key/backup choice. Public method discovery exposes only
enabled method names and safe presentation fields, not administrator settings.

Ordinary users get a small **My sign-in methods** view for their own methods and
sessions. Linking requires proof and cannot widen permissions. They cannot use
the administrator APIs or inspect another user's identity information.

## Identity and compatibility decisions

The current core authenticates device credentials into principals whose IDs are
device IDs. A viewing profile is not an authenticated person. Introduce or reuse
these concepts without changing playback ownership into user ownership:

| Concept | Responsibility |
| --- | --- |
| Local user | Stable access identity, enabled state, grant/policy ceiling and authorization revision. |
| Owned device | Existing device principal and credential lineage, with explicit optional user ownership. |
| External identity | Unique mapping of exact OIDC issuer plus subject to one local user. Provider email/display name is metadata. |
| Email identity | Explicitly enrolled and verified delivery address bound to one local user, with its own revision and enabled state. |
| Authentication transaction | Purpose, browser binding, provider/identity revisions, server epoch, expiry and single-use lifecycle. |
| Session family | Local credentials with device parent, user ownership, authenticating method and revocation/expiry bounds. |

The application-facing principal remains device-scoped. Add user context rather
than replacing `Principal.id` with a user ID. First successful browser sign-in
creates an explicitly owned browser device and a bounded parent/session chain;
for this implementation, each new SSO/email login creates one distinct browser
device per session family. Page refreshes keep the existing valid family; a new
login must not reuse a pairing parent or another method's device generation.
Present these as browser sessions, not verified physical device identities. This
keeps family/provider revocation independent without changing the current single
parent per device model. Never adopt a device from a caller-supplied ID. Do not expose a
long-lived device bearer secret to browser JavaScript or use a provider token as
a Motion credential. Discard provider tokens after verification unless a later
feature establishes a separately reviewed need to retain them.

Effective authority is the intersection of the user's grant ceiling and the
device/session restrictions. Check user enabled state and current revisions
before any administrator shortcut. Revocation and restriction changes apply to
ordinary API admission, writes, event streams, ticket issuance and ongoing
delivery checks. Preserve per-device playback control and viewing-profile rules.

Compose policy by meaning, not by blindly intersecting stored sets. Permissions,
profile access and library access cannot exceed either side's effective ceiling;
administration requires both sides to authorize it. In current non-admin policy,
empty libraries deny access, while an empty rating allowlist adds no rating
restriction. Intersect effective rating eligibility, combine blocked labels by
union and combine `allow_unrated` by AND. Represent unrestricted versus empty
eligibility explicitly so disjoint rating allowlists do not become unrestricted.
An administrator shortcut must not discard the other side's restrictions; test
administrator user/restricted device and restricted user/administrator device.

Legacy devices retain their existing grants and remain visibly unassigned until
an administrator assigns ownership. Do not infer users from profile names,
display names or email. Migration must preserve existing viewing data and avoid
silently broadening or removing access.

Admission is invitation or administrator approval by default. An unknown OIDC
identity can request approval after verified sign-in, but receives no media or
administrative access. Approval binds a specific verified identity, local user
and bounded grant. Email/domain/group claims never independently create local
authority. A contact email match, even a verified one, never links accounts.
Pending requests have bounded retention, expiry and abuse limits. Approval is
an admission decision, not a long-lived login proof: after a delayed approval,
require fresh sign-in as the approved issuer/subject before issuing credentials.
Adding that identity to an already existing user's access requires the linking
or explicit audited recovery rules below, not merely selecting a matching name.

## OIDC sign-in

Implement one generic OIDC integration first, using a maintained protocol library
and authorization code flow with PKCE S256. Require browser-bound state, nonce,
exact registered redirects and validation of signature, issuer, audience,
authorized party where applicable, nonce and token lifetime. Use issuer plus
subject as the external identity key; email claims are not stable identifiers.
Follow [OIDC Core](https://openid.net/specs/openid-connect-core-1_0.html) and
[OAuth security BCP](https://www.rfc-editor.org/rfc/rfc9700.html).

Each transaction records login/link/test purpose, initiating browser, target user
where relevant, provider revision, server/restore epoch and expiry. Resolve the
provider from that transaction, not callback input. Reject unsolicited callbacks
and provider changes during a transaction. Exchange codes on the server. A
duplicate callback cannot issue a second independent credential family.

Preserve the existing strict session cookie and mutation Origin/CSRF checks.
Use a separate short-lived callback correlation mechanism for the cross-site
redirect, compatible with the selected response mode; do not weaken the global
cookie or Origin rules. Constrain return destinations to approved local routes.
Rotate local session identity after login and reject attempts to turn a login
callback into a link operation. Redact callback query strings in application and
proxy logs and serve callback responses without third-party assets or caching.

Provider configuration is administrator-controlled. Require explicit public
origin configuration, with correct HTTPS/proxy handling and no dependence on an
untrusted Host header. Bound discovery, token and JWKS network requests; validate
their destinations, redirects and TLS. Private identity providers require
explicit allowed destinations, not a blanket exception permitting arbitrary
internal requests. Rate-limit refresh on unknown signing keys and bound caches.

Do not request offline access or add refresh-token persistence in this phase.
Choose finite local session expiry no longer than its parent credential. Record
the chosen lifetimes and renewal rules in the contract; silent renewal may not
extend the configured maximum interval since fresh authentication. Upstream
provider disable/logout is not immediate local revocation without a separately
implemented signal; document this delay and the local revoke action.

Desktop and TUI may keep pairing initially. Users can sign into a browser and
approve enrollment from there when authorized. A later native OIDC flow must use
the system browser and a verifier-bound return flow, following
[OAuth for native apps](https://www.rfc-editor.org/rfc/rfc8252.html). Do not send
provider tokens through deep links, process arguments or embedded login windows.

## Passwordless email sign-in

Support email through the chosen OIDC provider first. Then add optional direct
email with a typed one-time code entered in the initiating browser. This avoids
requiring an email link to perform a login automatically. Email ownership proves
control of an enrolled channel, not administrator status or ownership of another
account with a matching address.

Direct email enrollment requires an existing authenticated user or an explicit
administrator-approved invitation, followed by proof of mailbox control. An
ordinary login request cannot enroll or replace an address. Establish one shared
address-validation/canonicalization policy with uniqueness checks; do not invent
provider-specific dot or plus-address equivalence. Changing addresses requires
fresh proof and explicit authorization; audit recovery overrides.

Return the same outward request result for unknown, disabled and eligible
addresses. Use cryptographically random, short-lived codes with keyed-hash
storage, attempt limits and limits by transaction, address, client/network and
deployment. Bind redemption to its initiating transaction/browser. A new send
invalidates the preceding code for that transaction, without allowing outsiders
to invalidate another browser's pending login. Reuse, expiry and cross-browser
submission fail. GET requests and mail scanner visits cannot consume a challenge
or log someone in. Apply the analogous one-time-token and enumeration protections
in [OWASP's reset guidance](https://cheatsheetseries.owasp.org/cheatsheets/Forgot_Password_Cheat_Sheet.html);
this is a sign-in flow, not a password-reset implementation.

Keep sending in an adapter with bounded retries and explicit delivery outcomes.
If an outbox must retain a deliverable code, encrypt that short-lived payload,
separate its key from database backups, and erase it after delivery/expiry. A
retry resends the same challenge rather than creating another valid one. Queue
acceptance or successful delivery is never evidence of authentication. Document
mail credentials, TLS, sender setup, failure handling and rate-limit controls.
Use a local mail sink for tests; send real test email only to an explicitly
selected test destination.

## Linking, administrator authority and recovery

Linking requires recent proof of the existing local identity and fresh proof of
the new method, both bound to one link transaction. Atomically enforce external
identity uniqueness; never move an identity between users on a duplicate or
retry. Administrator recovery is a distinct audited action with explicit target
and proof, not an unrestricted identity-edit field.

Sensitive actions such as granting administration, changing provider secrets,
replacing login methods or recovery require recent authentication. Define the
freshness interval and accepted evidence in core policy: a fresh provider/email
proof appropriate to that administrator's policy, or an action-bound approval
from another authorized device/local operator. Merely refreshing a page or
reusing the existing session is not fresh evidence. Do not require an external
provider for the server-local recovery path or lock out pairing-only admins.

Prevent removing the last viable sign-in method or usable administrator without
a tested local recovery route and explicit recovery procedure. Check this inside
the write transaction so concurrent removals cannot bypass it. No automatic
administrator role from SSO/email, network location, Tailscale identity or
loopback. Preserve the protected local operator mechanism; do not ask web users
to paste its secret into the browser.

Use these revocation semantics consistently in the UI, API and core:

| Action | Effect |
| --- | --- |
| Sign out / revoke session | Invalidate that browser's session family, including renewal authority. |
| Revoke device | Invalidate its parent and all descendant credentials. |
| Disable user | Deny all owned device/session access and issuance, regardless of sign-in method; invalidate outstanding enrollments for that user. |
| Re-enable user | Allow fresh authentication/enrollment; do not reactivate credentials revoked by disabling. |
| Unlink identity / disable provider or email method | Cancel related pending transactions and invalidate session families authenticated through it; independent pairing credentials retain their own authority unless the user/device is also revoked. |
| Reduce permissions | Recompute effective grants before committing writes or continuing authorized work; stale callbacks cannot restore the old grant. |

Every change has an audit record committed with the state change where practical.
Audit records contain identifiers and outcomes, not secrets or full tokens. Bound
retention and access; store only the network/device metadata needed by the UI.
Revocation fences future admissions and signals active work; it cannot erase
already delivered or downloaded media.

## Core, persistence and existing integration seams

Model admission, ownership, grant intersection, linking, challenge consumption,
disable/revoke and freshness rules as production decisions in `crates/core`.
Supply verified provider facts, observed time and delivery outcomes as explicit
inputs. Keep cryptographic verification, discovery, mail, SQLite and timers in
adapters; do not implement a second JWT validator inside a state model.

Consume a transaction and mint its local credential family in one database
transaction, with fresh authorization and revision checks. Atomically record any
link or enrollment that issuance relies on. External code exchange cannot share
that SQLite transaction: reserve the attempt durably, fence duplicate workers,
then commit the verified result once. After a crash with an uncertain exchange
outcome, fail closed and restart sign-in rather than retrying a consumed code or
creating extra credentials. Retrying a response must not expand authority.

Revisit these specific current seams during implementation:

- `crates/core/src/access.rs`: credentials require device parents; extend the
  existing chain with user/method authority instead of inventing an orphan session.
- `src/v2/auth.rs` and `src/v2/events.rs`: missing credential and ingress fields
  currently identify the operator path. Introduce an explicit caller-source
  distinction; no missing or malformed new identity can become an operator.
- `crates/core/src/playback_session.rs`: continuation rebuilds authority from the
  owning device. Supply current user/method facts as well, without conflating all
  of a user's devices or bypassing revocation for established delivery work.
- `src/v2/identity.rs`: retain secure cookie/session exchange behavior while
  adding separate callback correlation and browser credential issuance.
- `src/db.rs`: restore rotates an epoch while retaining credential rows. Bind
  new authentication transactions and session families to the restore epoch;
  reject pre-restore artifacts. Hold new SSO/email sign-ins after restore until
  the local operator reviews restored users, grants and provider settings.
  Document legacy paired-credential restore behavior separately; do not claim
  epoch rotation alone prevents all historical revocations from being undone.

Extend the reviewed v2 API and capability advertisements for method discovery,
login transactions, enrollment/linking, users, providers, session inventory and
revocation. Share existing or newly landed pairing/invitation endpoints with the
TUI. Keep YAML/JSON contracts, generated clients and migrations consistent.
Allocate migrations against current main and never edit applied migrations.

## Implementation sequence and acceptance

Use one implementation owner with reviewable phases. The administrator screen is
a required delivery, not an optional follow-up after authentication backend work.

| Phase | Work | Acceptance |
| --- | --- | --- |
| A1: identity and contract | Reconcile shared TUI primitives; add user ownership, method lineage, explicit caller source and core lifecycle rules; publish API/migration changes. | Existing paired devices keep working; owned-user disable and grant ceilings apply across API/events/delivery; no implicit operator path for new identities. |
| A2: administrator screen | Build Authentication & Access navigation, users, devices, pairings/invitations, audit view and disabled provider configuration drafts against real APIs. | Admin can approve/deny a browser, manage access and revoke it; non-admin direct routes/API calls fail; stale screens cannot commit stale authority. |
| A3: OIDC | Implement generic provider, browser callback, admission, explicit linking, provider tests and sign-in controls in the administrator screen. | One real configured provider and a controlled test provider exercise complete login/link/revoke; existing pairing remains usable during provider failure. |
| A4: direct email | Add opt-in delivery configuration, explicit enrollment, bounded challenge lifecycle and browser code entry. | Mail-sink end-to-end tests pass; configured test delivery is demonstrated separately; request responses do not enumerate accounts and duplicate codes cannot issue twice. |
| A5: recovery and qualification | Finish restore/freshness/recovery behavior, browser UX/accessibility, docs and focused cross-client regressions. | Complete the walkthroughs below and record exact source/artifact evidence, independent review and any remaining release gates. |

If implementation is split into PRs, follow A1 before dependent changes. A2 and
A3 UI work can proceed against an agreed contract, but share one implementation
owner unless ownership is explicitly reassigned. Provider-backed email is usable
after A3; do not label direct Motion email complete until A4 passes.

Required verification:

- Production-backed Stateless checks for duplicate callbacks, linking races,
  identity uniqueness, stale revisions, expiry, disable versus issue, concurrent
  last-admin removal and restore. Cover disjoint/absent rating constraints,
  mixed administrator/restricted grants and independently revocable browser
  families from different methods. Assert precise credential/effect counts and
  grants; report explored bounds and properties rather than claiming exhaustive
  correctness.
- Real SQLite/API tests for transaction atomicity, crash boundaries, lineage,
  session revocation and migrations from a populated legacy database. Include
  SSE and active playback restrictions, not only login endpoint tests.
- Protocol tests for wrong issuer/audience/nonce/state/PKCE, expired tokens,
  malicious redirects, discovery/JWKS failures and key rotation. Exercise login
  CSRF, session fixation, cross-browser code use and insufficient linking proof.
- Real browser walkthrough: admin configures a provider; new user requests or
  redeems approved access; admin approves it; user signs in, selects an allowed
  profile and plays permitted media; denial/revocation stops further access.
- Browser administration checks: provider/email secrets never appear in reads,
  rendered state or logs; direct non-admin requests fail; last-admin protection,
  recent-proof prompts and conflict/error states work; self-service cannot grant
  administration. Verify browser refresh and session-expiry behavior.
- Email enrollment/login, expired and repeated codes, rate limits, delivery
  failure/retry, safe address change and scanner/GET behavior. Test provider and
  mail outages while pairing and local recovery remain available.
- Cross-client regression: existing TUI pairing/admin APIs and native paired
  playback keep working. Do not require native OIDC to claim this scoped feature.

## Agent execution instructions

- Read `AGENTS.md`, architecture section 12 and the current identity/access code.
  Reconcile overlapping work with the standalone TUI plan at the API/schema seam.
  Preserve unrelated files and live databases, credentials and media.
- Use a dedicated branch/worktree based on current `origin/main`, suggested name
  `motion-auth-signin`. Put its worktree and isolated build/artifact directories
  under the seed1 volume's `Projects` directory. Use isolated database/mail/IdP
  fixtures; no production migrations, provider changes or unsolicited email.
- Use the shared Kyoto script for builds. Construct the host path as follows:

  ```sh
  motion_volume_root="$(printf '/%s/%s' Volumes seed1)"
  export KYOTO_SCRIPT="$motion_volume_root/Projects/kyoto/kyoto.py"
  python3 "$KYOTO_SCRIPT" --help
  ```

  Read the current queue policy. Build/test only affected Rust targets and web
  assets; build the TUI only when a shared API/client change requires it. Avoid
  full application builds unless needed for a concrete integration check. Record
  completed queue jobs and distinguish compilation from executed tests.
- Obtain **GPT-6.1 Sol review of implementation commits and the final PR diff**.
  Resolve findings, record dispositions and request focused review of fixes.
  Reviewer attention must include caller identity, grant composition, atomic
  issuance, linking, provider secrets, restore and delivery revocation.
- Deliver scoped commits and reviewable PRs with current contracts, migration
  instructions, administrator/user documentation and qualification evidence.
  Distinguish implemented, tested, reviewed and deferred work. Do not merge or
  deploy without a subsequent instruction.

Done means an ordinary Motion administrator can configure sign-in, enroll and
manage users, approve access and revoke it from the dedicated screen without
database edits or shell commands, while pairing-only operation remains viable.
