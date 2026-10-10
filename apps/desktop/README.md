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
fallback is rejected. OS keychain persistence is not exercised by the smoke test.
Closing the window or disconnecting requests a bounded player teardown and leaves
the independently managed server running. No server process is spawned or killed.
`desktop_owned` mode is rejected until the backend exposes a verifiable protected
bootstrap, readiness and ownership protocol.

Run `npm test` for policy tests and `npm run test:shell` for the production
main/preload/chrome smoke with a disposable loopback fixture and private userData.
The latter checks connection identity before secret exchange, renderer isolation,
HttpOnly sessions, blocked cross-origin navigation, runtime-epoch disconnect and
server survival. It writes a source-hashed receipt in `qualification/desktop` and
retains its temporary screenshot and diagnostic files. Sources are copied byte
for byte to temporary storage to avoid external-volume loader stalls.

The separate `node apps/desktop/proof/run.mjs` command, from the repository root,
qualifies the SSR player with real Demuxe and mocked API services. Neither proof
qualifies a real production backend, durable offline progress, desktop-owned
startup, packaging, signing or updating. SessionStorage progress survives tab
reload but can be lost when the native view closes, especially while offline.
