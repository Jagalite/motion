# Playscale state and behavior boundary audit

Audit date: 2026-10-29. Source baseline: d2d091aeac3ae127778243496447bfde6042adcd.

Playscale has a substantial deterministic core, but its correctness boundary is incomplete. Several workflows model only an inner predicate while adapters still decide identity, ownership, recovery, publication, or replay semantics. Some model harnesses also supply behavior that production implements differently. The appropriate changes are focused domain states and pure policies, with explicit adapter contracts. They are not a wholesale move of application data or I/O into one global state machine.

This is a source audit. No compilation, tests, model exploration, live transport, or performance checks were executed. Test references below describe existing test source, not newly verified passes. Source-established counterexamples are distinguished from architectural gaps and unverified failure hypotheses. Application code was not changed. The pre-existing untracked MOTION_CATABOLIC_BACKEND_INTEGRATION.md was left untouched.

## Scope and classification

The report contains 73 audit entries, including related policy extractions and adapter conformance follow-ups. They do not imply 73 separate state machines.

The inventory covers 80 server Rust files, 43 core source modules, 34 core test files, 8 motion-ui source files, 3 motion-ui-host source files, 64 SQL migrations, browser playback/UI bridge modules, desktop modules, and terminal production Go. Server and host production functions were divided by domain for inspection; relevant core functions, models, callers, schema enforcement, and test sources were cross-checked. Client presentation and external dependencies have narrower coverage described below.

Excluded from a full implementation audit: vendored Demuxe/Shaka internals, research/prototypes, build/package tooling, and qualification scripts. Existing qualification documents were not treated as current execution evidence. No claim is made that this audit proves absence of other defects.

Finding classes:

- **State:** prior events, identity, revisions, ownership, retries, recovery, publication, or uncertainty must influence a production transition.
- **Policy:** deterministic decisions should be shared with core, but need no additional persistent state.
- **Conformance:** core already specifies a rule, or should specify an effect/ordering contract; the adapter must enforce it. SQL, network, filesystem and timer mechanics remain outside core.
- **Client state:** a focused deterministic client workflow is appropriate. It need not be forced into Rust core if its production owner is JavaScript or Go.

Priority 1 means address a concrete discrepancy or protect an important identity/publication/recovery boundary first. Priority 2 means substantive missing behavior/model coverage. Priority 3 means lower-risk policy consolidation. These are work priorities, not vulnerability severity ratings.

## Findings requiring corrective investigation first

These cases are stronger than stylistic extraction opportunities.

1. **Metadata revision reuse.** With all other sources fixed, NFO A at revision 1 produces resource revision 2; withdrawal deletes the row and produces revision 1; a different NFO B is recreated at revision 1 and produces resource revision 2 again. An old If-Match can therefore pass after the represented state changed. Core explicitly assumes documents are never deleted, while production deletes them. See N01.

2. **Processing retries exceed the admission bound.** New processing requests require fewer than 100 active jobs, but retry paths do not apply that check. A retryable failed job alongside 100 active jobs can become the 101st active job. This concerns queue admission, not proof of 101 concurrent encoders. See P01.

3. **Scan recovery model and production disagree.** The model retains the recovered attempt and removes the queued follow-up. Production retains the queued follow-up, cancels the interrupted attempt, and transfers verification mode and direct-request ownership. The harness is checking a different composition policy. See I01.

4. **Recovered mail uncertainty can be discarded.** Core infers ambiguity from an expired Sending lease. The adapter's unavailable-payload branch then constructs Failed from the old message, dropping that newly inferred ambiguity. See M02.

5. **Bounded next-timeline search can falsely report exhaustion.** Fifty unreadable successors followed by a readable 51st successor return no result because the limit precedes the readability checks. See V02.

6. **Offline resume depends on whether acknowledged events were compacted.** For a paused event exactly at duration, opening uses that duration before compaction; checkpointing clears it, so opening defaults to zero afterward. See D05.

7. **Browser mutation uncertainty differs from terminal policy.** A lost acknowledgement followed by a 403 clears the browser's pending idempotency key, even though the second response does not establish whether the first attempt committed. The terminal preserves uncertainty in this case. See L01.

Additional adapter issues requiring focused reproductions are preparation's second pool acquisition while holding a writer, SMTP post-terminator write/flush ambiguity, and whole-mail-attempt deadline enforcement. These are not reasons to move database/network operations into core.

## Catalog identity and source lifecycle

### C01 Source registration and removal

**Priority 2 · State.** [libraries.rs:186](/Volumes/seed2/Projects/playscale/src/libraries.rs:186), [libraries.rs:415](/Volumes/seed2/Projects/playscale/src/libraries.rs:415), and [libraries.rs:471](/Volumes/seed2/Projects/playscale/src/libraries.rs:471) decide same-root reuse versus relocation, busy exclusion changes, pending-demand deletion refusal, and hard-delete versus disable-and-withdraw. Core covers source overlap, exclusions, revision checks and relocation, but not this lifecycle.

Model root identity, memberships, active work, retained files/history and demands. Return explicit reuse, relocation-required, busy, delete, or disable/withdraw effects. Keep directory identity observation and SQL execution outside. Existing coverage: [libraries tests:224](/Volumes/seed2/Projects/playscale/tests/libraries.rs:224), [321](/Volumes/seed2/Projects/playscale/tests/libraries.rs:321), [569](/Volumes/seed2/Projects/playscale/tests/libraries.rs:569). Add sequences through registration, work, removal, recovery and re-registration.

### C02 Split occurrence ownership

**Priority 1 · State.** [curation.rs:578](/Volumes/seed2/Projects/playscale/src/curation.rs:578) decides which physical files and identical-content copies follow split versions, depending on revisions still pinned by retained versions. Core SplitPlan models structural movement, not independent occurrence ownership.

Include occurrence IDs, current owners and revision pins in the reviewed plan; return exact destination assignments. Model shared revisions, retained versions, stale plans and duplicate commit. Structural SQL/core comparison tests at [catalog_persistence.rs:483](/Volumes/seed2/Projects/playscale/tests/catalog_persistence.rs:483) do not establish this extra rule. NFO ownership consequences are integration-tested at [catalog_workflows.rs:1222](/Volumes/seed2/Projects/playscale/tests/catalog_workflows.rs:1222) and [1348](/Volumes/seed2/Projects/playscale/tests/catalog_workflows.rs:1348).

### C03 Merge contribution and provenance precedence

**Priority 2 · State/Policy.** [curation.rs:325](/Volumes/seed2/Projects/playscale/src/curation.rs:325) chooses target-wins metadata by source, conditional NFO-origin transfer, and artwork precedence by role/source. Multiple retired sources are processed in plan order. Core merge plans omit these transfers.

Plan exact contribution, origin and artwork winners, including revision effects and multi-source ordering. Preserve SQL atomicity. Existing NFO merge tests cover selected cases; identity_model covers structural and external-ID conflicts, not these contribution ownership effects. Coordinate with N01 and N02.

### C04 Authorization of the complete mutation footprint

**Priority 1 · Policy/Conformance.** [v2/catalog.rs:433](/Volumes/seed2/Projects/playscale/src/v2/catalog.rs:433) separately reconstructs all works affected by merge relationships/order membership; [1984](/Volumes/seed2/Projects/playscale/src/v2/catalog.rs:1984) separately checks old/new order-group owners. Core plans the mutations but does not expose their complete authorization footprint.

Have the plan return affected owners and authorize that set against current grants at commit. Existing [v2_catalog.rs:2173](/Volumes/seed2/Projects/playscale/tests/v2_catalog.rs:2173) checks hidden prior group ownership. Model permission changes between preview/commit and assert every mutated owner is authorized.

### C05 Matching history and one open proposal

**Priority 2 · State.** [matching.rs:207](/Volumes/seed2/Projects/playscale/src/matching.rs:207) selects/stales proposals by history, and [425](/Volumes/seed2/Projects/playscale/src/matching.rs:425) adds a competing-open-proposal guard after core reopen. The model stores only one proposal.

Represent per-file open proposal identity and relevant prior decision identity/order. Use the same raise/keep/stale/reopen transition in production and models. Keep the unique SQL index. Existing history/reopen tests: [catalog_workflows.rs:714](/Volumes/seed2/Projects/playscale/tests/catalog_workflows.rs:714), [1425](/Volumes/seed2/Projects/playscale/tests/catalog_workflows.rs:1425). Add two-proposal interleavings and exact decision-receipt preservation.

### C06 Relocation completeness and plan envelope

**Priority 2 · Policy.** [libraries.rs:567](/Volumes/seed2/Projects/playscale/src/libraries.rs:567) decides which files must be verified from enabled/available/excluded state before core sees the plan. [v2/libraries.rs:562](/Volumes/seed2/Projects/playscale/src/v2/libraries.rs:562) also implements actor/source/expiry admission.

Core should select or validate the complete required file set and bind the plan to actor, source and expiry. Keep hashing/stat outside. Existing [libraries.rs:385](/Volumes/seed2/Projects/playscale/tests/libraries.rs:385) and [v2_catalog.rs:1892](/Volumes/seed2/Projects/playscale/tests/v2_catalog.rs:1892) cover stale/replayed relocation; model omitted-required-file cases.

### C07 Order placement effects and revision changes

**Priority 2 · State.** [curation.rs:1268](/Volumes/seed2/Projects/playscale/src/curation.rs:1268) decides membership changes and which old/new group revisions advance. [1487](/Volumes/seed2/Projects/playscale/src/curation.rs:1487) removes invalid memberships and advances revisions. Core placement only establishes eligibility.

Return membership and exact revision effects from a focused order transition. Cover no-op movement, stale/exhausted revisions, cross-group movement and merge/split invalidation. Existing golden tests cover eligibility; identity_model lacks order-group state.

### C08 Catalog command preconditions

**Priority 2 · Policy.** [catalog.rs:337](/Volumes/seed2/Projects/playscale/src/catalog.rs:337) adds expected-edition/same-work reassignment checks; [curation.rs:1063](/Volumes/seed2/Projects/playscale/src/curation.rs:1063) checks new timeline ownership; [v2/catalog.rs:946](/Volumes/seed2/Projects/playscale/src/v2/catalog.rs:946) forbids changing derived membership/media family.

Extend the relevant core command facts with these ownership and immutability preconditions. Existing reassignment/version policies remain useful. Integration cases exist at [catalog_workflows.rs:877](/Volumes/seed2/Projects/playscale/tests/catalog_workflows.rs:877) and [v2_catalog.rs:477](/Volumes/seed2/Projects/playscale/tests/v2_catalog.rs:477); add pure stale/foreign-owner refusal matrices.

### C09 Historical receipt disclosure

**Priority 1 · Policy.** [v2/catalog.rs:1149](/Volumes/seed2/Projects/playscale/src/v2/catalog.rs:1149) reauthorizes saved plan owners; [1170](/Volumes/seed2/Projects/playscale/src/v2/catalog.rs:1170) filters historical library/parent/default-timeline/artwork identities against current visibility.

Use a typed receipt projection policy, shared with current reads where semantics agree. Model replay after revocation, merge and ownership change. [v2_catalog.rs:1549](/Volumes/seed2/Projects/playscale/tests/v2_catalog.rs:1549) already covers reconciliation replay scope changes; generic idempotency models do not execute this filtering.

### C10 Candidate and derived catalog policies

**Priority 3 · Policy.** [matching.rs:127](/Volumes/seed2/Projects/playscale/src/matching.rs:127) decides title/year candidate eligibility, source precedence, evidence codes and candidate cap/order. [curation.rs:470](/Volumes/seed2/Projects/playscale/src/curation.rs:470) chooses split kind; [1162](/Volumes/seed2/Projects/playscale/src/curation.rs:1162) classifies generated versions. [v2/catalog.rs:479](/Volumes/seed2/Projects/playscale/src/v2/catalog.rs:479) derives availability and default timeline, with indivisible multipart visibility elsewhere in that module.

Centralize reusable domain predicates, not ordinary query materialization. For SQL visibility, a core reference predicate plus differential adapter tests is sufficient. Matching models currently receive fabricated candidates; existing visibility tests cover examples, not policy equivalence.

## Scanning and metadata

### N01 Durable metadata revision identity

**Priority 1 · State · static counterexample.** [core metadata.rs:225](/Volumes/seed2/Projects/playscale/crates/core/src/metadata.rs:225) sums source revisions under a no-deletion assumption. [nfo.rs:168](/Volumes/seed2/Projects/playscale/src/nfo.rs:168) deletes a contribution and [116](/Volumes/seed2/Projects/playscale/src/nfo.rs:116) recreates it at 1. [v2/metadata.rs:161](/Volumes/seed2/Projects/playscale/src/v2/metadata.rs:161) derives the resource revision and [274](/Volumes/seed2/Projects/playscale/src/v2/metadata.rs:274) checks If-Match against it.

Use a durable aggregate generation, or durable per-source generations/tombstones, covering add/replace/withdraw/recreate/merge/split. Model that different acknowledged resource states cannot reuse a revision. Existing normal precondition and NFO withdrawal tests do not cover this composed sequence.

### N02 NFO contribution lifecycle

**Priority 1 · State.** [nfo.rs:97](/Volumes/seed2/Projects/playscale/src/nfo.rs:97) chooses preserve-on-unusable, withdraw-on-absence, origin transfer, manual-pin suppression and identity-conflict fallback to values without an external identity.

Core's manual-pin predicate already exists; the missing piece is the combined mutation plan over prior origins/contributions and current ownership. Return precise contribution/origin/revision effects. Existing tests at [catalog_workflows.rs:977](/Volumes/seed2/Projects/playscale/tests/catalog_workflows.rs:977), [1041](/Volumes/seed2/Projects/playscale/tests/catalog_workflows.rs:1041), [1222](/Volumes/seed2/Projects/playscale/tests/catalog_workflows.rs:1222) cover useful adapter scenarios. Add state exploration across sidecar changes, moves, conflicts and rollback.

### N03 Metadata refresh admission and completion

**Priority 2 · State/Policy.** [nfo.rs:249](/Volumes/seed2/Projects/playscale/src/nfo.rs:249) compares current path/root/root identity, binding/content revisions, availability and work membership. [metadata_jobs.rs:120](/Volumes/seed2/Projects/playscale/src/metadata_jobs.rs:120) combines these with attempt/authorization/publication results into counts and a generic success.

Model accepted/rejected observation identities and a completion decision with current attempt and publication acknowledgement. Keep savepoints/transactions outside. Existing core refresh_complete checks counts, not those identities. Integration tests cover cancellation/retry/replaced roots at [v2_metadata_processing.rs:1353](/Volumes/seed2/Projects/playscale/tests/v2_metadata_processing.rs:1353), [1434](/Volumes/seed2/Projects/playscale/tests/v2_metadata_processing.rs:1434), and [catalog_workflows.rs:1856](/Volumes/seed2/Projects/playscale/tests/catalog_workflows.rs:1856). Include ancillary error metadata in no-op properties; a reachable stale-worker error race was not established.

### N04 Chapter and subtitle evidence retention

**Priority 2 · Policy.** [scan.rs:928](/Volumes/seed2/Projects/playscale/src/scan.rs:928) retains chapters only for unchanged content when no new observation exists. [components.rs:21](/Volumes/seed2/Projects/playscale/src/components.rs:21) replaces sidecar sets only for complete directory coverage.

Extract revision-aware chapter update and coverage-aware sidecar reconciliation policies. Current scan models have neither evidence dimension. Existing [v2_playback.rs:7470](/Volumes/seed2/Projects/playscale/tests/v2_playback.rs:7470) and [catalog_workflows.rs:1273](/Volumes/seed2/Projects/playscale/tests/catalog_workflows.rs:1273) exercise adapter outcomes. Keep traversal/probing outside.

### N05 Incremental probe reuse

**Priority 2 · Policy.** [scan.rs:637](/Volumes/seed2/Projects/playscale/src/scan.rs:637), [663](/Volumes/seed2/Projects/playscale/src/scan.rs:663) and [db.rs:111](/Volumes/seed2/Projects/playscale/src/db.rs:111) decide full inspection versus reuse/chapter-only probing from verification mode, fingerprint and probe-schema freshness.

Return Reuse, ProbeChapters or InspectFully from explicit evidence facts. OS fingerprint construction and probing remain adapters. Scan models begin with already supplied revisions; add policy tests for old HDR/attachment probe versions and incomplete evidence.

### N06 Scan request initialization

**Priority 2 · Policy.** [scans.rs:195](/Volumes/seed2/Projects/playscale/src/scans.rs:195) selects source membership and [223](/Volumes/seed2/Projects/playscale/src/scans.rs:223) creates failed demands for disabled sources before core demand admission. [v2/scans.rs:181](/Volumes/seed2/Projects/playscale/src/v2/scans.rs:181) separately checks distinct selections and a 100-source limit.

A request plan should return per-source fail/admit decisions from memberships and enabled state. Decide whether the size cap is a transport limit or a domain guarantee. Core already handles joining/upgrading/follow-up/cancellation after this stage. Existing [scans tests:205](/Volumes/seed2/Projects/playscale/tests/scans.rs:205) covers disabled sources.

### N07 Metadata validation duplication

**Priority 3 · Policy.** [metadata.rs:175](/Volumes/seed2/Projects/playscale/src/metadata.rs:175) duplicates [core normalization](/Volumes/seed2/Projects/playscale/crates/core/src/metadata.rs:192); only the adapter restricts excluded tags to the local source and reserves source names.

Use one source-aware core policy for API updates and imports. Keep wire decoding and HTTP errors outside. Scanner default audio/video classification at [scan.rs:902](/Volumes/seed2/Projects/playscale/src/scan.rs:902) is another small pure policy candidate.

## Processing and previews

### P01 Processing retry admission

**Priority 1 · State/Conformance · static counterexample.** [v2/processing.rs:656](/Volumes/seed2/Projects/playscale/src/v2/processing.rs:656) applies the core active-job limit for new requests; [v2/jobs.rs:174](/Volumes/seed2/Projects/playscale/src/v2/jobs.rs:174) adds retry admission only for scans/previews. [processing.rs:365](/Volumes/seed2/Projects/playscale/src/processing.rs:365) has the same omission in the legacy path.

Use a processing-specific retry decision with phase/attempt/revision and active count, within the mutation boundary. Generic jobs may remain capacity-agnostic. Add 99/100-active retry tests and a model asserting active count never exceeds the intended bound. No focused retry-at-capacity test was found.

### P02 Preview publication and reclamation

**Priority 1 · State/Conformance.** [v2/previews.rs:823](/Volumes/seed2/Projects/playscale/src/v2/previews.rs:823) decides equivalent publication, replacement and upsert; [986](/Volumes/seed2/Projects/playscale/src/v2/previews.rs:986) withdraws stale/missing/invalid artifacts and selects orphan/staging cleanup.

Model attempt, staged/published artifact identity, layout identity, prior publication, withdrawal and durable removal acknowledgement. Keep rename/fsync/SQL/deletion outside. previews_model explores admission booleans and capacity, not this lifecycle. Worker tests at [v2_playback.rs:8419](/Volumes/seed2/Projects/playscale/tests/v2_playback.rs:8419) cover important corruption/reconstruction cases; add rename/commit crash and deletion-failure interleavings.

### P03 Preparation binding validity

**Priority 1 · Policy.** [v2/processing.rs:244](/Volumes/seed2/Projects/playscale/src/v2/processing.rs:244) determines exact original whole-file binding in SQL, then passes a boolean to core publication/admission.

Share a typed binding policy with playback/downloads. Supply timeline/version/file identity, ownership, revision, origin, part count and interval. Keep optimized SQL only with conformance tests. Current core tests vary the boolean; integration tests cover wrong timeline and changed interval.

### P04 Preparation selection and recipe eligibility

**Priority 2 · Policy.** [v2/processing.rs:399](/Volumes/seed2/Projects/playscale/src/v2/processing.rs:399) decides component/ordinal agreement and subtitle eligibility. [encoding.rs:49](/Volumes/seed2/Projects/playscale/src/encoding.rs:49) defines profile bounds and minimum-duration applicability. Legacy and v2 entry points have separate recipe/backend/source checks.

Return exact selected streams and typed refusal from core. Retain FFmpeg argument rendering outside. Explicitly resolve whether the differing legacy/v2 SDR/dimension checks are intended. Existing profile/HDR integration and encoding unit tests do not explore all these facts in processing_model.

### P05 Recipe and preview identity

**Priority 2 · Policy.** [v2/processing.rs:24](/Volumes/seed2/Projects/playscale/src/v2/processing.rs:24) defines which inputs change recipe identity. [v2/previews.rs:106](/Volumes/seed2/Projects/playscale/src/v2/previews.rs:106) and [552](/Volumes/seed2/Projects/playscale/src/v2/previews.rs:552) duplicate layout identity construction.

Put canonical identity/equivalence rules in core and keep encoder-version descriptors tied to the adapter contract. processing_model calls finish, not finish_preparation or recipe derivation. Add model inputs for recipe/backend/profile/subtitle/layout changes rather than only booleans.

### P06 Additional processing observations

**Priority 2/3 · Policy/Conformance.** [processing.rs:422](/Volumes/seed2/Projects/playscale/src/processing.rs:422) decides restored generated-output invalidation in SQL. [v2/previews.rs:530](/Volumes/seed2/Projects/playscale/src/v2/previews.rs:530) treats a failed running-state query as continue-running; publication still rechecks, so unsafe publication is not established. [v2/jobs.rs:115](/Volumes/seed2/Projects/playscale/src/v2/jobs.rs:115) maps job kinds to control permissions outside core.

Make restore/cache identity and unknown-observation behavior explicit; share the permission mapping if it is reused. Worker hardware reservations at [processing.rs:1167](/Volumes/seed2/Projects/playscale/src/processing.rs:1167) are separate from recovered execution witnesses at [413](/Volumes/seed2/Projects/playscale/src/processing.rs:413). Joint recovery accounting needs a contract before CPU/hardware budgets diverge; current oversubscription was not demonstrated.

## Sign in and authority

### A01 Sign-in issuance and recipient eligibility

**Priority 1 · State/Policy.** [signin/flows.rs:55](/Volumes/seed2/Projects/playscale/src/v2/signin/flows.rs:55), [230](/Volumes/seed2/Projects/playscale/src/v2/signin/flows.rs:230) and [735](/Volumes/seed2/Projects/playscale/src/v2/signin/flows.rs:735) decide purpose-specific authority, fresh evidence, owned-user requirements, recipient eligibility and silent placeholders.

Core should produce IssueChallenge, SilentPlaceholder or refusal with exact bound identity/revision/deadline. Current signin_model starts with a constructed transaction. Integration tests cover non-enumeration and initiator/evidence changes; add issuance sequences with disabled/foreign identities.

### A02 Completion effects and consumed replay

**Priority 1 · State/Conformance.** [flows.rs:369](/Volumes/seed2/Projects/playscale/src/v2/signin/flows.rs:369), [416](/Volumes/seed2/Projects/playscale/src/v2/signin/flows.rs:416), [894](/Volumes/seed2/Projects/playscale/src/v2/signin/flows.rs:894) and [927](/Volumes/seed2/Projects/playscale/src/v2/signin/flows.rs:927) choose test/link/session/pending-request/refusal effects and registration replay. Core complete primarily validates and marks Consumed.

Model exact effect identity/count, explicit protocol failure and durable receipt replay. Registration model's own Tx::Done handling is not the production replay decision. Existing [signin_api.rs:1377](/Volumes/seed2/Projects/playscale/tests/signin_api.rs:1377), [1497](/Volumes/seed2/Projects/playscale/tests/signin_api.rs:1497), [1575](/Volumes/seed2/Projects/playscale/tests/signin_api.rs:1575) cover callback reservation and single issuance; retain those adapter tests.

### A03 Provider and email configuration evidence

**Priority 1 · State.** [administration.rs:237](/Volumes/seed2/Projects/playscale/src/v2/signin/administration.rs:237), [387](/Volumes/seed2/Projects/playscale/src/v2/signin/administration.rs:387), [458](/Volumes/seed2/Projects/playscale/src/v2/signin/administration.rs:458) and [854](/Volumes/seed2/Projects/playscale/src/v2/signin/administration.rs:854) govern issuer immutability/uniqueness, tested revision/epoch, enablement, explicit untested-change override and dependent invalidation.

Represent configuration revision, enabled state, test identity/result, actor authority and override. Emit exact invalidation effects. Current models change supplied revision/enablement facts but do not execute these administrative transitions. [signin_api.rs:1939](/Volumes/seed2/Projects/playscale/tests/signin_api.rs:1939) covers direct-email test/enablement examples.

### A04 Independent recovery method classification

**Priority 1 · Policy.** [administration.rs:324](/Volumes/seed2/Projects/playscale/src/v2/signin/administration.rs:324) determines usable independent methods in SQL. Core preserve_method only receives a count and local-recovery flag.

Supply candidate credential/family/identity/provider facts and classify usable independent methods in core. Test generation, expiry, revocation, owner, disabled provider and browser-family parent distinctions. Existing model checks zero/nonzero counts, not their derivation.

### A05 Pending request approval and expiry

**Priority 1 · State.** [administration.rs:1291](/Volumes/seed2/Projects/playscale/src/v2/signin/administration.rs:1291) owns pending/revision/expiry/method/issuer approval and denial. [registration.rs:327](/Volumes/seed2/Projects/playscale/src/v2/signin/registration.rs:327) expires requests in SQL.

Add a request transition with target user/revision, method identity/revision, explicit recovery confirmation and exact link/approval/denial effects. Core registration stops at PendingApproval. Existing API tests cover creation/approval; add competing decisions, stale provider and expiry interleavings.

### A06 Action-proof lifecycle

**Priority 1 · State.** [administration.rs:1479](/Volumes/seed2/Projects/playscale/src/v2/signin/administration.rs:1479), [1556](/Volumes/seed2/Projects/playscale/src/v2/signin/administration.rs:1556), [1634](/Volumes/seed2/Projects/playscale/src/v2/signin/administration.rs:1634) and [store.rs:180](/Volumes/seed2/Projects/playscale/src/v2/signin/store.rs:180) compose creation, approval, current credential binding, approver authority and consumption.

Extend existing proof predicates with credential identities, approval/consumption state, action/target, epoch/expiry and observed authority/ownership. Consume proof and protected mutation atomically. Current model tests isolated predicates; add rotation/revocation/ownership changes between approval and use.

### A07 Identity relinking and logout effects

**Priority 1 · State/Conformance.** [flows.rs:558](/Volumes/seed2/Projects/playscale/src/v2/signin/flows.rs:558), [store.rs:410](/Volumes/seed2/Projects/playscale/src/v2/signin/store.rs:410) and [administration.rs:1844](/Volumes/seed2/Projects/playscale/src/v2/signin/administration.rs:1844) choose dependent family/device revocation and revision effects. [v2/identity.rs:728](/Volumes/seed2/Projects/playscale/src/v2/identity.rs:728) chooses family-wide logout versus single-cookie deletion and its last-administrator exception.

Return exact invalidation effects from focused core decisions, reusing existing revoke/ownership/grant policies. Family validity tests do not prove every required dependent was invalidated. Keep lookup and transaction execution outside.

### A08 Configuration and restore invalidation of transactions

**Priority 1 · State.** [administration.rs:352](/Volumes/seed2/Projects/playscale/src/v2/signin/administration.rs:352) rewrites transaction phases/results and erases secrets without a phase restriction. Registration configuration invalidation at [registration.rs:596](/Volumes/seed2/Projects/playscale/src/v2/signin/registration.rs:596) restricts changes to Pending.

Specify terminal receipt behavior explicitly: current method invalidation also changes already Consumed receipts to Failed/method_changed. Model typed invalidation reasons, immutable terminal outcomes where intended, secret erasure and exact queued-mail cancellation. This source-visible distinction is not automatically a security defect, but it needs a deliberate rule.

### A09 Rate and capacity admission

**Priority 2 · State/Policy.** [store.rs:108](/Volumes/seed2/Projects/playscale/src/v2/signin/store.rs:108) defines window reset/budget/retry-after; [registration.rs:347](/Volumes/seed2/Projects/playscale/src/v2/signin/registration.rs:347) collapses daily/pending capacity to a boolean; provider/pending-request limits are separate adapter literals.

Use one observed time and explicit counters/window state in core policies, with separate durable reservation of rejected-attempt budget where required. The registration model's one-address state cannot exhaust its users_created < 2 bound; it does not cover production daily/pending thresholds.

### A10 Invitation identity and replay

**Priority 1 · Policy.** [access_management.rs:727](/Volumes/seed2/Projects/playscale/src/v2/access_management.rs:727) derives issuer currency from administrator/operator identity, revision and credential. [832](/Volumes/seed2/Projects/playscale/src/v2/access_management.rs:832) decides creation-receipt replay. [signin/registration.rs:320](/Volumes/seed2/Projects/playscale/src/v2/signin/registration.rs:320) adds exact email binding outside core and selects from only the latest four invitation candidates.

Expose typed issuer/address/current-phase observations and a replay policy. Test registered-email versus device-claimed phases, rotated operator keys, changed grants, and older eligible invitations behind newer ineligible candidates. Current models use issuer booleans and one invitation. Existing API tests cover selected redemption/replay cases. The creation-replay branch's device_id check without identity_id is a test target, not a demonstrated exploit.

### A11 Mail test confirmation

**Priority 2 · State.** [administration.rs:1019](/Volumes/seed2/Projects/playscale/src/v2/signin/administration.rs:1019) owns expiry, attempt budget, duplicate confirmation, code destruction and configuration-bound receipt.

Model that challenge separately from authentication issuance. Expiry/budget checks currently precede the already-confirmed branch, so success on the fifth attempt is not replayed as confirmed on the next call. Define intended retry semantics and test boundary/configuration-change cases.

### A12 Bootstrap proof and desktop issuance

**Priority 1 · State/Conformance.** [v2/mod.rs:152](/Volumes/seed2/Projects/playscale/src/v2/mod.rs:152) owns one-use proof/deadline state; [v2/identity.rs:604](/Volumes/seed2/Projects/playscale/src/v2/identity.rs:604) consumes it before issuance commits, and [654](/Volumes/seed2/Projects/playscale/src/v2/identity.rs:654) chooses initial desktop administrator creation and credential generation.

Model proof match/time, current desktop identity/revocation/generation, reservation and issuance outcome. Decide explicitly whether failed persistence burns the proof. Existing one-use API tests and core generation checks do not cover the combined rollback lifecycle.

### A13 Canonical access validation and status

**Priority 3 · Policy.** [access_management.rs:115](/Volumes/seed2/Projects/playscale/src/v2/access_management.rs:115) and [v2/identity.rs:983](/Volumes/seed2/Projects/playscale/src/v2/identity.rs:983) duplicate rating/label/grant validation, with differing whitespace rules. Pairing/invitation status precedence is also adapter-derived.

Use shared core validation with observed known IDs; retain body/header parsing outside. This does not require a new global state machine.

## Mail delivery

### M01 Purpose-specific owner eligibility

**Priority 1 · Policy.** [outbox.rs:144](/Volumes/seed2/Projects/playscale/src/v2/signin/outbox.rs:144) derives transaction/invitation/test/account-notice liveness; [222](/Volumes/seed2/Projects/playscale/src/v2/signin/outbox.rs:222) applies the Test exception to disabled email.

Core should consume typed owner facts and derive eligibility. Current mail model supplies an owner boolean, fixed RegistrationCode/Direct mode/key, and restore_hold false. Extend owner kinds, redemption/revocation, epochs and deadlines.

### M02 Payload failure during recovery

**Priority 1 · State · static discrepancy.** [outbox.rs:301](/Volumes/seed2/Projects/playscale/src/v2/signin/outbox.rs:301) obtains core claim's recovered attempt, but [320](/Volumes/seed2/Projects/playscale/src/v2/signin/outbox.rs:320) constructs Failed from the old message when payload is unavailable. [core mail.rs:273](/Volumes/seed2/Projects/playscale/crates/core/src/mail.rs:273) may already have inferred ambiguity.

Add explicit payload availability/failure input preserving recovered uncertainty and defining phase/attempt accounting. Current model cannot lose/corrupt payload before terminal state. Add expired Sending lease plus missing/decryption-failed payload and assert phase, ambiguity, lease and payload erasure.

### M03 Bulk cancellation conformance

**Priority 2 · Conformance.** [outbox.rs:430](/Volumes/seed2/Projects/playscale/src/v2/signin/outbox.rs:430) and [443](/Volumes/seed2/Projects/playscale/src/v2/signin/outbox.rs:443) directly implement cancellation SQL although core mail::cancel exists.

Bulk SQL may remain, but prove it matches the core phase matrix, including Sending lease retention for late acceptance and terminal no-ops. Existing configuration-change integration covers queued cancellation, not the complete matrix.

### M04 Transmission ambiguity observation

**Priority 1 · Conformance/Policy · unexecuted failure hypothesis.** [smtp.rs:284](/Volumes/seed2/Projects/playscale/src/mail/smtp.rs:284) groups DATA payload write and flush, classifying any failure as Temporary. A flush failure after the terminator may leave acceptance uncertain.

Keep socket I/O in the adapter. Supply write-progress/final-response facts to a phase-aware outcome classifier and preserve Ambiguous when the terminator may have reached the peer. Existing tests cover lost final reply after successful writes; add fault injection around terminator/flush.

### M05 Whole-attempt deadline

**Priority 1 · Conformance.** [core mail.rs:13](/Volumes/seed2/Projects/playscale/crates/core/src/mail.rs:13) requires the complete attempt to fit inside its lease. [mail/mod.rs:213](/Volumes/seed2/Projects/playscale/src/mail/mod.rs:213) starts a route cutoff after DNS and only prevents starting another route; [outbox.rs:282](/Volumes/seed2/Projects/playscale/src/v2/signin/outbox.rs:282) rechecks the database without a whole-attempt deadline.

Make the effect's deadline and expiry observation explicit and enforce the whole attempt in the adapter. Model lease expiry after authorization and during submission; test slow DNS/database/route phases and ambiguity on interruption. Normal delivery exceeding the lease was not demonstrated.

## Viewing and organization

### V01 Viewing delivery admission and rebinding

**Priority 1 · Policy.** [v2/viewing.rs:557](/Volumes/seed2/Projects/playscale/src/v2/viewing.rs:557) derives open status from strings, and [849](/Volumes/seed2/Projects/playscale/src/v2/viewing.rs:849) checks same profile/timeline before core rebind.

Pass typed ownership and phase facts into core. Current timeline-viewing models cover authority/revision/closure/sequence but not foreign delivery ownership. Preserve live lookup and transaction execution outside.

### V02 Complete next-timeline selection

**Priority 1 · Policy/search state · static counterexample.** [v2/viewing.rs:535](/Volumes/seed2/Projects/playscale/src/v2/viewing.rs:535) limits candidates before checking readability. A readable 51st candidate after 50 hidden ones is omitted.

Define Found, NeedMore and Exhausted over ordered candidates. Keep database pagination outside and distinguish read failure from unreadability. Add 49/50/51 boundary tests, hidden runs and group boundaries. Existing release-order tests do not establish this completeness boundary.

### V03 Organization disclosure and references

**Priority 2 · Policy.** [v2/organization.rs:393](/Volumes/seed2/Projects/playscale/src/v2/organization.rs:393) filters historical receipt entries against current visibility; [1083](/Volumes/seed2/Projects/playscale/src/v2/organization.rs:1083) separately derives queue disclosure. [organization.rs:300](/Volumes/seed2/Projects/playscale/src/organization.rs:300) enforces same-profile saved filters and live, nonretired references.

Use typed disclosure/reference-admission policies. organization_model explores queue navigation/editing over four entries, not changing authorization or receipt history. Existing integration tests at [582](/Volumes/seed2/Projects/playscale/tests/v2_organization_viewing.rs:582) and [1241](/Volumes/seed2/Projects/playscale/tests/v2_organization_viewing.rs:1241) cover examples.

## Downloads and offline storage

### D01 Download binding and ownership admission

**Priority 1 · Policy.** [v2/downloads.rs:94](/Volumes/seed2/Projects/playscale/src/v2/downloads.rs:94), [151](/Volumes/seed2/Projects/playscale/src/v2/downloads.rs:151) and [183](/Volumes/seed2/Projects/playscale/src/v2/downloads.rs:183) derive device/owner/profile eligibility and exact whole-file/rendition binding before passing booleans to core.

Share typed binding policies with playback/preparation and explicit ownership admission. downloads_model focuses on reconciliation; it does not execute these SQL binding decisions. Existing [v2_downloads.rs:304](/Volumes/seed2/Projects/playscale/tests/v2_downloads.rs:304) covers exact-source and grant behavior.

### D02 Subtitle artifact lifecycle and transfer completion

**Priority 1 · State/Conformance.** [v2/downloads.rs:1119](/Volumes/seed2/Projects/playscale/src/v2/downloads.rs:1119) defines first-artifact-wins and immutable digest/size; [1175](/Volumes/seed2/Projects/playscale/src/v2/downloads.rs:1175) owns deduplication, permanent/transient failure, waiters and cancellation; [1462](/Volumes/seed2/Projects/playscale/src/v2/downloads.rs:1462) separately gates VerifiedLocal on required subtitles.

Model demand/attempt/source-track/artifact identity, ready/failure state, publication and cancellation. Core transfer admission should see artifact readiness directly. Existing [v2_downloads.rs:1676](/Volumes/seed2/Projects/playscale/tests/v2_downloads.rs:1676) covers many adapter cases; transfer_reports_model uses a constant size and lacks artifact interleavings.

### D03 Restored-pin validation

**Priority 1 · Policy.** [host/store.rs:661](/Volumes/seed2/Projects/playscale/crates/motion-ui-host/src/store.rs:661) validates held downloads, current history, source/timeline revisions and causal bounds before core reapply.

Move validation into a core policy with held identities. Correction to the initial quick audit: downloads_model DOES explore restore/discard/reapply, but supplies a known-valid pin. Add invalid/foreign/absent-cache/changed-source pins. Existing store tests cover unknown downloads and foreign history.

### D04 Cache publication and shared ownership

**Priority 1 · State/Conformance.** [host/store.rs:463](/Volumes/seed2/Projects/playscale/crates/motion-ui-host/src/store.rs:463), [553](/Volumes/seed2/Projects/playscale/crates/motion-ui-host/src/store.rs:553) and [799](/Volumes/seed2/Projects/playscale/crates/motion-ui-host/src/store.rs:799) decide idempotent publication, replacement, shared media/side-file release and recovery from staged/blob state.

Core should decide AlreadyPublished, Publish, Recover, RetryTransfer and exact release effects from identity/reference/verification/removal observations. Do not duplicate bytes or filesystem state in core. Existing filesystem tests are substantial; no corresponding cache publication/ownership Stateless model exists. Add interrupted manifest commit/removal, corrupt leftovers and recovery-before-collection.

### D05 Resume checkpoint equivalence

**Priority 1 · Policy/Conformance · static discrepancy.** [host/lib.rs:679](/Volumes/seed2/Projects/playscale/crates/motion-ui-host/src/lib.rs:679) uses the latest event position unless ended. [host/store.rs:775](/Volumes/seed2/Projects/playscale/crates/motion-ui-host/src/store.rs:775) drops checkpoints at duration; [911](/Volumes/seed2/Projects/playscale/crates/motion-ui-host/src/store.rs:911) filters rebase positions separately.

Use one resume policy for opening, acknowledgement and rebase, with checkpoint persistence before compaction. Core permits paused-at-duration observations, exposing the before/after discrepancy. Model checkpoint state and compaction failure, rewind and completion boundaries. Existing checkpoint tests do not model this equivalence.

### D06 Local playback tickets

**Priority 2 · Client state.** [host/lib.rs:654](/Volumes/seed2/Projects/playscale/crates/motion-ui-host/src/lib.rs:654), [779](/Volumes/seed2/Projects/playscale/crates/motion-ui-host/src/lib.rs:779), [808](/Volumes/seed2/Projects/playscale/crates/motion-ui-host/src/lib.rs:808) and [829](/Volumes/seed2/Projects/playscale/crates/motion-ui-host/src/lib.rs:829) govern replacement, exact ticket retirement, removal and reader admission.

Use a small local playback transition over opening identity, admitted ticket/download and snapshot outcomes. Files, Arc lifetime and locks stay adapters. Existing cache integration tests verify old-reader survival and exact retirement; add deterministic failure/cancellation interleavings.

### D07 Offline target and cache-entry policy

**Priority 2/3 · Policy.** [v2/downloads.rs:1914](/Volumes/seed2/Projects/playscale/src/v2/downloads.rs:1914) selects effective duration from curated timeline or matching pinned download. [host/lib.rs:149](/Volumes/seed2/Projects/playscale/crates/motion-ui-host/src/lib.rs:149) mixes domain identity/revision/media support with storage-format validation.

Extract the domain subset and share duration policy; retain schema/version/size/path validation in the host. The reconciliation model's fixed duration omits these choices.

## Maintenance and durable contracts

### S01 Schedule dispatch lifecycle

**Priority 1 · State.** [v2/schedules.rs:282](/Volumes/seed2/Projects/playscale/src/v2/schedules.rs:282) disables schedules on lost authority/source failure; [415](/Volumes/seed2/Projects/playscale/src/v2/schedules.rs:415) advances or preserves progress under queue pressure. [migration 0041:7](/Volumes/seed2/Projects/playscale/migrations/0041_schedule_dispatch_progress.sql:7) resets cursors on revision change.

Core already owns timing and backoff, but not revision-bound cursor/dispatch decisions. Model schedule revision, task/owner, cursor, eligible observations, queue results, disabled state and committed batches. Return exact jobs and cursor/next-run changes atomically. Keep the reset trigger as enforcement. Existing resumable/fairness tests at [v2_metadata_processing.rs:1839](/Volumes/seed2/Projects/playscale/tests/v2_metadata_processing.rs:1839), [2187](/Volumes/seed2/Projects/playscale/tests/v2_metadata_processing.rs:2187) should be extended with crash/revision/retry interleavings.

### S02 Backup retention lifecycle

**Priority 1 · State/Conformance.** [v2/admin.rs:285](/Volumes/seed2/Projects/playscale/src/v2/admin.rs:285) represents withdrawal/reclamation using backup_expired/backup_reclaimed error strings. [storage.rs:350](/Volumes/seed2/Projects/playscale/src/storage.rs:350) separately selects owned automatic snapshots/old staging directories for retention.

Represent ready, withdrawn and reclaimed availability with retention selection and removal acknowledgement/failure. Require durable withdrawal before deletion and never advertise reclaimed bytes. Core currently covers publication facts, not this lifecycle. Existing [v2_metadata_processing.rs:1901](/Volumes/seed2/Projects/playscale/tests/v2_metadata_processing.rs:1901), [1970](/Volumes/seed2/Projects/playscale/tests/v2_metadata_processing.rs:1970) cover retention and interrupted retries. Marker/stat/read/delete mechanics remain adapters.

### S03 History pruning and generated-cache cleanup contract

**Priority 2 · Policy/Conformance.** [storage.rs:393](/Volumes/seed2/Projects/playscale/src/storage.rs:393) decides which terminal jobs/sessions may disappear while preserving authoritative viewing barriers, preparation references and cleanup provenance. Put those reference/phase eligibility rules in a pure retention policy.

Generated-cache eligibility and attempt-fenced acknowledgement already live in core. [maintenance.rs:234](/Volumes/seed2/Projects/playscale/src/maintenance.rs:234) withdraws availability before deletion; [290](/Volumes/seed2/Projects/playscale/src/maintenance.rs:290) retains held/unreadable execution witnesses; [314](/Volumes/seed2/Projects/playscale/src/maintenance.rs:314) acknowledges cleanup. Specify and test this multi-effect ordering rather than moving filesystem calls into core. Existing cache tests cover pins, failed deletion and responsiveness; core state_policies covers individual eligibility predicates.

### S04 Representation invalidation dependencies

**Priority 2 · Conformance.** [migration 0054:10](/Volumes/seed2/Projects/playscale/migrations/0054_preparation_dependencies.sql:10), [0037:4](/Volumes/seed2/Projects/playscale/migrations/0037_smart_collection_revisions.sql:4) and catalog revision triggers encode which changes invalidate prepared representations and reviewed plans.

Define the dependency-change-to-revision contract or a small reference policy and test trigger conformance. Keep triggers, foreign keys and uniqueness in storage. Existing [v2_metadata_processing.rs:2399](/Volumes/seed2/Projects/playscale/tests/v2_metadata_processing.rs:2399) and [catalog_persistence.rs:409](/Volumes/seed2/Projects/playscale/tests/catalog_persistence.rs:409) cover meaningful dependency changes; do not characterize this as wholly untested.

### S05 Historical ownership migration

**Priority 3 · Policy/Conformance.** [migration 0058:5](/Volumes/seed2/Projects/playscale/migrations/0058_legacy_access_ownership.sql:5) decides which untouched synthetic users are detached/deleted versus retained using edit evidence, device count and invitation references.

Extract a pure disposition policy for future forward repairs and add migration-conformance fixtures. Do not rewrite the shipped migration. Current ownership tests cover present-day unassigned behavior, not every historical retain/detach combination.

## Playback and delivery

### R01 Live output qualification and adaptive publication

**Priority 1 · State.** [delivery.rs:1612](/Volumes/seed2/Projects/playscale/src/delivery.rs:1612) maintains reported segments, rolling bitrate and video inspection state; [1777](/Volumes/seed2/Projects/playscale/src/delivery.rs:1777) maintains separate low-variant verification/end state. The adapter decides index continuity, size/bitrate bounds, independent frames, copied-stream keyframe start alignment, paired variant publication and completion.

Pass variant/index/size/duration/independence/start/end observations to a focused production publication state machine. Keep MP4 parsing, file reads and FFmpeg outside. The delivery model uses adaptive false and no bitrate cap, so generic Ready/Segment/Completed exploration cannot verify these qualification decisions. Existing [v2_playback.rs:5652](/Volumes/seed2/Projects/playscale/tests/v2_playback.rs:5652), [6259](/Volumes/seed2/Projects/playscale/tests/v2_playback.rs:6259), [6534](/Volumes/seed2/Projects/playscale/tests/v2_playback.rs:6534) cover capped/adaptive transport behavior.

### R02 Exact playback binding

**Priority 1 · Policy.** [v2/playback.rs:739](/Volumes/seed2/Projects/playscale/src/v2/playback.rs:739) validates signed claims against ordered parts, file/binding revisions, library, duration and clip. Core receives Observed.bound; [playback_session_model.rs:175](/Volumes/seed2/Projects/playscale/crates/core/tests/playback_session_model.rs:175) uses no clip/parts and bound true.

Move the comparison into core using explicit binding observations; share appropriate primitives with P03/D01 without erasing their distinct eligibility rules. Existing interval/additional-part tests at [v2_playback.rs:4973](/Volumes/seed2/Projects/playscale/tests/v2_playback.rs:4973), [5270](/Volumes/seed2/Projects/playscale/tests/v2_playback.rs:5270) remain necessary adapter tests.

### R03 Partial-replan selection identity

**Priority 1 · State.** [delivery.rs:3930](/Volumes/seed2/Projects/playscale/src/delivery.rs:3930) observes a pending-or-active selection before asynchronous work, then [4153](/Volumes/seed2/Projects/playscale/src/delivery.rs:4153) checks it has not changed. The legacy path does the same at [4987](/Volumes/seed2/Projects/playscale/src/delivery.rs:4987). Core Change carries expected active generation, not the pending selection basis.

Include expected selection identity/revision in the core command, or have core apply the partial fields to current state. Model concurrent partial changes with the same active generation but different pending selections and assert no stale overwrite. Keep the mutex in the adapter.

### R04 Valid selection as an admission prerequisite

**Priority 1 · Policy.** [delivery.rs:4053](/Volumes/seed2/Projects/playscale/src/delivery.rs:4053) and [4512](/Volumes/seed2/Projects/playscale/src/delivery.rs:4512) compose clip/multipart/bitrate/adaptive constraints; [3217](/Volumes/seed2/Projects/playscale/src/delivery.rs:3217), [3303](/Volumes/seed2/Projects/playscale/src/delivery.rs:3303), [3561](/Volumes/seed2/Projects/playscale/src/delivery.rs:3561) validate burn/sidecar/backend choices.

Core admission assumes a valid pin. Use a shared validate-selection policy or validated selection type with observed source/platform/filter facts. Planning, first admission and replanning should call it. Existing core helpers cover parts of this policy; do not replace them with a second implementation. Device probing and command rendering stay adapters.

### R05 Compatibility and track/subtitle resolution

**Priority 2 · Policy.** [v2/playback.rs:896](/Volumes/seed2/Projects/playscale/src/v2/playback.rs:896) classifies browser codec/container/HDR support before core sees Support. [1231](/Volumes/seed2/Projects/playscale/src/v2/playback.rs:1231) resolves component/track disagreements; [1673](/Volumes/seed2/Projects/playscale/src/v2/playback.rs:1673) retries planning with burn-in; [1754](/Volumes/seed2/Projects/playscale/src/v2/playback.rs:1754) decides strict refusal versus automatic omission. Legacy [playback.rs:182](/Volumes/seed2/Projects/playscale/src/playback.rs:182) supplies its own processing/provenance eligibility.

Centralize compatibility and selection composition around existing core plan/automatic_subtitle/presentation helpers. Model actual support/selection facts rather than fabricated supported booleans. Existing text, sidecar, bitmap, preference, prepared-audio and multipart tests are substantial and should be retained.

### R06 Generated-file authorization lineage

**Priority 1 · Policy/Conformance.** [v2/content.rs:72](/Volumes/seed2/Projects/playscale/src/v2/content.rs:72) derives library scope from registered/current output revisions and registered/current source revisions. Core sees only the final library list.

Define a pure lineage/scope policy over original/generated status, rendition registration, revisions and source memberships. Keep efficient joins but test equivalence, including stale/orphaned output. This is an authorization fact derivation gap, not a demonstrated disclosure.

### R07 Preferences and monitoring authority selection

**Priority 2/3 · Policy/Conformance.** [v2/playback.rs:302](/Volumes/seed2/Projects/playscale/src/v2/playback.rs:302) and [viewing.rs:486](/Volumes/seed2/Projects/playscale/src/viewing.rs:486) duplicate preference normalization/validation; the v2 path also decides revision changes. [playback_monitor.rs:454](/Volumes/seed2/Projects/playscale/src/playback_monitor.rs:454) selects authoritative viewing before time/revision/ID tie breaks; core comparison assumes that authority.

Share a preference-update policy and specify the monitor authority selection contract, especially equal timestamps. Monitoring persistence acknowledgement already uses core correctly. Keep read projection and sampling outside.

## Infrastructure and recovery

### I01 Scan recovery composition must be production core

**Priority 1 · State · production/model divergence.** [db.rs:583](/Volumes/seed2/Projects/playscale/src/db.rs:583) preserves an existing queued follow-up, merges full_scan/direct_request and cancels the interrupted attempt. [demands_model.rs:221](/Volumes/seed2/Projects/playscale/crates/core/tests/demands_model.rs:221) instead preserves the recovered attempt, removes the follow-up and merges verify.

Extract a recovery plan over attempt IDs/phases/modes, direct owners and demand/barrier facts. Both production and the model must call it. Assert exact retained/cancelled IDs and preservation of pre-restart ownership. Existing [scans.rs:243](/Volumes/seed2/Projects/playscale/tests/scans.rs:243) and [330](/Volumes/seed2/Projects/playscale/tests/scans.rs:330) test production behavior but cannot make the divergent harness representative.

### I02 Restore authority withdrawal and baseline capture

**Priority 1 · State/Conformance.** [db.rs:740](/Volumes/seed2/Projects/playscale/src/db.rs:740) decides history preservation, new restore epoch, pending baseline, authority deletion/revocation and baseline capture; [253](/Volumes/seed2/Projects/playscale/src/db.rs:253) admits one-time current-epoch capture. [backup.py:91](/Volumes/seed2/Projects/playscale/scripts/backup.py:91) implements a parallel restore path.

Define one restore contract over verified snapshot, old/new history, baseline state and authority-withdrawal acknowledgements, with publication eligibility only after required effects. Keep copy/integrity/checkpoint/fsync/final-path publication in adapters. Existing [catalog_persistence.rs:809](/Volumes/seed2/Projects/playscale/tests/catalog_persistence.rs:809), [849](/Volumes/seed2/Projects/playscale/tests/catalog_persistence.rs:849), [898](/Volumes/seed2/Projects/playscale/tests/catalog_persistence.rs:898) cover real restore/baseline effects. Downstream epoch models do not specify the full restore operation.

### I03 Registration and legacy repair policy callers

**Priority 2 · Policy.** [db.rs:426](/Volumes/seed2/Projects/playscale/src/db.rs:426) is another source-registration entry point checking root identity and managed ownership; include it in C01's shared policy. [upgrade.rs:191](/Volumes/seed2/Projects/playscale/src/upgrade.rs:191) adds update-timestamp provenance outside core retract_unattributed_legacy.

Pass that provenance to the existing core repair predicate and its existing legacy_import_model. Most legacy attribution/repair logic is already core. [catalog_persistence.rs:764](/Volumes/seed2/Projects/playscale/tests/catalog_persistence.rs:764) tests repair/receipt rollback; add timestamp-only provenance differences.

### I04 Online resume selection

**Priority 2 · Policy.** [presentation.rs:863](/Volumes/seed2/Projects/playscale/src/presentation.rs:863) restarts watched media or positions within the final second and otherwise resumes stored progress.

This is playback behavior, not formatting. Define intended online/offline/terminal resume semantics with watched status, position and optional duration. Share policy where semantics agree, preserving intentional differences rather than silently imposing one client's rule. Coordinate with D05.

## Client workflows

### L01 Pending mutation uncertainty

**Priority 1 · Client state · source-level divergence.** [bridge.js:161](/Volumes/seed2/Projects/playscale/packages/ui-bridge/bridge.js:161), [279](/Volumes/seed2/Projects/playscale/packages/ui-bridge/bridge.js:279), and [auth.js:195](/Volumes/seed2/Projects/playscale/packages/ui-bridge/auth.js:195) discard pending keys on most 4xx replies without tracking earlier uncertain attempts. [terminal errors.go:119](/Volumes/seed2/Projects/playscale/apps/terminal/internal/operations/errors.go:119) preserves uncertainty when a later authentication/visibility refusal cannot settle an earlier attempt.

Represent request/body/key/principal/server identity, prior unknown outcome and receipt evidence. Distinguish first refusal, uncertain submission, exact retry and definitive settlement. Persist unresolved intent before sending when reload recovery is promised. Use a production JS coordinator and cross-language conformance cases; do not write a test-only Rust substitute. Existing browser tests cover single failures; terminal tests at [jobs_test.go:133](/Volumes/seed2/Projects/playscale/apps/terminal/internal/operations/jobs_test.go:133) cover resubmission. Add lost-ack then 401/403/404/receipt-conflict sequences.

### L02 Desktop download reconciliation

**Priority 1 · Client state/Conformance.** [downloads.mjs:582](/Volumes/seed2/Projects/playscale/apps/desktop/src/downloads.mjs:582) decides complete-inventory cleanup, per-pass removal exclusion despite stale inventory, durable removal reporting, recovery-before-collection, failed-transfer collection deferral, and sync-before-rebase.

Model inventory completeness/identity, owned cache/staging/tombstone identities, report intents, already-removed IDs, recovery results and history acknowledgement. Emit exact DeleteDemand/RemoveLocal/Transfer/Report/Collect/Rebase effects. Core offline via the host, or a focused pure JS reducer, can own this coordination. Existing [downloads.test.mjs:454](/Volumes/seed2/Projects/playscale/apps/desktop/test/downloads.test.mjs:454), [549](/Volumes/seed2/Projects/playscale/apps/desktop/test/downloads.test.mjs:549), [652](/Volumes/seed2/Projects/playscale/apps/desktop/test/downloads.test.mjs:652), [743](/Volumes/seed2/Projects/playscale/apps/desktop/test/downloads.test.mjs:743) cover important adapter cases; model crashes and restore/authority changes between effects.

### L03 Cross-source player replacement

**Priority 1 · Client state.** [player.js:716](/Volumes/seed2/Projects/playscale/packages/ui-bridge/player.js:716) intertwines flags and effects deciding resume/retire/swap/stop from preparation, leases, page ownership, rebind acknowledgement, ambiguous response/read-back and local swap.

Extract a production client coordinator with operation identity and Prepared/LeaseFailed/RebindAcknowledged/Unknown/ReadBack/PageRetired inputs. Its policy already recognizes that observing the old binding does not prove an ambiguous PUT cannot commit later. Model that delayed commit and prohibit unsafe predecessor resumption. Keep DOM/player/HTTP/timer operations outside; the existing generation coordinator is a suitable pattern.

### L04 Viewing conflict recovery policy

**Priority 3 · Client policy.** [viewing.mjs:103](/Volumes/seed2/Projects/playscale/packages/playback/viewing.mjs:103) appropriately owns predecessor-revision adoption; [bridge.js:221](/Volumes/seed2/Projects/playscale/packages/ui-bridge/bridge.js:221) separately chooses watched-mark retry revisions.

Consolidate pure client intent/change-evidence policies while preserving distinct operations. If standardized across clients, an explicit server command carrying original intent/lineage could let core decide atomically. Existing [viewing.test.mjs:141](/Volumes/seed2/Projects/playscale/packages/playback/viewing.test.mjs:141), [bridge.test.mjs:444](/Volumes/seed2/Projects/playscale/packages/ui-bridge/bridge.test.mjs:444) cover examples. This is not a demonstrated authority bypass.

## Adapter issues to keep separate from state extraction

### B01 Preparation acquires another pool connection

**Priority 1 · Adapter correction.** [v2/processing.rs:548](/Volumes/seed2/Projects/playscale/src/v2/processing.rs:548) holds a writer while selection calls [timeline_components](/Volumes/seed2/Projects/playscale/src/v2/processing.rs:391), which starts another transaction. If all other connections are occupied, acquisition can stall until timeout. It also leaves the caller's snapshot boundary.

Use the existing [timeline_components_in](/Volumes/seed2/Projects/playscale/src/components.rs:60) helper with the current connection. Test preparation with all other pool slots held. An analogous playback regression exists at [v2_playback.rs:8687](/Volumes/seed2/Projects/playscale/tests/v2_playback.rs:8687); this preparation case was not executed.

### B02 Blocking subtitle validation lifetime

**Priority 2 · Adapter verification.** Burn subtitle validation uses timeout-wrapped plain spawn_blocking at [delivery.rs:5827](/Volumes/seed2/Projects/playscale/src/delivery.rs:5827) and [5888](/Volumes/seed2/Projects/playscale/src/delivery.rs:5888), while regular conversion retains admission through admitted_blocking at [5711](/Volumes/seed2/Projects/playscale/src/delivery.rs:5711).

Verify which permit/ownership survives timeout and cancellation before assuming equivalent lifetime guarantees. Keep filesystem/blocking work outside core and supply real completion acknowledgements. No resource leak or oversubscription was demonstrated.

## Coverage ledger

- **Catalog/source/matching:** all production functions in catalog, curation, matching, libraries and v2 catalog/libraries; related core, schema and tests.
- **Scan/metadata:** scan/scans/watch/NFO/metadata/jobs/artwork/interchange/search/markers/media/components and v2 scans/metadata/markers; relevant core/model/test sources.
- **Processing:** processing/progress/execution/encoding/v2 processing/jobs/previews/renditions/artifact leases; policy and adapter conformance checks.
- **Playback:** playback/delivery/fmp4/monitoring/v2 playback/content/monitor and corresponding core models.
- **Authority:** every sign-in module and protocol adapter; auth/access management/identity/admin plus legacy administration; core and integration sources.
- **Viewing/organization/events:** legacy and v2 service/handler modules with relevant policy and model checks.
- **Downloads/host:** all production functions in v2 downloads, host lib/main/store and motion-ui offline; download/offline/transfer models and tests.
- **Infrastructure:** main/lib/db/api/config/desktop/presentation/system/CORS/runtime/metrics/operations/upgrade; the parallel restore script was checked despite other tooling being out of scope.
- **Maintenance:** schedules, cache cleaning, storage backup/retention and durable ordering.
- **Schema:** all 64 migration files, 1,747 lines. Storage enforcement was distinguished from missing domain policy.
- **Clients:** full inventory; deep reads of playback coordinators, desktop lifecycle/download/outbox/recovery/ownership, browser mutation/auth/player/offline workflows, terminal session/account/preconditions/jobs/uncertainty/store and TUI authority fencing. Remaining Rust UI and terminal rendering/CLI/API-forwarding were structurally surveyed with targeted reads, not a line-by-line audit of every presentation function.

## Implementation order

1. Resolve the seven concrete source discrepancies at the start of this report with focused regression cases. Also reproduce B01 and the SMTP ambiguity/deadline hypotheses. Fixing these is more valuable than mechanical extraction.
2. Establish shared typed binding/lineage and authorization decisions: C04/C09, P03, R02/R04/R06, A04/A06/A10 and D01/D03. Make production call them before adding broader models.
3. Extract the missing publication/recovery workflows: N01/N02/N03, P02, R01/R03, A02/A03/A05/A08/A12, M02, D02/D04/D05, S01/S02 and I01/I02.
4. Add explicit client coordinators for L01/L02/L03, keeping JS/Go/host effect execution with the correct owner.
5. Consolidate lower-priority policies and verify SQL trigger/optimized-query conformance. Keep each change narrow enough to review with its own preserved semantics, regression cases and remaining gaps.


## Existing architecture to preserve

- Processing output validation, attempt/revision/cancellation admission and atomic publication already substantially use core. Keep SQLite publication-unit rollback tests.
- Work scheduling, tickets and execution ownership use core work transitions. OS witnesses, process groups, descriptor inheritance and reap observations remain adapters.
- Artifact pin/cleanup ownership uses core ArtifactUse; RAII and locking remain adapters.
- Merge/split structural planning, matching manual-pin protection, source overlap/exclusions/relocation checks, marker validation, artwork selection and import planning already have core decisions.
- Access ceilings, owner setup, pairing, credential derivation, generic revision/idempotency and event disclosure/reset policies already have core implementations.
- Download reconciliation, ordered transfer reports, offline event admission/compaction/rebase and restore decisions are substantial core logic. The gaps are the surrounding facts and artifact/cache workflows.
- OIDC verification, DNS, TLS, cryptography, MIME/SMTP framing, HTTP parsing, byte ranges, media probing/encoding, native watch translation, rendering, metrics, FTS and ordinary query projection should remain adapters or dedicated protocol libraries.
- Unique constraints, foreign keys, immutable receipt enforcement and atomic historical event ownership are correct storage responsibilities. Do not mechanically migrate every SQL conditional.

## Verification approach

For each extracted rule, production and Stateless must call the same decision function. Test-owned workflow composition should be replaced with production composition before increasing exploration bounds.

Use finite identity sets and explicit interleavings: at least two competing jobs/proposals/tickets where identity matters; old/current/future revisions; stale and duplicate completions; cancellation/retry/recovery; before/at/after deadlines; valid/invalid observations; commit/rollback/crash acknowledgements. Assert intended outcomes and exact effect identities/counts, not merely no panic or some effect.

Keep integration tests for transaction reservation, uniqueness, trigger conformance, filesystem durability/rename/delete order, actual process lifetime, HTTP retries and credential scope. Model checks do not prove those adapters implement the contract.

The current workspace pins statelessness 0.2.0 as the stateless development dependency. Thirty core test files implement models. README.md still mentions 0.1.1. Existing model configurations are bounded; no explored-state totals or fresh passing results are claimed here.

