# Consolidation integration/release ledger

Date: 2026-10-10. Base: `2344d03`. Owner: lane 4 (integration/release,
A00/A05/A14), branch `consolidation-release`. Independent reviewer: GPT-6.1 Sol.
This is a lane delivery record, not M1–M4 completion or release approval.

## Ownership and interfaces

Lane 4 edits package/source scripts, CI workflows, root manifests/locks when
needed, and this evidence index. It does not allocate migrations or rewrite the
shared router. Other lanes supply reviewed implementation SHAs and their domain
receipts before combined-tree qualification:

| Defect | Owner | Required input / current disposition |
|---|---|---|
| C01 | Data/catalog | Forward upgrade repair and schema-9/already-upgraded preservation receipts; pending |
| C02 | Playback + clients | Accurate server capability facts and real Go negotiation receipt; pending |
| C03 | Integration/release | Full checkout source capture, mutation tests and extracted workspace rebuild gate implemented; rebuild and packaged artifact evidence required below |
| C04 | Data/catalog | Served operation updates, authorization/body/precondition/recovery tests; pending |
| C05 | Playback + data/catalog | Canonical timeline authority and core work aggregation policy; pending |
| C06 | Playback | Frozen delivery observation contract and no-history-write pacing tests; pending |
| C07 | Clients + integration/release | Selected Demuxe/provider archive and product live-HLS matrix; pending |
| C08 | Playback | Restart/restore distinction and measured revocation receipt; pending |
| C09 | Integration/release | Client suites and source-aware heavy triggers implemented; successful full combined-tree CI remains required |

No other lane's checkpoint worktree is an implicit integration input. Record each
accepted SHA here before composing it. The initial lane-4 base has 107/146 listed
operations; consult [the inventory](../../contracts/served_operations.txt) and
[contract](../../contracts/Motion_Server_API_v2.yaml) for current identities.
Inventory coverage does not establish endpoint body or behavior conformance.

## Package and dependency decisions

The server package source archive now captures tracked files plus nonignored new
files, including all workspace members, contracts, UI/bridge assets, fixtures,
client lockfiles and recipes. Missing files and symlinks fail capture. Git ignore
rules exclude local credentials and build/download outputs. Pre/post compilation
inventories and archive-time comparison reject changed bytes or membership.
Source extraction verifies exact membership and SHA-256 hashes before building
all workspace members with `--release --locked` in a fresh target directory.
This is rebuildability evidence, not a claim of bit-for-bit reproducible binaries.
Cargo may fetch pinned registry and Git dependencies; this is not an offline SDK.

The package remains a server/browser distribution. It does not yet install the
Go/TUI or Electron host. M2's one-app package remains open. The existing package
relocation and upgrade/restore harnesses still use legacy traffic; their passing
results cannot close v2-only cutover. Keep those checks until replacement callers
land; then rerun the exact candidate through the complete acceptance workflow.

- Stateless: explicitly retain registry `statelessness = 0.1.1` and its Cargo.lock
  checksum, as documented in [vendor/STATELESS.md](../../vendor/STATELESS.md).
  This supersedes the architecture's migration-branch source-commit instruction
  for this consolidation. No library upgrade or model coverage claim follows.
- Topcoat: retain revision `341f3ff2fe16a73af5469685cf597693af5acb25`.
- Electron: retain `44.7.0`; controller tests do not qualify an installed shell.
- Demuxe: packaging still pins `1.0.0`; historical client receipts used `1.1.0`.
  Preserve both facts. C07 must supply an actual selected archive/provider and
  matrix receipt before changing the package pin or claiming player qualification.
  Historical 28-pass/2-blocked engine receipts remain in
  [client-playback](../client-playback/README.md), including the WebKit setup failure.

## Merge and release gate

Repository branch protection is not presumed. The integration owner must inspect
checks for the exact final PR head and record their URLs/SHAs before merging:

1. `CI`: Rust fmt/clippy/fast tests, contract structure/parity, client controllers,
   desktop host and Go terminal tests; dependency notices/privacy also pass.
2. `CI full`: heavy production-model/live-delivery tests, real-process integration,
   upgrade/restore, package extraction/relocation, and source reconstruction pass.
   All production Rust, tests, migrations, clients, contracts, scripts, embedded
   assets, root manifests/locks and workflow changes trigger this workflow.
   For documentation-only combined candidates, explicitly dispatch `ci-full.yml`.
3. Review every skipped/blocked result and retain native-host limitations. A green
   workflow containing a hardware skip does not qualify that hardware behavior.
4. Once lanes 1–3 land, qualify the combined installation against plan section 6,
   including candidate setup traffic, retired-route absence, original/live playback,
   cross-client resume, restricted scopes, revocation/restart and backup restore.
   Bind receipts to archive, binary, contract, provider and source hashes.

Do not merge using only the fast tier, an older green head, pending CI, or review
approval. PR completion and release/milestone acceptance are separate decisions.

## Correctness boundary

These changes govern build input integrity and CI scheduling. They introduce no
application state transition or viewing/delivery policy, so no production core or
Stateless model changes are warranted. Filesystem/Git inventory and subprocess
execution stay in build adapters. Regression tests exercise the actual inventory
and archive writer; source reconstruction separately checks adapter conformance.
Domain models and their exploration bounds remain the implementation lanes' work.

## Evidence

Implementation: `14ef924` (with subsequent review fixes recorded below).

- `python3 scripts/test_packaging.py`: 11 pass, including embedded-input changes,
  additions/deletions, symlink refusal, byte-for-byte archive round trip and
  corruption rejection before compilation.
- `node --test packages/playback/*.test.mjs packages/ui-bridge/*.test.mjs apps/desktop/test/*.test.mjs`:
  69 pass, zero skipped; controller/host units only, no browser/native A/V claim.
- `ruby scripts/contract_json.rb --check`: pass; no wire contract was changed.
- Go terminal and extracted-source reconstruction: submitted to shared Kyoto;
  final job status and receipts must be recorded before claiming success.
- Full combined-tree CI, installed package, browser/Electron and physical A/V:
  not yet qualified for this consolidation candidate.
