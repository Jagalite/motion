# Native email delivery and registration qualification

Scope: [MOTION_NATIVE_EMAIL_AND_REGISTRATION_PLAN.md](../../MOTION_NATIVE_EMAIL_AND_REGISTRATION_PLAN.md)
phases N1–N6 on branch `feat/native-email-registration`. Everything below ran on
isolated loopback services. **No real mailbox, mail provider, public DNS or
port-25 delivery was used**, and no email was sent outside this machine.

## Source and build

Rust builds and test compilation ran through the shared Kyoto queue with the
pinned 1.98.0 toolchain; test binaries were then executed directly.
`cargo clippy --locked --workspace --all-targets -- -D warnings -A clippy::too_many_arguments -A clippy::result_large_err`
(the CI command) and `cargo fmt --check` pass.

## Native test results

| Target | Result |
| --- | --- |
| Core library (includes `mail` and route-combination units) | 179 passed |
| `mail_delivery_model` (Stateless) | 2 passed |
| `registration_model` (Stateless) | 2 passed |
| `signin_model` (Stateless, unchanged bounds) | 11 passed |
| `native_mail` (fixture DNS, STARTTLS, Node DKIM verifier) | 6 passed |
| `signin_api` (real router + SQLite + loopback SMTP) | 25 passed |
| `access_api` | 58 passed |
| `catalog_persistence` (restore clears mail tests/outbox) | 16 passed |
| `contract` | 5 passed |
| Server library | 97 passed, 1 failed (see below) |
| `packages/ui-bridge/auth.test.mjs` | 24 passed |

The one library failure, `scan::inventory_tests::unsupported_directory_names_leave_parent_coverage_incomplete`,
is environmental: macOS APFS refuses to create the test's non-UTF-8 directory
name, so `create_dir` panics before Motion code runs. It is unrelated to this work.

The OpenAPI contract passes `scripts/check_contract.py` (OpenAPI 3.1 validation).

## Stateless exploration (production decisions, graph exhausted)

- **Mail delivery:** 61,714 states, 463,205 transitions. One message, two workers
  with crash, recheck before DATA, accepted/ambiguous/temporary/permanent/withdrawn
  observations, owner completion, configuration/epoch/enablement invalidation,
  times 0/30/lease/lease+30/expiry−1/expiry, attempts 0–6. Properties: bounded
  attempts equal to reservations, lease iff sending, no transmission while invalid,
  acceptance only after transmission, unknown only after ambiguity, terminal
  phases absorbing and payload-free, stale observations change nothing, exact
  recorded phase and back-off, ambiguity never forgotten.
- **Registration:** 26,836 / 35,460 / 31,652 / 34,896 states (130k–183k transitions)
  from each initial policy. Two concurrent registrations for one address, up to
  two policy changes, one invitation (revoke, expiry, issuer loss, consumption),
  concurrent linking by another account, administrator-bearing default grant.
  Properties: one activation per address, one pending request, owner never
  reassigned, completion only under the unchanged captured policy revision,
  owned addresses get only a notice, activation never grants administration,
  invitation completion exact, invite-only requires an invitation, consumed proof
  stable.

Model exploration does not prove SQLite atomicity or SMTP/DNS conformance; the
API and adapter tests exercise those separately.

## Adapter evidence

`tests/native_mail.rs` runs a loopback SMTP server with real STARTTLS (fixture CA
in `tests/fixtures/mail`) behind fixture DNS: exact transmitted bytes verified by
the independent Node DKIM verifier (tampered body, changed subject and an added
From fail; refolding passes); STARTTLS absent, wrong certificate name or
injected plaintext after `220` never sends MAIL in plaintext and fails as a TLS
policy error, while `454` stays temporary; greeting `421` fails over; `550` at
RCPT is authoritative with no DATA and no second exchanger; a missing final reply
is ambiguous; `451` after DATA is temporary; withdrawn eligibility sends no DATA;
mixed-code multiline replies are rejected; Null MX, NXDOMAIN, temporary DNS,
implicit MX and private/link-local/mapped/documentation destinations are
distinguished.

## Real-client walkthrough

`browser.cjs` drove an isolated server (server binary SHA-256
`5ef1ad48c3e869532cfc9f616d1728473fcda6029f29cf51a44355a4c3babff3`, built from the
second commit's sources) with headless Chromium 149.0.7827.55 (Playwright 1.61.1)
and `qualification/auth-signin/mail.py` as the loopback mailbox. The administrator
used a paired browser session; each sensitive change went through the real
action-approval UI, approved by the local operator. Result: **passed**, 10 steps,
no page errors (`browser-report.json`):

1. Closed policy (default) offers no registration.
2. Open registration over the SMTP relay activates usable access.
3. Switching to native direct delivery (loopback qualification route, generated
   DKIM key) while email stays enabled; DNS records are displayed.
4. Direct test accepted; the received message's DKIM signature verified
   independently; receipt confirmed by entering the code in the UI.
5. Approval-required registration stays pending (no session); approval in the
   UI links the verified address; email login then works. Direct-mode mail
   verified with DKIM.
6. Invite-only: an invitation emailed from the UI carries no code; registering
   that address completes the invitation (status `registered`).
7. Delivery activity lists messages without recipient addresses.
8. Relay restored from the UI while enabled; open registration over relay again.

axe-core (WCAG 2.0/2.1 A/AA) found **no violations** and no horizontal overflow
for the registration sign-in page and the Email delivery and Registration
sections at 1440, 390 and 320 CSS pixels.

The walkthrough found and fixed one product issue: once a user's only method is
email, disabling email is refused, which previously made changing the delivery
transport impossible. An explicitly confirmed change can now be applied while
email stays enabled (`apply_untested`).

## Independent review

GPT-6.1 Sol reviewed the implementation three times. The first review found seven
issues (in-flight cancellation violating the outbox CHECK, enumeration via an
unusable sender, key generation before authorization, an evictable activation
counter, transient TLS failures treated as permanent, two contract mismatches);
the second found three (residual enumeration/timing path, transient TLS alerts,
lapsed pending requests). The third found two (the harness could send the
operator token to a non-loopback `MOTION_BASE`; request expiry and pending
observations used different clock reads) and confirmed that `apply_untested`
keeps the test requirement for enabling, restore hold, session revocation and
queued-mail cancellation. All findings were fixed; the affected suites and the
CI clippy command were re-run and pass.

## Remaining limits

See [Remaining limits](../../MOTION_NATIVE_EMAIL_AND_REGISTRATION_PLAN.md#remaining-limits).
In particular, real-provider inbox qualification needs a domain with published
DKIM/SPF/DMARC/PTR records and explicit authorization to send; it was not done.
No manual screen-reader, touch-device or non-Chromium check was performed.
