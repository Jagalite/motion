# Provisional combined consolidation candidate

Date: 2026-10-11. Owner: integration/release (lane 4).
Branch: `consolidation-combined`, stacked on `consolidation-release` (PR #14).
This composes reviewed draft inputs. It is not M1–M4, cutover, or release acceptance.

## Exact inputs and conflict decisions

| Input | Source SHA | Composition commit |
|---|---|---|
| [Integration/release #14](https://github.com/Jagalite/motion/pull/14) | `28f63324beb054b5e1b84c13a1b96520403f0968` | Base |
| [Data/catalog #17](https://github.com/Jagalite/motion/pull/17) | `14b9aaa780c36c851721caee45199574f36d4422` | `efc3754` |
| [Playback/viewing #16](https://github.com/Jagalite/motion/pull/16) | `f772a57b322e12443b2f174c0d9f3415539c0984` | `5864354` |
| [Clients #15](https://github.com/Jagalite/motion/pull/15) | `9520b397e0486c3bf2bef0b525706aa7ad7a00b5` | `f721e70` |

The lane branches and their working trees were not edited by this composition.
New input heads require explicit reconciliation and affected reruns.

- Keep lane 4's portable `KYOTO_SCRIPT` spelling in the shared plan.
- Keep data/catalog's scoped, transactional `organization::facts_in` query;
  aggregate the authorized timelines using production core's `work_watched`.
  Do not replace it with the unscoped playback-lane query. Retain both the
  authorization/hidden-cut regression and the legacy-history/manual-override case.
- Retain every player regression from both lanes: ordered pacing, replacement
  identity, epoch fencing, restart persistence and provider capability observation.
- The auto-merged upgrade harness retains old-binary v1 setup, candidate v2
  observations, canonical repair checks and the schema-37 expectations.
- The identical client/integration Linux cache fix coalesces without replacing
  either lane's other changes.

Core owns authority, attribution, order, revision, watched aggregation and pacing
rules. This merge introduces no new domain policy; adapters continue to enforce
transactional scope and publication ordering. Its critical composition boundary
is preserving authorization while joining the two watched-state implementations.

## Evidence and review

GPT-6.1 Sol reviewed each merge and cumulative composition through
`f721e70b6bab922f003fea4e2d6046ab53f9367d`; no actionable findings. Source review
included upgrade/restore auto-merges and rendered epoch/write preconditions.

- JavaScript controller/host tests: 73 pass, zero skipped.
- Packaging regressions: 12 pass.
- OpenAPI structure: 97 paths, 146 operations, 148 schemas, 2,573 references.
- YAML/JSON parity and formatting: pass.
- Focused Rust `v2_organization_viewing` compilation: Kyoto job
  `6c462d6feea44f32acb3a00c6dbac61d`, pending at record creation. Test execution,
  combined upgrade/restore, package and browser/native qualification are separate
  gates. Use the stacked PR's current checks and evidence, not input-branch passes.

The served inventory now lists 124/146 operations. This is route inventory, not
payload, authorization, workflow or product qualification.

## Concrete remaining integration gates

The following foundation/library operations remain absent from the served
inventory and are required before the relevant caller/cutover workflows can pass:

| Group | Missing operations |
|---|---|
| Preparation | `getProcessingCapabilities`, `listRecipes`, `createPreparation`, `listRenditions` |
| Schedules | `listSchedules`, `createSchedule`, `getSchedule`, `replaceSchedule`, `deleteSchedule` |
| Operations | `getStorage`, `createBackup`, `listBackups` |

Preparation is an observed runtime blocker: the clients' real smoke reached
`POST /api/v2/processing/preparations` and received HTTP 404. These routes belong
to the reviewed preparation/catalog/operations services; this composition does
not introduce a private API or treat legacy provisioning as v2 acceptance.

C07 also remains open. The Demuxe 1.1.1/Shaka candidate is not qualified and failed
two installed-package API checks in the client lane. Packaging continues to pin
1.0.0 pending selection and product-matrix proof. Do not equate an installed
provider or passing asset inventory with actual live conversion/player acceptance.

Remaining current-server v1 callers, default Topcoat cutover, legacy-route absence,
one-app Go/Electron installation and the full section-6 installed workflow remain
open. The canonical viewing repair/pacing changes must be tested together with
those callers. M5/M6 remain their declared later scope. This provisional merge
supplies a concrete combined source for the next gates, not their completion.
