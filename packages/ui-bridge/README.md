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
