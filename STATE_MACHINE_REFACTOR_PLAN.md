# Playscale state machine refactoring plan

## Objective

Move as much domain logic and behavior as practical into deterministic production state machines in crates/core. This includes the decisions that connect operations: admission, selection, authorization, effect sequencing, publication, retries, cancellation, recovery, retention and replay. Adapters should gather observations, execute identified effects, and return results.

A collection of extracted predicates is not the finish line. The production workflow must call the core decision that determines its next action, and Stateless must exercise that same workflow composition.

This is a plan only. AGENTS.md and application code remain unchanged. The scope and finding IDs come from [the state boundary audit](/Volumes/seed2/Projects/playscale/STATE_BOUNDARY_AUDIT.md), which records 73 entries and the source/test evidence behind them.

## Target boundary

Each focused domain machine should define:

- **State:** the identities, revisions, ownership, previous decisions, outstanding effects and uncertainty necessary for future decisions.
- **Inputs:** commands, current observations, effect outcomes, observed time and explicit persistence acknowledgements where needed.
- **Transitions:** complete domain admission and next-action decisions, including rejection, no-op, retry and recovery.
- **Effects:** typed operations with stable target and attempt/operation identities. Define which effects and state changes form an atomic persistence unit.
- **Policies:** pure computations used by transitions, or exposed directly for ordinary deterministic selection/validation. Do not persist reliably derived data just to call it state.
- **Verification:** production reducers/policies used directly by models, plus integration tests for their actual adapter contracts.

Use focused machines linked by explicit identities and contracts. Do not introduce a global database state machine or a generic workflow framework before concrete domains justify it.

SQL can prefilter candidates and enforce constraints, but it must not be the only specification of eligibility, ownership or invalidation. Network and filesystem adapters may classify protocol observations; domain acceptance and what to do next belong in the relevant machine.

For local browser/terminal workflows, prefer reuse through an existing Rust host/core boundary when practical. When JavaScript or Go must own local UI/transport coordination, use a deterministic production reducer in that language. Keep shared domain rules authoritative in Rust and use cross-language conformance cases for shared client contracts. A test-only Rust copy of a client algorithm is not sufficient.

## Milestone 0 Establish the behavior and migration baseline

No extraction starts by preserving every current behavior blindly. The audit found seven concrete source discrepancies, plus adapter hypotheses needing reproduction.

1. Add focused reproductions for metadata revision reuse, processing retry admission at capacity, scan recovery identity/ownership, mail recovery with unavailable payload, next-timeline search past 50 hidden candidates, offline resume across compaction, and browser mutation uncertainty after an unknown attempt.
2. Record the intended outcome for each. Keep intentional behavior changes separately visible from structural refactoring.
3. Reproduce preparation pool exhaustion and SMTP ambiguity/deadline concerns before labeling their failure modes confirmed.
4. For every future extraction, identify the production entry points, state owner, current database representation, relevant model and effect-order contract.
5. Establish targeted baseline tests for the touched domain. Use Kyoto for Rust compilation/check/test compilation as required by the repository environment; distinguish compiled tests from binaries actually executed.

**Exit:** regression cases and desired outcomes are reviewable; unresolved behavior choices are explicit; current adapter tests and migration fixtures are identified. Do not require the entire suite as a repeated prerequisite for every small change.

## Milestone 1 Establish shared core facts and command boundaries

Build only the shared types needed by the first consumers. Integrate each consumer as the types arrive.

1. Define observed binding and lineage facts: timeline/version/file identity, current and pinned revision, ordered parts, interval, duration, origin and source membership.
2. Define reusable ownership, current-credential/issuer, reference-admission and affected-owner facts. Establish the restore history, authority-withdrawal and baseline contract before dependent offline work; its adapter migration can land later. Keep observations distinct from decisions; do not replace a hidden policy with another opaque approved/bound boolean.
3. Define command outcomes and exact effect identities for admission, refusal and no-op. Reuse existing job, work, revision, access and artifact ownership types.
4. Centralize preparation/playback/download binding predicates while retaining their intentionally different eligibility requirements.
5. Consolidate policy normalization and identity construction only where semantics agree: metadata contributions, access policy, preferences, recipe/layout identity and media selection.
6. Add narrow conformance tests for optimized SQL queries that continue deriving equivalent facts.

**Exit:** at least one real adapter in each affected family uses the production core decision. No unused parallel core framework; no test-only reference implementation standing in for production.

**Primary dependencies:** P03, R02, R06 and D01 share binding/lineage facts. C04/C09, A04/A06/A10 and V03 share authorization/reference concepts. Coordinate these types before parallel domain work.

## Milestone 2 Move catalog, scan and metadata workflows into core

Split this milestone into independently reviewable changes:

1. **Metadata lifecycle and revision identity.** Model add/replace/withdraw/recreate and ownership transfer. Introduce a durable aggregate generation or durable source generations/tombstones. Handle pre-migration validator invalidation explicitly; initializing from the current sum alone does not eliminate historical revision reuse.
2. **NFO and refresh.** Model absent/unusable/parsed observations, manual pins, external-identity conflicts, origin transfer and exact refresh completion. Replace aggregate success/count conclusions with the relevant observation identities and core acceptance decisions.
3. **Scan recovery and request initialization.** Extract the actual production recovery plan, including retained/cancelled attempt IDs, full-scan mode, direct-request ownership and barriers. Delete the separate recovery algorithm from the model harness.
4. **Source lifecycle and relocation.** Move reuse/relocation-required/busy/disable/delete decisions and complete relocation evidence selection into core.
5. **Catalog mutation plans.** Extend merge/split plans with occurrence and contribution ownership, provenance, affected authorization owners and exact order-group revision effects.
6. **Matching and evidence.** Model per-file proposal history, chapter/sidecar retention and probe reuse. Consolidate candidate selection and source-aware metadata validation.

**Exit:** preview and commit use the same complete plan; metadata revisions cannot identify different acknowledged states; recovery preserves demand/direct ownership; authorization covers every affected owner. SQL applies the plan and retains uniqueness/foreign-key enforcement.

**Audit coverage:** C01–C10, N01–N07, I01, I03. Shared binding and validation work may already be delivered in Milestone 1.

## Milestone 3 Move identity administration and mail orchestration into core

Use several small domain machines rather than expanding the generic sign-in transaction into one large object.

1. **Challenge issuance and completion.** Model purpose-specific initiation, recipient eligibility, exact completion effects, explicit protocol failures and consumed receipt replay.
2. **Configuration evidence.** Model provider/email configuration revision, tests, current actor/epoch, enablement and explicit override semantics.
3. **Pending requests and proofs.** Model approve/deny/expire, action-proof creation/approval/consumption, current credential bindings and authority changes.
4. **Identity and invitation lifecycle.** Plan relink/logout/revocation effects; derive usable independent recovery methods and issuer/address eligibility from typed facts.
5. **Invalidation.** Specify which phases and receipts are preserved versus invalidated on method/configuration/restore changes. Represent required secret erasure and dependent cancellation effects.
6. **Bootstrap and budgets.** Model one-use bootstrap with explicit issuance-failure semantics, confirmation challenges and bounded admission windows/capacities.
7. **Mail.** Add owner-purpose eligibility, payload failure/recovery and exact cancellation decisions. Make ambiguity and whole-attempt deadline observations explicit. Verify bulk SQL against core cancellation.

**Exit:** core produces exact identity/session/family/proof/mail effects; stale approvals and changed authority cannot authorize new work; terminal/replay semantics are explicit; recovered transmission uncertainty is preserved. Protected writes and proof consumption remain one atomic adapter operation.

**Audit coverage:** A01–A13, M01–M05.

**Behavior decisions to settle before the corresponding implementation:** whether failed bootstrap issuance burns the proof; whether administrative invalidation rewrites consumed receipts; replay after successful final-attempt mail confirmation. Record the chosen semantics and tests rather than inheriting accidental branch order.

## Milestone 4 Move processing and playback publication into core

1. **Processing admission.** Use one production processing-specific retry/admission policy for both APIs, including current capacity.
2. **Preparation decisions.** Core resolves component/track/recipe/backend/profile choices and canonical identity; adapters render commands and observe output.
3. **Preview lifecycle.** Model staged/published identities, duplicate publication, replacement, withdrawal, artifact loss and cleanup acknowledgements.
4. **Live delivery qualification.** Feed explicit variant/index/size/duration/independence/keyframe/end observations into a publication machine. Core decides bitrate qualification, paired adaptive publication and completion.
5. **Replanning.** Include the expected pending-selection basis or apply partial changes directly in core. Centralize valid-selection checks for first admission and replanning.
6. **Viewing.** Move delivery ownership/phase admission, next-timeline search completeness, preference updates and monitoring authority selection into policies/transitions.
7. **Adapter corrections.** Reuse the existing transaction connection for preparation components. Verify blocking subtitle work keeps its resource owner until actual completion, including timeout.

**Exit:** no segment/artifact becomes publishable solely because an adapter sent a generic success; stale partial replans cannot replace newer pending selections; capacity applies to retries; next-timeline lookup distinguishes incomplete search from true exhaustion.

**Audit coverage:** P01–P06, R01–R07, V01–V03, B01–B02. Generated output invalidation and ownership must agree with Milestone 2; identity authorization primitives come from Milestone 1.

## Milestone 5 Move download, cache and local playback coordination into state

1. **Download admission and subtitle preparation.** Model exact ownership/binding, required artifact identities, conversion attempts, waiting callers, permanent/transient failure, cancellation and transfer readiness.
2. **Cache publication.** Model already-published/recover/publish/retry decisions, shared blob ownership, removal intent and precise release effects.
3. **Restore pins and checkpoints.** Validate restored identities in core; use one explicit resume policy for open, acknowledgement and rebase. Persist checkpoints before compaction.
4. **Local playback tickets.** Model replacement, exact retirement, cancellation and snapshot failure. File handles and already-admitted readers stay adapter-owned.
5. **Desktop reconciliation.** Model complete inventory, stale inventory identity, tombstones, durable report intents, recovery-before-collection and sync-before-rebase.
6. **Browser command and player coordination.** Track uncertain mutation outcomes durably where promised. Extract cross-source player replacement with explicit prepared/lease/rebind/swap/retire outcomes and stale-operation fencing.

**Exit:** crashes/retries cannot silently change ownership or restart removed transfers; cache cleanup cannot preempt required recovery; uncertain requests retain identity; an ambiguous rebind cannot permit unsafe predecessor resumption; compaction does not change intended resume behavior.

**Audit coverage:** D01–D07, L01–L04, I04. Some L01 uncertainty work can proceed immediately after its reproduction; it need not wait for server/cache refactoring.

## Milestone 6 Move scheduling, retention and restore sequencing into core

1. **Schedule dispatch.** Represent schedule revision, task/owner, cursor, batch identity, queue outcomes, disablement and next-run decision. Model the cursor reset on schedule revision as part of the transition.
2. **Backup and cache retention.** Represent ready/withdrawn/reclaimed states and durable withdrawal before removal. Define cleanup failure/retry and exact artifact ownership effects.
3. **History pruning.** Use explicit phase/reference/provenance policies preserving authoritative viewing barriers and preparation dependencies.
4. **Restore.** Define verified snapshot, old/new history, authority withdrawal and baseline capture prerequisites before publication. Align runtime and backup-script behavior with one contract.
5. **Schema conformance.** Specify which dependency changes require representation/revision invalidation. Keep triggers as enforcement and verify their agreement. Add historical ownership migration fixtures without editing shipped migrations.

**Exit:** no cleanup precedes durable withdrawal; restarted dispatch cannot skip or misattribute a batch; restore cannot publish saved authority or the wrong baseline; dependency invalidation is specified and tested rather than implicit in trigger order.

**Audit coverage:** S01–S05, I02. This milestone depends on stable identity, catalog, artifact and offline-history contracts from earlier work.

## Milestone 7 Close the model and adapter gaps

1. Check every audit ID against delivered production code, model properties and adapter tests. Mark a finding closed only when the appropriate boundary is actually used.
2. Remove superseded adapter decisions and test-owned workflow algorithms. Avoid leaving two authorities active during a long transition.
3. Extend models across at least two identities where races matter, old/current/future revisions, stale/duplicate events, cancellation/retry, crash/rollback and threshold boundaries.
4. Assert exact outcome and effect identity/count, monotonic revisions, no unauthorized mutation, stable replay and preservation of uncertainty.
5. Verify that modeled branches are reachable: capacity false, binding invalid, adaptive delivery, invalid restore pins and provenance differences must not be fixed away by the harness.
6. Run focused executed model tests and integration tests after each slice, then the required broader CI for the integrated changes. Record configured bounds, actual exploration/termination, skipped checks and adapter coverage separately.
7. Re-audit the production adapters. Remaining domain decisions require a concrete boundary justification; protocol mechanics, projections and actual I/O may stay.

**Exit:** all 73 entries have an explicit disposition, including adapter-only fixes and justified retained boundaries. The project does not claim model coverage for behavior supplied by booleans or replicated only inside a test harness.

## How to deliver the work

Use small end-to-end pull requests. Each should contain the production state/policy, converted callers, model properties, adapter conformance tests, and any required forward migration. Avoid a PR that only adds a model or extracts unused helpers.

For each change:

1. Specify the current and intended behavior with a concrete regression or equivalence case.
2. Add the smallest production state and observations that can decide it.
3. Return exact effects and define atomicity/durable ordering.
4. Switch all relevant live callers, including alternate APIs and recovery paths.
5. Exercise the same production decision in Stateless.
6. Test real persistence/effect execution and remove the duplicated logic.

Prefer additive forward migrations with an explicit compatibility and rollback boundary. Do not rewrite historical migrations. Preserve in-flight attempt identity and existing receipts where the new contract permits them; explicitly invalidate incompatible validators or recovered work. Do not introduce dual authoritative writes merely to stage the extraction.

## Dependency and parallel work

Milestone 0 establishes evidence; Milestone 1 establishes shared facts incrementally. Catalog/scan/metadata, identity/mail, and processing/playback work can then proceed in separate file-owned lanes after shared type contracts are settled. Delivery publication depends on stable selection/binding contracts, not completion of every metadata convenience policy.

Offline/cache/client work follows the required artifact and identity contracts. Schedule/retention/restore work follows the affected domain publication/withdrawal contracts. Model and adapter conformance work happens in every milestone; Milestone 7 is the final closure audit.

Do not parallelize changes to the same shared core types, migrations or model harness without assigning one owner. An implementation should land one complete rule at a time while steadily reducing adapter-owned behavior.

## Recommended starting slice

Start with the scan-recovery mismatch: extract the current production recovery plan into core, switch the adapter and demands_model to it, and preserve exact attempt identity, direct ownership and verification mode in tests. It is a bounded change that establishes the desired pattern: production workflow composition is the model.

Next address metadata revision identity with a migration design, processing retry-at-capacity, and mail payload recovery. Those provide immediate correctness value before the broader lifecycle extractions.

