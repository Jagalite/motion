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
