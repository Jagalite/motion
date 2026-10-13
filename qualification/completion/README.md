# Completion ledger

## Additional rewrite review findings

Branch: `fix/rewrite-review`; worktree: `motion-review-fixes`; base: `28f4b32`.
Documents: `779be9d` (reconciled review and Catabolic backend proposal).
Implementation source: `5d12744`, comprising `1b3ef3d` (restore), `fd47ae2`
(components), `eb20215` (playback/resource ownership), `1ab57ae` (scan/watch),
`d2317aa` (API/revisions/scope/inventory) and `5d12744` (clients).
Implementation owner: this branch's single implementation agent. Independent
reviewer: GPT-6.1 Sol. Research agents performed read-only investigation.

This section covers the 34 findings retained in
[the rewrite review](../../MOTION_REWRITE_CODE_REVIEW.md). The separately assigned
completion and access work keeps its ownership. Integrate this section into that
branch's ledger when the branches meet; do not replace its receipts. The Catabolic
proposal remains a design document and is not implemented by these repairs.

### Current patch batch

Twenty findings have implementation changes. Compilation and Rust regressions
remain pending; these are not merged or release-qualified. Completed non-Rust evidence is recorded below; the queued Rust checks do not yet
qualify these implementation commits.

| Findings | Implemented behavior | Verification / remaining qualification |
|---|---|---|
| A01 | Both restore paths revoke saved devices and remove credentials, tickets, pairings and idempotency receipts with the new epoch. Rust publishes a checked staging database without replacing an existing destination. | Python backup regression passed; real SQLite restore/authority tests pending. Policy deliberately requires device re-pairing after restore. |
| A02 | Production core arbitrates readers versus cleanup; process-local RAII leases cover live deliveries, multipart sources, byte responses and late blocking/probe/subtitle work. GC records unavailable/expired intent under the writer before filesystem removal. | Core graph and real cancellation/GC/prepared-delivery tests pending. Process restart interrupts runtime deliveries; existing unavailable/expired/cache-cleaned state drives cleanup retry. |
| A05 | Preparation revisions follow source, binding, timeline, version and output dependencies, rather than every catalog event. | SQLite dependency regression pending; no performance benchmark claim. |
| A07 | Organization and logical schedule invalidations carry previous scope. Core requires the resource's actual permission and emits anonymous resets when a resource leaves visible scope. | 1,152-case policy matrix plus SQLite/SSE deletion/move/reconnect regression pending. Historical unknown scope is conservative; no restore-epoch rotation. |
| C01 | Match acceptance and reconciliation authorize all affected works, relationship endpoints and order-group owners within the writer transaction. | Source/target/relationship/order-owner denial fixture pending. |
| C08/C09/C10 | Scanner enumeration/retained paths and native watcher ingress are bounded. Lost events request catch-up; unsupported path names leave truthful incomplete coverage. | Inventory and callback boundary tests pending; no NAS performance qualification. |
| C11 | GET artwork selection exposes every role, effective asset, pin revision, provenance and conflicts. Contribution mutations advance the item ETag, including ownership moves. | Contract parity passed; role and contribution-only revision HTTP tests pending. |
| P01 | Playback planning uses one read connection for component and preference queries. | Three-of-four-pool-slots-held planning regression pending. |
| P02 | Ambiguous track names no longer imply correspondence across files/versions. Ambiguous IDs hash canonical tuple identities. | Production-core cases pending. Existing unambiguous `cmp:` IDs remain stable; their pre-existing mismatch with generic contract `Id` remains open. |
| P04 | Subtitle locks and capacity follow real blocking work through cancelled waiters. | Barrier cancellation fixture pending; actual converter lifetime stays independently owned. |
| U02 | Next-title navigation preserves the selected profile. | Functional player bridge regression passed. |
| U03 | Opening a saved title retires the prior snapshot first; exact-ticket close, page unload and startup failure release snapshots. Admitted bodies retain their file handle. | Host HTTP snapshot tests pending; native low-disk/reconnect qualification remains. U04 journal exhaustion is still open. |
| U05/U07 | Library editing preserves music/photos kinds; search retains its submitted text. | UI crate compile and existing rendering tests pending. |
| U06 | Active download inventory filters before paging; clients stop safely on cyclic/truncated inventories without deleting saved copies. Cancelled rows remain for causal offline history. | 16 desktop download tests passed; 5,001-cancelled-row HTTP regression pending. |
| U08 | Human CLI list output exposes continuation cursors. | Kyoto Go compile and terminal CLI suite passed. |
| K09 | Every HLS master uses codec metadata read from the emitted initialization segment. | Actual FFmpeg conversion/manifest regression pending; high-frame-rate hardware/device qualification remains. |
| D02 | README identifies the production bridge and retained legacy fixture correctly. | Source-path inspection. |

Migrations 0050–0053 leave 0045–0049 available for concurrent completion/access
work. Reconcile numbering again before integration; do not renumber applied
migrations. No original media, live catalog, running service or original worktree
change has been modified.

### Core and adapter boundary

Production core owns ambiguous correspondence, reader/cleanup admission and
resource disclosure. Existing core curation and revoked-device authentication
rules consume complete adapter facts. SQLite transactions, restore publication,
filesystem/subprocess lifetime, scanner/watch ingress and SQL projection revisions
remain adapters. Real adapter checks are required alongside the pure policy
checks; no new global state machine is introduced.

Independent review found and the implementation addressed contribution-only
artwork ETags and artifact leases omitted from late subtitle/source/keyframe
probe paths. The final GPT-6.1 Sol review of all six implementation commits through `5d12744`
found no additional concrete defect and verified the previously requested repairs.
Review is static evidence, not compilation or runtime qualification.

### Completed verification so far

- `scripts/test_backup.py`: one passing test.
- Player/offline bridge and desktop downloads Node tests: 41 passed.
- OpenAPI validator: 104 paths, 157 operations, 164 schemas, 2,772 references.
- All 47 migration files applied to isolated SQLite with foreign keys enabled;
  `foreign_key_check` returned no rows.
- Kyoto compiled the terminal CLI test binary; executing that binary passed.
- Rust Kyoto compilation found a lockfile dependency disambiguation and a missing
  import, both corrected. A compile concurrent with a core edit also observed
  stale core declarations. A consolidated compile of the stable source is queued;
  no Rust-test success is claimed yet.

### Kyoto verification handoff

Shared queue owner: `2d141a38-74f5-48f3-a8c6-6f82288224f1`.
Dedicated target resource: this worktree's `target` directory.

- `e34ff990bb82449f9681c41be39b9d99`: consolidated Rust compile, queued against
  implementation source `5d12744`; pressure deferrals may change its start time.
- `0768ee27f6f84737ba9397686d311ebf`: Go CLI test compilation succeeded; compiled
  CLI suite executed separately and passed.
- Earlier Rust attempts found the corrected lock/import errors. Superseded host,
  artifact-model and access-only submissions were cancelled to avoid duplicate
  compilation; the consolidated job includes all their targets.

The queued command is:

```sh
cargo test --locked --jobs 1 --workspace --lib \
  --test catalog_persistence --test v2_metadata_processing \
  --test v2_playback --test v2_downloads --test cache --test contract \
  --test access_api --test artifact_leases_model --no-run \
  --target-dir <worktree>/target
```

Once compilation succeeds, execute the produced binaries separately. Run the
catalog persistence, metadata, download, host cache and contract targets; core
component/disclosure tests and the artifact ownership graph; root inventory,
watch ingress, maintenance and cancelled-I/O tests; and the focused playback
planning, prepared-GC, conversion-codec and restore tests. Run the access/SSE
ownership regression. Record actual counts, model bounds and any failures before
changing pending status. A compile-only success is not runtime-test success.

### Remaining fourteen findings

These are open implementation work, not accepted deferrals. The owner of each is
the rewrite-fixes implementation lane. The next step and dependencies are:

| Finding | Dependency and concrete next step |
|---|---|
| A03 | Persist durable schedule device identity/generation/mode independently of the creating login. Carry it into metadata fanout and publication reauthorization; backfill from the original credential generation. Coordinate with the access branch's authority schema. |
| A04 | Add bounded v2 expiry/history pruning, retaining canonical viewing sessions and sequence high-water marks. Define replay horizon at least as long as seven-day idempotency; delete credential leaves without cascading unbounded children or losing schedule/job authority witnesses. |
| A06 | Add shared versioned cursor context binding endpoint, parent/filter/sort, principal policy and server/restore identity. Preserve ordering keys, permit page-size changes, and allow bounded 500-character Unicode titles. Adapt every list caller together. |
| C02 | Add scoped parent/number creation/replacement. Represent order-group ownership without fake series editions, and separate order membership from the legacy single timeline position. Update ordering, merge/split, read and play-next consumers together; coordinate with assigned UI work. |
| C03 | Share self/child/grandchild read membership across browse, metadata, search, organization and events. Filter returned memberships; require all descendant memberships for curation. Keep edition/timeline ownership direct. |
| C04 | After C03, use connection-taking scoped FTS search in the HTTP read snapshot. Return ranked pagination and index staleness; bind cursor to normalized query through A06. Test actor/tag/alias/Unicode terms through the real router. |
| C05 | Extract transaction-taking matching correction helpers. Expose revisioned reopen/unmatch/manual identification with retained merge receipt linkage. A reopen cannot undo a merge; offer an explicit reviewed split before correcting merged identity. |
| C06 | Scope candidate evidence before the cap and derive visible pending/review status in core. Explicit proposal refresh advances revision; GET remains read-only. Add indexed normalized-title facts with dirty/readiness handling. |
| C07 | Make smart collection directory summaries independent of full evaluation. Add member pages bounded by both candidates scanned and matches returned, allowing empty nonterminal pages. Bind collection revision and scope in cursors; coordinate existing organization UI owner. |
| P03 | Factor per-original planning into structured candidate outcomes and core-owned fallback. Preserve exact pins/components/failed identities and probe connection ownership while considering later authorized originals. |
| P05 | Retain descriptors for every part and observe fingerprints immediately before live publication. Core must verify exact generation/source evidence and fence changed/unknown sources. Dedicated check permits must survive timeouts; ctime makes some renames conservatively invalidating. |
| U01 | Carry cursors/page metadata through both UI facade implementations, list routes and rendered navigation. Preserve profile/filter context and distinguish page size from total count; cover library/profile directories. Coordinate A06 and the existing UI owner. |
| U04 | Add atomic finalization of exact pending event plus terminal checkpoint using a small durable journal reserve. Core/host enforce recovery-required state; bridge preserves IDs across retries and closes only after durable acknowledgement. Update all log readers/compaction, then qualify actual full-journal native reconnect. |
| K19 | Add local lock and intentional-null intent to metadata core/provenance/API replacement. Locks capture a value, absent withdraws, null deliberately blanks. Preserve intent in normal UI saves; choose and document a non-null displayed-title policy. |

Full-suite, browser/native/device, hardware, packaging and release checks have not
been run for this batch. Queue submissions do not count as completed verification.
