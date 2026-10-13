# Authentication sign-in qualification

The A1–A5 implementation is delivered in PR [#38](https://github.com/Jagalite/motion/pull/38),
stacked on shared access/TUI PR [#36](https://github.com/Jagalite/motion/pull/36).
OIDC and direct email are opt-in and remain disabled by default. No live database,
external provider, real mailbox, production migration, PR merge or deployment was used.
The user approved isolated local services for final provider/mail qualification.

## Source and correctness boundary

Native qualification used `6868a4c0258f56d2ec1526228dfc6bded8c4300c`.
The earlier application source review accepted `ad0b481`; its application changes after
that native-tested commit are authentication JavaScript and tests: recovery safeguard explanations,
no stale-revision retry advice for those safeguards, and immediate email-test
status refresh. Follow-up `c85b07f` adds only the reviewed callback fault fixture
and two adapter tests. Final source review accepted `1a75385`, including the
contrast/count wording fixes and accelerated executable hashing described below.

The branch integrates access/TUI head `1a10cd4` through merge `5d0c088`.
Migration 0045 is unchanged. Forward migrations 0046/0047 reconcile legacy
ownership and add authentication persistence. Existing paired-device credentials
and device-scoped playback/viewing ownership remain intact.

Production core owns caller provenance, effective-grant intersection, linking
ceilings, transaction purpose/browser/epoch/revision fences, freshness and session
deadlines, one-time consumption and recovery safeguards. Adapters verify OIDC
observations, constrain network destinations, encrypt secrets and deliver mail.
SQLite commits exchange reservation before network I/O and atomically consumes
proof/challenge state with identity or family issuance. An interrupted exchange
fails closed. Executed email adapter tests separately check durable attempt
budgets, one-time issuance and family revocation. The additional OIDC callback
reservation/cancellation tests passed. Model exploration does not prove SQLite
or network conformance.

## Completed native and contract checks

Kyoto compiled the affected suites with `--no-run` in job
`f0ded6c31aba4eeb921eea0d29721e9a`, then separately executed the emitted binaries.
**346 tests passed, zero failures**, across 14 executables:

| Target | Passed |
| --- | ---: |
| Core library | 138 |
| Sign-in model | 11 |
| Caller model | 1 |
| Core ownership | 6 |
| Access administrator model | 4 |
| Sign-in API | 15 |
| Playback API / real FFmpeg delivery | 28 |
| Contract | 5 |
| Downloads API | 5 |
| Server ownership | 2 |
| Server library | 73 |
| Real HTTP/JWKS/JWT protocol | 1 |
| Access API | 55 |
| UI library | 2 |

Execution job `f281a2148daa423bae8d0d8cafe3b977` passed the first six targets,
then was cancelled when macOS stalled the server executable at `_dyld_start`
during the FFmpeg supervisor warmup. No application code had run in that stalled
process. An identical SHA-256 artifact copy launched normally. The target path
was temporarily pointed at that copy; resumed Kyoto job
`8c1380692d114d389ffa7734b682a741` verified all test-binary hashes, skipped only
already-passed identical executables, and passed the remaining targets. The
original target was restored afterward. The receipt records this host workaround;
it is not presented as an uninterrupted first-attempt pass.

The suites cover populated legacy ownership migration, grant ceilings, explicit
operator/credential/ingress authority, stale transactional and SSE callers,
active conversion shutdown after user disable/narrowing, independent method
families, enrollment/linking ceilings and freshness, last-administrator/method
protection, restore hold and old-family invalidation. Email cases include generic
responses, browser binding, POST-only redemption, one-time issuance, attempt and
network limits, revision/epoch changes, outage retry and encrypted-payload erasure.
Protocol cases exercise wrong issuer/audience/nonce, expired/old authentication,
wrong authorized party/signature/key/PKCE and key rotation using real HTTP and JWTs.

Stateless runs the production decisions with graph exhaustion asserted:

- For each of five purposes (login, link, test, reauthentication, enrollment),
  OIDC explores 5,760 states / 59,328 transitions and email 30,720 states /
  377,856 transitions. Bounds: one transaction, two browsers, explicit epoch and
  provider/user/identity/actor revisions, availability failures, time 0/599/600,
  attempts 0–5; limits 50,000 states, 1,000,000 transitions, depth 20. Exact
  acceptance and effect identities/counts are asserted; no checks were skipped.
- Access administration explores 2,808 states / 50,544 transitions with two
  administrators, one pairing and one invitation/two claims, issuer loss,
  user revisions, replay, revocation and expiry.
- Caller exploration asserts 192 states and graph exhaustion for one
  device/parent/session chain, explicit caller sources and time 0/99/100.
  Limits are 1,000 states, 20,000 transitions and depth 20.

Follow-up Kyoto compile `c323f9a563c74b2d9081085163dcc157` and execution
`daaa8259a6a4409ab3745e1b1b0f09f4` passed all **17 sign-in API tests** on
`c85b07fb2ec3bd91dfb0e339edb4fe2504f3f403`, including two new tests. Together
with the unchanged suites above, this is 348 distinct native tests across the
recorded source checkpoints, not 363 distinct tests. The new real HTTP fixture
holds token exchange while an independent SQLite reader observes the committed
reservation. A wrong-browser callback leaves it pending; a concurrent duplicate
cannot start another exchange; successful provider-test completion publishes one
audit effect and no login family. Aborting the first request, reopening the actual
database and reconstructing runtime preserves the reservation and fences replay
through combined reservation/epoch checks. This is cancellation/restart fault
injection, not OS process-kill or concurrent ordinary-login-family qualification.
The first added-test compile failed because Reqwest's optional JSON helper is
not enabled; the fixture now parses response bytes with the existing JSON library.

All 61 browser-bridge tests passed on `ad0b481` (18 authentication-specific).
Contract validation covers 142 paths, 201 operations and 213 schemas. Current-head
CI contract, client controllers/desktop host/Go terminal, and notices checks pass.
Kyoto Go job `9ad5620dc3f54d89b235171c7bf435d7` also passed the API/TUI packages on
the earlier implementation snapshot. CI run `38427205893` passed formatting,
Clippy and fast tests on `6868a4c`; run `38427205825` passed real-process integration,
upgrade and restore. Its heavy/package jobs were superseded by the next push,
not counted as passes. Final-head CI results are recorded separately below.

## Isolated browser walkthrough

The collaborative browser exercised Motion at loopback port 8950 against genuine
`oidc-provider` 9.12.2 at 8951 and an SMTP sink at 8952. The tested server artifact
was built by Kyoto job `955a33510e5449d58669d43f9e47a004` from `6868a4c`, SHA-256
`3ac02d7cd41a7e54b3db0eac33ea6a73ad918d527c3592e091c3357dcd91dded`.
Operator credentials stayed outside the browser; the local operator only bootstrapped
pairing and independently approved displayed action codes. Configuration changes
used ordinary administrator screens and real APIs.

Completed interactions:

1. Pair a browser, configure public origin, create a disabled provider draft,
   run configuration and genuine browser tests, then explicitly enable it.
   Each sensitive step required fresh action approval and explicit resubmission.
   Saved secrets stayed blank with retain selected; reads expose presence metadata.
2. Unknown Alice OIDC login produced a pending request and no application access
   (`GET /me` 401). An administrator created a bounded user and approved the exact
   issuer/subject. A fresh Alice login issued an owned non-administrator family.
   Direct administrator API and screen access failed.
3. Alice selected the permitted Everyone profile/library/title and played the
   20-second H264/AAC fixture in Demuxe: advancing time, readyState 4 and decoded
   frames were observed; playback reached the end with 484 decoded frames.
4. Self-service blocked removal of the last usable identity. Revoking the current
   family produced the session-ended screen and `/me` 401. A fresh OIDC login
   succeeded despite the lingering revoked cookie and issued a different device.
5. Administrator UI configured disabled SMTP, explicitly sent a test to the sink,
   then enabled email. Existing administrator authority plus a fresh provider
   proof explicitly linked the OIDC identity; email equality never linked it.
6. Fresh administrator OIDC login allowed explicit email enrollment. An incorrect
   code failed; the delivered code linked the method without issuing another
   family. Public email login then issued its own administrator family.
7. Two administrator tabs held the same user revision. The first approved save
   committed revision 2; the second stale save remained rejected after fresh
   approval, with revision 2 unchanged. No automatic replay occurred.
8. Administrator UI last-admin removal was blocked. Administrator revocation of
   Alice's credentials revoked both owned browser devices and advanced user revision.
9. With both IdP and SMTP stopped, OIDC failed closed, email start remained generic,
   and pairing request/approval plus local operator recovery remained usable.
   After SMTP restart, the original challenge was delivered on attempt 2 with
   the same code hash and erased encrypted payload, without editing retry times.
   Browser-context redemption succeeded once; replay returned `signin_already_used`.
10. Keyboard Enter expanded an edit disclosure; Tab reached its labelled field.
    A DOM audit found no unlabelled administrator inputs, selects or buttons.
    Expired freshness prompted a new action-specific approval; refresh alone
    did not renew authentication. The five-proof-per-device limit was observed.

Browser media evidence uses the existing Demuxe 1.1.0 fixture bundle (266 verified
files, archive SHA-256 `ddb82eb5dfaa94ca2c806c974bab4e45785e835b0264cd3abab160969f60f66f`).
Direct MP4 playback passed. The bundle lacks the Shaka adaptive provider: its
bundle probe has one pass, one failure and two explicit shared-runtime skips.
This is not browser adaptive-HLS or full player-package qualification. Native
paired playback and active FFmpeg delivery are covered by the 28 passing API tests.

## Independent review and evidence

GPT-6.1 Sol reviewed implementation commits, focused corrections, the shared TUI
integration and final `origin/main...ad0b481` diff. No actionable source findings
remain. The final evidence ledger and plan status were also reviewed and accepted.
Findings resolved include linking grant widening, freshness on device
admin elevation, enrollment evidence deadlines, disabled owned-email re-enrollment,
proof response schema, key/AAD documentation, authentication-time session deadlines,
stale-cookie fresh login, and misleading safeguard retry UI. Review is separate
from executed qualification.

Raw logs and private fixture state remain in the sibling
`motion-auth-signin-artifacts` directory. Relevant receipts include
`native-6868a4c-receipt.json`, per-target native logs, `browser-progress.json`,
`supervisor-launch-workaround.json`, the warmup process sample, and before/after
outage challenge receipts. Fixture secrets, mailbox contents and database files
are not committed. Source snapshots and artifact hashes bind the evidence.

Earlier compile/CI failures were corrected, not counted as passes: UI macro
parameter, caller-model transition initializer, SSE fixture event kind, OIDC
closure ownership, Clippy findings, and three fixture assumptions (fresh invitation
proof, JSON boolean, and anonymous identity state). Earlier Kyoto builds were
cancelled or deferred under host pressure; their partial logs do not qualify runtime.

## Presentation qualification and release boundaries

T3 navigation, DOM reads, clicks, typing and keyboard interaction worked, but
screenshot capture and viewport resizing repeatedly failed. The user then
explicitly authorized an isolated headless browser. The final Chromium checks
below complete the plan's keyboard, accessible-label and narrow-layout checks.
A manual screen-reader walkthrough was not performed; this is not a claim of
comprehensive accessibility conformance. No installed desktop/native OIDC
walkthrough is claimed. The scope preserves native pairing rather than adding
native OIDC. Production provider interoperability, real mailbox delivery and
deployment remain outside the approved isolated qualification.

## Final rebuilt presentation

The successful `c323f9a563c74b2d9081085163dcc157` compile also emitted the
non-test server executable. Its preserved artifact from `c85b07f` has SHA-256
`c0c38b055a9833e3960d241ec2ff441ba7bfef6984b08ae6b6289fd42a14e98e`.
The isolated server was restarted with that artifact and the same database/key.
The browser loaded `auth.9299b72a98df.js`. Last-administrator disable displayed
specific recovery guidance without a stale-state panel. Explicit test delivery
to the sink immediately refreshed the shown timestamp and named the destination,
without a document reload; the SMTP sink independently received the message.
Receipts: `server-c85b07f-receipt.json` and `browser-final-ui.json`.

The earlier rebuild `dda1425e1b8b459495e26db71e1dd1cc` was deferred four times
and superseded by this successful compile. A test-run submission with an incorrect
expected commit was rejected by the receipt guard before any executable ran;
the corrected execution above passed. These are not counted as runtime passes.

During the later denied-account walkthrough, the old isolated server exited with
SQLite code 5 (`database is locked`) from its worker group under host storage
contention. The precise worker was not identified; no causal attribution to
sign-in is claimed. After restart the database passed integrity and foreign-key
checks, the committed pending request still granted no access, fresh email login
worked, and administrator UI denial persisted. The interruption remains a
stability observation, not a clean uninterrupted walkthrough pass.

## CI timeout investigation and final executable

CI integration runs `38435524333` (`c85b07f`) and `38437953691` (`c50da03`)
failed smoke and operations on ten-second first-call capabilities timeouts;
their other integration checks passed. Receipts/logs are preserved. The earlier
Kyoto rerun `94c55be85dbb43e09f5fa1e0d0b36b79` passed the unchanged scripts on
`c85b07f` in 6.539 / 12.189 seconds. An unprofiled first capabilities call took
6.099 seconds versus 0.228 seconds warm. Sampling a later cold request captured
software SHA-256 on an async worker; only two samples were captured, so no
statistical dominance claim is made. That profiled request took 10.266 seconds.

`c9885e1` enables the maintained `sha2` accelerated implementation and moves
both executable reading and hashing into `spawn_blocking`. The cached SHA-256
identity and failure behavior remain unchanged. This is derived artifact metadata
and adapter scheduling, not a domain-state transition. Dependency notices include
`sha2-asm`. No assertion or HTTP timeout was weakened.

The final application source is `1a753851f0b8b38c85dc1887df845f44a08487e8`.
Kyoto compile `cfc8a610f6bf411a80c59d99925682f9` succeeded after one pressure
deferral. Separate execution `f3d3a79ba9a24e3490e614151ab35b35` passed all
**98 affected native tests**: server library 73, sign-in API 17, protocol 1,
contract 5, UI library 2. These repeat tests in the 348-distinct-test total above;
they are not 98 additional distinct tests. Bridge tests passed 61/61 after the
final wording change. Structural/JSON contract validation passed 142 paths,
201 operations and 213 schemas.

The final preserved server is 131,397,800 bytes, SHA-256
`5985ac53394947f477cd6210a4fa28e6e4b253d696210b7f8c244ca128bccfc1`.
Its cold capabilities request completed in 0.9187 seconds, followed by
0.0049 / 0.0043 seconds warm. All three reported digests matched an independent
Python SHA-256 calculation. Kyoto execution `b59cd3a5e0044750839b4535089b16b4`
passed the unchanged smoke and operations scripts in 1.638 / 2.316 seconds.
The CI rerun remains a separate gate; local latency evidence alone does not
establish the remote runner's causal diagnosis.

Receipts: `server-1a75385-receipt.json`, `native-1a75385-receipt.json`,
`capabilities-1a75385-timing.json`, `integration-1a75385/receipt.json`,
`cold-capabilities-profile.json` and `cold-capabilities-sample.txt`.
Superseded compiles `756c536fdff344df9be5fc33fe68e23a` (pressure deferred) and
`717a0104a068438b90fb7bcd33cd34ec` (33-second cancellation to preserve JSON
formatting) are not counted as passes. The final JSON differs only in the new
administrator-device count description; API shape is unchanged.

## Final headless browser evidence

The user-approved isolated Chromium 149.0.7827.55, Playwright 1.61.1 and axe-core
4.11.1 exercised the recorded `1a75385` executable. Fresh genuine OIDC and email
logins in separate browser contexts each returned authenticated `/me` and loaded
My sign-in methods for the intended administrator. No browser context received
the operator token. Codes were read only from the isolated SMTP sink.

All **27 viewport/state audits** passed with zero reported axe violations,
zero page JavaScript errors and no document horizontal overflow:

- `/signin`, `/settings/access` and `/settings/signin` at widths 1440, 390 and
  320 CSS pixels, each 1000 pixels high.
- All eight administration sections at 1440 and 320 pixels, with their available
  detail/disclosure forms expanded, including provider secrets, user grants,
  invitations and destructive confirmations.
- Incorrect email-code recovery at 390 and 320 pixels, including its accessible
  alert and focus. Chromium exposes the alert as a polite, atomic live region.

Screenshots were inspected at desktop and narrow widths. The initial audit found
sidebar section-heading contrast of 3.62:1; `859b7f1` switched those headings to
the existing muted color and the rebuilt audit passed. `93b9e13` / `1a75385`
clarify that usable administrator counts are devices, not independent people or
recovery accounts; the final browser displays the explanation.

Keyboard assertions checked visible Skip to content, Enter moving focus to main,
Enter toggling disclosure state, and Tab reaching the labelled User name input
with a visible 2px outline. The browser accessibility tree exposes the expected
headings, controls, labels and live regions. Axe ran WCAG 2 A/AA and 2.1 AA rules.
Nine audit states retained incomplete contrast evaluations on fixed mobile brand
or navigation labels because axe could not resolve the overlapping backdrop.
Follow-up inspection identified those labels, screenshots showed them readable,
and worst-case white-behind-backdrop calculations give 5.779:1 for muted labels
and 12.497:1 for the brand. These manual checks are recorded separately from the
automated result. Physical mobile hardware, touch behavior, other browser engines
and a manual screen reader were not qualified.

Evidence is under `motion-auth-signin-artifacts/headless/1a75385/`:
`final-report.json`, screenshots, `final-accessibility-tree.json`,
`contrast-review.json`, `contrast-manual.json` and `fresh-logins.json`.
Harness scripts are in the parent `headless/` directory; private browser storage
and mail stay outside Git. GPT-6.1 Sol accepted the implementation and each final
fix; review is not substituted for runtime evidence.

This ledger records completed local qualification at the application-source
checkpoint above. GitHub checks and the PR description track the subsequent
final-head CI outcome separately; a queued or running job is not a pass.
No PR merge, deployment or fully qualified production release is claimed.
