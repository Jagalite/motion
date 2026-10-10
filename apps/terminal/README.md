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
motion libraries list --json
motion catalog search "Film"
motion jobs list
motion jobs retry JOB_ID                     # failed/cancelled jobs; requires processing:request
motion diagnostics --operator-token-file /path/to/data/admin-token
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
| A08 presentation | Actual pinned Topcoat Tower service at the composition root; request-scoped facade traverses authorized v2 reads, preserves the original URI and genuine peer metadata, and bounds renders to eight concurrent requests and 15 seconds. Home, libraries, search, item details (children, editions, timelines and versions), matches, jobs and diagnostics have real reads. | Timeline viewing/player, profile preferences, scan-backed sources still need their complete service bridges. Missing services render unavailable; continue-watching and item viewing history explicitly say unavailable, and item pages offer no Play link until the playback service is connected. Qualify the real browser independently of router tests. |
| A08 other domains | Logical libraries/source registration; catalog item CRUD/search, editions, reviewed merge/split, timeline/version reads; metadata contributions, matching and artwork reads; collections/filters/playlists/queues; owned jobs and cancellation/retry; diagnostics. | Source relocation and scan demands, relationships and timeline/version creation, full file-track evidence, metadata refresh/upload/selection/markers, v2 viewing/delivery, preparation/schedules, offline/downloads, durable backup/import jobs. The reviewed contract remains the target, not a claim that every operation is routed. |
| A10 CLI | Identity/device/profile commands, event tail, Rust `serve`, library/source creation, catalog search/show, jobs list/show/cancel/retry, diagnostics, typed scan requests and capability-gated token-free browser-player launch. | Real source-scan and playback workflows depend on v2 domain services. `server stop` has no reviewed lifecycle operation yet; no PID guessing or unrelated-process termination is used. |
| A10 TUI | Overview, profiles, devices, events, libraries, catalog/search, scans, jobs/cancellation and diagnostics; selected browser-player launch is capability-gated. Unicode input focus, narrow-screen navigation, cancellation, bounded paging, event reset/re-query and stale-load rejection are tested. | An isolated real-server PTY check exercised overview, libraries, catalog, search, jobs and diagnostics; scans correctly returned the missing-endpoint error. Scan and playback completion require the services above; a typed command or screen is not backend completion. |
| A10 credentials | Explicit opt-in restrictive file store and server-identity checks. | OS credential-store integration is not implemented; the documented file-store limitation remains. |

Transport URL encoding and reconnect delay calculation stay in the Go HTTP
adapter: they do not decide server authorization or domain transitions. Resource
IDs are opaque and escaped once; reconnect delays saturate before arithmetic can
overflow. Regression tests cover reserved characters/Unicode and 10,000 retry
attempt counts. Existing race tests cover request identity, cancellation,
reconnect, reset and stale query completion. These are client tests, not new
Stateless model-checking claims.

Integration inputs are recorded in [`research/a08-a10-integration-inputs.json`](../../research/a08-a10-integration-inputs.json). The original worktrees were not modified. The provisional delivery migration was assigned 0018 in this integration, after timelines/occurrences and logical sources; 0019 adds job requester identity and revisions. Legacy jobs retain an explicit unknown requester (`legacy`) and are administrative, rather than being assigned to the current caller.

Correctness boundaries: logical source membership is observed by SQLite and passed to core access policies. Catalog filters apply before page limits, version availability uses the production identity policy with authorized occurrences, and shared-file edits require every containing library. Job ownership and cancellation use core rules; the adapter reserves the writer, reauthorizes, and commits the change with its retry acknowledgement. Filesystem root inspection and actual worker execution remain adapter effects. SSR uses the same read adapter and credential checks as HTTP clients and never synthesizes an administrator. Browser cookies cover the root presentation routes, and session exchange expires the former API-only cookie path. Match and reconciliation retries recheck current visibility before returning stored acknowledgements.

This remains a partial A08/A10 integration until the listed dependencies and qualification gates are resolved.

### Checkpoint validation

At the requested stop/review/commit checkpoint, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`, `go test -race ./...`, and all 23 UI bridge/playback JavaScript tests passed. The earlier focused Rust integration run passed 49 tests. A later workspace run passed 15 presentation tests, 26 server library tests, one shutdown test, 28 access tests nine catalog persistence tests and 14 catalog workflow tests before being stopped during further executable launches.

The final focused Rust rerun (including the newly added replay/shared-metadata regressions) and extended real-server Go E2E test are **not qualified** at this checkpoint. The browser check reached real pairing and exposed the cookie-path bug; corrected cookie behavior still requires the extended E2E gate. No full-workstream or release-completion claim is made. Commands used Homebrew Rust/Cargo 1.99.0, Go 1.24.2 and Node 23.5.0; the repository's Rust 1.98 toolchain pin was not separately qualified. Owned test/discovery processes and the isolated fixture server were stopped; live settings and media were untouched.


### Follow-up review

The previously unqualified focused Rust checkpoint now passes all 53 tests:
28 access, four libraries, eight catalog, seven metadata/matching and six
organization tests. This includes the replay/shared-metadata regressions named
above. `motion jobs retry JOB_ID` is implemented; Go race tests verify stable
keys across transport retries, fresh keys for separate invocations, escaped IDs,
and conflict reporting.

Item detail pages now compose authorized item, child, edition, timeline and
version reads. They distinguish unavailable viewing history from an unstarted
title and suppress playback links until the player service exists. Each render
is bounded to 200 children, 200 editions, 20 timelines and the API-bounded version
sets; an additional page returns unavailable rather than silently truncating an
aggregate. The existing 15-second render deadline also applies. Core access and
identity policies still decide visibility and version availability. Presentation
only maps their results; it neither writes viewing state nor substitutes default
progress for missing service evidence.

Final follow-up validation (2026-10-10):

- `cargo test --locked --workspace --test access_api --test compose -j 2`:
  30 real-router/SQLite tests and 15 presentation composition tests passed.
  The new item regression checks hidden titles, children and versions, escaped
  titles, missing-resource parity, read-only rendering and the timeline bound.
  The retry regression asserts exact acknowledgements, revisions and receipt
  counts after later worker progress, plus ownership and revocation enforcement.
- `cargo clippy --locked --workspace --all-targets -j 2 -- -D warnings`,
  `cargo fmt --all --check` and `git diff --check` passed.
- `go test -race ./...` passed. The separately enabled real-server gate,
  `MOTION_E2E_SERVER_BIN=$PWD/../../target/debug/playscale go test -race -count=1 -timeout 15m -run E2E -v .`,
  also passed after the final rebuild. It checks pairing, real cookie-jar paths
  for SSR and API access, profile denial, scoped events, conditional updates,
  source/library operations, diagnostics and revocation. The qualified debug
  executable SHA-256 is
  `14e33fe7de657e29f13119ab3436a11a7819e3bb2d24f50286a2ed34817f4845`.
- T3 browser DOM checks on an isolated generated-media fixture completed
  browser pairing (operator approval via the API), library navigation and the
  real item page. CSS loaded, the desktop page had no horizontal overflow, and
  unavailable history/playback text appeared with no Play link. This browser
  check preceded the final rebuild; final-source markup is covered by the
  router tests above. Snapshot capture failed, and the preview host disconnected
  during the mobile resize: no screenshot or mobile-layout qualification.

The disposable servers were stopped. Existing settings, media and the original
adapter worktrees were not changed. Validation used Homebrew Rust/Cargo 1.99.0;
the repository's 1.98 pin and the remaining domain/deployment gaps still require
separate qualification. No new Stateless exploration bound is claimed: core
policies were unchanged and the new tests check their adapter enforcement.
