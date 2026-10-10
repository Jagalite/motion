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

## Implemented production integration

- Opt-in `--topcoat` mounts the real presentation facade. Reads dispatch through
  the authenticated public API router with per-request credentials and verified
  ingress identity, bounded response size, read count and total deadline. Missing
  backend routes return unavailable; they never substitute fixture data.
- Native Start local server owns a spawned child, private bootstrap/readiness
  pipes, verified player assets and the existing exclusive data-directory lock.
  Live identity/epoch/contract validation precedes attachment. Shutdown targets
  only the spawned child. Local state stays under private desktop userData.
- Browser viewing records now use localStorage. Controlled native detach saves
  them with OS encryption, scoped by server/origin and principal, and restores
  them only after verified authentication. Save failure retains the window.
  This checkpoint does not prove unexpected-crash recovery or offline authority.
- Keyboard skip-to-content retains the player; narrow navigation can wrap.

These are presentation/transport/process adapters. They do not create a second
implementation of server viewing authority, authorization or durable ordering.
The real API tests cover adapter authentication; the native integration test
covers actual bootstrap, lock ownership and restart identity.

## Required integration work still open

1. The facade is connected, but committed v2 catalog, preferences, viewing,
   continue-watching, next, playback planning and delivery services are still
   incomplete in this worktree. Their active branches contain uncommitted work.
   Do not import another worker's unfinished files or guess v1/v2 translations.
   Resolve the provisional delivery migration number against committed catalog
   migrations 0015/0016 through the migration owner before integrating it.
2. Qualify real conversion/HLS transitions, viewing conflicts, stale manual
   epochs, restart and queue/next playback against those services. Mock media
   proves client transport behavior, not server authority or production parity.
3. Implement the M5 presentation-only offline helper with the A13 cache/event
   port, then qualify disconnected cold start and causal reconciliation. The
   close-time encrypted online outbox is not a substitute for this cache port.
4. Complete the applicable accessibility, UI load/performance and platform
   matrix. Focused keyboard/narrow-layout checks are only part of those gates.

Packaging, signing, updating and installed-artifact qualification belong to A14
and remain separate release gates. Current Electron runs are unpackaged.

## Reproduction and evidence boundaries

See `packages/ui-bridge/README.md` for client/Rust commands and
`apps/desktop/README.md` for native shell commands. JSON receipts next to this file
record the actual platform, source/artifact hashes, checks and limitations.
A passing mock receipt must not be reported as production API parity, offline
recovery or release qualification. Temporary diagnostic paths in receipts are
local evidence and are not shipped product dependencies.

## Latest local verification

Current source has passed 35 focused JavaScript tests (18 bridge, 17 desktop), 23 real access/API
integration tests, 16 presentation composition tests and four production facade/asset-verification unit tests. Earlier in this run,
all 9 catalog persistence and 14 catalog workflow tests passed after integrating
schema migrations 0015/0016. The real owned-server receipt records four passing
bootstrap/locking/restart checks. The follow-up native run reached app readiness but timed out before connection
checks completed; its receipt is failed. The latest playback attempt also timed out at 180 seconds before any
observations (see latest-player-attempt.json); the older passing playback receipt
does not qualify current source. Staging the pinned Electron runtime internally
then failed with ENOSPC. The failed temporary runtime copy was removed; no user
files were deleted. Rust formatting, JavaScript syntax and diff checks passed.
No full A11/A12 completion is claimed.

## Follow-up lifecycle review

Repeated close/quit requests now remain blocked until persistence and owned-child
shutdown succeed. Failure returns the gate to a retryable state; it never grants
permission to close. A startup result superseded before attachment is stopped,
and a superseded view is checked before it is attached. Disconnect persistence
errors appear in trusted chrome and leave its control retryable.

Five regression tests execute the production lifecycle adapter and chrome code,
covering duplicate close, save failure/retry, cancellation after readiness,
attachment failure/success, and disconnect error reporting. These rules concern
Electron process/window ownership and remain in the native JavaScript adapter;
server authorization and viewing ordering remain production core/API rules.
The JavaScript suites (35 tests) and syntax/diff checks pass. Native GUI qualification
remains separately gated; unit tests do not establish actual Electron event order.
