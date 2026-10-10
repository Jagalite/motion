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
Electron run to 180 seconds; child processes are stopped on success or failure.
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

## Playback coordination

The browser admits viewing authority with the rendered revision, queues ordered
progress before sending, and retries the exact event identity after an uncertain
response. Decimal sequences use BigInt. A rejected authority is archived for
explicit reconciliation; the client does not silently acquire replacement
writing authority. The outbox survives reloads in the same tab via sessionStorage;
it is not a durable offline store across browser or native-window closure.

Candidate trials are bounded to three in automatic quality mode and one for an
explicit mode. Every trial preserves timeline, source and track pins. Generation
changes prepare an overlapping candidate before activation, reconcile uncertain
activation responses, and dispose late candidates. Disruptive replacement is
explicit. Logical seek and quality/version controls use the public change APIs.
The player preserves volume, mute, playback rate and playing intent on replacement.
Initial playback waits for a server-confirmed active generation.

Navigation and native-shell teardown request a bounded final progress flush.
Lease expiry and server authority remain authoritative when transport fails.
These client lifetime, transport ordering and presentation rules live in the
JavaScript adapter; server/core remains responsible for viewing authority,
generation admission and activation atomicity. Tests execute the production
coordinators with controlled event interleavings, not a duplicate domain model.
No additional Rust core or Stateless coverage is claimed.

Run all focused client tests:

```sh
node --test packages/playback/*.test.mjs packages/ui-bridge/*.test.mjs apps/desktop/test/policy.test.mjs
```

## Integration limits

The Electron playback proof uses mock viewing/delivery services. It tests real
Demuxe opening, playback, replacement and browser commands, but does not establish
production backend correctness. HLS replacement, real conversion workers and
cross-process offline recovery need separate qualification. The native connection
shell and its test instructions are documented in `apps/desktop/README.md`.

Real `UiQueryFacade` integration and API/SSR parity require the backend's
production interfaces. The API branch currently lacks v2 preferences, timeline
viewing, next/continue-watching, playback plans and viewing/delivery sessions.
The A07 delivery implementation is still an experimental v1 surface.
Desktop-owned startup additionally needs a protected bootstrap/readiness and
process-ownership protocol. Offline-host integration, packaging, signing and
updates belong to their planned integration stages and remain unqualified.
