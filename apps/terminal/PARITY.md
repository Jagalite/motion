# Terminal operation parity

This is the readable matrix for [`operations.json`](operations.json), the
machine-readable operation inventory described in the
[shared operations plan](../../MOTION_CLI_TUI_SHARED_OPERATIONS_PLAN.md).
`TestOperationInventoryIsCompleteAndTruthful` checks the inventory against
the code: every runnable CLI command is listed, every named CLI entry point
exists, every shared operation is declared in `internal/operations` and has
operation, CLI and TUI tests, and every missing entry point has a recorded
exception. `TestInterfacesDoNotBypassSharedOperations` fails when CLI or TUI
code calls, or takes as a method value, an API method that a shared operation
owns; when either interface refers to none of a shared operation's
implementation; when interface code builds a player URL; or when it refers to
the platform browser opener, which only `cmd/motion` injects. These are
syntactic checks: the adapter tests listed in `operations.json` are what show
that each entry point calls its operation with the right inputs.
`playback.launch` owns no API method exclusively (its reads also serve other
workflows), so its wiring is covered by the reference and URL checks.

**Shared** means both interfaces call the same operation in
`internal/operations`. **Pending** means the workflow has not been migrated
yet: interface code still composes API calls itself. A pending row is tracked
migration work, not a claim of parity.

| Operation | Status | CLI | TUI | Shared implementation or remaining gap |
| --- | --- | --- | --- | --- |
| `session.connect` | Shared | `motion server status` | motion tui receives the verified session from the same pathway | `Connect`, `Settings`, `Session` |
| `jobs.list` | Shared | `motion jobs list` | Processing page and Overview work list | `ListJobs`, `CollectJobs`, `Collect` |
| `jobs.inspect` | Shared | `motion jobs show` | Processing page: enter on a job | `InspectJob` |
| `jobs.cancel` | Shared | `motion jobs cancel` | Processing page: c, then confirm | `NewJobAction`, `ResumeJobAction`, `SubmitJobAction` |
| `jobs.retry` | Shared | `motion jobs retry` | Processing page: R reviews the job, then confirm | `NewJobAction`, `ResumeJobAction`, `SubmitJobAction` |
| `playback.launch` | Shared | `motion play` | p on a title or item | `LaunchPlayer`, `PlayerURL`, `PlaybackReady` |
| `server.status` | Pending | `motion server status` | Server page | Both interfaces compose health, principal and capability reads separately. |
| `auth.status` | Pending | `motion auth status` | Server page principal | Principal read is composed separately in each interface. |
| `auth.pair` | Pending | `motion auth pair` | — | Pairing stores this device's credential before any session exists; the TUI starts only from a verified session. Disposition: pending: add a TUI pairing flow with the session slice |
| `auth.redeem` | Pending | `motion auth redeem` | — | Invitation redemption reads a secret from standard input before a credential exists. Disposition: pending: decide with the TUI pairing flow |
| `auth.logout` | Pending | `motion auth logout` | — | No TUI account action exists. Disposition: pending |
| `access.pairings.approve` | Pending | `motion auth approve` | Pairing requests: approve | Approval forms and keys are composed separately in each interface. |
| `access.pairings.deny` | Pending | — | Pairing requests: deny | No CLI command. Disposition: pending |
| `access.users.manage` | Pending | — | Users and access: list, create, edit | No CLI commands. Disposition: pending |
| `access.invitations.manage` | Pending | — | Access invitations: list, create, revoke | No CLI commands. Disposition: pending |
| `devices.list` | Pending | `motion devices list` | Devices page | Direct API reads in each interface. |
| `devices.show` | Pending | `motion devices show` | Devices page detail | Direct API reads in each interface. |
| `devices.revoke` | Pending | `motion devices revoke` | Devices page: revoke | Each interface reads the ETag and revokes on its own. |
| `devices.policy` | Pending | `motion devices policy show` | — | The TUI edits user policies, not device policies. Disposition: pending: decide whether device policy editing belongs in the TUI |
| `devices.policy.set` | Pending | `motion devices policy set` | — | As devices.policy. Disposition: pending |
| `profiles.list` | Pending | `motion profiles list` | Profiles page | Direct API reads in each interface. |
| `profiles.show` | Pending | `motion profiles show` | Profiles page detail | Direct API reads in each interface. |
| `profiles.create` | Pending | `motion profiles create` | — | No TUI form. Disposition: pending |
| `profiles.rename` | Pending | `motion profiles rename` | — | No TUI form. Disposition: pending |
| `profiles.delete` | Pending | `motion profiles delete` | — | No TUI action. Disposition: pending |
| `libraries.list` | Pending | `motion libraries list` | Sidebar libraries and Library page | Direct API reads in each interface. |
| `libraries.add` | Pending | `motion libraries add` | — | No TUI form. Disposition: pending |
| `sources.list` | Pending | `motion sources list` | Sources page | Direct API reads in each interface. |
| `sources.show` | Pending | `motion sources show` | Sources page detail | Direct API reads in each interface. |
| `sources.add` | Pending | `motion sources add` | — | No TUI form. Disposition: pending |
| `scans.request` | Pending | — | — | internal/cli defines a scan command that is not registered, so neither interface can request a scan. Disposition: pending: register it through a shared scan operation and add the TUI scan form |
| `scans.list` | Pending | — | Scans page | No CLI command. Disposition: pending |
| `catalog.list` | Pending | `motion catalog list` | All titles and Library pages | Direct API reads in each interface. |
| `catalog.search` | Pending | `motion catalog search` | Search | Direct API reads in each interface. |
| `catalog.show` | Pending | `motion catalog show` | Item page | The TUI composes the item aggregate (editions, timelines, versions, viewing state); the CLI shows only the item. |
| `viewing.continue` | Pending | — | Home: continue watching | No CLI command. Disposition: pending |
| `diagnostics.read` | Pending | `motion diagnostics` | Overview and Server pages | Direct API reads in each interface. |
| `diagnostics.storage` | Pending | — | Overview storage | No CLI command. Disposition: pending |
| `diagnostics.metrics` | Pending | — | Overview charts | No CLI command. Disposition: pending |
| `playback.sessions` | Pending | — | Now playing | No CLI command. Disposition: pending |
| `playback.history` | Pending | — | Playback history | No CLI command. Disposition: pending |
| `events.follow` | Pending | `motion events tail` | Activity and refresh signals | Both use the API adapter's Follow directly; reconnect and cursor policy is in the adapter. |
| `server.serve` | Pending | `motion serve` | — | Starting the bundled server is a local process operation. Disposition: permanent: CLI-only unless a TUI lifecycle workflow is added deliberately |

## Interface-specific behavior

Shell completion, terminal layout, refresh schedules and navigation are
interface concerns. `motion serve` stays CLI-only: it starts a local process.
The TUI asks for confirmation before cancelling or retrying a job; the CLI
treats the command itself as the confirmation.

## Unknown outcomes

When a job cancellation or retry may have reached the server but no
acknowledgement arrived, both interfaces report that the outcome is unknown.
They resubmit the same request with the same idempotency key, so the server
replays its saved acknowledgement or applies the intent once. A refusal that
follows an unanswered attempt inside the API adapter's own retries does not
settle the outcome either. The CLI prints the key and, for a retry, the
reviewed ETag (`idempotency_key`, `if_match` and `outcome_unknown` in
`motion.cli.v1` error output); `--idempotency-key KEY` resubmits the
request, and a retry also needs the original `--if-match ETAG` so the
resubmission is the same request rather than a new review. The TUI remembers
the action, with its reviewed ETag, while it stays open and offers to
resubmit it on the next `c` or `R` for that job. It forgets these actions
when the credential is rejected.

## Reviewed revisions

A job retry names the revision the user reviewed (`If-Match`). The server
admits it only at that revision and only for a failed or cancelled job, using
the production `playscale_core::jobs::admit_retry` policy: a stale review is
refused with 412, a missing precondition with 428, and one review admits at
most one retry. The Stateless model `playscale.jobs.reviewed_retry` explores
that policy with the job reducer.
