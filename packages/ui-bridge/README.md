# Motion browser bridge

`bridge.js` submits server-rendered command forms to the public API. Run its
focused regression tests with `node --test packages/ui-bridge/bridge.test.mjs`.
Run presentation tests with `cargo test -p motion-ui`.

Pending idempotency keys belong to the request's operation, precondition and
body. They survive uncertain responses, form edits and reloads in the same tab
using session storage, with an in-memory fallback when storage is unavailable.
Success or a definite rejection retires the key. Duplicate submissions of one
form are suppressed while its request or pairing flow is active. Pairing keeps
the claimed credential only in memory and retries session exchange without
claiming again after a successful claim.

These are client transport lifecycle rules, so they remain in the browser
adapter. Authorization, match-decision validity, durable idempotency and job
transitions remain server/core responsibilities; disabling or hiding a form is
not an authorization boundary. The tests execute the production bridge with
controlled responses and storage. They cover retry identity, rejection classes,
reloads, malformed storage, duplicate submissions, source-list encoding and
pairing recovery. They are not Stateless model checks or proof of server-side
atomicity. No production core rule is changed here.

The Electron smoke proof is `node apps/desktop/proof/run.mjs` from the repository
root. Set `MOTION_DEMUXE_DIR` to an installed Demuxe package when this worktree
does not contain one. It rebuilds the proof server and verifies the Demuxe
installation receipt. Its API services are mocks: the receipt does not qualify
real backend integration, production desktop ownership or offline lifecycle.

The runner launches a byte-identical copy of the built proof server from the
host temporary directory to avoid the observed macOS loader stall on the
external checkout volume. Both SHA-256 values are checked before launch and
recorded in the receipt. Server readiness is bounded to 30 seconds and the
Electron run to 120 seconds; child processes are stopped on success or failure.
Temporary diagnostic artifacts are retained. These launcher mechanics belong
to the qualification adapter and do not change application/core transitions.

## Player delivery leases

`player.js` renews the currently owned delivery with its exact active generation,
using the server's heartbeat interval. Only one renewal is in flight. Uncertain
transport failures retry within the last confirmed expiry; revocation, changed
delivery/generation, invalid lease data or expiry stop playback and retire the
delivery. Leaving the page aborts renewal, and the player epoch fences late
responses. Run `node --test packages/ui-bridge/*.test.mjs` for both bridge suites.

Lease eligibility, authorization and durable expiry remain server/core rules.
The browser owns transport scheduling, cancellation and conservative local
teardown; it does not grant itself a renewed lease. Controlled-clock tests run
the production scheduler through success, retries, expiry, revocation, identity
mismatch and stale callbacks. They do not establish server conformance or replace
Stateless checks of the server's lease policy. The Electron proof separately
requires an actual heartbeat command with the expected delivery/generation.

## Remaining A11/A12 scope

The branch has authorized mock-backed Topcoat screens and a local-origin Electron
smoke harness. The next playback work is viewing-session admission with an
explicit viewing revision, ordered progress with exact retries, generation
activation/replacement, and navigation flush. The current bridge does not save
viewing progress. Full typed playback coordination is still outstanding.

Real `UiQueryFacade` integration and API/SSR parity need the backend owners'
production interfaces. The desktop directory remains a proof harness: packaged
trusted connection chrome, verified local attachment, server-scoped remote
sessions, ownership-aware shutdown, and offline-host lifecycle are not implemented
by it. Packaging, update/signing and offline cache integration require their
respective owners and separate qualification. Passing the smoke receipt does not
complete these plan sections.
