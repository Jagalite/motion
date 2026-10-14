# Motion native email delivery and account registration

Status: implemented on branch `feat/native-email-registration` (2026-10-14), except
the items listed under [Remaining limits](#remaining-limits). The original direction
is kept below; the [N1 decisions](#n1-decisions) record what was chosen. Verification
links are still not implemented; registration uses the existing code-entry flow.

## Outcome

Offer an optional built-in sender that delivers Motion's transactional email
directly to recipients' mail servers. Keep the existing SMTP relay option. Both
delivery modes should serve the same authentication, verification and invitation
flows and use the same durable outbox.

Add explicit registration policies so a server owner can choose whether verifying
an email activates an account immediately, completes an invitation, or creates a
request awaiting approval. Automatic activation is a supported product direction;
administrator approval is not an inherent requirement of email verification.

This is an outbound feature for Motion-generated messages. Inboxes, IMAP, POP,
bulk marketing, a public SMTP listener and arbitrary third-party mail relaying
are outside scope. Local passwords are also outside scope: Motion currently uses
pairing, OIDC and passwordless email codes, so there are no local passwords to reset.

## Existing implementation and relationship to earlier plans

Source inspected at local main `dcabb7b`:

- `src/signin_protocol.rs` sends email through `lettre` and a configured SMTP relay.
- `src/v2/signin/` owns the existing API, persisted sign-in configuration,
  challenges, encrypted outbox and administrative operations.
- `crates/core/src/signin.rs` and its model tests own existing sign-in policies.
- Email codes are time-limited, attempt-limited, browser-bound and consumed once.
  Retry delivery retains the same code. Ordinary login does not enroll an unknown
  email address; current email enrollment requires an already owned identity.
- Unknown OIDC identities currently use an administrator approval flow.
- Pairing remains available without an external email or identity provider.

See [Authentication](AUTHENTICATION.md), the
[SSO/email plan](MOTION_SSO_AND_EMAIL_AUTH_PLAN.md), the
[administrator access plan](MOTION_ADMIN_ACCESS_MANAGEMENT_PLAN.md), and
[auth qualification evidence](qualification/auth-signin/README.md).

The older documents use **direct email** to mean Motion's own email-code login,
as distinct from signing in through an OIDC provider. That implementation still
uses an SMTP relay. In this document **direct delivery** means delivery to the
recipient domain's mail servers without an upstream relay.

The earlier SSO plan excluded public registration. The registration policy work
below is an explicit extension to that scope. Existing deployments retain their
current enrollment behavior until an administrator selects a new policy. Do not
label the existing behavior as a completed email approval-registration flow.

## Two delivery modes

| Mode | Delivery path | Administrator supplies |
| --- | --- | --- |
| SMTP relay | Motion → configured relay → recipient mail server | Relay hostname, port, TLS configuration, sender and authentication where required |
| Native direct delivery | Motion → recipient domain's MX servers | Sending domain, hostname, signing configuration, published DNS records and suitable outbound network access |

The relay handles onward routing and retries after accepting a message; Motion
still handles failures while submitting it to that relay. In direct mode, Motion
owns routing and retries through recipient-server acceptance. Both modes need
the same challenge expiry, cancellation and abuse controls.

Keep relay behavior working while direct delivery is developed. Select the mode
explicitly in administration. Do not silently switch transports after an ambiguous
send: the recipient might already have accepted the message. Any future fallback
policy must be configured and account for duplicate-delivery risk.

SSO and email remain opt-in because each deployment needs configuration and tests.
Once enabled, they are ordinary sign-in choices for users. Installing this feature
must not enable public registration or change an existing deployment's defaults.

## What native delivery can satisfy

A built-in sender can meet recipient-provider requirements. Native implementation
does not inherently cause spam placement. Successful delivery also depends on
configuration and factors outside the application.

| Requirement | Motion responsibility | External responsibility |
| --- | --- | --- |
| DKIM | Generate or import protected keys, sign final message bytes, rotate selectors | Publish the public key in domain DNS |
| SPF and DMARC | Produce setup guidance and validate observed records/alignment | Publish records matching the actual sender and sending IPs |
| TLS | Negotiate SMTP STARTTLS and apply the defined certificate/transport policy | Recipient must offer compatible TLS |
| SMTP hostname | Configure and validate the sending identity | Maintain matching hostname/address configuration |
| Reverse DNS | Diagnose the observed PTR and forward mapping | IP owner or hosting provider configures PTR records |
| Network access | Test outbound delivery connectivity | Network/provider permits access to recipient SMTP servers, normally port 25 |
| Reputation | Bound volume, prevent abuse and expose failures | Recipient providers assess domain/IP reputation and filtering |

For authentication messages, the proposed initial policy requires TLS and has no
silent plaintext downgrade. Define certificate and recipient-domain policy handling
explicitly before implementation; document recipient compatibility limits.
Strict TLS alone must not be described as full MTA-STS or DANE support.

Gmail's [sender requirements](https://support.google.com/mail/answer/81126) provide
one concrete interoperability target; requirements vary by recipient provider.
Preflight success is not an inbox-placement guarantee. Distinguish **queued**,
**accepted by mail server**, **retry pending**, **failed** and **expired**.
Only a recipient completing the challenge proves receipt for verification.

Direct delivery does not require Motion to host incoming mail. Configure a valid
envelope sender and an externally managed return/bounce mailbox as needed.
Initial scope has no automatic inbound bounce processing; a later bounce after
SMTP acceptance may therefore be visible only in that mailbox.

## Dependencies and code ownership

The discussion explored both using mail crates and implementing a narrow sender
inside Motion. Neither a final crate choice nor a requirement to remove all mail
dependencies has been approved by this document.

| Building block | Existing availability / proposal |
| --- | --- |
| Async sockets and deadlines | Reuse direct dependency `tokio` |
| TLS | `rustls`, `tokio-rustls` and certificate roots already occur transitively; declare compatible direct dependencies if used by Motion code |
| Hashing and signing | `sha2` is direct; `rsa` and `base64` are currently transitive candidates for DKIM signing/encoding; verify features, versions and advisories before selecting |
| Durable queue and configuration | Extend existing SQLx/SQLite outbox and secret protection |
| DNS MX discovery | Evaluate `hickory-resolver`; this is optional, not a protocol requirement to use that particular crate |
| Existing relay transport | Keep `lettre` working; replacing it is separate work and not required for adding direct delivery |
| Mail-focused alternatives | Evaluate `mail-send` and `mail-auth` as dependencies or implementation references |

Proposed approach: reuse established TLS and cryptographic primitives, own the
small delivery policy in Motion, and use a DNS resolver library unless there is
a concrete reason to own DNS wire handling. Do not implement cryptographic
primitives ourselves. Transitive presence does not make a crate a supported
direct API or eliminate dependency/license review. Reference implementations are
useful; copied code must retain the applicable license obligations.

If choosing a custom SMTP/DKIM implementation, Motion owns bounded response
parsing, multiline replies, command ordering, STARTTLS state reset, message
headers/encoding, CRLF handling, dot-stuffing, canonicalization and signature
construction. Start with one recipient and small plain-text messages. Specify
supported address/character encodings explicitly; reject unsupported forms.

### Why DNS is involved

Direct delivery discovers a recipient domain's mail servers using MX records,
then resolves their IPv4/IPv6 addresses. Ordinary hostname-to-IP lookup does not
supply MX records. A configured relay does not require recipient MX lookup.

Owning a narrow DNS client is feasible but adds parsing, compressed-name bounds,
request/response matching, deadlines, resolver failover, UDP truncation/TCP retry,
aliases, caching and operating-system resolver configuration. Handle nonexistent
domains, temporary errors, absence of MX, and explicit Null MX distinctly. Query
existing DNS resolvers; there is no need to build a recursive resolver.
Use [SMTP routing rules](https://www.rfc-editor.org/rfc/rfc5321) and
[Null MX semantics](https://www.rfc-editor.org/rfc/rfc7505) as normative references.

The conversational estimate of days for a prototype and roughly one to two weeks
for a narrow tested DNS client was preliminary, not a delivery commitment or an
estimate for the entire native-mail feature. Scope and platform requirements must
be settled before scheduling.

## Verification, invitations and registration

Email transport delivers a message. The authentication layer creates and verifies
purpose-bound challenges; registration policy decides what access follows.

Initial implementation should extend the existing code-entry flow. Verification
links are a possible follow-up, not an existing capability or required first step.
If links are added, a scanner or GET request must not consume a token or create an
account; require explicit confirmation and a defined browser-binding policy.

| Registration policy | Result after valid email verification |
| --- | --- |
| Approval required | Create a pending access request; no library access until an authorized administrator approves an explicit grant |
| Invite-only | Complete an unexpired, unrevoked invitation and activate the account with its predefined bounded grant |
| Open registration | Activate a new account automatically with the administrator's configured default profiles, libraries and permissions |

These are proposed policies. Open registration should not require a second manual
approval when its conditions are met. It must never implicitly grant administrator
or local-operator authority. Existing local-user, device and grant ceilings apply.

Verification establishes control of an email address, not ownership of another
local account. Never attach a new registration to an existing account solely
because its email matches. Handle concurrent registrations and existing-address
collisions transactionally without exposing account existence publicly. Continue
using explicit authenticated linking/recovery for existing identities.

Capture the registration intent and policy revision, then recheck current policy,
invitation, user and grant state when consuming proof. If policy changed, require
a fresh attempt rather than granting stale or wider rights. Invitation consumption,
challenge consumption, identity ownership and account activation must be atomic
and replay-safe. Return a stable outcome on retries, without creating another user
or session family.

First scope: email registration and email delivery of existing invitations.
Preserve current OIDC admission behavior; any application of open registration to
OIDC must be an explicit follow-up keyed by issuer/subject, not email equality.
Password-reset mail remains future scope unless local password authentication is
separately introduced. Email recovery must not silently bypass existing linking
and administrator-recovery protections.

## Shared state and adapter boundaries

Extend the existing outbox instead of adding an independent queue or auth system.
In deterministic `crates/core` policies, represent the facts that decide whether
an attempt, retry, acceptance or account activation remains valid:

- Message/challenge purpose and identity; owner, configuration and restore epoch.
- Attempt/lease identity, cancellation, deadline and retry budget.
- Selected mode and configuration revision, signing-key identity and byte identity.
- Observed SMTP stage/status, temporary/permanent refusal or ambiguous acceptance.
- Registration policy revision, invitation revision, ownership and allowed grant.

Adapters perform DNS, sockets, TLS, signing, time observations and SQLite effects.
They supply explicit observations to core policy and enforce required transactions
and durable ordering. A generic `success` boolean cannot represent acceptance
uncertainty, stale ownership or an expired challenge.

Reserve an attempt durably before network work; recheck eligibility before sending
secret content. A timeout after final DATA can leave acceptance unknown. Do not
claim exactly-once external delivery: retry may produce duplicate mail. Preserve
the same challenge/message identity on retry and enforce exactly-once proof
consumption locally. Cancellation cannot recall bytes already sent.

Do not start new sends after expiry, cancellation or configuration invalidation.
Old messages may still arrive, but their proofs must fail. Preserve current restore
invalidation and administrator review behavior. Protect signing keys and queued
payloads using the existing secret-storage approach, and erase secret payloads at
terminal states. Audit identifiers/outcomes without codes, tokens or key material.

Direct routing must not turn public email endpoints into a network probe: reject
disallowed private, loopback, link-local and special-use destinations, including
IPv6 equivalents, and pin checked DNS results to actual socket destinations.
Keep isolated test exceptions explicit. Bound DNS/SMTP bytes, redirects or aliases,
connections, per-domain concurrency and global work. Retain generic public responses
and limits per recipient, browser, network peer and deployment.

## Administrator and user experience

Extend **Settings → Authentication & Access** with:

1. **Email delivery:** select relay or native direct delivery; save a disabled draft.
2. **Native setup:** sender, envelope sender, hostname, DKIM selector/key handling,
   DNS records to publish and a clearly explained network requirement.
3. **Diagnostics:** DNS/authentication observations, TLS/connectivity results,
   configuration revision and test timestamp; separate actionable failures from
   facts Motion cannot prove, such as final inbox placement or future reputation.
4. **Test send:** explicitly select a recipient, send an identifiable test, and
   optionally enter its code to prove receipt. Transport acceptance alone must
   not authenticate anyone or be labeled confirmed receipt.
5. **Registration:** choose policy and default access; manage email access requests
   and invitations through existing user/grant APIs.
6. **Delivery activity:** bounded, paginated status and redacted failure details;
   explain retries, expiry and ambiguous acceptance without displaying secrets.

Configuration changes invalidate old setup tests. Keep retain/replace/clear
semantics for secrets. Pairing and protected local recovery remain usable during
mail outages. UI and TUI must share administrative operations; this document adds
no separate terminal authorization model. Terminal feature coverage can follow the
[shared CLI/TUI operations plan](MOTION_CLI_TUI_SHARED_OPERATIONS_PLAN.md).

Public flows should clearly distinguish verify-address, pending approval,
invitation accepted and account active. Open-registration success should lead to
usable access under the configured default grant. A resend must not extend an old
challenge's authority without an explicit new transaction.

## Implementation sequence and acceptance

| Phase | Deliverable and acceptance |
| --- | --- |
| N1 — design and contracts | Confirm dependency/DNS choice, TLS policy, supported address forms, return-path handling and registration semantics; define API/schema changes without reusing applied migration numbers |
| N2 — transport | Add direct sender with fixture DNS/SMTP/TLS and independent DKIM verification; preserve relay regression coverage; no public registration yet |
| N3 — durable integration | Extend existing outbox with attempt identity, failover, expiry, ambiguous acceptance and recovery; verify real SQLite ordering and production core policies |
| N4 — registration | Implement the three policies, purpose-bound verification and invitation completion; prove ownership, grant and replay behavior |
| N5 — administration | Deliver setup, diagnostics, mode/policy selection, test receipt and bounded delivery activity in existing access management |
| N6 — qualification | Exercise both transports and all registration policies through real clients; record exact source/artifact, deployment and mailbox evidence and remaining limits |

Registration can be developed and qualified with the existing relay while native
transport work proceeds. Both depend on agreed challenge/outbox/configuration
contracts. Integrate those contracts before changing overlapping migrations or
workers in parallel. A single agent can execute these phases sequentially.

Required verification includes:

- Production core/Stateless checks for expiry, replay, cancellation, stale attempts,
  configuration changes, restore, policy changes, invitation revocation, grant
  ceilings and concurrent account creation. Record explored bounds and properties.
- Adapter tests for DNS edge cases, bounded malformed SMTP replies, STARTTLS,
  refusal classification, failover, uncertain acceptance, crashes and recovery.
- Independent DKIM verification of exact serialized message bytes and header
  injection/address validation tests; no self-referential signing-only test.
- Live SQLite tests proving atomic invitation/challenge consumption and account
  activation; model exploration alone does not prove durable ordering.
- Browser/TUI tests of the implemented surfaces, including keyboard/narrow layouts,
  pending/expired/denied states and continued pairing during email failure.
- Explicitly authorized sends to controlled real mailboxes for provider
  interoperability. Fixture acceptance is not real inbox qualification; successful
  delivery to one provider is not a universal deliverability claim.

Implementation work should use a clean isolated worktree, preserve unrelated
changes and live data, and use the shared Kyoto build queue (`kyoto.py`) for Rust build
work following the current Kyoto skill. Compile/test the smallest affected scope;
build the full app only when packaging or interactive validation needs it. Use
GPT-6.1 Sol as an independent reviewer of implementation commits and PRs, and
address findings before integration. This document itself changes no runtime
settings, installs no dependencies and sends no email.

## N1 decisions

- **Dependencies.** `hickory-resolver` 0.26 (system resolver configuration) for MX,
  A/AAAA, TXT and PTR; `tokio-rustls` with the `ring` provider and `webpki-roots`
  for STARTTLS; `ring` for RSA PKCS#1 v1.5 SHA-256 DKIM signatures; `rsa` only to
  generate keys and decode imported PKCS#1/PKCS#8 PEM (it performs no private-key
  operation on message data, so RUSTSEC-2023-0071 timing exposure does not apply to
  signing). `lettre` stays for relay submission (`send_raw` of Motion-built bytes).
  The SMTP client and DKIM signer are small Motion code; `mail-send`/`mail-auth`
  were used only as references. Licenses are recorded in `THIRD_PARTY_NOTICES.txt`.
- **TLS policy.** Direct delivery requires STARTTLS and a WebPKI certificate valid
  for the exchanger host name from DNS, on every route, with no plaintext fallback.
  Plaintext bytes buffered after `220` to STARTTLS are rejected. When every route
  fails only on TLS policy the message fails permanently. This is not MTA-STS or
  DANE, and the MX name itself is not DNSSEC-authenticated. Recipients without
  valid STARTTLS cannot receive Motion mail in direct mode; use relay mode for them.
- **Routing.** RFC 5321 MX preference order, at most 10 exchangers, 4 addresses each
  (IPv4 first) and 8 routes; implicit MX for domains without MX; Null MX and
  NXDOMAIN are permanent; temporary DNS failures retry. Only public unicast
  destinations on port 25 (the existing OIDC destination policy, rejecting private,
  loopback, link-local, mapped, NAT64, documentation and special-use ranges for
  IPv4 and IPv6); resolved addresses are pinned to the socket actually opened.
  A 4xx/transport/TLS failure tries the next route; a 5xx reply is authoritative;
  a missing reply after the final terminator is *ambiguous*. A 554 greeting tries
  the next route. Explicit isolated exceptions: an injected test fixture (DNS,
  trust anchors, loopback ports) and an administrator-visible loopback qualification
  port that sends plaintext to `127.0.0.1` only.
- **Address forms.** ASCII dot-atom only (the existing `canonical_email` policy),
  lower-cased; optional printable-ASCII display name for the sender; fixed ASCII
  subjects; 7-bit text bodies with lines ≤ 998 octets. No SMTPUTF8, IDN or encoded
  words; such input is rejected, never encoded.
- **Return path.** Direct mode uses an administrator-configured envelope sender
  mailbox managed outside Motion; relay mode uses the From address. No inbound
  bounce processing.
- **DKIM.** rsa-sha256, relaxed/relaxed, `d=` is the From domain, signed headers
  From (over-signed), To, Subject, Date, Message-ID, MIME-Version, Content-Type,
  Content-Transfer-Encoding and Auto-Submitted. Keys are 2048–4096-bit RSA,
  generated or imported, sealed with the existing secret encryption, identified by
  a SHA-256 SPKI prefix that is part of each message's configuration fence.
- **Outbox contract.** Messages are composed and signed once; retries send identical
  bytes (`message_hash`). Each attempt is reserved (lease token, attempt number)
  in a committed write before any network work, eligibility is rechecked
  immediately before DATA (relay: before submission), and the observation is
  applied by `playscale_core::mail`. Six attempts, back-off 30 s–10 min, 600 s
  lease (longer than the 240 s route cutoff plus per-route deadlines). A crash
  leaves an expired lease that is recovered as ambiguous. Terminal states erase the
  sealed payload. Configuration, key, restore-epoch, enablement or owner changes
  cancel queued messages; in-flight attempts resolve through their own observation.
- **Registration semantics.** `Closed` (default, existing behavior), `ApprovalRequired`,
  `InviteOnly`, `Open`. An email-bound invitation takes precedence under any
  non-closed policy. Admission and completion decisions are
  `playscale_core::registration::{admit, decide}`; the challenge captures the policy
  revision and invitation revision, and completion rechecks them together with
  address ownership, capacity and the default grant (never `system:admin`). An
  owned address receives a code-free notice. All outcomes share one public response.
  The invitation email carries no secret.
- **API and schema.** Migration 0064 (next free number on every branch at the time).
  New operations: `startEmailRegistration`, `getRegistrationSettings`,
  `replaceRegistrationSettings`, `confirmSigninEmailTest`, `diagnoseSigninEmail`,
  `listSigninEmailActivity`; extended email settings, test result, request and
  invitation schemas. Proof action `registration.write` was added.

## Correctness boundary (implemented)

Core state and transitions: `mail::{claim, may_transmit, observe, cancel,
combine_routes}` decide attempt reservation, recovery, retry, terminal state and
route failover from explicit observations; `registration::{admit, decide,
invitation_registrable, default_grant_allowed}` decide admission and the outcome
of verified proof; `signin::validate` rejects a registration transaction that
carries a bound user or a non-registration transaction that carries registration
facts. Adapters (`src/mail`, `src/v2/signin/{outbox,registration}.rs`) perform DNS,
sockets, TLS, signing, time reads and SQLite effects; every decision and its
effects commit in one reserved SQLite writer (challenge consumption, identity and
user creation, invitation claim, request insertion, session issue).

## Remaining limits

- Real-mailbox interoperability (Gmail or any provider) was **not** exercised: it
  needs a domain with published DKIM/SPF/DMARC/PTR records and an explicitly
  authorized send. Fixture acceptance is not inbox qualification.
- Verification links, inbound bounce processing, MTA-STS/DANE, SMTPUTF8/IDN and
  fallback between transports are not implemented.
- Terminal (TUI/CLI) screens for delivery settings and registration policy are not
  implemented; they use the same administrative API and can follow the shared
  CLI/TUI operations plan. The terminal invitation list tolerates the new
  `registered` status and `email` field but does not display them.
- Open-registration capacity is a fixed bound (100 activations per rolling day,
  1000 pending requests); it is not administrator-configurable yet.

## Reference implementations and standards

- [mail-send](https://github.com/stalwartlabs/mail-send): SMTP/TLS and signing components.
- [mail-auth](https://github.com/stalwartlabs/mail-auth): email authentication components.
- [lettre](https://docs.rs/lettre/latest/lettre/): existing relay library.
- [Hickory resolver](https://docs.rs/hickory-resolver/latest/hickory_resolver/): DNS option.
- [SMTP, RFC 5321](https://www.rfc-editor.org/rfc/rfc5321) and
  [Null MX, RFC 7505](https://www.rfc-editor.org/rfc/rfc7505): routing and failure semantics.
- [DKIM, RFC 6376](https://www.rfc-editor.org/rfc/rfc6376): signing and canonicalization;
  consult its updates when choosing algorithms and key sizes.
- [Gmail sender requirements](https://support.google.com/mail/answer/81126): one
  recipient-provider target, to recheck during implementation and qualification.
