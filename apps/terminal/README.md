# Motion terminal client (A10)

`motion` is the CLI and Charm TUI over the public Motion API v2
(`contracts/Motion_Server_API_v2.yaml`). It has no database, scanner,
provider or FFmpeg dependency; `boundary_test.go` enforces that.

```sh
go build -o motion ./cmd/motion
motion --server http://127.0.0.1:8787 server status
motion auth pair --credential-store file          # device shows a code
motion auth approve PAIRING_ID --code ABCD-EFGH \
  --permission catalog:read --permission events:read \
  --operator-token-file /path/to/data/admin-token   # on the server host
motion profiles list
motion events tail --json
motion tui
motion serve --data-dir ... --access-mode restricted  # runs the bundled Rust server
```

## Contract for scripts

* `--json` writes one versioned object per result to stdout:
  `{"schema":"motion.cli.v1","data":...}`; `events tail` writes one
  `{"schema":...,"event":...}` per line. Errors and progress go to stderr
  (`"error"`/`"notice"`). JSON mode never contains terminal control sequences.
* Exit codes: `0` ok, `1` internal, `2` invalid input, `3` authentication or
  authorization (including a changed server identity), `4` conflict or failed
  precondition, `5` unavailable (network, 429/503, timeout), `6` not found,
  `130` interrupted.
* `--timeout` bounds each command (connect, TLS, response headers); a healthy
  event stream body is not cut off. Ctrl-C cancels in-flight requests.
* Retries: reads and keyed writes are retried with the same
  `Idempotency-Key` and body, honoring `Retry-After`. Unkeyed writes are not.
* Conditional updates read the current ETag unless `--if-match` is given.

## Credentials

A credential is sent only to a URL this client registered for the same
`server_id`, after the unauthenticated health check confirms that identity;
redirects are never followed. There is no OS keychain integration yet, so
`--credential-store file` must be accepted explicitly: the credential is kept
in a 0600 file in a 0700 directory under `$MOTION_CONFIG_DIR` (default: the
user config directory), and files readable by others are refused. The file
store is refused on Windows, where a mode does not make a file private.
`auth logout` forgets the local copy; revoke the device to invalidate it.

## Tests

```sh
go test -race ./...
cargo build && MOTION_E2E_SERVER_BIN=$PWD/../../target/debug/playscale go test -run E2E .
```

The mock in `internal/cli` is a contract-shaped fake, not evidence of server
behavior; the E2E test runs the real server in restricted mode.

## A08/A10 implementation audit (2026-10-09)

This checkout is a partial implementation of plan sections 11–13. The
normative OpenAPI document describes the target surface; serving that document
is not evidence that every operation is routed. This audit inspects
`src/v2/mod.rs`, `src/v2/content.rs`, `internal/cli/root.go`, and
`internal/tui/tui.go` on the `api-terminal-a08-a10` branch.

| Area | Current implementation | Remaining work |
| --- | --- | --- |
| A08 identity/system | Health, capabilities, pairing, sessions, access tokens, devices/policies, profiles, scoped SSE; restricted legacy boundary, approved-origin CORS, per-principal limits, trusted ingress and desktop bootstrap. | Ingress capabilities distinguish implementation, configured enablement and unqualified deployment status. Qualify real ingress deployment separately. |
| A08 content | Content-access issue/revoke and original-file GET/HEAD. | Delivery-generation and managed-download ticket integration depends on those resource services; original-file tests do not qualify HLS or delivery lifecycles. |
| A08 presentation | Optional router composition, full URI, verified identity, JSON API/media fallbacks and shared host/origin boundary. | Wire the actual A11 Topcoat service and authorized query facade; qualify HTML/API visibility parity, CSP, streaming/media preservation and browser lifecycle. A stand-in router test does not satisfy these gates. |
| A08 other domains | Existing legacy services remain available under their existing access rules. | Catalog/storage/scans, metadata, viewing/playback, processing, organization, offline and operations v2 adapters are not routed here. Integrate each owner's service and adapter with contract/scope/precondition/idempotency tests; do not substitute legacy wire shapes. |
| A10 CLI | Server status, pairing/approval/status/logout, device/policy/profile management, event tail, TUI launch and Rust `serve` delegation. | Library/source creation, scans, catalog search/show, jobs/cancel, authorized player launch and lifecycle-safe `server stop`. `libraries list` has a typed client/command but its v2 server route is absent here. |
| A10 TUI | Overview, profiles, devices and events; bounded event queue, reset/re-query and stale-load rejection. | Library/catalog/search, scans/jobs, diagnostics and selected playback controls, followed by real-server workflow qualification. |
| A10 credentials | Explicit opt-in restrictive file store and server-identity checks. | OS credential-store integration is not implemented; the documented file-store limitation remains. |

Transport URL encoding and reconnect delay calculation stay in the Go HTTP
adapter: they do not decide server authorization or domain transitions. Resource
IDs are opaque and escaped once; reconnect delays saturate before arithmetic can
overflow. Regression tests cover reserved characters/Unicode and 10,000 retry
attempt counts. Existing race tests cover request identity, cancellation,
reconnect, reset and stale query completion. These are client tests, not new
Stateless model-checking claims.

Next integration order: integrate the catalog v2
adapter and exercise `libraries list` against it; expand the typed CLI/TUI as
owner services land; then qualify the real A11 presentation mount and the
remaining cross-domain workflows. This table must not be read as full A08/A10
completion.
