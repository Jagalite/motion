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
the current smoke passes that check.
Unit tests separately exercise identity isolation and failure handling.
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
authority. Encryption or save failure keeps the window open. On reconnection, a temporary script-free same-origin document recovers the
authenticated principal’s browser records, encrypts them, and restores them before
server page scripts run. Failed encryption preserves the browser copy. A crashed
desktop-owned server may restart on another port, so observations after the last
native checkpoint on the old origin remain a recovery limitation. These online
records do not grant offline viewing authority.


**Open downloads offline** explicitly launches the separate Rust presentation
helper against a private cache root, with one-use bootstrap and a parent lifetime
pipe. The renderer remains sandboxed. Leaving offline mode waits for local event
persistence, then stops only that helper. Remote connection failures never create
an offline host or a library. See [the cache port](../../crates/motion-ui-host/README.md)
for scope, limits and the A13 integration boundary.

`node test/run-offline.mjs` builds and tests cold-start playback, seek, durable
progress, restart resume and parent-pipe loss against a synthetic cache. Use
`MOTION_DEMUXE_DIR` for the installed player. `MOTION_ELECTRON_BINARY` can point to
a byte-identical internally staged Electron runtime when external-volume launches
stall. Current native receipts qualify macOS arm64, Electron 44.7.0, unpackaged.
