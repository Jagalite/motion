# Motion completion ledger

Single progress ledger for plan section 12
([MOTION_CONSOLIDATION_AND_COMPLETION_PLAN.md](../../MOTION_CONSOLIDATION_AND_COMPLETION_PLAN.md))
and [the single-agent handoff](../../MOTION_SINGLE_AGENT_COMPLETION_HANDOFF.md).
Baseline: main `28f4b32` (2026-10-12). CI on that commit: CI, CI full and
dependency notices passed. No open PRs at the start.

States, kept distinct:

- **Implemented:** code exists on a branch with focused tests.
- **Tested:** the named focused checks passed at the recorded SHA.
- **Merged:** on `main`.
- **Release-qualified:** the exact release artifacts passed the release matrix
  (section 8 of the handoff). Nothing in this ledger is release-qualified yet.

Missing code is "not implemented", never a test deferral. Every unfinished item
names an owner, a dependency and the next concrete step. The owner of every
item below is the single completion agent unless stated otherwise.

## Summary

| Item | State | PR | Next step |
|---|---|---|---|
| 0 Plan reconciliation | Implemented (docs), reviewed | docs PR | Merge by user |
| 1 Download correctness and retention | Implemented, tested | downloads PR | Sol review |
| 2 Library query scaling | Not started | — | Scan history ordering |
| 3 Production playback and preparation | Not started | — | Filter and color-fact observation |
| 4 Library-management UI | Not started | — | After item 2 |
| 5 Offline breadth | Not started | — | After item 1 |
| 6 M5 convenience and receiver | Not started | — | Marker/preview inventory |
| 7 M6 and parity backlog | Not started | — | After items 1–6 |
| 8 Release qualification | Not started | — | After feature implementation |

## 0. Plan reconciliation

- **Source SHA:** branch `plan-completion/docs` from `28f4b32`.
- **Behavior:** documentation only. Plan section 11 (the 2026-10-11 N1–N3
  assignment, previously only in a local uncommitted edit) is committed with
  a dated status banner marking it superseded, and machine paths replaced by
  neutral names. Section 12 adds corrected status claims, the milestone and
  parity matrix, and points here. Lane ledgers (M4, N2, N3) gain dated status
  notes; their historical receipts are unchanged.
- **Corrected claims:** FFmpeg tools contain zimg/libass but production HDR
  and styled/text burn-in are not implemented; live HLS WebVTT text and the
  bounded two-rung ABR are implemented; 152/156 operations served.
- **Focused evidence:** `python3 scripts/check_publication.py`-equivalent
  source scan of changed files (no machine paths or private hosts); link check
  of changed relative links. No application build is needed.
- **Review:** GPT-6.1 Sol (high effort, read-only) reviewed `8d19d28` against
  `origin/main`: no actionable findings. It independently reran the privacy
  scan, the relative-link check and the 152/156 served-operation count.
- **Release checks:** none.

## 1. Download correctness and bounded storage

- **Source SHA:** branch `plan-completion/downloads`, stacked on the docs PR
  (it shares this ledger). Commit recorded in the PR.
- **Implemented:**
  - **S30.** On a 403 from the download list the agent reconciles every owned
    root first and independently (`reconcileOwned`), then withdraws. Sync
    needs only `viewing:write` on the server; without it the server refuses
    and the events stay. Restore-epoch fencing and identity-based compaction
    are unchanged.
  - **S31.** `withdraw()` runs `cache collect` on each owned root after removing
    its copies. Nothing on a withdrawn root is wanted, so an unreferenced blob
    can only be a crash leftover. Collection is per root under the cache lock;
    another server's or principal's roots are never touched.
  - **Retention protocol.** Each sync may carry `acknowledged_through`, the
    device's own compaction point (its log base). Core `downloads::retain`
    moves a per-device-and-profile floor to it: never backward, never past
    `next_sequence - 1` (else 422 `acknowledgement_ahead`). The adapter deletes
    records through the floor in the same `BEGIN IMMEDIATE` transaction,
    before deciding the batch, and stores the floor (migration 0045,
    `offline_sequences.pruned_through`). Core `downloads::reconcile` then
    decides each event under the floor:
    - a recorded event above it is still an exact duplicate or a conflict;
    - an unrecorded sequence at or below it is a stale copy, refused as
      `sequence_acknowledged` (not consumed);
    - a new event while `MAX_RETAINED_EVENTS` (10,000) consumed events remain
      unacknowledged is refused as `acknowledgement_required` (not consumed).

    Both new refusals are non-consuming in `offline::consumed`, so the device
    never compacts them, and the agent stops reconciling that root until the next pass. Sequence
    identity (`next_sequence`) and causal lineage (`offline_lineage`) are never
    pruned. A compliant agent keeps at most one batch (200) of records on the
    server between syncs; older agents keep up to 10,000.
- **Contract:** `OfflineSyncInput.acknowledged_through` (optional, nullable);
  `OfflineEventResult` documents the two new non-consuming codes. Additive;
  older agents keep working without pruning.
- **Unsupported / limits:** event IDs are deduplicated only while their record
  is retained. A device that reused a pruned event ID with a new sequence
  would not be caught as a conflict; the agent generates fresh IDs.
  Offline `history_only`/`rejected` records are pruned with the rest. No API
  reads them, and their effect on viewing state is already applied.
- **Core/adapter boundary:** retention floor, stale-copy refusal and the bound
  are core decisions (`retain`, `reconcile`, `offline::consumed`). The adapter
  observes `(next_sequence, pruned_through)`, deletes and records the floor in
  the request transaction. The agent only forwards the cache tool's base.
- **Focused evidence (2026-10-12, darwin-arm64, Rust 1.98, Kyoto compile then
  direct execution):** RESULTS_PLACEHOLDER
- **Review:** pending (GPT-6.1 Sol).
- **Release checks:** the N3 deferred rows (installed package, large or
  interrupted transfers, physical full disk, Electron/Demuxe on an
  agent-published root) remain in section 8 of the handoff.

## 2. Library scalability and query support

Not started. Inputs: N1 ledger section 2 "Lists are bounded"
([m3-workflows.md](../next-lanes/m3-workflows.md)). The broader 10,000-item /
100,000-file performance qualification is a release item (section 8).

## 3. Production playback and preparation

Not started. Inputs: N2 handoff section 6
([native-media-tools.md](../next-lanes/native-media-tools.md)) and the M4
deferral table ([m4-expansion/README.md](../m4-expansion/README.md)).
Copy, hardware and ABR defaults stay unchanged.

## 4. Library-management UI workflows

Not started. Online provider matching and provider artwork remain the approved
deferral.

## 5. Offline experience breadth

Not started. Depends on item 1.

## 6. M5 convenience and receiver work

Not started. Existing primitives to inventory first: `crates/core/src/markers.rs`,
`src/markers.rs`, migration 0030. Physical receiver qualification needs
hardware; if unavailable the exact blocked check will be named here.

## 7. M6 and wider parity backlog

Not started. Optional external/commercial/social services are out of scope.

## 8. Release qualification

Not started. Runs after feature implementation, against frozen identities.
