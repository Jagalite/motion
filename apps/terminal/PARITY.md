# Terminal operation parity

This is the readable matrix for [`operations.json`](operations.json), the
machine-readable operation inventory described in the
[shared operations plan](../../MOTION_CLI_TUI_SHARED_OPERATIONS_PLAN.md).
Every workflow is **shared**: the CLI and the TUI call the same function in
`internal/operations`, with the same inputs, defaults, input rules,
preconditions, idempotency keys and error classification. Interfaces differ
only in how they collect input, confirm and present results. `motion serve`
is the one interface-specific exception.

## How this is enforced

- `TestOperationInventoryIsCompleteAndTruthful` keeps the inventory
  consistent with the code. It checks that:
  - every runnable CLI command, including groups that also run, is listed;
  - every named CLI entry point and shared function exists;
  - every row names operation, CLI and TUI tests that exist;
  - every row appears here;
  - no pending, half-migrated workflow remains.

  It checks names, not behavior: the named tests are what exercise each row.
- `TestCLIEntryPointsRunTheirSharedOperation` resolves each CLI entry point
  to its Cobra command. What that command runs must call one of the row's
  shared functions.
- `TestInterfacesDoNotBypassSharedOperations` type-checks `internal/cli` and
  `internal/tui`. It fails when either of them:
  - calls an API client method, whether on the client, on a session
    embedding it, or on an interface carrying client methods (such as the
    TUI's `Backend`);
  - constructs a client;
  - builds or sends a raw HTTP request;
  - builds a player URL;
  - refers to the browser opener;
  - refers to none of a shared row's functions.

  TUI entry points are free text. For the TUI, this reference check and
  the adapter tests show the wiring.
- `TestSharedOperationsStayIndependentOfInterfaces` keeps
  `internal/operations` free of Cobra, Bubble Tea, terminal I/O, process
  launches and the interface packages. It also keeps the CLI and the TUI
  from importing each other.
- The adapter tests share one fixture, `internal/operations/opstest`. It is
  served in process to the TUI and over HTTP to the CLI. It replays keyed
  writes only for an identical request body, records device owners and
  policies, and issues per-device credentials. Two scenarios must leave the
  same domain state, compared without generated IDs, whether run directly,
  through CLI commands or through TUI keystrokes:
  - management (`opstest.ApplyManagement`);
  - defaults (`opstest.ApplyDefaults`).
- `TestE2ECLIReachesTheSharedRealServerState` and
  `TestE2ETUIReachesTheSharedRealServerState` run one scenario through each
  interface on a fresh real server and must observe the same state:
  profiles, sources, libraries, a scan, an access user, a revoked
  invitation and a redemption. `scripts/tui_pty_check.py` types into the
  real `motion tui` in a pseudo-terminal. The real-server scenario also
  assigns a device that was approved without a user, as an older client
  could, through each interface.

These checks prove the wiring and the shared decisions. They do not prove
server behavior: the real-server E2E tests and the Rust tests do that.
Pairing approval runs against the real server only in
`TestE2EPairApproveUseAndRevokeAgainstRealServer` (CLI). The TUI's pairing
flow is exercised against the fixture.

| Operation | Status | CLI | TUI | Shared implementation or exception |
| --- | --- | --- | --- | --- |
| `session.connect` | Shared | `motion server status` | motion tui receives the verified session from the same pathway | `Connect`, `Settings`, `Session` |
| `server.status` | Shared | `motion server status` | Server page: identity, status and features | `ReadServerStatus` |
| `auth.status` | Shared | `motion auth status` | Server page: this device | `WhoAmI`, `ReadSession` |
| `auth.pair` | Shared | `motion auth pair` | P while signed out: pair, show the code, wait for approval | `StartPairing`, `PendingPairing` |
| `auth.redeem` | Shared | `motion auth redeem` | I while signed out: redeem an invitation code | `RedeemInvitation` |
| `auth.logout` | Shared | `motion auth logout` | Server page: O signs out | `SignOut` |
| `access.pairings.list` | Shared | `motion access pairings list` | Pairing requests page | `ListPairings`, `CollectPairings` |
| `access.pairings.approve` | Shared | `motion auth approve` | Pairing requests: a, choose the user, review | `ApprovePairing` Note: Both interfaces name the access user the device belongs to, and pre-fill its grant (operations.InheritedGrant). A device-only approval is accepted only by servers that do not advertise identity.access_management; existing unassigned devices are assigned with devices.assign. |
| `access.pairings.deny` | Shared | `motion access pairings deny` | Pairing requests: d, review | `DenyPairing` |
| `access.users.list` | Shared | `motion access users list` | Users and access page | `ListUsers`, `CollectUsers` |
| `access.users.show` | Shared | `motion access users show` | Users and access: detail of the listed user | `InspectUser`, `CollectUsers` |
| `access.users.create` | Shared | `motion access users create` | Users and access: n | `CreateUser` |
| `access.users.update` | Shared | `motion access users update` | Users and access: e, at the reviewed revision | `ReplaceUser` |
| `access.invitations.list` | Shared | `motion access invitations list` | Access invitations page | `ListInvitations`, `CollectInvitations` |
| `access.invitations.create` | Shared | `motion access invitations create` | Access invitations: n, choose the user; the code is shown once | `CreateInvitation` |
| `access.invitations.revoke` | Shared | `motion access invitations revoke` | Access invitations: d, at the reviewed revision | `RevokeInvitation` |
| `devices.list` | Shared | `motion devices list` | Devices page | `ListDevices`, `CollectDevices` |
| `devices.show` | Shared | `motion devices show` | Devices: detail of the listed device | `InspectDevice`, `CollectDevices` |
| `devices.revoke` | Shared | `motion devices revoke` | Devices: d, at the reviewed revision | `RevokeDevice` |
| `devices.assign` | Shared | `motion devices assign` | Devices: u on an unassigned device, choose the user, review | `AssignDevice` |
| `devices.policy` | Shared | `motion devices policy show` | Devices: p reads the policy into the editor | `InspectDevicePolicy` |
| `devices.policy.set` | Shared | `motion devices policy set` | Device policy editor, at the revision read | `ReplaceDevicePolicy` |
| `profiles.list` | Shared | `motion profiles list` | Profiles page | `ListProfiles`, `CollectProfiles` |
| `profiles.show` | Shared | `motion profiles show` | Profiles: detail of the listed profile | `InspectProfile`, `CollectProfiles` |
| `profiles.create` | Shared | `motion profiles create` | Profiles: n | `CreateProfile` |
| `profiles.rename` | Shared | `motion profiles rename` | Profiles: e, at the reviewed revision | `RenameProfile` |
| `profiles.delete` | Shared | `motion profiles delete` | Profiles: x, at the reviewed revision | `DeleteProfile` |
| `libraries.list` | Shared | `motion libraries list` | Sidebar libraries | `ListLibraries`, `CollectLibraries` |
| `libraries.show` | Shared | `motion libraries show` | Library page | `ReadLibraryContents` |
| `libraries.add` | Shared | `motion libraries add` | Sources: l | `CreateLibrary` |
| `sources.list` | Shared | `motion sources list` | Sources page | `ListSources`, `CollectSources` |
| `sources.show` | Shared | `motion sources show` | Sources: detail of the listed source | `InspectSource`, `CollectSources` |
| `sources.add` | Shared | `motion sources add` | Sources: n | `CreateSource` |
| `scans.request` | Shared | `motion libraries scan` | Scans: n, choose a library | `RequestScan` |
| `scans.list` | Shared | `motion scans list` | Scans page | `ListScans`, `CollectScans` |
| `scans.show` | Shared | `motion scans show` | Scans: detail of the listed scan | `InspectScan`, `CollectScans` |
| `catalog.list` | Shared | `motion catalog list` | All titles | `ListCatalog`, `CollectCatalog` |
| `catalog.search` | Shared | `motion catalog search` | Search | `SearchCatalog`, `CollectSearch` |
| `catalog.show` | Shared | `motion catalog show` | Item page (catalog show --details shows the same aggregate) | `InspectItem`, `ReadItemDetails` |
| `viewing.continue` | Shared | `motion catalog continue` | Home: continue watching | `ContinueWatching` |
| `diagnostics.read` | Shared | `motion diagnostics` | Overview and Server pages | `ReadDiagnostics` |
| `diagnostics.storage` | Shared | `motion diagnostics storage` | Overview and Server storage | `ReadStorage` |
| `diagnostics.metrics` | Shared | `motion diagnostics metrics` | Overview charts | `ReadMetrics` |
| `playback.sessions` | Shared | `motion playback sessions` | Now playing | `ReadPlaybackSessions` |
| `playback.history` | Shared | `motion playback history` | Playback history (d filters to a device) | `ListPlaybackHistory`, `CollectPlaybackHistory` |
| `events.follow` | Shared | `motion events tail` | Activity and refresh signals | `FollowEvents` |
| `jobs.list` | Shared | `motion jobs list` | Processing page and Overview work list | `ListJobs`, `CollectJobs`, `Collect` |
| `jobs.inspect` | Shared | `motion jobs show` | Processing page: enter on a job | `InspectJob` |
| `jobs.cancel` | Shared | `motion jobs cancel` | Processing page: c, then confirm | `NewJobAction`, `ResumeJobAction`, `SubmitJobAction`, `PendingJobActions`, `UncertainError` |
| `jobs.retry` | Shared | `motion jobs retry` | Processing page: R reviews the job, then confirm | `NewJobAction`, `ResumeJobAction`, `SubmitJobAction`, `PendingJobActions`, `InspectJob` |
| `playback.launch` | Shared | `motion play` | p on a title or item | `LaunchPlayer`, `PlayerURL`, `PlaybackReady`, `Browser` |
| `server.serve` | CLI only | `motion serve` | — | Starting the bundled server is a local process operation, not a server API workflow. Permanent: cli-only unless a tui lifecycle workflow is added deliberately. |

## Shared rules

- **Pages.** Single-page reads validate `limit` (1–200) and return the
  next cursor. Aggregates use `operations.Collect`: it stops at a bound,
  reports truncation and refuses a repeated cursor. A partial result is
  never shown as complete.
- **Keyed writes.** Creates, approvals, denials, scans and job actions carry
  one idempotency key per intent. A resubmission reuses the key and the
  request. CLI commands take `--idempotency-key` to resubmit. A TUI dialog
  reuses its key and locks its fields after an unknown outcome.
- **Conditional writes.** Renames, deletions, revocations, policy and user
  replacements, and job retries name a precondition. The TUI uses the
  revision the user reviewed and never substitutes a newer one. The CLI uses
  `--if-match`, or by default deliberately reads the current revision. A
  stale precondition is a conflict (exit 4) and never an overwrite.
- **Unknown outcomes.** A write whose acknowledgement was lost is reported as
  possibly applied. These later answers do not settle it: a refusal given
  before the server's receipt lookup (rate limit, authentication,
  visibility), or a conflict about the receipt itself
  (`idempotency_key_reused`, `idempotent_replay_unavailable`). The CLI
  reports `outcome_unknown`, `idempotency_key`, `if_match` and, for an
  invitation or user-bound approval, the exact `request` (user revision and
  grant). `--resubmit-request` takes that request back verbatim and
  replaces the grant flags. Suggested commands are shell-quoted. The TUI
  locks the dialog and offers to resubmit the same request.
- **Defaults.** A new user gets `operations.DefaultUserPermissions` and
  `DefaultGrantPolicy`. An invitation or user-bound approval starts from
  the user's grant without `system:admin` (`operations.InheritedGrant`).
  Permissions are sent in the contract's order.
- **Device ownership.** A server that advertises
  `identity.access_management` manages access by user. Both interfaces
  then approve a pairing only for a named access user, whose grant caps
  the device. A device approved without a user, by an older client, is
  assigned with `devices.assign`; it is never transferred once owned.
- **Guidance.** Permissions and capabilities shown by the TUI only guide.
  The server authorizes every request, and the player launch reads current
  permissions and capabilities.

## Interface-specific behavior

Shell completion, terminal layout, refresh schedules and navigation are
interface concerns. A credential change in the TUI (pairing, redemption or
sign-out) restarts the change-hint stream for the new principal and
discards the previous principal's activity. Abandoning a pairing never
installs its credential; if the credential was already installed, the TUI
says so. The TUI asks for confirmation before every mutation; the
CLI treats the command itself as the confirmation. Secrets are read
privately: the CLI reads invitation codes from standard input, and the TUI
masks the code field. Neither shows a device code or credential.
`motion serve` stays CLI-only because it starts a local process.

## Reviewed revisions

A job retry names the revision the user reviewed (`If-Match`). The server
admits it only at that revision and only for a failed or cancelled job, using
the production `playscale_core::jobs::admit_retry` policy. A stale review
is refused with 412 and a missing precondition with 428, and one review
admits at most one retry. The Stateless model
`playscale.jobs.reviewed_retry` explores that policy with the job reducer.
