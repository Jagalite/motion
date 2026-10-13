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
| 0 Plan reconciliation | Implemented (docs) | pending | Sol review; merge by user |
| 1 Download correctness and retention | Not started | — | S30 agent change |
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
- **Review:** pending (GPT-6.1 Sol).
- **Release checks:** none.

## 1. Download correctness and bounded storage

Not started. Inputs: N3 ledger S30, S31 and the retention deferral
([downloads-offline.md](../next-lanes/downloads-offline.md)).

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
