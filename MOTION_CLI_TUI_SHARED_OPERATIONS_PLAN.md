# Motion CLI and TUI shared operations plan

Date: October 13, 2026

Status: In progress. Steps 1–3 are implemented in `apps/terminal`: the operation inventory ([operations.json](apps/terminal/operations.json), [PARITY.md](apps/terminal/PARITY.md)), the shared session and platform extraction, and the jobs and playback-launch slices in `internal/operations`. Step 5's enforcement covers the migrated operations. Steps 4 and 6 remain: every other workflow is recorded as pending in the ledger.

## Decision

Motion's CLI and TUI will invoke the same typed client operations. Both interfaces should expose the same supported user workflows wherever practical, with explicit exceptions for interactions that belong to one interface. Parity will be maintained through shared implementation and verification, rather than separate implementations that happen to call the same endpoints.

The server API and production Rust core remain authoritative for permissions, domain validation, ownership, revisions, job transitions, and durable effects. The shared Go layer coordinates client workflows; it does not become a second domain engine.

This plan applies to `apps/terminal`. It complements the [consolidation plan](MOTION_CONSOLIDATION_AND_COMPLETION_PLAN.md) and preserves the [public API contract](contracts/Motion_Server_API_v2.yaml). It does not require migrating the web or desktop clients to Go or expanding the server API as part of the extraction.

## Current structure

The clients already share typed HTTP methods in [internal/api](apps/terminal/internal/api). However, sharing transport alone leaves workflow behavior distributed across the interfaces:

- [CLI connection setup](apps/terminal/internal/cli/env.go) owns server selection, identity verification, credentials, and `Conn`.
- [CLI commands](apps/terminal/internal/cli/root.go) and [job and scan commands](apps/terminal/internal/cli/operations.go) compose requests and create operation idempotency keys.
- [The TUI backend interface](apps/terminal/internal/tui/data.go) exposes API-shaped methods. TUI code separately composes reads and actions.
- [The TUI entry point](apps/terminal/internal/tui/tui.go) accepts `*cli.Conn` and uses CLI browser helpers. This makes reusable functionality depend on an interface package.

The migration should preserve the working API client, credential store, cancellation behavior, and tests while moving shared responsibilities out of `cli` and `tui`.

## Target pathway

```text
CLI arguments and commands ──┐
                            ├── Shared typed operations ── HTTP API client ── Server API/core
TUI forms and actions ───────┘              │
                                          └── Credential and platform adapters
```

Both interfaces call operations directly in process. The TUI does not invoke command handlers, launch CLI subprocesses, or parse CLI output.

Use `apps/terminal/internal/operations` as the proposed shared package. Organize it by workflow, such as session, catalog, libraries, jobs, devices, and playback. Prefer small typed methods and narrow dependency interfaces over a generic string-based command dispatcher or a single interface containing every operation.

Move verified connection/session construction into this shared layer or a focused session package. Keep credentials in `internal/store`, HTTP mechanics in `internal/api`, and browser/process effects in explicit platform adapters. Construct dependencies in the executable composition root.

| Layer | Responsibilities |
| --- | --- |
| CLI | Parse arguments and environment settings; obtain confirmation when required; render text or versioned JSON; map errors to exit codes. |
| TUI | Collect selections and form values; obtain confirmation; schedule asynchronous calls; render progress and results; manage navigation and view state. |
| Shared operations | Resolve verified sessions; apply common client input rules and capability guidance; compose typed API calls; own logical action identity and revision handling; return typed results and errors. |
| API adapter | Encode URLs and wire bodies; send requests; implement permitted transport retries with unchanged keys and bodies; decode responses; handle SSE transport. |
| Server API and core | Authorize every request; enforce domain rules and transitions; coordinate durable ordering and atomicity through server adapters. |

Operations accept `context.Context` and explicit inputs. They do not print, call `os.Exit`, prompt, read terminal state, or depend on Cobra or Bubble Tea. Simple reads may delegate directly to existing API methods; add orchestration only where a workflow needs it.

## Shared operation semantics

### Session and capability handling

Use the same server selection, profile selection, identity verification, credential binding, and capability interpretation for both interfaces. Configuration parsing can remain at the entry point, but its resolved values must enter one shared session pathway.

Capability and cached permission checks guide discovery and provide useful errors. They never grant authority or replace server authorization. A changed server identity must prevent credential transmission regardless of which interface initiated the request.

### Mutations and retries

Create one idempotency key per logical mutation and reuse it with the same body for every permitted transport retry. A new intentional action receives a new key. Do not add a second retry loop around the API adapter or automatically retry unkeyed writes.

If a request may have reached the server but its acknowledgement was lost, preserve that uncertainty. Use the server's existing replay or reconciliation semantics before presenting a fresh submission as safe. Cancelling the local request does not prove that the server cancelled or rolled back the operation.

Conditional writes carry the revision the user reviewed when one is available. A TUI form must not silently fetch a newer revision and overwrite intervening changes. If a command intentionally reads the current revision before writing, expose that behavior through the shared operation. Conflicts remain visible; neither interface silently overwrites or retries against a new revision.

### Reads and pagination

Share filters, query composition, ordering expectations, and aggregate completeness rules. Return pagination and truncation information explicitly. A CLI can request one page while the TUI incrementally loads more, provided both use the same operation and neither presents a partial result as complete.

Keep TUI refresh frequency, chart history rendering, and navigation caches in the TUI. Move multi-request domain read composition into shared operations when either interface needs the same logical result.

### Errors and effects

Return typed outcomes that retain authentication, authorization, conflict, unavailable, not-found, invalid-input, cancellation, and uncertain-mutation distinctions. CLI output and TUI messages may differ, but must convey the same result and recovery choices. Preserve the existing `motion.cli.v1` output contract and exit codes during extraction.

Browser-player launch uses shared capability checks and token-free URL construction, followed by an injected browser adapter. Browser authentication remains separate. Bundled server startup stays a local process operation with an explicit CLI-only exception unless a TUI lifecycle workflow is deliberately added.

## Parity policy

New user-facing operations should ship with both CLI and TUI entry points. Any omission must appear in a parity ledger with its reason, intended disposition, and verification coverage. Temporary migration gaps are tracked work, not permanent exceptions by default.

Parity means equivalent inputs, defaults, permissions, state effects, results, and failure semantics. It does not require identical layouts, prompts, refresh schedules, or one command for every screen.

Maintain a machine-readable operation inventory and a readable workflow matrix. Each entry records a stable operation identifier, shared implementation, CLI entry point, TUI entry point, capability requirements, tests, and any exception. A ledger entry alone is not proof that a UI invokes the operation; adapter tests must exercise the wiring.

The following is the target scope, not a claim of current parity:

| Workflow | CLI target | TUI target |
| --- | --- | --- |
| Server identity and capabilities | Status command | Server view |
| Pairing and credential management | Auth commands | Pairing and account flow |
| Profiles and device policies | Inspect and mutate commands | Inspect and edit actions |
| Sources and libraries | Register, inspect, and supported management commands | Equivalent forms and actions |
| Scan requests | Explicit scan options | Equivalent scan form |
| Catalog and viewing reads | Browse, search, details, and viewing queries | Browse, search, details, and viewing views |
| Jobs | List, inspect, cancel, retry | Equivalent processing actions |
| Diagnostics and metrics | Structured queries | Overview and charts using the same queries |
| Playback launch | Play command | Play action |
| Event subscription | Event stream output | Activity and refresh signals |

Shell completion and terminal layout controls are natural interface-specific features. Server operations unavailable in the backend must remain unavailable in both clients; adding a menu or command does not complete the backend workflow.

## Correctness boundaries

Follow [AGENTS.md](AGENTS.md) when a workflow touches state-dependent behavior. Authorization, resource ownership, source revision acceptance, durable retry eligibility, publication, and job cancellation decisions belong in production `crates/core` policies and transitions wherever practical. Go operations submit intent and interpret observations without reproducing those rules.

Client state still needs explicit modeling where it affects client correctness. Action identity, reviewed revisions, connection identity, and pending outcomes belong in shared client state when used by both interfaces. TUI request generations, selected routes, and stale response rejection remain in the TUI because they govern presentation. Keep decisions deterministic and effects injectable; do not introduce a global client state machine for ordinary reads.

An extraction that preserves server behavior does not require an unrelated Rust refactor. If implementation exposes a missing server correctness rule, repair it in the production core and extend the relevant Stateless model. Report actual explored bounds and properties. Go race tests and HTTP fakes do not establish server transaction or model-checking correctness.

## Migration sequence

1. **Inventory current workflows.** Record actual CLI and TUI coverage and existing exceptions. Identify direct API calls, duplicated orchestration, shared helper imports, and output compatibility requirements.
2. **Extract session and platform dependencies.** Remove TUI imports of `internal/cli`. Preserve identity checks, credential restrictions, browser URL rules, and process-launch constraints. Update the existing process boundary test to recognize the new exact adapter locations without broadening allowed launches.
3. **Migrate jobs as the first vertical slice.** Route list, inspect, cancel, and retry through shared operations. Preserve CLI behavior and add missing TUI actions through that same layer. Verify action identity, cancellation, conflicts, and uncertain responses.
4. **Migrate remaining workflows incrementally.** Move sources/scans, catalog reads, profiles/devices, playback, diagnostics, and events in bounded changes. Add the missing interface entry points alongside each migrated workflow.
5. **Enforce the architecture and ledger.** Prevent CLI and TUI workflow code from constructing HTTP requests or invoking concrete API client methods directly. Allow shared data types without allowing a transport bypass. Require coverage or an explicit exception for every inventoried operation.
6. **Qualify the full pathway.** Run isolated real-server scenarios through both interfaces, update the terminal README, and record remaining parity exceptions and platform limitations.

Preserve unrelated worktree changes and existing user configuration. Use isolated configuration and server data roots for integration qualification.

## Verification and completion

Shared-operation tests should assert exact requests, logical mutation identities, revisions, results, and effect counts. Include transport retries, changed server identity, revoked permissions, stale capabilities, revision conflicts, pagination bounds, cancellation, and lost acknowledgements where relevant.

CLI adapter tests verify argument-to-operation mapping, JSON/text output, and exit codes. TUI adapter tests verify form-to-operation mapping, confirmation, pending-action behavior, profile changes, and rejection of stale completions. Both use the same operation fixtures; neither reimplements the expected workflow in its fake.

Integration tests use isolated real servers and equivalent fixture state for each interface. Compare resulting domain state and receipts rather than generated resource IDs. Include a real TUI interaction check so model tests alone do not stand in for keyboard and terminal behavior. Run `go test -race ./...`; enable the existing real-server E2E tests explicitly with `MOTION_E2E_SERVER_BIN` and report any skipped checks.

The migration is complete when all inventoried workflows use the shared pathway, each has both entry points or a documented exception, package boundaries prevent bypass, output compatibility is preserved, and the relevant operation, adapter, and real-server checks pass. A green inventory check or shared HTTP client alone does not establish parity.
