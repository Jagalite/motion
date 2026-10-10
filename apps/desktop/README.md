# Motion desktop connection shell

Run `npm ci` and `npm start` in this directory. Connect using an exact server
origin, expected server ID and a paired device credential. Remote origins require
HTTPS; loopback HTTP can attach to an independently managed local service.

The trusted connection window verifies health identity before exchanging the
credential, then verifies runtime epoch and the pinned API contract digest.
Server pages run in a sandboxed WebContentsView without Node, preload or native
IPC. Cookies and storage use a partition derived from origin and server identity.
Cross-origin requests, navigation, popups, downloads and webviews are blocked.
A runtime identity/epoch change disconnects the view. Network failure does not
extend playback leases. Explicit connection changes clear the old browser state.

Remembered credentials use Electron safeStorage and a private file. Plaintext
fallback is rejected. The shell smoke includes pending-progress encryption through the OS storage API;
the latest run timed out before exercising it.
unit tests separately exercise identity isolation and failure handling.
Closing the window or disconnecting requests a bounded player teardown and leaves
the independently managed server running. The trusted Start local server action launches the configured server with separate
inherited bootstrap and readiness pipes. The server owns its existing OS data
lock; readiness is checked against live identity/epoch/contract before opening
content. Stop local server and application quit stop only that spawned child.
Arbitrary connection requests cannot claim desktop ownership. Development builds
can set MOTION_SERVER_BINARY and MOTION_DEMUXE_DIR; packaged builds use resource
paths. Local state is isolated under the desktop userData/server directory.

Run `npm test` for policy tests and `npm run test:shell` for the production
main/preload/chrome smoke with a disposable loopback fixture and private userData.
The latter checks connection identity before secret exchange, renderer isolation,
HttpOnly sessions, blocked cross-origin navigation, runtime-epoch disconnect and
server survival. It writes a source-hashed receipt in `qualification/desktop` and
retains its temporary screenshot and diagnostic files. Sources are copied byte
for byte to temporary storage to avoid external-volume loader stalls.

The separate `node apps/desktop/proof/run.mjs` command, from the repository root,
qualifies the SSR player with real Demuxe and mocked API services. The Node `test/server-owned.mjs` integration check additionally launches the real
Rust server in a disposable directory and verifies protected bootstrap, exclusive
locking, stop/restart and runtime epoch changes. It does not establish full
production UI/playback integration, offline cold-start playback, packaging,
signing or updating.

Viewing records use browser localStorage, with migration from sessionStorage.
On controlled native disconnect/close, pending records are encrypted in a private
host outbox scoped to server/origin and principal, then restored after verified
reconnection. The desktop-owned scope remains stable across ephemeral loopback
ports. Old runtime records remain history; restoring them grants no new viewing
authority. Encryption or save failure keeps the window open. Unexpected app or
renderer crashes before this close-time checkpoint remain a recovery gap; this
is not the A13 offline event/cache implementation.
