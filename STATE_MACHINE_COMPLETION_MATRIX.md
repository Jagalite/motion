# State-machine completion matrix

Base: `6edaa2d59822bdba1a1eb2c20843588a6fd7d22b`. Historical pass claims are inherited receipts, not new execution. No entry is closed by source extraction alone.

| ID | Finding | Current status |
|---|---|---|
| C01 | Source registration and removal | Registration/removal policies; 114-state lifecycle and libraries API passed |
| C02 | Split occurrence ownership | Exact split pins/ownership; 24-state model and persistence/large-plan API passed |
| C03 | Merge contribution and provenance precedence | Exact merge contribution winners; core and NFO/merge SQL passed |
| C04 | Authorization of the complete mutation footprint | Complete mutation footprint including prior order owners; core and scoped API passed |
| C05 | Matching history and one open proposal | Proposal history/reopen; 179-state model and real workflow passed |
| C06 | Relocation completeness and plan envelope | Exact relocation scope and history-bound envelope; core and relocation/history API passed |
| C07 | Order placement effects and revision changes | Exact order effects/no-op/overflow; core and order API passed |
| C08 | Catalog command preconditions | Typed catalog preconditions; core and assignment API passed |
| C09 | Historical receipt disclosure | Current-authority receipt projection; core and receipt API passed |
| C10 | Candidate and derived catalog policies | Candidate policy and version visibility; core plus 62 SQL cases and catalog API passed |
| N01 | Durable metadata revision identity | Durable metadata generation/ETag; inherited implementation re-executed in clean canonical fast tier; model/adapter targets passed |
| N02 | NFO contribution lifecycle | Exact NFO lifecycle; inherited implementation re-executed in clean canonical fast tier; model/adapter targets passed |
| N03 | Metadata refresh admission and completion | Attempt-fenced metadata completion; inherited implementation re-executed in clean canonical fast tier; model/adapter targets passed |
| N04 | Chapter and subtitle evidence retention | Chapter/sidecar evidence; inherited implementation re-executed in clean canonical fast tier; model/adapter targets passed |
| N05 | Incremental probe reuse | Incremental probe reuse; inherited implementation re-executed in clean canonical fast tier; model/adapter targets passed |
| N06 | Scan request initialization | Scan initialization effects; inherited implementation re-executed in clean canonical fast tier; model/adapter targets passed |
| N07 | Metadata validation duplication | Metadata validation; inherited implementation re-executed in clean canonical fast tier; model/adapter targets passed |
| P01 | Processing retry admission | Processing retry admission; inherited implementation re-executed in clean canonical fast tier; model/adapter targets passed |
| P02 | Preview publication and reclamation | Generation publication/cleanup core; 101-intent fairness and legacy upgrade reclamation passed |
| P03 | Preparation binding validity | Exact preparation/playback/download binding; core and binding adapters passed; canonical full gate passed |
| P04 | Preparation selection and recipe eligibility | Selection/recipe policy; bounded model and reserved-writer pool regression passed |
| P05 | Recipe and preview identity | Full recipe identity; 50-state model and preparation adapters passed |
| P06 | Additional processing observations | Restore outputs/receipts and hardware witnesses; core and transactional rollback tests passed |
| A01 | Sign-in issuance and recipient eligibility | Typed issuance/recipient policies; email and five completion-purpose model seeds plus sign-in API passed |
| A02 | Completion effects and consumed replay | Exact completion/replay effects; production models and sign-in API passed |
| A03 | Provider and email configuration evidence | Configuration effects and revision policy; models/API and final helper wiring passed in clean fast tier |
| A04 | Independent recovery method classification | Independent recovery raw-fact classification; core matrix and recovery API passed |
| A05 | Pending request approval and expiry | Approve/deny/expire transitions; model and approval API passed |
| A06 | Action-proof lifecycle | Authority-bound action proofs; 320-state model and proof API passed |
| A07 | Identity relinking and logout effects | Relink/logout exact effects; purpose models and sign-in API passed |
| A08 | Configuration and restore invalidation of transactions | Terminal receipts preserved, all queued owner mail canceled; actual phase×mail SQLite regression passed |
| A09 | Rate and capacity admission | Rate/capacity/expiry policy; 1,000-request boundary and shared trim expiry tests passed |
| A10 | Invitation identity and replay | Issuer-bound invitation replay and complete keyset search; core and 70-stale-invitation API passed |
| A11 | Mail test confirmation | Terminal mail-test confirmation receipt; core and expiry/final-attempt API passed |
| A12 | Bootstrap proof and desktop issuance | Bootstrap proof burns before issuance; model and injected rollback API passed |
| A13 | Canonical access validation and status | Canonical access validation/status; core and 69 access API tests passed |
| M01 | Purpose-specific owner eligibility | Typed purpose-specific mail owner; exhaustive 2-state/2,400-transition model and mail adapters passed |
| M02 | Payload failure during recovery | Unavailable mail payload recovery; inherited implementation re-executed in clean canonical fast tier; model/adapter targets passed |
| M03 | Bulk cancellation conformance | Queued-only cancellation; real SQLite phase matrix passed |
| M04 | Transmission ambiguity observation | SMTP terminator/flush ambiguity; actual transport regression passed |
| M05 | Whole-attempt deadline | Whole-attempt DNS/authority deadline; adapters and final backward-clock budget clamp passed |
| V01 | Viewing delivery admission and rebinding | Viewing admission; 112-transition model and viewing HTTP passed in final canonical full target |
| V02 | Complete next-timeline selection | Complete next-timeline cursor; page-boundary models and hidden-pages API passed |
| V03 | Organization disclosure and references | Organization disclosure; 20-state model and 8 API tests passed |
| D01 | Download binding and ownership admission | Owner/device/binding admission; core and downloads API passed |
| D02 | Subtitle artifact lifecycle and transfer completion | Subtitle publication/report ordering; 120-state artifact model and real FFmpeg downloads API passed |
| D03 | Restored-pin validation | Restored-pin exactness; core and real host Store negatives passed |
| D04 | Cache publication and shared ownership | Typed cache publication/release; 1,908-state model and Store/Cache/real-host Node passed; whole-pass ordering shared JS, not global Rust model |
| D05 | Resume checkpoint equivalence | Durable checkpoint/resume equivalence; 55-state model and host compaction/rebase tests passed |
| D06 | Local playback tickets | Independent ticket identity; 21-state model and same-download stale-close host test passed |
| D07 | Offline target and cache-entry policy | Offline effective duration/identity; core and host Store/Cache passed |
| S01 | Schedule dispatch lifecycle | Exact dispatch batch/cursor; 332-state model and real schedule API passed |
| S02 | Backup retention lifecycle | Retention withdrawal/reclamation and durable cursor; 501-blocked-prefix API passed |
| S03 | History pruning and generated-cache cleanup contract | Bounded history retention core eligibility; SQL/storage adapters passed |
| S04 | Representation invalidation dependencies | Representation dependency contract; 35 historical trigger cases passed |
| S05 | Historical ownership migration | Historical ownership contract; 48 shipped-migration cases passed |
| R01 | Live output qualification and adaptive publication | Exact publication decisions/window traces and cancel/retry adapter passed; corrected real-media target passed; canonical full gate passed |
| R02 | Exact playback binding | Exact binding model and wrong-timeline API passed; canonical full gate passed |
| R03 | Partial-replan selection identity | Selection generation fence/model and wire compatibility passed; long delivery lifecycle model passed; canonical full gate passed |
| R04 | Valid selection as an admission prerequisite | Selection admission composition/multipart models and HTTP paths passed; canonical full gate passed |
| R05 | Compatibility and track/subtitle resolution | Compatibility/track/recipe core and HTTP coverage; corrected copied-media fixtures passed |
| R06 | Generated-file authorization lineage | Generated lineage core and 64 SQLite cases passed; canonical full gate passed |
| R07 | Preferences and monitoring authority selection | Preference/monitor core, 36-state model and HTTP exhaustion test passed |
| I01 | Scan recovery composition must be production core | Scan recovery composition; inherited implementation re-executed in clean canonical fast tier; model/adapter targets passed |
| I02 | Restore authority withdrawal and baseline capture | Shared restore authority inventory and baseline fence; Rust restore/Python installed-tool tests passed; released upgrade/rollback gate passed |
| I03 | Registration and legacy repair policy callers | Legacy repair/exact-root registration; inherited implementation re-executed in clean canonical fast tier; model/adapter targets passed |
| I04 | Online resume selection | Shared online/offline resume policy; core and HTTP paths passed; canonical full gate passed |
| L01 | Pending mutation uncertainty | Shared uncertainty classification; Node and Go conformance passed |
| L02 | Desktop download reconciliation | Complete recovery-before-collection protocol; 1,296 combinations and real-host Node passed |
| L03 | Cross-source player replacement | Replacement coordinator plus actual deferred/success/rollback callers; 203-test Node gate passed, reviewed lifetime fixes closed |
| L04 | Viewing conflict recovery policy | Viewing conflict authority and retry; actual writer/bridge Node tests passed |
| B01 | Preparation acquires another pool connection | Existing writer reused; actual saturated-pool adapter regression passed |
| B02 | Blocking subtitle validation lifetime | Actual blocking caller retains permit/lock through timeout/cancel; production-seam tests passed |


## Evidence ledger and completion criteria

All 73 IDs have production-boundary implementations or an explicitly retained adapter boundary, with per-ID evidence below. This is **not** 73-ID release closure: Linux aggregate and resource qualification passed; native macOS packaging and remote CI remain open. Latest A03/M05 refinements passed the clean fast tier. No ID is classified unstarted or source-only; qualification limits are explicit in each row rather than hidden by a blanket “done”.

- [Catalog and client IDs](qualification/completion/state-machine-client-catalog.md): exact callers, effect assertions, bounds and real adapter names.
- [Processing and sign-in flow IDs](qualification/completion/state-machine-processing-flows.md): preview generations, blocking ownership, flow-purpose models and adapters.
- [Playback/viewing IDs](qualification/completion/state-machine-playback-viewing.md): exact-output models, page bounds, cancellation seams and real-media fixture diagnosis.
- [Download/storage IDs](qualification/completion/state-machine-storage.md): cache identity, ticket generations, dispatch, cleanup fairness and shared restore inventory.
- [Authentication/mail IDs](qualification/completion/state-machine-auth-mail.md): authority/revision/purpose decisions and actual transport/transaction evidence.
- [Inherited 11 IDs](STATE_MACHINE_REFACTOR_PROGRESS.md): original bounds/design; the clean canonical fast run re-executed their model and adapter targets, rather than adopting historical receipts as new results.

## Final local aggregate checkpoint — 2026-11-02 09:04 UTC

- Initial canonical fast tier: **895 passed, 4 failed, zero ignored**. Three new catalog fixture expectation/key mistakes were corrected without production weakening; focused final catalog rerun **25/25 passed**. Expanded mail-owner domain exceeded its old transition ceiling; raising only the ceiling while retaining required graph exhaustion produced **2 states / 2,400 transitions, zero skipped**, passed. The clean canonical fast rerun subsequently **passed: 899 tests, zero failures/ignored, 96 summaries, exit 0**.
- Initial canonical full tier failed: five test-fixture audio timestamp failures and an exhaustive downloads-model **SIGKILL (signal 9)**. Kernel cause was not directly observed; memory exhaustion was consistent with observations. Test-only audio timestamp normalization fixed the fixtures without weakening production gates. A lossless shared representation retained all 21 facts, exact equality/hash, original transitions/checks/codecs and bounds (12 million states / 200 million transitions / depth 100). Independent review and depth-5 reference parity (2,453 states / 8,768 transitions, DepthBound, zero skips) passed.
- Shared-representation full run **passed 75/75**, but took **6,306.2 seconds (105.1 minutes)**, exceeding the unchanged 90-minute CI budget. A separately reviewed test-only exact two-entry cache and allocation-free 21-field equality avoided reconstruction without reducing coverage. Eight targeted parity/mutation/cache tests and strict Clippy passed.
- **Final canonical full tier passed 77/77, zero failures/ignored, exit 0**, in **4,837.5 seconds (80.625 minutes)**, 562.5 seconds below the unchanged 90-minute budget on this machine. Same unoptimized debug profile and exact graph bounds; this is local timing evidence, not a remote CI pass. Downloads exhausted **4,765,164 states / 76,242,624 transitions**, zero skipped checks, in 2,305.34 seconds. Other graph counts also matched the prior successful run. Kernel child peak RSS was **2,668,851,200 bytes**, sampled aggregate peak **2,791,112,704 bytes**, minimum free disk **2,648,027,136 bytes**; no resource cutoff. Final model source is commit `5e73ec4f6c7428f41be3ca1c3cdf6033275201a0`; complete counts, bounds, log hashes and binaries are in [verification.json](qualification/completion/verification.json).
- Strict workspace/all-target Clippy passed with only the two pre-existing allowed lints; final rerun after A03/M05 changes also passed. Formatting passed.
- Real-host Node: **203/203 passed, zero skipped**. Host Store **26/26**, Cache **14/14**; terminal Go operations and shared mutation fixtures passed.
- API/adapter gates: library **115/115**, sign-in **26/26**, access **69/69**, catalog persistence **22/22**, catalog workflows **31/31**, metadata/schedules/backup **28/28**, downloads **13/13**, organization/viewing **8/8** passed in initial fast or focused final runs.
- Python backup **4 tests**, historical ownership **48 cases**, representation dependency **35 cases** passed. Contract/Ruby parity passed: **152 paths, 215 operations, 234 schemas, 3,732 references**.
- Real-process integration **passed all 16 checks** against candidate SHA-256 `a76a2172dd4ad9538f2bdd3b8964676661846330b328d8409b727476508bd3d6`. Released-baseline upgrade/rollback/restore **passed** from exact revision `52491bf3e6ef323a33dd546687129bdb3b51d4d1`, built with Rust 1.95 in a distinct target. Baseline SHA-256 `11419e29abfe76a1a6d016755e48dc7b6003e63f13d0fc69f2ca012d107ab825`; migration 9→69, identity/media/progress preservation, one verified upgrade backup, restart idempotency, baseline refusal of upgraded DB, both rollback restores, candidate restore and epoch rotation all passed.
- Official native package gate is **blocked on Linux**: it requires macOS arm64, signing/ditto and native media receipts. Portable helper tests (17/17 passed) do not waive that gate. Official source-publication preflight also found inherited absolute developer-path links in the audit/plan; those were made repository-relative, and preflight now reaches the explicit platform rejection.

## Independent review dispositions

Reviews were bounded changed-code reviews, not blanket approval of 73 entries. Preview cleanup starvation (100 failed prefix) and backup starvation (500 failed prefix) were fixed by durable advancing cursors and actual 101/501-record regressions. Blocking cancellation now tests the actual caller. Ticket models distinguish same-download generations. Client operation fencing is separate from installed callback lifetime; deferred-pause and post-resolution success/rollback regressions passed, and source re-review closed the findings. Playback models now assert exact decisions/window eviction and actual cancellation/retry.

OIDC correction: sign-in start already expired old requests. The fixed gap was requests expiring **between start and callback**, not an indefinite OIDC-only outage; both paths now use shared core expiry. Mail-purpose matching was an acceptance-quality gap, not a demonstrated exploit. Cache bounded graphs do not prove arbitrary sidefile combinations or whole-pass collection order: real host sidefile tests and the shared JavaScript recovery protocol supply distinct adapter evidence.

No push, merge or deployment performed. Prior paused source/evidence preserved. Only the explicitly approved old compiler-intermediate manifest was removed (1,152 files / 5,145,571,097 bytes).
