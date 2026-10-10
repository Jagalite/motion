# A11/A12 client implementation and integration status

This worktree implements the Topcoat SSR/mock-facade presentation, command
bridge, Demuxe playback transport coordinator and an isolated Electron server
connection shell. It is not an end-to-end production completion receipt.

## Implemented in this branch

- Authorized SSR screens and content-hashed external browser modules.
- Command retries with stable idempotency identity, preconditions and duplicate
  submission protection; pairing exchange recovery.
- Active-generation lease renewal with single-flight retries and expiry teardown.
- Viewing-session admission, persisted-before-send ordered progress, exact event
  retries, stale-owner fencing and rejected-authority archiving.
- Bounded candidate trials preserving explicit pins; logical seek and
  quality/version replacement with preparation, activation reconciliation and
  candidate disposal. Native playback preferences survive replacement.
- Bounded navigation/native-close progress flush and delivery retirement.
- Trusted Electron connection chrome, server/epoch/contract verification,
  server partitions, unprivileged remote view, encrypted credential option and
  attachment shutdown that leaves the independent server running.

Client scheduling, renderer ownership and host connection policy stay in the
JavaScript adapters. Authorization, viewing authority, durable ordering,
generation/resource admission and activation must be enforced by production
server/core services. The mock proof server is only a transport fixture. This
branch does not add or claim Rust core model/Stateless properties.

## Required integration work still open

1. Connect `UiQueryFacade` and public command routes to the authenticated v2
   backend. The organization adapter at `9844a5c` explicitly records missing v2
   preferences, timeline viewing, continue-watching, next, playback plans and
   viewing/delivery sessions. The A07 delivery surface remains experimental v1.
   Do not translate between these contracts by guessing authority semantics.
2. Qualify real conversion/HLS generation transitions and viewing conflicts,
   including stale manual epochs and server restart. Mock range media does not
   prove these behaviors. Queue/next playback also needs its production service.
3. Define and integrate protected desktop-owned bootstrap, readiness and process
   ownership. The current shell supports remote and service-owned attachment,
   rejects desktop-owned mode, and never launches or kills a server process.
4. Integrate the A13 durable offline host/outbox. Current sessionStorage supports
   same-tab reload only; closing a browser/native view can lose pending progress.
   The bounded final send is not a durability guarantee.
5. Complete A14 packaging, signing, updates and installed-artifact qualification.
   Current Electron runs are unpackaged and unsigned.

## Reproduction and evidence boundaries

See `packages/ui-bridge/README.md` for client/Rust commands and
`apps/desktop/README.md` for native shell commands. JSON receipts next to this file
record the actual platform, source/artifact hashes, checks and limitations.
A passing mock receipt must not be reported as production API parity, offline
recovery or release qualification. Temporary diagnostic paths in receipts are
local evidence and are not shipped product dependencies.

## Latest local verification

On 2026-10-09 (America/New_York), the final client suite passed 37 tests and
`cargo test -p motion-ui --locked` passed 16 presentation tests. The Electron
playback receipt passed all 21 checks, including actual range playback, seek,
ordered viewing commands, generation replacement, lease renewal and retirement.
The native shell receipt passed all seven checks. Both Electron receipts use
mock services and the scope limitations above still apply. Rust formatting and
`git diff --check` also passed. The player run uses a 180-second bound and emits
incremental checkpoints; an earlier 120-second run timed out during this host's
slow process startup and is not counted as a passing run.
