# Completion, access and rewrite integration

PR #66 reconciles the completion chain (#35, #37, #39–#59, #61–#65),
administrator access/sign-in (#36/#38), and rewrite repairs (#60). #34's plan
handoff merged separately. Catabolic is excluded. Original commit ancestry is
retained; the integration is validated together before landing on main.

## Repair ownership and independent review

- [Access repairs](access-fixes.md): 37bcbeb, five administrator access findings.
- [Completion repairs](completion-fixes.md): checkpoints 9e6e42f, a4cd66a and 505ee18; detailed dispositions distinguish fixed descendants from new fixes.
- [Library/marker repairs](completion-ui-fixes.md): 35ddbff, scoped descendant
  choices, durable manual correction order, chapter selection and probe bounds.
- [Preview repairs](preview-fixes.md): 064833b/0196469, shared core admission,
  durable layout identity, cancellation/publication/retention, exact UI source
  matching, CSP and focus behavior.
- [Rewrite repair history](rewrite-fixes.md): existing #60 evidence retained
  separately from the completion ledger.

GPT-6.1 Sol independently reviewed access, preview, root migration/restore and
issued-history changes at e03d845; no blocker was identified in that source
review. It then reviewed 35ddbff and integration 81710ef and found the scanner
was not requesting rotation fields. 5c4d219 requests both display-matrix rotation
and the legacy rotate tag and adds a production-scanner observation regression;
Sol rechecked the repair. These reviews are source evidence, not Rust test or
browser qualification. Sol reviewed a4cd66a and 505ee18, then converter admission repair c618fe8 and
populated restore regression3717d0d. Bounded cache, transfer-status expectation
and subtitle directory durability observations were repaired before acceptance.
No source blocker remained in those reviewed commits; CI still gates merging.

## Integration correctness boundaries

Completion migration bytes remain at 0045–0052. Rewrite/access/auth SQL bytes
remain unchanged but filenames move to 0053–0059. The explicit offline copy
utility in [MIGRATIONS.md](../../MIGRATIONS.md) maps only known version/checksum
pairs; ordinary startup never rewrites an unknown migration history. No live
DB, user credential, media tree, or deployment was changed by this integration.

Restore rotates history, captures the pending viewing baseline, revokes devices
and sign-in families and clears saved authority. Identity configuration and audit
remain; reviewed_epoch stays old so sign-in needs administrator restore review.
Core reanchor now refuses a fabricated predecessor history. The SQLite adapter
supplies exact recorded issued-history evidence in the sequence transaction.
An older backup cannot prove epochs issued only after it was taken: those logs
remain retained/refused. Repeated-restore recovery across such missing history
requires separate proof design and is not claimed complete.

Artifact leases from #60 are combined with completion's transactional download
pins. Playback regression coverage retains codec-byte comparison, restored-token
refusal and reader ownership after delivery retirement. Preview creation uses
access/auth's Caller methods while retaining the exact layout identity pin.

## Verification ledger

- Integrated client checkpoint 81710ef: 159 Node tests passed, three real-cache
  cases skipped without a built host binary. CI client job also passed, including
  Go terminal tests.
- Integrated API contract structure/JSON parity and dependency notices passed CI
  at 81710ef.
- Development migration copy and restore: eight Python tests passed. Coverage
  includes canonical completion, access, auth and rewrite schema lineages,
  missing canonical migrations, integrity/FK checks, checksums, collision refusal,
  unknown/dirty refusal, source preservation and authority invalidation. Lineage
  fixtures are empty-schema replay; populated ownership/auth migration runtime
  coverage must not be inferred from them.
- Rust integration compilation/test execution and final CI remain pending. Local
  Rust builds are submitted only through Kyoto; the host is pressure-deferred.
- Historical browser/a11y receipts remain historical. No fresh integrated
  browser/Electron/TUI walkthrough or installed-package qualification is claimed.

## Remaining plan work

The fourteen rewrite items A03, A04, A06, C02–C07, P03, P05, U01, U04 and K19
remain open implementation work, with concrete dependencies in the rewrite
ledger. They are not silently converted into accepted release deferrals by
merging these repair batches. Existing playback/release ledgers retain HDR,
live text subtitle, adaptive bitrate and browser/native qualification boundaries.
