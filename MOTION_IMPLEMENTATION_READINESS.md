# Motion implementation readiness review

Reviewed October 9, 2026 against checkout
`52491bf3e6ef323a33dd546687129bdb3b51d4d1` and the two supplied attachments.

**Decision: ready to start M0; not yet ready to declare M0 complete or launch all
dependent implementation workstreams.** The design is sufficiently concrete to
begin contract fixtures, identity modeling, persistence contracts, baseline
capture, and the desktop/player proof. No architectural restart is needed.

## Imported baseline

- [Architecture and implementation plan](Motion_Final_Architecture_and_Implementation_Plan.md)
- [OpenAPI v2 contract](contracts/Motion_Server_API_v2.yaml)

Both imports preserve the supplied bytes. The plan's relative contract link
resolves. Its Motion source baseline exactly matches this checkout. The earlier
architecture documents remain historical/current-implementation context; their
future proposals are not competing instructions for the new target. In particular,
the new plan selects a Motion-owned Rust backend, reference-only Catabolic,
explicit authentication modes, and an Electron shell.

## Checks performed

| Check | Result |
|---|---|
| YAML parsing with duplicate-key rejection | Passed |
| OpenAPI 3.1 structural validation | Passed with `openapi-spec-validator==0.7.2` |
| Contract size | 97 paths, 146 operations, 148 component schemas |
| Local `$ref` resolution | All 2,566 reference occurrences resolve; none are external |
| Operation IDs | All unique |
| Operation ownership/status/milestones | Present on all 146 operations |
| Milestone distribution | M1: 82; M3: 44; M4: 10; M5: 8; M6: 2 |
| Local Markdown artifact links | Contract resolves; three companion links are missing, listed below |
| Source comparison | Reviewed workspace structure, existing API/auth, database writer boundary, scanner core, model harness, packaging documentation, and migration inventory |

The structural validator can be rerun in an isolated Python environment:

```sh
python3 -m venv /tmp/motion-contract-check
/tmp/motion-contract-check/bin/python -m pip install \
  'openapi-spec-validator==0.7.2' 'PyYAML==6.0.2'
/tmp/motion-contract-check/bin/python -m openapi_spec_validator \
  contracts/Motion_Server_API_v2.yaml
```

These are document/schema checks and targeted source inspection, not runtime
conformance. Generated Go/TypeScript clients were not built. Application tests,
migration/restore, browser/Electron playback, performance, and physical-device
qualification were not run for this documentation-only change. External project
commits and the plan's upstream-source claims were not independently requalified.

## Prerequisites before dependent implementation

### 1. Complete the handoff artifacts

Sections 11.7, 19.1, and 23.1 describe a larger package than the two attachments.
The directly linked `contracts/API_ENDPOINTS.md`, `agents/README.md`, and
`qualification/PARITY_LEDGER.md` are absent. The advertised contract examples,
Catabolic reference register/corpus, individual workstream briefs, document
validation report, validation script/dependencies, JSON contract, and checksum
inventory are also absent.

Obtain those companion artifacts or recreate explicitly labeled replacements.
The endpoint index and JSON form can be generated from YAML. The G01–G45 parity
mapping and reference fixtures require their actual source requirements/evidence;
do not invent the missing mapping or claim the advertised example checks ran.
Author positive/negative fixtures for the first vertical slice before freezing
its client contracts. Missing later-feature traceability need not prevent the
initial domain work, but it prevents a claim of complete scope coverage.

### 2. Make Wave 0 interfaces and ownership executable

The current workspace contains the `playscale` server and `crates/core`, with
nine migrations; the proposed `motion-*` crates, Go module, TypeScript packages,
and Electron app do not yet exist. That is expected future work. Section 4.1
defines useful service responsibilities, but concrete Rust types, persistence
commands, error variants, and transaction/effect acknowledgments still need to
be frozen for the first slice.

Assign actual owners to A00 contract changes, A02 migrations, and A14 lockfiles
before concurrent implementation. Map existing modules into the new structure
incrementally. Keep production correctness logic in `crates/core` while it owns
that boundary; update `AGENTS.md` alongside any deliberate move to
`motion-domain`. Avoid creating every proposed crate before there is a tested
behavioral slice to place in it.

### 3. Establish the migration and compatibility baseline

The plan correctly specifies a migration strategy, but it does not supply the
executable old-to-new mapping or restore evidence. Freeze representative v1
request/response fixtures and disposable database/asset/config fixtures. Specify
library/source IDs, item/edition/timeline/version mappings, ambiguous progress,
numeric revision compatibility, database/config path handling, and retained
original/rendition URLs. Record explicit restricted-mode changes to old clients.

Demonstrate backup restoration and preserve current test failures before schema
cutover. Keep the existing `BEGIN IMMEDIATE` writer boundary in `src/db.rs` or
prove its replacement. The current scanner's complete-inventory model in
`crates/core/src/scan.rs` does not by itself implement per-directory coverage,
shared scan demands, binding revisions, and dirty generations. Those are new
facts to model and persist, not flags to add only in the scanner adapter.

### 4. Prove the selected dependencies and client contract

The existing workspace pins published `statelessness` 0.1.1, while section 17.1
requests a specific source commit for the migration branch. Make that a separate
dependency compatibility change and rerun production-reducer model tests.

Select an exact Demuxe archive and Electron version/origin, then execute the M0
worker/fetch/range/audio/isolation proof. The current packaging pin and prior
browser evidence do not qualify the proposed desktop origin. Generate and compile
both client types against the actual 3.1 schemas, including conditional schemas,
unions, nullable fields, and decimal-string revisions. Structural validation alone
does not establish generator support or lifecycle conformance.

## Recommended first implementation slice

1. Capture the current v1, database, packaged-asset and test baseline in a
   disposable installation; add the missing M0 contract fixtures and ownership
   register.
2. Define library/source, occurrence/revision, and item/edition/timeline/version
   identities in production core, with exact expected identities and effects for
   copies, cuts, moves, and ambiguous legacy progress. Add corresponding model
   cases and independent golden fixtures.
3. Add schema and mapping work under the single migrator; test upgrade, foreign
   keys, rollback restoration, writer contention, and preservation of v1 IDs.
4. Integrate authorized source registration → guarded scan → logical browse →
   original playback → ordered timeline resume. Keep v1 as an adapter over the
   same authority when each migrated write path is enabled.
5. Gate wider client/workstream integration on the frozen interfaces, schema-valid
   mocks, and exact desktop/player proof required by Wave 0.

M1 carries 82 API operations, so it should be split into these reviewable slices
rather than treated as one change. Mock client work can proceed once its interface
is frozen; mocks are not evidence that the backend or M2 product is complete.

## Correctness and verification boundary

The supplied design aligns with `AGENTS.md`: identity, source/binding revision,
attempt ownership, partial coverage, cancellation, viewing sequence/manual epochs,
delivery generations, and publication eligibility belong in production core
decisions. SQLite, filesystem journals, hashing/probing, process termination,
networking, and timers remain adapters that report the facts those decisions need.

Statelessness must exercise those production decisions with stale/duplicate
events, failure interleavings, and exact effect identities/counts. Real integration
tests must separately prove transaction atomicity, durable effect ordering,
filesystem recovery, and process ownership. This import changes no behavior and
therefore adds no new reducer or model claim.

## Design 1.1.0: Topcoat presentation (adopted October 9, 2026)

The supplied Topcoat revision is now the official design, replacing the
React/Vite application decision:

- [Architecture and implementation plan](Motion_Final_Architecture_and_Implementation_Plan.md),
  revision "Topcoat frontend adoption" (sha256 `4b6e4d87…5d0e`).
- [OpenAPI v2 contract](contracts/Motion_Server_API_v2.yaml) (sha256
  `cd4100be…cfa0`). The only change from 1.0.0 is the non-wire annotation
  `x-design-version: 1.1.0`; paths, operations and schemas are unchanged.
- [Topcoat research](research/TOPCOAT_RESEARCH.md) (sha256 `00bce317…5be7`).

Bytes are imported as supplied. The sections of this review above describe
the 1.0.0 import and remain valid except where they name React/Vite.

The plan links to companion artifacts that were not supplied and are absent:
`CHANGELOG_TOPCOAT.md`, `contracts/TOPCOAT_PRESENTATION_CONTRACT.md`,
`qualification/TOPCOAT_ACCEPTANCE.md` (TC01–TC20) and
`qualification/TOPCOAT_REVISION_VALIDATION.json`. As with the earlier
missing companions, obtain them or author explicitly labelled replacements;
do not infer TC01–TC20 from the plan text alone.

The React/TypeScript A11/A12 work started against design 1.0.0 is
discarded from the active line. It is preserved, unmerged, on branch
`archive/a11-a12-react-typescript` for reference only. Note that design
1.1.0 still assigns A11 a small external TypeScript bridge and the playback
coordinator (`packages/ui-bridge`, `packages/playback`) and A12 the Electron
host; those must be rebuilt against the Topcoat presentation contract
rather than revived wholesale.
