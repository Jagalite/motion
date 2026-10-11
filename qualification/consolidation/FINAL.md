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
