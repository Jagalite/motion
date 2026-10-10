# Topcoat and desktop acceptance matrix

This matrix separates A11/A12 implementation from backend, device and release
qualification. A passing fixture test never establishes production service parity.
The current native target is macOS arm64, unpackaged Electron 44.7.0/Chromium
152.0.7977.130. Linux headless is a server target; Windows desktop and other native
architectures are not qualified here.

| Requirement | Current evidence | Boundary / remaining gate |
| --- | --- | --- |
| Authorized SSR, hidden items, escaping, read-only rendering | `tests/access_api.rs` real SQLite/v2 catalog tests; `motion-ui/tests/compose.rs` | Preferences, viewing, continue/next, scan and playback APIs are not integrated in this snapshot. Their absence is explicit. |
| API remains JSON, range media unchanged | Composition/access tests; real owned-server receipt; concurrent HTML + range proof | Production conversion/HLS remains a separate service gate. |
| Strict CSP, external hashed assets, exact Demuxe tree | Composition tests and `desktop/topcoat-electron-proof-darwin-arm64.json` | Native-only reduced Demuxe bundle; no Wasm/WebCodecs qualification. |
| Client commands, retries, preconditions and conflicts | Production JS adapter tests; native proof's scan/conflict commands | Mock scan transport; real source/library creation is covered by access API tests. |
| Playback, seek, replacement, leases, navigation, teardown | Native proof: 26 passing checks; focused JS interleaving tests | Real Demuxe/media with mock viewing/delivery authority. No production conversion/authority parity claim. |
| Keyboard, labels, accessibility tree, reflow, reduced motion | Nine screens at 320 and 1280 pixels, one main/H1, accessible control names; skip link and player controls | Physical screen reader, switch input and full manual keyboard traversal remain device qualification. |
| Render load and media coexistence | 24 simultaneous HTML requests plus HTTP 206 read, recorded latency/HTML sizes and playback process snapshot in native proof | Small mock dataset and one machine. Large real catalog, sustained CPU/RSS, playback stalls and production capacity remain performance qualification. |
| Local server ownership and bootstrap | `desktop/owned-server.json`, real binary, isolated database, one-use inherited-pipe secret, lock/restart tests | Signing, packaging, updates and installed paths are A14 gates. |
| Remote renderer containment and connection fencing | `desktop/native-shell-darwin-arm64.json`, production main/preload/chrome | Loopback remote fixture; physical remote/Tailscale deployment remains deployment qualification. |
| Encrypted progress checkpoint and crash recovery | JS recovery tests and native reconnect test | Abrupt owned-server restart on a different port can leave browser-only observations after the last native checkpoint at the old origin. |
| Offline cold start, reusable Topcoat views and helper containment | `desktop/offline-cold-start.json`: nine checks, real helper + Demuxe, restart resume and parent-pipe loss | Synthetic A13 cache. Transfer, sidecars, quotas, compaction and causal server reconciliation are A13 integration gates. |
| Offline sequence/identity correctness and durable ordering | Production `core::offline`, Stateless 1,000 cases/50,000 transitions, four-record bound; cache integration tests | Model covers admission, not filesystem conformance; adapter tests separately cover publication failure, retries, confinement and restart. |
| A/V and release output | No claim | Audible output, A/V sync, color/HDR, installed/signed package and rollback/update matrix remain physical/A14 qualification. |

Reproduce with the commands in `desktop/A11-A12-status.md`,
`../apps/desktop/README.md` and `../crates/motion-ui-host/README.md`. Receipts carry
artifact/source hashes and their own limitations. Historical failed attempt files
are diagnostic history, not the latest passing receipt.
