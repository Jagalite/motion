# Final consolidation gate ledger

Owner: integration/release (this coordinating thread). Status: in progress.
This ledger supersedes provisional completion assumptions, not historical receipts.

## Frozen inputs and order

| Order | Lane | Input | Composition |
|---|---|---|---|
| 1 | Release #14 | 28f63324beb054b5e1b84c13a1b96520403f0968 | base |
| 2 | Catalog #17 | 69e0ff4fdf7c8b0f271e4c6cefbdff2712fc15e0 | ba41edd |
| 3 | Playback #16 | 7a4c07b1c60641687e8ceaabe8cef891f73aaca8 | 8692195 |
| 4 | Clients #15 | 3c3c450dde41e2d94fcf1dfdafcba11fef4eb6b3 | 6590b87 |

Lane histories already share earlier prerequisites; these are reviewed input
identities, not a claim of independent commit ancestry. No lane worktree is edited.
Integration owns shared router/entry-point files, manifests, package scripts and
this ledger. Preserve migrations 0001–0041 and canonical history precedence.

## Deliverables and accountable owners

| Gate | Implementation owner | Integration acceptance owner | State / required receipt |
|---|---|---|---|
| Catalog and upgrade protection | Data/catalog #17 | Integration/release | Schema41 inputs composed; combined upgrade/repair/restart/rollback rerun required |
| Playback authority and tickets | Playback/viewing #16 | Integration/release | Inputs composed; scoped requests, revocation, pacing and restore tests required |
| Deployed live player | Clients #15, playback #16 | Integration/release | Moving-window startup/audio switching failures retained; causal fix and exact provider matrix required |
| One-app macOS distribution | Integration/release | Integration/release | Bundle Go/TUI, Electron, server, helper, Topcoat and verified media/player dependencies; clean install/relocation required |
| Remaining public caller prerequisites | Data/catalog #17, clients #15 | Integration/release | Positive Next writer and dynamic profile/backend disposition required before retiring their old callers |
| Default Topcoat and legacy absence | Integration/release, clients #15 | Integration/release | Switch after replacements work; anonymous/authenticated unsafe methods must not mutate legacy or canonical state |
| Combined CI and source reconstruction | Integration/release | Integration/release | Final exact-head fast/full/package/source jobs required |
| Installed section6 workflow | Integration/release | Integration/release | Web/Electron/Go, original/prepared/live, restricted grants, recovery and no-v1 traffic receipts required |
| M0–M4 closure | Integration/release | Integration/release | Open until individual gates have exact source/artifact receipts and declared limits |

M5/M6 remain deferred as specified in the plan. Existing unit/model/browser and
package receipts qualify their recorded revisions only. Hardware skips are not
hardware acceptance. A provider version label is not deployment qualification.

## Current evidence and review

79 JavaScript/setup tests pass after composition. GPT-6.1 Sol reviewed all three
merges; the catalog merge initially omitted a plan ownership section and row. The
section is restored by the client merge and the row is restored with this ledger.
No production merge findings remain. Retained scoped watched queries, restore
epoch headers, forward migration files, valid idempotency keys and both lanes'
regressions require runtime confirmation on the final candidate.

## Integration checkpoint: 2026-10-11

PR #14 merged at `b3aeeadcae4a988397f9ad2a3c7cf9cb43f0bdbc` after its
exact-head checks passed. PR #17 incorporates that main baseline at `b46314d`;
its fast gates pass while full macOS checks remain queued. PRs #16 and #15 have
not been merged. PR #18 carries the combined implementation; this is not milestone
closure and does not supersede the required integration order.

| Deliverable | Implementation / review | Evidence and remaining gate |
|---|---|---|
| Paced HLS availability | `8b9f34a`; GPT-6.1 Sol reviewed | Fresh provider rebuild Kyoto `02254edf69104207bfc9ed714480b8be` passed with identical provider identity. Seven parser/timeline regressions pass. `paced-provider-diagnostic.json` records browser startup, delayed playback, audio switch, far seek and end-of-stream observations. Full engine and installed-player matrix remains open. |
| One-app bundle | `1346cbd`, `4c646d2`, `33e6fcc`; GPT-6.1 Sol reviewed | Electron shell, Go CLI/TUI, Rust services, private owned-server channels, media tools, exact player closure and notices assembled by the recipe. Fourteen packaging and 22 desktop tests pass. Kyoto package build `6539318a08e243088306d05a04c287c1` at `33e6fcc` is in progress; no installed-product claim yet. |
| Public Next prerequisites | `1770fef`; GPT-6.1 Sol reviewed, alias finding fixed | Kyoto `ece3076e64694974a5d6205147c26746` passed; compiled binaries passed 16 catalog HTTP tests and four contract tests. Public order-group creation, canonical alias replay, stale revisions, scope/ownership rejection and ordered Next all exercised. |
| Dynamic preparation inputs | `67c5b39`; GPT-6.1 Sol reviewed | Bounded profiles/backends are persisted and included in publication identity. Current-server profile smoke now uses v2, with EOF/deadline-bounded SSE replay. Contract/syntax pass; Kyoto `638f3eba0db54e3d829da53218203b02` compilation and real profile acceptance remain pending. |
| Complete source reconstruction | `d957b49`; GPT-6.1 Sol reviewed | Gate now reconstructs the complete package outside Git, verifies source/provider identity, then exercises the reconstructed archive. Source checks pass; full reconstruction is not yet run. |

The initial package attempt at `4c646d2` failed publication screening on synthetic
path strings in a test fixture, before compilation. `33e6fcc` preserves the exact
test bytes while making the source fixture compatible with the privacy checker;
both publication checks and packaging tests pass. It does not relax the checker.

Integration/release (this thread) owns the remaining work: ordered merges;
legacy mount/asset removal with anonymous, scoped and operator absence tests;
Topcoat default; service-regression preservation; exact-head combined CI;
source reconstruction; relocated installed CLI/TUI and Electron acceptance;
and the player engine/deployment matrix. Catalog, playback and client lane owners
remain accountable for their domain fixes, with integration owning the final
acceptance. Missing hardware/physical A/V evidence is explicitly unqualified.
All M0–M4 rows remain open until their individual acceptance receipts exist.


### Public prerequisite acceptance before route cutover

At `67c5b39`, Kyoto `0546920f915f4333b5f665843a975e7b` compiled the focused
metadata/preparation target successfully with one compiler job. The earlier
combined test-compilation request was deferred twice under host memory pressure
and cancelled in favor of this narrower request; it is not passing evidence.
All 18 metadata/preparation HTTP tests passed. The v2-only real-profile smoke
passed five software profiles (H.264, HEVC, 30 fps, fractional 29.97 fps, 1 fps
and 2 fps as recorded), prepared plans/deliveries, range bytes, complete decode,
SSE replay and restart identity. See `profile-v2-67c5b39.json` for exact source
media and server binary hashes. The FFmpeg used by this receipt is the host tool;
it does not qualify bundled HEVC or hardware encoding.

The route-retirement implementation removes runtime v1/media and legacy assets,
uses Topcoat by default, and keeps legacy configuration values from reopening
old routes. Historical service regressions have a test-only router; the new
v2 media matrix, production default UI and retirement/no-mutation matrix use
the actual production router. Cutover compilation and runtime tests are pending.
