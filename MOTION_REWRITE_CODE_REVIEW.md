# Motion rewrite code review

Static review of the rewrite against [the completion handoff](MOTION_SINGLE_AGENT_COMPLETION_HANDOFF.md), [the original architecture](Motion_Final_Architecture_and_Implementation_Plan.md), and the current lane ledgers. The main concern is integration correctness: the code has substantial production core policies, but some adapters, queries and client transitions do not carry those policies through the complete workflow. The existing architecture should be completed and repaired in focused changes.

Reviewed on October 12, 2026, at local `main` commit `28f4b32c0df0c31d636488fdb524c23549c89bdc`. The existing uncommitted consolidation-plan changes and untracked handoff were included as review context and left unchanged. No builds, tests, benchmarks, servers, browsers or media tools were run. No remote branch or CI refresh was performed. This document records potential issues supported by source inspection, not reproduced failures or a release qualification.

Reconciled with [the completion handoff](MOTION_SINGLE_AGENT_COMPLETION_HANDOFF.md) at the user's request. Its assigned feature backlog, documentation consolidation and general release checks have been removed from this review because an implementation owner is already handling them. Removal means assigned elsewhere, not fixed or verified. Original finding IDs are preserved.

The remaining review records **31 code findings**: two P1, 26 P2 and three P3, plus **two additional gaps and one documentation finding**. These describe concrete defects or missing contracts not specifically assigned in the handoff; touching the same subsystem does not create a separate implementation assignment. The highest-priority authorization findings are A01 (restore of tickets/grants) and C01 (whole-work matching scope).

## Reading the findings

- **P1:** prioritize before relying on the affected authorization or persistence boundary.
- **P2:** material correctness, availability or missing workflow behavior.
- **P3:** smaller usability or documentation issue.
- **Source supported:** the relevant control flow or missing wiring is visible in this checkout. Runtime manifestation has not been reproduced.
- **Static risk:** the missing safeguard is visible, but the consequence also depends on a particular environmental failure or media behavior.
- **Additional gap:** absent from the handoff's explicit assignments; it may already appear in another ledger and is not necessarily a newly discovered regression.

Source locations below refer to the original review baseline, not a fresh audit of the implementation owner's work. Suggested checks are future work only. Owner labels describe subsystems; handoff section references identify related work for coordination, not duplicate tasks. Use the handoff's existing completion ledger to track accepted follow-ups rather than creating another progress ledger.

## Structural assessment

**Preserve the production core.** Identity, access, job attempts, delivery generations, viewing authority, offline reconciliation and several resource policies already have deterministic implementations in `crates/core`. Their production-backed model tests are useful foundations. Normal queries and media bytes should remain in adapters; this review does not recommend moving every read into a global state machine or undertaking another crate-wide rewrite.

**Complete ownership across service boundaries.** Prepared-file garbage collection still observes legacy playback state; subtitle blocking work can outlive its admission permit; offline snapshot and journal limits are not fully composed with desktop lifecycle. These are missing facts and transitions at specific boundaries. A model for each isolated component cannot establish the missing cross-component invariant.

**Review reachable workflows, not only available types and endpoints.** Series/season structures, order groups, matching correction, richer search and organization have implementations that are not consistently reachable through the public API and product UI. Fixtures that insert otherwise-uncreatable SQL state can hide this gap. A successful route inventory is not evidence that an ordinary user can construct and complete the workflow.

**Make bounds part of the query and recovery contracts.** The handoff's 1,000-row and roughly 300-title limits are only part of the problem. Ordinary pages silently stop at 200 rows, smart collections fail based on total catalog size, historical downloads accumulate, and several v2 histories have no retention. Increasing constants would postpone failure without establishing bounded operation.

**Give adapters consistent connection and revision ownership.** Nested acquisition from a four-connection SQLite pool can block ordinary playback planning. Unscoped preparation-revision triggers amplify every unrelated catalog update. Both issues can be repaired without weakening SQLite writer fencing or the existing identity model.

## Authorization and operational findings

### A01 P1 Restore preserves ephemeral tickets and can revive older grants

**Source supported.** Both restore paths copy the database and rotate `restore_epoch`, but leave `content_tickets`, credentials and device grants from the snapshot intact: [scripts/backup.py](scripts/backup.py), lines 83–113; [src/db.rs](src/db.rs), lines 632–640. File-ticket admission reads the stored token hash, expiry and revocation flag without checking the restore epoch ([src/v2/content.rs](src/v2/content.rs), lines 385–433). Credential authentication likewise checks restored device generation, revocation and expiry, without a history/key epoch ([src/v2/auth.rs](src/v2/auth.rs), lines 248–293; [crates/core/src/access.rs](crates/core/src/access.rs), lines 414–449).

A still-unexpired file ticket from a snapshot can work after restore, including one revoked after the snapshot. Generating a new credential-derivation key does not invalidate an already-issued secret whose hash is still present. A device revoked after the backup can similarly regain its older grant when that backup is restored. The latter needs an explicit durable-credential restore policy; this review does not assume the plan already mandates blanket device re-pairing.

The original architecture §18.6 explicitly requires restore to revoke ephemeral tickets. Revoke those tickets in both restore paths and decide how durable device authorization survives restoration, including any external revocation witness or deliberate re-pair flow. The core should express credential/history admissibility; restore adapters must commit the epoch and invalidations before serving.

**Future check:** back up, issue/revoke grants around the snapshot, restore before their expiry, and attempt old file tickets, cookie sessions and device credentials. Verify ephemeral refusal and the documented durable-device policy. Owner: access and restore; handoff §§5,8.

### A02 P2 Garbage collection does not protect current v2 prepared playback

**Source supported.** [src/maintenance.rs](src/maintenance.rs), lines 164–213, protects a prepared output only through legacy `playback_sessions`, then marks the generated file unavailable, expires the job and schedules removal of its directory. V2 uses `viewing_sessions`, `delivery_sessions` and `delivery_owners` instead ([src/v2/viewing.rs](src/v2/viewing.rs), lines 221–246; [src/delivery.rs](src/delivery.rs), lines 4018–4037).

Once a prepared rendition crosses its retention age, maintenance can collect it during active v2 playback. A subsequent byte request or delivery revalidation then fails. Merely adding another status predicate would still need coordination with new playback admission.

Represent active artifact pins and cleanup intent explicitly in the shared core policy. The adapter must atomically fence new admission against cleanup and retain pins for admitted readers until their actual completion. Keep filesystem deletion recoverable and owned.

**Future check:** play an age-eligible prepared rendition through v2, race GC with open and range requests, and verify it survives until the last pin ends. Include cancellation, paused playback, restart and failed deletion. Owner: delivery and storage; architecture §§6.3,17.2; handoff §8.

### A03 P2 Browser-created schedules expire with the creating session

**Source supported.** Schedule creation stores the presented credential hash ([src/v2/schedules.rs](src/v2/schedules.rs), line 185). Each due dispatch authenticates that exact credential and persists `enabled=0` on failure until explicitly re-enabled (lines 269–285; [src/metadata_jobs.rs](src/metadata_jobs.rs), lines 36–61). Browser sessions last 12 hours ([crates/core/src/access.rs](crates/core/src/access.rs), line 15).

A daily schedule created in the UI reaches its first due time after its creating cookie session has expired and is disabled without executing. Renewing the browser session does not replace the schedule's stored hash; saving unchanged settings also does not change it. Short-lived access tokens have the same lifetime mismatch. Operator and trusted-ingress schedules follow different paths and are not affected in the same way.

Define durable schedule authority separately from the transient credential used to submit it. Recheck current device/principal grants and library scope on dispatch, while preserving explicit revocation. Model the distinction between session expiry and authority withdrawal in core; keep lookup and transactional dispatch in the adapter.

**Future check:** create a daily schedule with a browser session, advance beyond session expiry, renew/login, and verify the intended dispatch. Separately revoke the device or source permission and verify denial. Owner: schedules and access; handoff §§4,6; architecture §§9,12.

### A04 P2 Retention still targets legacy history while v2 records grow

**Source supported.** [src/storage.rs](src/storage.rs), lines 393–403, prunes old physical scan jobs, legacy playback sessions and selected processing jobs. V2 appends `viewing_events` for acknowledged observations ([src/v2/viewing.rs](src/v2/viewing.rs), line 803), while the only deletion of those records is profile deletion. `viewing_sessions`, expired `idempotency_records`, expired credentials and file tickets also lack a general expiry-pruning path in the inspected production sources. Their schema expiry columns and indexes are not cleanup by themselves.

Ordinary viewing and new command/token identities therefore increase the database indefinitely even when the configured history retention is working as written. This is additional to the handoff's explicitly unbounded `offline_events`. Operational metrics and playback-monitor history already have their own bounded pruning; they should not be described as unbounded.

Design bounded retention per record family. Keep canonical viewing state and the current authority barrier; define the replay horizon and stale-session refusal before compacting acknowledgement history. Remove expired idempotency records under their documented horizon and prune credentials/tickets in dependency-safe batches. Do not apply age-only deletion to causal offline state.

**Future check:** generate expired and live records around every retention boundary; compact in bounded transactions; verify exact retries within the supported horizon, explicit refusal outside it, current viewing authority and no deletion of live grants. Owner: storage, viewing and access; handoff §§1,8; architecture §§6,16.1,21.

### A05 P2 Every catalog change updates every preparation revision

**Source supported.** [migrations/0039_logical_schedules.sql](migrations/0039_logical_schedules.sql), lines 22–30, performs an unqualified `UPDATE preparations SET revision=revision+1` for each catalog/library/source event and each version-file insert, update or delete.

A scan affecting N files with M retained preparations can perform roughly N×M preparation-row updates, with additional events/binding changes increasing the work. The cost is inside the writer transaction. An unrelated library scan also invalidates all preparation ETags. This is visible write amplification, not a measured latency claim.

Use a forward migration to restrict invalidation to affected source/timeline/version dependencies, or a deliberate dependency revision that can be derived without rewriting every preparation. Preserve applied migrations and stale-source protection.

**Future check:** large unrelated scan plus many preparations; assert unrelated revisions stay stable, affected ones invalidate, and mutation work is bounded by actual dependencies. Measure writer delay separately when performance qualification is authorized. Owner: persistence and preparation; handoff §§2,3,8; architecture §§6.1,21.3.

### A06 P2 Ordinary page cursors do not bind query context

**Source supported.** The shared [src/v2/mod.rs](src/v2/mod.rs) `PageQuery::after` accepts an arbitrary nonempty string and `page` returns the last row ID as `next_cursor` (lines 633–653,665–690). Ordinary list endpoints use this as `id > cursor`, without encoding or validating the resource family, filters, sort or scope context. Some specialized endpoints implement richer cursors; this finding concerns the common helper and its callers.

Reusing a cursor after changing a library, query or list type silently starts partway through the new result set instead of rejecting/resetting it. Authorization is reapplied by the individual queries, so this is not by itself a cross-library disclosure claim. It does not meet the original plan's cursor-context rule and complicates the handoff's stable pagination work.

Specify a versioned opaque cursor carrying the relevant query/sort/scope identity and tie-breaker; validate it before reading. Do not promise a multi-request frozen snapshot. Keep pagination as a bounded read contract rather than a global state machine.

**Future check:** swap filters, library, principal and endpoint while reusing a cursor; require explicit invalidation where context changed. Exercise concurrent inserts/deletes under the declared ordering. Owner: API and query adapters; handoff §2; architecture §§6.4,11.2.

### A07 P2 Scoped event subscribers miss schedule and organization invalidations

**Source supported.** Schedule mutations emit the `schedules` topic ([migrations/0039_logical_schedules.sql](migrations/0039_logical_schedules.sql), lines 13–18), but [src/v2/events.rs](src/v2/events.rs), lines 134–169, has no schedule classification and falls back to administrator-only disclosure. A non-admin who can list/manage the scoped schedule can therefore receive no corresponding invalidation.

Organization deletion has a related ownership loss: a collection deletion emits its ID, then classification queries the now-deleted collection to recover the owning profile (lines 154–161; [migrations/0014_organization.sql](migrations/0014_organization.sql), line 55). The missing owner becomes `Administrative`; `disclose` resets missing item scope but withholds this case ([crates/core/src/access.rs](crates/core/src/access.rs), lines 836–854). An authorized subscriber can retain stale organization state until an independent refetch.

Persist sufficient prior owner/scope information with invalidations, and classify schedules under their actual readable scope. Represent disclosure/reset decisions in core; adapters supply current and prior facts without exposing inaccessible IDs.

**Future check:** restricted subscribers create/change/delete their schedules and collections; verify the correct hint or anonymous reset, no foreign identifiers, and correct reconnect behavior after deletion. Owner: events, organization and schedules; handoff §§2,4; architecture §6.4.

## Catalog findings

### C01 P1 Accepting a local match bypasses whole-work write scope

**Source supported.** [src/v2/metadata.rs](src/v2/metadata.rs), lines 667–686, checks curation scope for the selected file but only *read visibility* for the target candidate. The file helper covers that file's memberships, not its entire work (lines 874–881). [src/matching.rs](src/matching.rs), lines 293–325, can then merge whole works, moving editions/contributions and creating aliases through [src/curation.rs](src/curation.rs), lines 312–392.

An A-only writer can accept an A subject into a target shared by A and inaccessible B, changing shared state without authority over B. A single-version subject work with other B file occurrences presents the same problem on the source side; multi-version subject works are already refused by the attachment policy. This bypasses the whole-work `may_curate` policy already used by explicit reconciliation.

Reuse the explicit reconciliation authorization boundary for all affected work, relationship and order owners, inside the same transaction as the merge. Core should decide whether the complete affected membership set is curatable; adapters must supply that complete set.

**Future check:** A-only acceptance into an A+B work, and a subject with hidden occurrences; require refusal with unchanged revisions, aliases, contributions and receipts. Include foreign relationship/order owners. Owner: matching and access; handoff §4; architecture §§8.2,12,17.

### C02 P2 Television hierarchy and release ordering cannot be authored through v2

**Source supported.** `CatalogItemInput` contains only kind/title/libraries, while creation supplies no parent or number ([src/v2/catalog.rs](src/v2/catalog.rs), lines 768–772,853). Core requires both for seasons and episodes ([crates/core/src/catalog.rs](crates/core/src/catalog.rs), lines 3–12). Replacement preserves the old hierarchy; generic relationships do not update `item_structure`. Series/seasons cannot receive editions (catalog handler lines 1086–1092), but order groups require an edition (lines 1848–1885).

A clean public-API workflow therefore cannot construct the TV graph that the ordering code expects. [tests/structure.rs](tests/structure.rs), lines 35–44, inserts a series edition directly in SQL, bypassing the public restriction. This is additional backend work behind handoff §4's proposed ordering UI.

The schema also stores only one order group/position on each timeline ([migrations/0015_timelines_versions.sql](migrations/0015_timelines_versions.sql), lines 7–12). Moving a timeline replaces its prior group membership. This is a backend constraint for the already-assigned alternate-order workflow, not another request to implement its UI.

The additional work is to make the existing public identity/ordering contract express the required hierarchy and release membership, with revision and scope fences. **Future check:** construct the required series/season/episode and release state entirely through public routes; SQL-only fixture setup must not bypass the public restrictions. UI implementation remains with handoff §4. Owner: catalog identity and contract; architecture §§5,8.4,20.

### C03 P2 Scoped browsing hides valid series and seasons

**Source supported.** [src/v2/catalog.rs](src/v2/catalog.rs), lines 365–375,409–411,518–522,659–663, derives item membership from files in directly owned editions. Valid series/seasons cannot own editions. Their visible descendant episodes do not establish parent membership.

A library-restricted caller can see isolated episodes but receive 404 for their series/season; library-filtered browsing excludes those parents even for an administrator, and episode responses can strip the parent IDs. This affects retained hierarchies independently of C02's creation gap.

Use a consistent hierarchy-membership query or explicit membership model for browse, search and parent links. Derive only permitted descendant information; readable parents must not expose hidden siblings or relax whole-work curation.

**Future check:** a series with A-visible and B-hidden episodes; A sees the appropriate parent navigation without B titles/counts, while curation retains its stricter rule. Owner: catalog queries and access; handoff §§2,4; architecture §§5.1,8.4,12.

### C04 P2 Public search bypasses the full-text implementation

**Source supported.** `/api/v2/catalog/search` delegates to the title-only `instr(lower(i.title),lower(?))` browse query ([src/v2/catalog.rs](src/v2/catalog.rs), lines 659–666,729–745). The richer scoped FTS implementation exists separately in [src/search.rs](src/search.rs), lines 15–118,151–216.

Actor, tag, retired alias/origin-title and some normalized Unicode searches can fail through the public endpoint despite indexed data and direct service tests. The substring query also lacks the intended indexed search behavior; no latency measurement is asserted.

Wire the existing scoped FTS service into the public response/cursor contract, retaining explicit stale-index behavior and scope before ranking/counts. **Future check:** public HTTP title, Unicode, tag, actor and alias searches with hidden matches and multiple pages. Owner: search and API; handoff §2; architecture §8.4.

### C05 P2 Match correction services have no public entry point

**Source supported.** [src/v2/metadata.rs](src/v2/metadata.rs), lines 36–52,621–627, exposes proposal creation and accept/reject/defer. Re-proposal preserves decisions ([src/matching.rs](src/matching.rs), lines 227–233), and accepted/rejected proposals cannot be decided again. Reopen, unmatch and manual-identify implementations at matching lines 407–506 have no production caller in the inspected tree.

After an incorrect rejection or acceptance, Find matches can return the same decided proposal and correction fails with `match_already_decided`. Splitting catalog works does not reopen that proposal. This is separate from the approved online-provider deferral.

Expose explicit correction/reopen/unmatch operations with proposal/work revisions, exact file identity, authorization and retained receipts, then add the UI. **Future check:** reject → reopen → accept and accept → correction/unmatch through v2, followed by rescan. Owner: matching and contract; handoff §4; architecture §8.2.

### C06 P2 Hidden matches consume candidate limits and influence public status

**Source supported.** [src/matching.rs](src/matching.rs), lines 142–175, scans all works and stops after 50 matches without caller scope. [src/v2/metadata.rs](src/v2/metadata.rs), lines 365–377, removes hidden candidates only when serializing, preserving status derived from the original candidate count ([crates/core/src/matching.rs](crates/core/src/matching.rs), lines 72–79).

Fifty private same-title works can suppress a later accessible candidate. A visible proposal's `pending` versus `review` status can also reveal the existence of additional private matches. Filtering response bodies after limits and count-derived decisions is insufficient.

Separate global matching evidence from scoped candidate querying and scoped presentation decisions. Index normalized matching facts instead of scanning all works under the writer transaction. **Future check:** hidden candidates before the visible one and one-visible versus one-visible-plus-private status cases. Owner: matching, queries and access; handoff §4; architecture §§8.2,8.4,12.

### C07 P2 One oversized smart collection breaks the collection directory

**Source supported.** [src/v2/organization.rs](src/v2/organization.rs), lines 637–655, evaluates up to 10,001 visible works before applying the filter, refusing more than 10,000 candidates or 1,000 results. Listing propagates one collection's failure to the whole page (lines 720–725).

At 10,001 accessible works, even an empty-match smart filter can make `listCollections` return 409 and hide unrelated manual collections. A broad filter reaching 1,001 results has a similar directory-level effect. Fixing the UI's title-read budget alone will not address this.

Keep directory summaries available when one collection exceeds its evaluation/member budget; expose that collection's bounded status and separately paginated members. The general per-title batching and filter UI work stays with handoff §§2,4. **Future check:** selective, empty and broad smart filters plus manual collections beyond both bounds; one failed evaluation must not hide unrelated collections. Owner: organization and queries; architecture §§8.4,21.3.

### C08 P2 Media-file limits do not bound scanner traversal

**Static resource risk.** [src/scan.rs](src/scan.rs), lines 165–183, collects and sorts an entire directory before applying limits; lines 196–205 enqueue all directories. The 100,000-file limit applies to supported media/sidecars, not arbitrary entries, directories or retained coverage.

A huge ignored-file directory or broad tree of empty directories can consume unbounded memory/work below the media-file cap. Cancellation is checked between directories, so collection/sorting of a huge directory also delays cancellation. Actual resource consumption depends on the source tree.

Budget entries, directories, depth, enumeration and coverage storage separately. Feed exhaustion into existing core incomplete-coverage decisions; never turn it into false absence. **Future check:** small injected traversal budgets, ignored-file/empty-directory fixtures, cancellation during enumeration and preservation under incomplete ancestors. Owner: scanner and source adapter; architecture §§7,9,21; handoff §8.

### C09 P2 Watcher coalescing sits behind an unbounded queue

**Static resource risk.** [src/watch.rs](src/watch.rs), lines 69–82, sends native event path vectors into `mpsc::unbounded_channel`. Coalescing happens only after consumption, while the same consumer awaits demand writes (lines 115–129).

An event burst while database admission is stalled can accumulate arbitrary path vectors before the bounded core debounce state sees them. Use bounded nonblocking ingress plus an explicit overflow/lost-event observation that guarantees catch-up reconciliation. The native callback should not block waiting for queue space.

**Future check:** block demand admission, exceed the ingress budget, and prove bounded retained state plus eventual adequate rescan. Owner: watcher and demand admission; architecture §§7.1,9.

### C10 P3 Non-UTF-8 directories are silently omitted from scan completeness

**Source supported.** [src/scan.rs](src/scan.rs), lines 165–169, skips a directory whose relative path cannot be represented as UTF-8. Its parent is not marked incomplete. Non-UTF-8 media filenames, by contrast, produce `non_utf8_path` at lines 232–233.

A Unix directory with an invalid UTF-8 component can contain skipped media while the scan reports complete. This finding concerns truthful coverage, not a demonstrated deletion of previously cataloged data. Record a bounded unsupported-path observation against the representable parent and mark the relevant coverage incomplete without inventing a lossy identity.

**Future check:** invalid-byte directory and filename fixtures, with valid siblings still published and both omissions reported. Owner: scanner; architecture §7.4.

### C11 P2 Non-poster artwork selections cannot be read back through v2

**Source supported.** [src/v2/metadata.rs](src/v2/metadata.rs), lines 990–1029, accepts poster/backdrop/banner/thumbnail selection. The item representation reads only the poster ([src/v2/catalog.rs](src/v2/catalog.rs), lines 488–516), and artwork asset responses expose content identity without role-selection state (metadata lines 705–760).

A successful backdrop/banner/thumbnail selection cannot be rediscovered as that role's current selection after reload. The additional finding is the missing public read contract: handoff §4 assigns UI work on the assumption that the existing API can support it. Storage and image validation already exist.

Expose scoped role-to-asset selection, revision and provenance. **Future check:** select each role, reload through public reads, and confirm exact identities and stale-edit refusal. Artwork controls remain with handoff §4. Owner: artwork read API; architecture §8.3.

## Playback findings

### P01 P2 Concurrent playback plans can exhaust the database pool

**Source supported.** [src/v2/playback.rs](src/v2/playback.rs), line 1052, acquires a pooled connection and retains it while awaiting `timeline_components` at lines 1151–1153. That helper acquires a second connection ([src/components.rs](src/components.rs), line 54). Automatic subtitle preferences also read through the pool while the first connection remains held (playback lines 1277–1282). The production pool has four connections ([src/db.rs](src/db.rs), line 279).

Four overlapping plans can each own one connection and all wait for a second, stalling until acquisition timeout releases one. Unrelated API reads and delivery revalidation share that pool. Build plan facts through connection-taking helpers in one read snapshot, or explicitly release and revalidate when combining independent reads. Enlarging the pool only changes the concurrency threshold.

**Future check:** barrier-controlled concurrent plans against the actual four-slot configuration, with subtitle off/auto and a concurrent lightweight read; all should finish without pool timeout. Owner: playback adapter and persistence; handoff §§3,8; architecture §§4.1,6.1,10.2.

### P02 P2 Multipart audio correspondence is guessed from order when tracks are ambiguous

**Source supported.** [crates/core/src/components.rs](crates/core/src/components.rs), lines 361–408, groups each file's nth track with the same kind/language/roles/title as a shared component. [src/v2/playback.rs](src/v2/playback.rs), lines 1718–1771, treats those derived occurrences as exact per-part mappings.

If two untitled English tracks have identical roles and reverse order in the next part, selecting one program on part 1 silently selects the other on part 2. Exact stream/revision pins do not repair an identity inferred incorrectly. Existing multipart tests deliberately use distinct Matroska titles because MP4 drops them ([tests/v2_playback.rs](tests/v2_playback.rs), lines 5038–5041); the already-fixed titled-track case is not the finding.

Keep cross-file occurrences distinct when correspondence is ambiguous; require unambiguous evidence or an explicit mapping in core. A local ordinal distinguishes streams but does not prove program continuity across files or versions.

This finding concerns inferred equivalence during existing multipart original playback. Handoff §3 already owns audio/subtitle selection for preparation; those controls do not by themselves correct the cross-file identity rule described here.

**Future check:** same-language untitled tracks with reversed ordering, a missing selected program and explicit mappings; assert identity in core and distinct real audio output across the part boundary. Owner: component identity and playback; handoff §3; architecture §§5.1,8.3,10.6,17.2.

### P03 P2 Automatic planning considers only the first readable original

**Source supported.** `original_of` returns the first readable revision-valid original in ID order before evaluating codec, quality or track suitability ([src/v2/playback.rs](src/v2/playback.rs), lines 598–641). The handler builds one original's `SourceFacts` for core planning (lines 1079–1103,1459–1510). Its later original enumeration discovers prepared renditions rather than additional original-route candidates.

For an unpinned request, an unsupported HDR original can prevent Auto playback even when a later equivalent SDR original is playable. A component existing only on the later version can be rejected early; exhaustion of the first original's routes also does not advance to another source. Explicitly pinned requests should remain strict and are not an argument for widening selection.

Supply an authorized candidate set to deterministic core ranking/fallback, retaining exact explicit choices and candidate failure identities. **Future check:** two originals with reversed ID order, the first unsupported or failed, and a component available only on the second; explicit pins must still refuse unintended fallback. Owner: playback policy and observation adapter; handoff §3; architecture §§10.2,10.4.

### P04 P2 Subtitle timeouts free capacity before blocking work finishes

**Source supported.** [src/delivery.rs](src/delivery.rs), lines 4754–4764, holds extraction and subtitle permits in the async owner. Source opening at lines 4793–4798 and the later metadata check at 4838–4848 run in `spawn_blocking` under timeouts, without transferring those permits into the blocking closures. Cached subtitle-segment parsing at lines 4705–4721 has a related request-cancellation lifetime gap.

A timed-out NAS call continues after its join handle is dropped, but the enclosing task returns and releases capacity. Retries can create more concurrent blocked work than the configured slot count. Existing preparation/media IO code already demonstrates how to retain ownership in uncancellable work.

Move shared permit ownership into blocking operations until actual completion. Keep timeout/fencing distinct from confirmed exit in resource policy; adapters enforce the real lifetime.

**Future check:** stall open/metadata behind an injected barrier, pass the timeout, and assert capacity remains held until release; repeat for cancelled subtitle-segment requests. Owner: delivery and execution; handoff §§3,8; architecture §§9.4,9.5,17.2.

### P05 P2 Live publication has no physical source fence between rescans

**Static risk.** The live path verifies the root and file fingerprint when opening ([src/delivery.rs](src/delivery.rs), lines 1477–1487), then drops its own first-source handle after passing the descriptor to the encoder (lines 1705–1706). Output observation validates segment properties without re-observing source fingerprints (lines 1235–1407); periodic revalidation checks database revisions/availability (lines 1898–1945).

An in-place overwrite while a long encode is reading can therefore supply changed bytes before the watcher/rescan advances SQLite. A descriptor protects against path replacement, not mutation of its inode. Whether changed media becomes observable depends on format and timing; no corrupt playback was reproduced. Catalog-revision invalidation is already implemented and is not the missing check.

Retain physical observations for all part descriptors and feed changed/unknown source facts into publication fencing. Contrast the original-byte path, which checks its descriptor after reads ([src/media.rs](src/media.rs), lines 186–202), and preparation's post-conversion source check. Define path replacement separately from in-place modification.

**Future check:** modify an unread later region of a paced source without first updating its DB revision; require refusal of subsequent stale-generation publication. Cover later multipart files and separate rename cases. Owner: delivery publication and source observation; handoff §§3,8; architecture §§7.3,10,17.2.

## Client and offline findings

### U01 P2 Ordinary UI pages silently discard pagination after 200 rows

**Source supported.** [src/presentation.rs](src/presentation.rs), lines 295–302, discards `next_cursor`; library/search force `limit=200` at lines 440–479, and matches/jobs use the same helper at 737–740 and 771–774. The facade/view supplies a grid and page-length count without continuation navigation.

The 201st authorized title, search result, proposal or job is unreachable from the corresponding screen. Counts can look like total results. This is a separate, lower limit than the handoff's explicit 1,000-row scan/rendition and roughly 300-title organization bounds.

Carry cursor and page metadata through the facade and render bounded navigation with query/profile identity. **Future check:** 201+ rows in each screen, concurrent changes and restricted results, verifying all authorized records remain reachable. Owner: query facade and UI; handoff §2; architecture §§8.4,14.6,21.3.

### U02 P2 Next and autoplay lose an explicitly selected profile

**Source supported.** [packages/ui-bridge/player.js](packages/ui-bridge/player.js), lines 152–165, asks for the next title using the current profile but navigates to `/play/NEXT?play=1` without `profile_id`. The renderer then falls back to the profile cookie/default ([src/presentation.rs](src/presentation.rs), lines 347–362). CLI playback explicitly supplies a profile in the original URL ([apps/terminal/internal/cli/play.go](apps/terminal/internal/cli/play.go), lines 18–30).

Open a B-profile player link in a browser whose cookie selects A. The first title uses B; Next/autoplay can switch to A and write A's viewing history. Both profiles may be authorized, so permission checks do not detect the identity mistake.

Carry the selected profile through continuation navigation and keep it an explicit client transition input. **Future check:** cookie A plus URL B, manual Next and autoplay; next-page preferences and all viewing sessions remain B. Revoking B must refuse instead of falling back silently. Owner: player bridge; handoff §§3,6,8; architecture §§12,14.4,17.

### U03 P2 Serial offline playback can need twice the reserved snapshot space

**Source supported.** [crates/motion-ui-host/src/lib.rs](crates/motion-ui-host/src/lib.rs), lines 542–580, allocates a new complete playback snapshot before replacing `host.media`. Destroying the renderer's previous player does not release that host snapshot. The downloader reserves only the largest single title plus 512 MiB ([apps/desktop/src/downloads.mjs](apps/desktop/src/downloads.mjs), lines 168–172).

With room for one snapshot, opening a title works, but returning to Downloads and reopening it or another large title can fail the free-space check while the old anonymous file is still held. The previous player need not still be active. Restarting the helper releases the file; ordinary navigation does not.

Add explicit snapshot retirement/admission with pins for still-active range bodies, or deliberately reserve supported overlap. Keep disk probing/copying in adapters and ownership decisions explicit.

**Future check:** quota-limited storage with one-snapshot headroom, open → close → open, plus old readers, failed opens and crashes. Owner: offline cache host and desktop; handoff §§1,5,8; architecture §16.1.

### U04 P2 A full offline journal blocks reconnecting to compact it

**Source supported.** The host bounds the log to 10,000 events/8 MiB ([crates/motion-ui-host/src/lib.rs](crates/motion-ui-host/src/lib.rs), lines 37–39,281–298). The bridge retains a rejected event and retries it on every `close` ([packages/ui-bridge/offline-player.js](packages/ui-bridge/offline-player.js), lines 18–52). Native detach refuses to leave offline mode without successful flush; connect and mode changes call detach first ([apps/desktop/src/main.mjs](apps/desktop/src/main.mjs), lines 98–108,152–155,264–269).

Once the log is full, a permanent append refusal prevents the reconnect needed for online reconciliation/compaction. Normal close can fail too. At one observation per five seconds, the count cap is approximately 14 accumulated playing hours; status changes can shorten it. That duration is an inference, not a measurement. Killing the app escapes but can lose the rejected observation held only in memory.

Represent exhaustion and recovery explicitly. Provide a durable terminal checkpoint/reserve or a recovery transition that hands retained history to sync without requiring another append. Surface permanent Full distinctly from transient IO failure; preserve identity and sequence.

**Future check:** each real journal cap plus a pending observation, native reconnect and close; durable events reconcile without silent loss/renumbering and mode changes remain recoverable. Owner: offline core, host and desktop; handoff §§1,5,8; architecture §§16.1,17.

### U05 P2 Editing a music or photos library changes its kind to movies

**Source supported.** The public library policy and CLI support `music` and `photos`, but the UI's library-kind select contains only movies, television, personal_video and mixed ([crates/motion-ui/src/screens.rs](crates/motion-ui/src/screens.rs), lines 225–230,286–291). With no matching selected option, the browser chooses the first option. Saving even a name/language edit submits `movies`, which the service persists ([src/libraries.rs](src/libraries.rs), lines 343–357).

This is accidental mutation of a supported library configuration, separate from missing dedicated M6 music/photo interfaces. Preserve every existing supported kind in the form and submitted replacement unless the user explicitly changes it; the current PUT contract requires the field.

**Future check:** create both kinds through CLI/API, edit only their names through the rendered form, and verify kind remains unchanged. Owner: library UI; handoff §§4,7.

### U06 P2 Historical downloads eventually make the Downloads page unavailable

**Source supported.** The facade drains at most 25 pages/5,000 download rows before filtering cancelled demands ([src/presentation.rs](src/presentation.rs), lines 797–801). The API retains and lists cancelled rows ([src/v2/downloads.rs](src/v2/downloads.rs), lines 511–517,549–575). The desktop agent rereads all pages without a total page bound ([apps/desktop/src/downloads.mjs](apps/desktop/src/downloads.mjs), lines 122–130).

Lifetime download churn can break the entire page above 5,000 rows visible to the current device/principal and permitted profiles, even with few active downloads, while each agent pass grows. Add scoped status filtering, pagination and a durable demand-retention policy. Do not blindly delete cancelled demands: offline reconciliation may still use pinned duration from them (download handler lines 850–867).

**Future check:** thousands of cancelled demands plus a few live ones and retained offline events; active navigation and agent work remain bounded and reconciliation stays correct. Owner: downloads, query facade and desktop; handoff §§1,2,5.

### U07 P3 Search results clear the editable query

**Source supported.** The search input in [crates/motion-ui/src/app.rs](crates/motion-ui/src/app.rs), lines 699–704, has no value binding. After a search the heading retains the query, but the editable field is blank, forcing users to retype it to refine the search. Bind the escaped query to the input. **Future check:** ordinary, quoted and Unicode queries. Owner: UI; handoff §6. No core state change is needed.

### U08 P3 Terminal list output hides the continuation cursor

**Source supported.** Human-readable catalog/search/source/job list output omits `next_cursor`, although those commands accept `--cursor`; JSON output retains it. Device/profile listings already show a continuation cue. See [apps/terminal/internal/cli/catalog.go](apps/terminal/internal/cli/catalog.go) and [apps/terminal/internal/cli/operations.go](apps/terminal/internal/cli/operations.go).

Relevant output branches are catalog lines 37–48,87–94 and operations lines 49–62; [apps/terminal/internal/cli/root.go](apps/terminal/internal/cli/root.go), lines 344–346,471–473, shows the existing continuation convention. Print that hint consistently. **Future check:** force a two-page response and confirm a user can obtain page two without switching output modes or guessing its cursor. Owner: terminal presentation; handoff §6.

## Additional gaps not assigned in the handoff

The repeated handoff backlog has been removed. These two rows remain because the handoff does not explicitly assign their concrete implementation work. Their original IDs are retained.

| ID | Remaining issue and source evidence | Dependency and next step |
|---|---|---|
| K09 | **Known manifest CODECS mismatch:** ordinary transcodes advertise fixed `avc1.64001f` ([src/v2/playback.rs](src/v2/playback.rs), 2576–2584), even when retained frame rate or hardware output requires different actual codec metadata. Already recorded in [M4 ledger](qualification/m4-expansion/README.md), 186–192. | Playback owner derives emitted init-segment codec information as copy/adaptive paths do; qualify high-frame-rate and hardware routes. This is an existing defect, not merely a device-test deferral. |
| K19 | **Metadata locks and intentional blanks are an additional original-plan gap:** [src/v2/metadata.rs](src/v2/metadata.rs), 229–249, refuses nonempty `locked_fields` and JSON-null intentional blanks. Local precedence exists but is not full lock/blank semantics. | Original architecture §8.1; this concrete gap is not assigned in the handoff. Track any accepted follow-up through the existing completion owner. Metadata core needs provenance-aware semantics before API/UI support and refresh/rescan acceptance. |

Online provider matching/artwork, optional external/commercial/social integrations and WebKit retain their existing explicit deferrals. They are not new commitments created by this review. Copy, hardware and adaptive defaults should retain their current qualification gates.

## Documentation issues

**D02 P3 README points to a retired client source path.** [README.md](README.md), around line 77, describes `web/demuxe.js`, while the retained module lives under [tests/fixtures/legacy-web/demuxe.js](tests/fixtures/legacy-web/demuxe.js); production playback is in the bridge. Update the description and distinguish retained regression fixtures from shipped client code. Preserve the current player-archive identity and per-player runtime limitations.

## Verification and coverage

No execution evidence was produced by this review. Existing receipts retain their original revision, artifact, fixture and platform limits.

### Evidence limits relevant to the remaining findings

The handoff owns the general model-coverage audit and release qualification in §8. The following observations are limited to the additional defects retained here; they do not create another release checklist.

| Retained findings | Limit of the inspected evidence |
|---|---|
| P02/P03: component identity and original selection | `route_selection_model.rs` supplies one constant original and does not establish cross-file component correspondence. Its route permutations cannot establish ambiguous-track refusal or fallback between different original candidates. |
| U03/U04: snapshot space and journal exhaustion | `downloads_model.rs` and `offline_model.rs` exercise production reconciliation decisions, but do not establish desktop mode switching under a full log or the host's overlapping snapshot lifetimes. Source exploration/fuzz settings are not counts executed by this review. |
| C01/C02/C08/C09: catalog authorization, API reachability and input bounds | Production identity/matching/demand/coverage models do not prove that HTTP supplies the complete affected scope, that valid TV state can be created through public routes, or that native enumeration/event ingress is bounded. |
| A02/P01/P04/P05: adapter lifetime and publication | Existing delivery/execution models do not establish actual SQL connection lifetime, blocked-filesystem permit ownership, source mutation checks or GC/admission coordination. The focused checks attached to these findings target those boundaries. |

Keep additional model properties tied to the retained defects: whole-work matching scope, ambiguous component refusal, multi-original fallback with strict pins, artifact pins versus GC, durable schedule authority, and offline exhaustion/reconnect. Exercise the production decisions, state their bounds and pair them with the relevant adapter checks.

`tests/contract.rs` checks registered route dispatch and delegates wire-shape checks to subsystem tests. It does not establish the public workflow reachability identified by C02/C05/C11. Any accepted contract repairs need corresponding schema/client updates under the existing implementation owner.

### Review coverage inventory

| Subsystem | Inspected scope |
|---|---|
| Catalog and persistence | Catalog/curation, libraries/sources, scans/watch/demands, metadata/NFO/matching/artwork, organization/search, markers/interchange, relevant migrations and v2 handlers. Critical paths were traced into production core and corresponding tests. |
| Access and operations | Production router/host/origin boundaries, credential/profile/device paths, content tickets, SSE, page/idempotency helpers, schedules/jobs, backup/restore/retention, startup/shutdown, diagnostics and sampled metrics/monitoring paths. |
| Playback | Planning, delivery/generations, execution, preparation/renditions, media/ranges, fMP4/encoding, components/subtitles/parts, canonical viewing, relevant core models and adapter tests. |
| Browser and native clients | `motion-ui`, presentation facade, playback/UI bridges, desktop lifecycle/recovery/outbox/download agent, offline host/store and offline production policies. |
| Terminal | API authentication/retry/redirect/SSE, credential store and server identity selection, CLI catalog/operations/browser launch, TUI paged reads/stale-response handling and selected action/presentation paths. Broader operation handlers were sampled, not individually proved. |
| Packaging and verification | Root manifests/lock identities, package/media-tool builders, source inventory/companion checks, CI tier/workflow definitions, contract route tests and subsystem/model test source. None executed. |
| Third-party and historical material | Motion-owned Demuxe/Shaka integration, vendor READMEs and paced-HLS patch; relevant qualification and legacy fixtures. Upstream archive payloads, generated/downloaded vendor trees, research prototypes and every historical receipt were not audited line by line. |

This is a repository-wide static review organized by production workflows, not an exhaustive proof of every source line or dependency. No clean bill of health is implied for sampled or unexecuted paths. The server, catalogs, original media, running services and existing worktree changes were not modified; the deliverable is this document.

## Priority of the additional findings

Coordinate these findings with the existing completion owner; the handoff remains the implementation sequence and progress ledger.

1. Prioritize A01/C01 authorization, A02 artifact protection, P01/P04 resource lifetime, and U02/U04 profile/recovery defects.
2. Address the additional query, retention and invalidation defects A04–A07, C03/C04/C06/C07 and U01/U06. Keep A03's durable schedule authority distinct from enabling new scheduled task types.
3. Resolve C02/C05/C11's missing public contracts and K19 metadata lock/blank semantics as prerequisites to the assigned UI where relevant. Include U05's unintended kind change and the smaller U07/U08 presentation defects.
4. Address P02/P03/P05's component/candidate/source rules, K09's manifest metadata, C08–C10's scanner/watcher bounds and coverage, and U03's snapshot ownership. Correct D02's stale client-source reference.

These are proposed additional follow-ups only. Reconciliation changed documentation, not application behavior, and performed no builds or tests. Work removed from this document remains with the handoff's implementation owner; it is not claimed complete.

## Implementation status

Repairs started in branch `fix/rewrite-review`, from baseline `28f4b32`.
The review and Catabolic integration proposal were committed in `779be9d`.
The historical findings above remain the static review baseline. Current patch
status, verification, review disposition and outstanding work are recorded in the
[single completion ledger](qualification/completion/README.md#additional-rewrite-review-findings).
