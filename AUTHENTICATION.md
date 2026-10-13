# Browser sign-in and recovery

Motion keeps device pairing available. OIDC and direct email are optional and
start disabled. A viewing profile is not a login account. Each successful browser
login creates a distinct owned device/session family; permissions cannot exceed
its local user's ceiling or the linked method's recorded restrictions.

## Administrator setup

Open **Settings → Authentication & Access** using an authorized administrator
session. Configure Motion's exact public origin first. Use HTTPS in deployments;
plain HTTP is supported only for explicitly pinned isolated loopback testing.
The configured origin must match the server's configured origin. Configure your
reverse proxy to redact authentication callback query strings from access logs.

Create a disabled provider draft with its exact issuer, client ID and client
secret, then register the displayed callback URL at the provider. Motion uses
OIDC authorization code flow with PKCE and does not request offline access.
Run the configuration test, complete the administrator-bound browser sign-in
test, then explicitly enable the provider. A test login grants no application
access. Extra network destinations require explicit origins; private providers
also require explicit address pins. Redirecting discovery/token/JWKS responses
are rejected. Provider names and enabled status are public; configuration and
saved secrets are administrator-only.

Unknown verified provider identities create pending access requests. Review the
exact issuer and subject, choose an explicit local user and approve or deny.
Approval is an audited recovery/admission decision, never an email-name match.
The applicant must sign in again after approval. Provider email and display name
are display metadata, not account-linking keys.

Direct email setup is independent and opt-in. Configure the SMTP host, port,
sender and credentials. Production delivery uses TLS. Plain SMTP requires the
explicit loopback-test setting and a literal loopback address. Send a test only
after selecting its destination; successful delivery does not authenticate the
recipient. Enable email after a successful configuration test. An already owned,
authenticated user enrolls an address from **My sign-in methods**, proves recent
local authority, and enters the code delivered to that address. Ordinary login
never enrolls an unknown address. Address matching is case-insensitive ASCII;
Motion does not remove dots or plus tags.

Secret edits have retain, replace and clear operations. Saved values are never
returned to the browser. Changing provider or mail configuration invalidates its
tests and relevant method sessions; test the new configuration before enabling.

## Sessions, linking and freshness

Browser sessions last at most 12 hours from fresh authentication. Refreshing a
page does not renew that interval or create fresh proof. Sign in again after
expiry. The session cookie is HttpOnly and SameSite=Strict (Secure with HTTPS).
A separate 10-minute HttpOnly SameSite=Lax cookie binds OIDC callbacks and typed
email transactions; it does not authenticate ordinary application requests.

Sensitive changes require proof within five minutes: a recently authenticated
owned browser session, or an explicit action-bound approval from a different
authorized device/local operator. The approver must still be authorized when
the action commits. A device cannot approve its own request. Enter and compare
the displayed approval code, then explicitly retry the intended action. Pending
link/enrollment transactions expire with their initiating proof and capture
user, device, method and server revisions. Stale or repeated completion cannot
widen authority or mint another family.

Linking intersects the initiating device's rights, the local user's ceiling and
any existing method restrictions. A restricted device owned by an administrator
does not become an administrator by enrolling a new method. Identities cannot
be moved between users. Remove or replace methods only with another viable
method or an explicitly verified local recovery route.

Email challenges expire after ten minutes, allow five incorrect attempts and
work only in their initiating browser. GET requests cannot consume them. Requests
for unknown, disabled and eligible addresses have the same outward result.
Limits apply per address, browser, transport network peer and deployment. Motion
ignores forwarded headers when choosing the network bucket; reverse-proxied
clients share the proxy peer's bucket. Delivery retries resend the same code.

## Revocation and outages

Sign out or revoke a browser session to invalidate its family and parent device
credentials. Revoke a paired device to invalidate its credential descendants.
Disable a user to deny all owned devices, sessions and pending enrollments;
re-enabling does not revive revoked credentials. Disable/unlink a method to
invalidate its pending transactions and authenticated families. Independent
pairing credentials remain usable unless their user/device is also revoked.

Permission changes and revocation are checked at API admission, transactional
writes, event disclosure and ongoing delivery authorization. They cannot erase
media already downloaded or delivered. Upstream provider logout/disable has no
back-channel notification in this implementation: an existing local session can
remain valid until its 12-hour expiry. Use Motion's local revoke controls for
immediate local invalidation. Provider/mail outages do not disable pairing or
the protected server-local operator recovery mechanism.

## Storage, backup and restore

Provider/mail secrets, PKCE verifiers and pending mail payloads use authenticated
encryption with one encryption subkey derived from Motion's external credential
key and purpose/entity-bound authenticated associated data. Challenge HMACs use
separate purpose domains. Keep that protected key file separate from database-only backups; losing it
prevents decrypting saved authentication configuration. Preserve its restricted
filesystem permissions. Codes are keyed hashes in transaction rows; encrypted
outbox plaintext is erased after delivery, cancellation or expiry. Delivery uses
bounded attempts and durable leases. Audit records contain identifiers and
outcomes, never codes, tokens or provider secrets, and retain at most 10,000 rows.

A database restore rotates the restore epoch. Pre-restore browser families and
pending sign-in transactions fail, and new OIDC/email sign-ins remain held until
the local operator reviews restored users, grants and provider settings and
explicitly acknowledges the restore. Do not paste the local operator secret
into browser forms. Legacy paired credentials retain the existing restore
semantics: restoring an old database can restore historical credential rows;
review and revoke them during recovery. Epoch rotation alone does not make old
paired credentials safe.

Migration 0046 detaches only untouched synthetic legacy ownership introduced by
0045. Deliberately managed ownership remains intact. Migration 0047 creates the
sign-in tables. Back up the database and separately protect its credential key
before migration; never edit an already applied migration.

## Qualification fixtures

`qualification/auth-signin/idp` contains a genuine local OIDC provider fixture;
`qualification/auth-signin/mail.py` captures SMTP messages on loopback. Use only
isolated databases, mail directories and fixture credentials. Current evidence
and outstanding gates are recorded in `qualification/auth-signin/README.md`.
