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


### PR ancestry reconciliation: 2026-10-11

Reconciled remote integration input `7339500a4993a737dde5089b0a8dbca5f4404b21`
with main `38f80a9bbca47bb8dfdcc66a65aca139321cb7e6` after PR #17 merged
there. PR #15 head `3c3c450dde41e2d94fcf1dfdafcba11fef4eb6b3` was already
an ancestor of the integration input. Catalog feature head `69e0ff4` was also
already included. The remaining #17 history consisted of release merge ancestry
and `b46314d4d3c93dc6fd3561213f2dae12b80e8ae4`, a Kyoto variable instruction.
The only merge conflict duplicated the existing shared-Kyoto setup instruction;
the integration version retains that instruction below its example.

The reconciliation changes no production, test, migration, provider or packaging
bytes relative to the integration input. No build or runtime test is warranted
for this ancestry/documentation-only change, and no prior receipt is promoted to
final-head runtime qualification. PR #15's current head is fully preserved and
can be closed as superseded after checking that its head has not advanced; PR #17
is already merged. PR #18 remains the integration destination, not merged to main.
The earlier checkpoint statements about PR merge status are historical.

Outstanding gates remain exact-head combined CI, route-retirement runtime checks,
source reconstruction, relocated installed-product acceptance, and the full
player/restricted-grant/recovery matrix. Focused provider diagnostics do not erase
previous live-startup/switching failures or close M0–M4.

PR #15 advanced during the reconciliation to
`6977c694ecfffff981843a3683435b81107c4bd7`, incorporating reviewed playback
and main ancestry. That head is also merged into this reconciliation. It adds no
production or test changes to the integration tree; its duplicate Kyoto prose
conflict is resolved as above. Closure eligibility refers to this newer head.

### Current acceptance checkpoint: 2026-10-11, after installed testing

This checkpoint supersedes the pending statuses above, but does not close M0–M4.
The user selected **Chromium and Electron** for final player qualification.
**WebKit is explicitly deferred**, not passed or silently omitted.

PR #17 merged to main at `38f80a9` after all eight exact-head checks passed.
PR #16 had merged into the combined staging branch, not main; PR #19 carries
that reviewed playback lane into main after #17. The user subsequently confirmed
that PR #15 may remain closed as superseded: all its work is preserved in #18.
This supersedes the interim plan to reopen and merge #15 after #19.
PR #19's initial integration run `38145957879` failed processing isolation:
generated output storage appeared as an extra logical library. Existing fix
`0198a89` was brought forward as `a9f9d28`, followed by its Host fixture correction
and unused-wrapper cleanup (`558b987`, `0d40f69`). GPT-6.1 Sol reviewed these fixes.
The client branch incorporates them with its existing implementation preserved;
its final tree at `d0c2cb4` equals `6977c69`. Exact-head CI remains pending.

| Gate | Evidence | Status / accountable owner |
|---|---|---|
| Production route absence and preserved service regression | `9c0b44f`; Kyoto `7db92c647c964c57ae28a0d50cab228a`; both previously failing compiled tests passed. The absence matrix checks all configured modes and DB non-mutation. Fast CI at `7339500` also passed. | Passed at recorded source; final combined CI owned by integration/release. |
| Relocated one-app package | `installed-7339500.json`; archive `97aa783c6a5ae873dad1e0ab5ece1a9c99891d61be255fd469b44df1a29e614b`; 13 package checks passed. | Passed at `7339500`; final artifact rebuild/reconstruction owned by integration/release. |
| Installed Electron and Go/TUI | Same receipt; 16 real native checks, bundled Go CLI and PTY TUI, child-process ownership, sandbox, isolated settings, disconnect/stop/restart/close behavior. | Passed at recorded package. Offline cache was seeded explicitly; this does not qualify M5 download publication or reconciliation. |
| Desktop-to-Chromium resume | Built-in Chromium resumed native progress and advanced canonical server progress. | Passed at recorded package; muted playback is not physical A/V proof. |
| Electron player matrix | `electron-player-7339500.json`: 30 passed, zero failed/blocked; original ranges, seek, progress, resume, audio/HLS switch, durable outage recovery, real preparation and rendition switch, conversion-only playback, v2-only traffic. Earlier `electron-player-b5db877.json` retained separately. | Passed for these checks at the recorded binary/provider. New provider bytes require fresh qualification. |
| Chromium full player matrix | First cold live open at `7339500` timed out in native preparation. Later attempts played, but do not erase the failure. Instrumentation showed preparation at the live edge before the requested-position seek; explicit initial media time zero is under investigation. | Open; integration/release owns diagnosis and clients/playback own domain fixes. |
| Complete source reconstruction | Full outside-Git package reconstruction gate is implemented. | Open until its actual final-head CI receipt passes; integration/release. |
| Restricted grants, long no-history playback, public Next and remaining section 6 UI workflows | Existing source/focused receipts are not a substitute for combined installed workflow acceptance. | Open; integration/release coordinates clients, playback and catalog lane owners. |

Installed testing found and fixed two real product defects: the outer CLI symlink
could select the wrong resource root (`cac9553`), and native session exchange
omitted the server-required Origin (`7339500`). The latter preserves the server's
authorization checks. Filesystem resource resolution and HTTP Origin supply are
adapter responsibilities; no domain authority was moved out of core.

The local prerequisite build requests `7a83a39c8ac542738f56d54910b682f2` and
`9cb80c840e524c378bc7dd2ea045a612` were cancelled while queued under host memory
pressure so review corrections could be applied. They supply no passing evidence.
Package signing is ad hoc, not notarized. Hardware encoding, expanded M4 format
coverage, online metadata, M5 and M6 retain their separately documented gates.

The startup fix `a7d7734` passed its four generated-adapter tests in Kyoto build
`543c8933e4174140a73f1376693d2a2d`, but package attempt
`1a812dee13544dd89a76b2f3c801c99d` rejected that provider before compilation:
the new adapter hash was outside the runtime's declared asset identity closure.
The follow-up registers the adapter as a real asset and runs actual runtime
manifest admission before publishing the provider. That failed artifact remains
unqualified. A reused Chromium origin also retained the old adapter; those
observations are not new-provider evidence. Stable provider URLs now prohibit
cache reuse and filesystem-conditional responses because reproducible packages
normalize mtimes. A same-mtime, changed-byte router regression covers the defect.
Neither correction weakens provider identity admission or media authorization.


### Qualified provider and installed checkpoint: 2026-10-11

This checkpoint supersedes the pending provider and installed statuses above.
PR #15 remains closed as superseded, as explicitly confirmed by the user. Its
head `d0c2cb4911bfabe9ce95a53c2b92527cd337ab8d` is preserved in #18 and has the
same tree as `6977c694ecfffff981843a3683435b81107c4bd7`. Main integration order
is #14, #17, #19 (carrier of #16), then #18 (including preserved #15).
GPT-6.1 Sol reviewed the history reconciliation without outstanding findings.

The provider at `2fa508f` passed runtime admission, four generated-adapter tests
and seven Shaka parser tests. Its exact identity is
`sha256:4d968b37420bc8d32072e2ca553dd1a09fcbb5a841b4de51314513c42d0c334a`.
Kyoto package build `21d5c29bb6db44b98358ddde59ec4d68` succeeded. Receipts retain
exact binary, archive, provider and harness hashes and prior failed attempts.

| Gate | Receipt and observed result | Scope / remaining owner |
|---|---|---|
| Installed one-app | `installed-2fa508f.json`: 13 relocated package checks, 15 clean native checks, and a separate 16-check browser-handoff run passed. Bundled Go CLI and real PTY TUI exercised. | Integration/release; ad hoc signing only. The separate handoff run needed one disclosed manual Start click; `34b85b7` fixes the harness readiness race and the clean native repeat passed. Seeded offline cache does not qualify M5. |
| Electron player | `electron-player-2fa508f.json`: 30 passed, zero failed/blocked. Original ranges, seek/resume, audio/HLS, durable outage/restart recovery, prepared rendition switching and conversion-only playback; v2-only traffic. | Clients/playback implementation; integration acceptance. Muted playback is not physical A/V verification. |
| Chromium provider | `chromium-provider-2fa508f.json`: original playback, seek, audio switch, cold conversion, far seek and moving-window playback passed with the new provider. | Uses the recorded earlier server binary. Retained-window reopening is a direct provider probe, not a complete Motion resume workflow. Its later 401 predates explicit revocation and is not causal revocation evidence. |
| Installed Chromium restricted playback | `chromium-restricted-2fa508f.json`: no-history device played beyond the initial 48-second window to 53.34 seconds with unchanged canonical history and no history events. Explicit revocation returned 204, bearer access became 401, and the player was removed within 5.908 seconds. Owned output disappeared. | Exact installed `2fa508f` server/provider. Integration/release owns any later candidate rerun; this is the causal revocation receipt. |
| Public playable Next | `2d2c006`: existing playable timelines can be ordered through a fenced public PUT. All 17 catalog HTTP tests passed after Kyoto `7de68aa648294af7aed318a24a3797ac`; four contract tests and structural/parity checks passed. | GPT-6.1 Sol reviewed. Debug-server browser Next played the selected converted item. Final installed-package repetition remains required. |

The ordering endpoint supplies observed timeline/group identity and revisions to
existing production `access::revise` and `identity::place_in_order` decisions.
SQLite locking, destination and previous-owner authorization, and transactional
revision updates stay in the adapter. HTTP regressions cover stale timeline and
group revisions, occupied positions, hidden/foreign owners, and unchanged file
bindings/history. No separate model duplicates the existing core order policy.

Remaining deliverables have explicit owners: integration/release owns final-head
combined CI, complete source-companion reconstruction, final installed candidate
and ledger closure. Data/catalog (A01) and clients (A10–A12) own the broader
cross-work episode hierarchy and complete section 6 UI workflow evidence; the
same-work playable Next check alone does not close M3. Playback/viewing (A06–A08)
owns expanded multipart/interval, hardware/copy, subtitle and HDR/ABR format
gates. Online metadata, M5/M6 and WebKit remain explicitly deferred under the
plan/user scope. Passing these receipts or merging the four lanes does not close
unmet milestone gates.


Supplemental receipts: `catalog-workflow-2fa508f.json` records local metadata
correction surviving a v2 rescan and canonical watched smart-collection results.
Its fileless Next resolver check is explicitly not playable Next acceptance.
`integration-a476c0e.json` binds the successful 14-check real-process CI job and
schema-9-to-41 upgrade/restore to its source and binary. Restart preserves the
upgrade receipt, the old binary rejects schema 41, both rollback methods work,
and candidate restore rotates the epoch. These passes do not qualify later
ordering changes or the still-pending heavy/package jobs.


CI at `a476c0e` subsequently completed successfully: all eight displayed checks
passed. `integration-a476c0e.json` now records exact check URLs and all 40 full-tier
tests across six targets. Test counts are not model exploration-state counts.
`source-reconstruction-a476c0e.json` preserves the complete outside-Git application
reconstruction receipt and both 13-check package receipts. The reconstructed
provider identity matches the qualified `2fa508f` deployment. This is a passing
combined checkpoint; `2d2c006` and the final composition still require their own
exact-head CI before integration.


### Final local candidate: `2d2c006`

Kyoto `f10b63227d2a4fa8a7c66692db652a76` built the clean candidate successfully.
Archive SHA-256 is
`29acbc6224e353c6b6d3fcaaef638e2b0ebe3da09a61248bad3c8f292e7f6810`;
installed server SHA-256 is
`f794a6d0d20aaf5b2e48b6a3ca226b74dbcb60e7c8168e9da3e4473249a01e3e`.
The provider bytes retain the qualified `4d968b37…` identity.

- `installed-2d2c006.json`: all 13 relocated-package checks and all 15 real
  installed Electron/Go/TUI checks passed without manual intervention.
- `electron-player-2d2c006.json`: all 30 player matrix checks passed on the
  final installed server/provider, including outage/restart and conversion.
- `playable-next-2d2c006.json`: public ordering of existing playable timelines
  preserves bindings/history and rejects stale retries. Electron clicks the
  actual Next control, selects the ordered timeline, and advances converted
  playback from 0.55 to 3.42 seconds.
- The T3 Chromium Next repetition was interrupted by preview-host disconnection.
  It is incomplete, not a pass or a product failure. Earlier Chromium provider,
  no-history and causal-revocation receipts remain valid only for their recorded
  inputs. Integration/release owns this final repetition when the host returns.
- GPT-6.1 Sol source/evidence review and all 17 catalog HTTP tests pass. Local
  format, contract parity/structure and publication checks pass. The forthcoming
  final PR head still needs exact-head CI; inspect the linked PR checks before
  merging rather than treating the earlier `a476c0e` green run as final proof.

The final candidate is a qualified consolidation slice, not blanket M0–M4
closure. Remaining milestone deliverables and accountable owners above are not
waived. WebKit remains explicitly deferred by the user.
