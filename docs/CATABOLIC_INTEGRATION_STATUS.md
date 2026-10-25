# Native integration qualification ledger

Historical status recorded2026-10-20 after source snapshot28/native34 and selected27p6 feedback.
For the current Motion44/native50 preservation boundary and qualified40/46 baseline,
see [remaining product acceptance](CATABOLIC_REMAINING_PRODUCT_ACCEPTANCE.md). This is not a completion
claim. No live library migration, deployment, or repository publication has been
performed. The source is reconstructed from the published branch; unpublished
pre-reset source was not recovered.

## Current evidence and limits

Tests below ran against immutable paired snapshots, not an assumed identical
working tree. Historical failures and their original context remain in
[CATABOLIC_QUALIFICATION_HISTORY.md](CATABOLIC_QUALIFICATION_HISTORY.md).

| Operation | Qualified evidence | Remaining work |
|---|---|---|
| Default native startup | Actual packaged process creates, reads, registers sources, restarts; missing native configuration cannot silently resume legacy writers | Wider fault injection and platform coverage |
| Existing-library cutover | Real 1100-work bounded partition activation, replay and complete-set restart; interrupted-prefix recovery | Full supported-library parity and kill injection at every host/native boundary |
| Work create/update/read | Actual HTTP create/read/browse/update/replay, imported revision bridge, cancellation and revocation custody | Rich dependency-aware ETags and broader interactive mutations |
| Native source/discovery | Snapshot13p3 all six queue tests passed, including real scan/publication, removed-file GET/browse, cancellation and lost acknowledgment | Selected17p7 actual-process scan/download/cancel/restart passed; broader discovery/media cohort parity remains incomplete |
| Library and source identity | Snapshot12 source fidelity/restart and snapshot13 independently frozen-proof reconstruction tests passed | Full scope cardinality parity, richer membership contracts and physical scoped access |
| Scoped catalog reads | Snapshot13 application tests cover allowed library, wrong-library denial, rating/label denial | Existing 200-library policy limit must remain supported; physical routes currently require the narrower qualified scope |
| Viewing/history | Public viewing authorization and HEAD, exact historical IDs/revisions/epochs, repeated-read stability; native revision2 edit fixture passed | Edited-head HTTP playback/session behavior beyond the current conservative admission boundary |
| Original downloads/content | Snapshot13 public download and byte-range tests passed, including ownership, pins, restart, ctime/source replacement refusal and missing-file expiration | Selected17p7 actual-process scan-to-download passed; broader delivery modes and physical library-scoped access remain pending |
| Playback sessions | Snapshot17p6 actual original plan/delivery/session/ticket/range and media-only native manifest passed; shorter-slice projection, old-pin expiry and production-core clamp passed | Edited-head and generated delivery remain pending; public playback candidates require actual outer-router qualification |
| Tags, aliases and graph | Selected21 actual relationship HTTP create/read/delete, aggregate CAS, cycle refusal, replay and restart passed; bounded metadata HTTP passed | Complete-membership curation correction and native40 metadata expansion need Motion runtime qualification; edition/timeline/version/order, tag identity, artwork and matching parity remain incomplete |
| External processing | Selected20p1 actual Motion renderer worker → exact native generated receipt → projection/ACK → restart passed; native atomic promotion owner passed | Selected23p3 actual normal-server consumer/public preparation/generated-ready/restart passed. The pre-effect recovery and retirement fault matrix, generated delivery and wider edited-head playback remain pending. Disabled native encoder/broker prototypes are not prerequisites or publication authority |
| Migration upgrades | Schema84 retired-locator seven regressions and actual sealed upgrade/backup test passed; exact compiled transitions through90 are implemented as candidates | Broader interrupted upgrade/recovery matrix; no generic future-schema permission |

### Latest validation checkpoints

- Snapshot29/native35 is preserved as a source checkpoint after an environment
  restart. Native34 selected-current output tests3 and actual retirement denial1
  passed; pure NFO35 passed. One historical-receipt fixture omitted the official
  activation-hold release before expecting expiration; its correction is separate
  from production receipt semantics. Post29 selected projection/read/ACK/detail
  consumers, All-only generated direct-byte transport and current continuation
  authorization are source candidates with real output/offline-original/root
  replacement assertions, not yet qualified. Library PUT/detach similarly uses
  typed metadata plus exact scope acknowledgment and active HTTP/recovery tests.
  No schema41 has been registered.

- Native33p1 focused results: shared artwork core12, empty-edition owner4,
  metadata/artwork commands44, and real relationship-lineage1 passed. The
  logical-retirement lineage fixture initially attempted to change existing
  immutable item kinds; after its initialization-only correction, all14 logical
  command tests passed in33p2, including actual retirement lineage. The Motion27
  empty-edition handler create/replay/receipt-only recovery fixture also passed.
  Public artwork was subsequently qualified below; Motion90 library-only
  scoped admission remains a snapshot28 candidate.

- Motion27 application qualification: actual generated publication, injected
  ACK storage-failure recovery, and genuine retired-edition-before-ACK history/
  restart fixtures passed. This confirms the named-lineage correction reaches
  subsequent receipt-only ACK and restart, not merely projection startup.
  The separate structural-edge-detachment fixture initially used the wrong
  relationship kind before reaching deletion; after correcting that literal,
  its actual native-delete/refusal/restart test passed in27p1. All14 application
  cases have now passed across27/27p1. Library-only physical scope is a subsequent
  source-only candidate with an invoked real policy/restart/byte fixture, not
  covered by those passes. Subsequently27p6 metadata/artwork HTTP passed all10
  tests in84.44s, including upload, selection, replay/expiry, historical pin and
  verified content. Fixes retained exact source proof and added the missing
  trusted artwork field mask; the last selection assertion was corrected to
  the existing422 contract. Backend import qualification subsequently passed
  one unit and two integration tests (15.03s), retaining refusal of unknown
  receipts, foreign targets, unowned assets and mismatched content digests.

- Download-model memory qualification: the unchanged standalone exploration was
  killed again after431.39s with peak child RSS7,397,604KiB; an OOM event was not
  independently observable. A test-only lossless retained-state adapter passed
  all3 codec/prefix proofs in33.52s:10,408 exact states and39,248 edges, identical
  callbacks/results/counts, inline World512B versus packed16B plus owned bytes.
  Aggregate encoded prefix bytes were3,507,334 versus JSON16,748,765. The original
  model, transitions, inputs, checks and12M/200M/depth100 limits are unchanged.
  The full uncached packed run was deliberately interrupted after about23
  minutes to remove redundant decoding, not recorded as graph success or
  failure. A bounded two-entry exact-byte decoded-state cache is under
  qualification; mandatory successor roundtrips and every graph check remain.
  Its four proof tests passed in53.27s: the exact10,408-state/39,248-edge
  prefix took515.5ms unpacked,16.372s uncached packed and6.027s cached.
  Callback decodes fell167,401→10,389 with157,012 hits,39,249 independent
  roundtrips and maximum two cached entries. These are prefix measurements,
  not full-graph completion or a production performance claim.
  Full GraphExhausted and measured peak-RSS acceptance remain pending.

- Post28 public rendition-list candidate replaces legacy scope joins with
  current native membership and fresh authorization before pagination. A
  read-only receipt/history verifier binds exact saved admission, job options,
  output and current readiness; retired outputs remain descriptive, not byte
  grants. Preparation replay rechecks current source scope. Actual scoped,
  revocation and retired-history fixtures are source-ready, not yet run.

- Motion25p3 selected root: all162 tests passed in128.50s, including all four
  actual reserved-render recovery scenarios and held-writer processing claim.
  The unchanged changed-output case also passed its isolated rerun under the
  original tool deadline. Actual offline-upgrade process1, normal-server
  processes4 and catalog25 passed in25p2. Metadata25p3 passed all6 tests.
  Application25p3 passed12 of13: the corrected genuine split now commits, but
  restart fails when the old original typed version is requested as a live
  manifest (`catalog_identity_retired_version`). Captured27's named current
  lineage check precedes that read and is the candidate correction; its actual
  retirement/restart fixture remains unqualified. No whole-workspace green
  result is implied.

- Motion25 selected root run:160 passed and2 failed. Actual pre-effect reserved
  output recovery completed without rerender, and changed-input recovery refused
  correctly. Changed-output setup hit the unchanged tool capability-probe deadline
  before reservation; requester revocation hit the last-administrator policy
  before its intended assertion. Both require corrected/isolated qualification;
  this is not a full-root pass.
- Post26 source-only Motion90 adds default-deny disposable native physical-library
  membership proofs and consumers. Physical scope admission is still All-only;
  no library-scoped, rating-filtered or label-filtered physical access is qualified.

- Native31 focused qualification passed: pure NFO model18, qualified upgrade
  inventory2, metadata commands30, migrations20, and native upgrade positive
  preservation/exclusive-custody2. The custody fixture opens its idle competitor
  before the exclusive owner, then verifies DatabaseBusy during the ACK callback
  and successful write admission afterward. Motion25 backend schema89 companion
  tests all four passed: normal operator/ACK admission, native-commit-before-ACK
  recovery, post-commit mutation refusal, and missing-guard refusal without repair.
  Subsequent actual CLI and private pre-effect recovery passes are recorded above.

- Selected23p3: all four actual normal-server process tests passed in 120.78s,
  including scan → original plan → preparation → generated-ready → restart and
  cancellation. The initial scan-claim contention fix also passed its unit test
  and a real held-writer queue regression. Only failed pre-effect BEGIN contention
  is treated as idle; post-begin errors are not swallowed.
- Selected23p2: eleven of twelve application fixtures passed, including real
  rendering and receipt-only recovery after an injected Motion ACK storage abort.
  The genuine retired-edition-before-ACK fixture initially failed logical preview
  because its replacements did not partition the old bindings. After correcting
  that fixture without changing the owner,25p3 reaches the restart defect recorded
  above. No complete retirement/restart pass or full application-suite success is claimed.

- Selected snapshot21: root library 147 passed, native application nine passed
  (including actual outer-router profile/device operations), catalog 25 passed,
  and bounded metadata two passed. Catalog/metadata initial failures were short
  fixture idempotency keys, corrected without production rule changes. The
  later complete-membership curation correction is not covered by these passes.
- Native29p3: metadata command 27 passed, wide v2 typed import passed, exact
  schema40 preservation adversarial test passed, and all 20 native migration
  tests passed. The v2 core schema-evolution 21 tests and actual process-exit
  native40 activation/host-ACK restart test also passed. These are native-owner
  results. Motion23 workspace/all-target check passed in 4m12s, followed by
  root library 147, catalog 25 and native metadata HTTP 3 passes, including schema40
  field/tag expansion and ordinary reads. These do not qualify every import or
  explicit bound-catalog upgrade path.
- Post23 source, not yet qualified: production reservation/dispatch phase split
  and independently reauthorized exact saved-intent recovery, with real
  pre-effect shutdown/changed-output/revocation fixture candidates. Recovery
  never rerenders, invents a replacement command, or infers no effect from a
  missing receipt. Native positive39→40 reconciliation proof is implemented
  with exclusive locks and complete old-data/seed-domain validation; the Motion
  append-only schema89 upgrade companion and explicit offline CLI are now
  implemented in snapshot25; their native proof, backend and actual process
  tests are pending, not qualified.

- Snapshot13p3: all six scan queue tests passed in 73.37 seconds, including the
  actual active cancellation test and disappearance/recovery paths. The earlier
  late-read failure was a nullable unmeasured timeline field, corrected without
  weakening native authority.
- Snapshot13: six application fixtures passed in 49.78 seconds; all-target check
  passed. Focused backend run passed 26 tests, with one intentional child helper
  ignored and the unchanged long interrupted-prefix case excluded because it had
  passed in snapshot12. Do not add these counts into a unique-test total.
- Snapshot12: five application fixtures passed, including the real typed native
  revision2 edit and exact imported source descriptor fidelity. All-target check
  passed. Historical exporter failures and their narrow corrections are recorded
  in the historical ledger; do not infer a newer full exporter pass from code.
- Snapshot14: all-target check and full-workspace preflight build passed with the
  opt-in `native-qualification` profile. The actual observer was 108,158,616 bytes,
  below the unchanged 128 MiB bound, without manual stripping. The unfiltered
  suite finished and is not green: two bounded import cases hit native
  operation budgets; other failures include missing test Ruby, obsolete fixture
  schema expectations/default-deny setup, FFmpeg AAC fixture timestamps, restart
  deferred-observation reconciliation, and original-delivery fixture setup.
  The terminal log has 83 result reports totaling 985 passing, 15 failing and
  one ignored test, plus a downloads-model process killed by SIGKILL (cause
  unconfirmed) and two doctest targets failing to locate sqlx. Concurrent newer
  builds in the shared target may explain the doctest metadata failure but that
  is not yet proven. Future full runs must own an exclusive target through
  doctests. These counts are reports, not unique test cases or a success claim.
  Corrections are captured or in progress, not counted as passes until rerun.
  The seven other native application fixtures passed, including shorter-slice
  history/pin behavior. Isolated reruns must distinguish load-sensitive failures
  from production defects without increasing native budgets.
- Selected snapshot17p6: all eight native application fixtures passed in
  82.97 seconds, including actual original plan/delivery/session/ticket/range,
  bounded media-only manifest and immutable preparation capture. The subsequent
  renderer/publication roundtrip passed in selected snapshot20p1, recorded below.
- Selected snapshot17p5: all 133 root-library tests passed, including native
  aggregate/custody and private generated-file materialization. All six scan queue
  tests passed in 110.82 seconds. Initial shared failures were exact schema87/88
  inventory omissions and directory-fsync descriptor mode, corrected without
  inventory exemptions or weakened durability. Process/application targets are
  still being qualified; these are not a full-workspace pass.
- Selected snapshot17p7: all three actual-process fixtures passed in 73.3
  seconds, including public scan, original download, cancellation and restart.
  The offline-positive fixture uses actual AAC/M4A; unsupported WAV offline
  admission remains refused, while native WAV scan coverage remains intact.
- Isolated snapshot14 interrupted-prefix 1100-work recovery passed in 167.99
  seconds with unchanged native operation budgets and no competing build/model
  load. This supports a load-sensitive explanation for that earlier failure,
  not a full-suite pass. The separate complete 1100-work activation also passed
  in 127.41 seconds and the legacy rendition timeout case passed in 2.42 seconds
  when isolated, unchanged. These three reruns did not increase any budget.
- Selected snapshot20p1: all 139 root-library tests passed in 40.64 seconds
  and all eight native application fixtures passed in 121.70 seconds. This
  includes the actual production renderer worker, native generated receipt,
  projection, Motion acknowledgment and restart without duplicate publication.
  Normal-server processing consumption/public preparation, edited-head delivery
  and the combined storage-failed ACK/retirement/restart edge remain pending.
  The selected build passed all-target checking; this is not a full-suite pass.
- Focused snapshot20 follow-up: core publication reservation/receipt tests (3),
  backend generated provenance/history/private descriptor tests (3), and the
  retired-edition descriptive timeline basis regression (1) passed. These do
  not substitute for the combined real failed-ACK/retirement/restart fixture.
- Snapshot21/native27 preserves the next relationship aggregate/CAS, bounded
  metadata and exact profile/device outer-router candidates, plus admitted
  logical preview. These candidates await qualification. Metadata schema-v2
  evolution has not been qualified and is not implied by the bounded cohort.
- Native23p1: generated-output owner tests passed, including explicit atomic
  provisional-timeline establishment and original repinning; relationship tests
  and scoped historical restart reconciliation tests passed. These do not prove
  the Motion renderer-to-publication adapter.
- Snapshot18/native24 preserves core publication reservation, exact receipt/ACK
  journals, input capture and startup receipt-only reconciliation. Post18 live
  dispatch and immutable stale-attempt integration subsequently passed
  snapshot19 all-target checking and the selected20p1 actual renderer fixture.
  Full cancellation/fault injection and normal-server integration remain in
  progress; public processing stays closed until those paths are qualified.
- Snapshot17/native23 preserves schema87 publication/admission journals, schema88
  aggregate issuance, native generated-output owner, replay-ordering fixes and
  focused fixture/restart corrections. Capture alone does not qualify them.
- The official Debian Ruby runtime and standard YAML/JSON modules were extracted
  into a private test-tools directory, without global installation. The unchanged
  reviewed contract check passed with that runtime; no YAML golden was changed.
- Native processing prototype checks passed for restricted-process policy,
  actual kernel adversaries and real installed FFmpeg audio/video/probe/cancel.
  Fixed-FD broker unit and real-tool tests also passed. These are disabled
  prototypes, not execution cleanup or publication authority.

Use [CATABOLIC_TESTING.md](CATABOLIC_TESTING.md) for the reproducible full workspace
workflow. It builds the same workspace test feature union before verifying the
actual emitted bounded observer binary, without changing ordinary development
profiles or increasing the observer size limit. Ignored and platform-dependent
cases must be reported separately. No current full-workspace success is claimed.

## Authority boundaries that remain mandatory

- Catabolic is the catalog owner. Legacy SQL catalog writers do not become a
  fallback when configuration, authorization, proof or a supported operation is
  missing. Authentication, viewing, downloads and delivery remain Motion-owned.
- Cutover retains the exact original drain witness, immutable partition manifest,
  durable activation intents and all exact receipts. A successfully imported
  prefix cannot serve. Restart reconciles recorded intents and receipts; receipt
  absence does not authorize fresh dispatch.
- Compatibility projections are disposable caches. A private, never-pooled
  connection has a narrowly scoped capability; ordinary SQL connections default
  to denial. There is no SQL setter or persistent enable flag. Motion writer
  custody precedes native owner custody and is retained through actual effects.
- Native identity/incarnation/generation, current typed manifests and complete
  source fingerprints including ctime determine readiness. Missing inventory is
  not deletion proof. Retired locators preserve original IDs and paths rather than
  inventing paths or reassigning viewing history.
- Source descriptor insertion has a separate exact ten-field, insert-only
  capability. Existing imported descriptors are checked against the independently
  frozen source proof, not only root/path. Descriptors grant neither membership,
  permissions nor scan authority.
- Schema upgrades allow only exact compiled, contiguous transitions with verified
  original guards, rollback behavior and an isolated ledger capability. Unknown
  schema, missing guards or corrupt foreign keys refuse admission.
- Aggregate work revision issuance and complete native dependency capture remain
  inactive. Item-head CAS alone cannot certify a rich child-dependent response.
  Principal-filtered output or unrelated global generation must not become that
  response's semantic dependency digest.
- Motion-owned rendering never fabricates a native execution lifetime or cleanup
  witness. Its existing supervisor guarantees and limitations remain explicit.
  Native generated publication must independently verify exact preparation,
  current source/timeline/version owners, validated output bytes and retained
  read-only descriptor, then atomically record typed identity and a receipt.
  Uncertain effects retain outputs and reconcile the original durable intent.
- Restricted thread-group retirement is not global effect closure. The disabled
  broker is optional hardening, not a replacement for the established application
  ownership split. There is no schema40 external cleanup witness or cross-platform
  native processing completion claim.

## Bounded migration operator flow

With the Motion source stopped and backed up, use the operator binary with the
same explicit `--catalog` and `--motion-db` paths:

1. `freeze-motion-source --import-id <root> --expected-revision <current>`
2. `capture-motion-export-plan --root-import-id <root>`
3. `prepare-motion-partition-cutover --root-import-id <root>`
4. `activate-motion-partition-cutover`

Preparation refuses unresolved mapping/reference obligations and more partitions
than restart supports. Once sealed, retry the recorded activation to resume exact
immutable partition identities and intents; do not abandon or recapture a sealed
source. Serving requires every receipt and final host acknowledgment.

Remaining route families are tracked by
`native_catalog::unsupported_serving_routes()`. Passing a supported subset,
compilation or a restart test does not establish the entire integration.

## Remaining existing-product parity checklist

These are completion requirements for the integration, not optional new features.
A source implementation, private handler fixture or deliberate fail-closed response
alone does not establish parity at the actual server/router boundary.

- Processing: qualify the real renderer → exact native receipt → Motion ACK →
  restart path; wire normal native-mode recovery and the processing consumer;
  qualify cancellation, pre-ACK retirement, edited-head and generated delivery,
  and public preparation routes. Keep unresolved reservations explicit.
- Catalog operations: relationship CRUD and aggregate CAS, then native-backed
  edition/timeline/version detail and editing, ordering, merge/split and
  reconciliation through existing typed native owners.
- Metadata: current native documents/contributions, tags/pins, refresh, matching
  and artwork with exact ownership, scope and provenance. Do not serve retained
  legacy documents as current native facts.
- Sources and libraries: update/delete, safe source removal/relocation, schedules
  and watches routed through native demand/observation ownership.
- Application-owned routes: restore individually qualified profiles/users,
  access setup/devices, events, administrative authentication, diagnostics and
  metrics. Organization references that depend on catalog identity need separate
  native-current validation, not a broad application-route exemption.
- Physical access: library-scoped viewing/download/byte admission, current edited
  timelines, and generated media. Retain exact current descriptor/source proofs;
  an administrator-only cohort is not full access-policy parity.
- Remaining media/application integration: previews, markers and a coherent paired
  native-plus-Motion backup/recovery contract. A Motion-only VACUUM backup cannot
  be represented as a complete native catalog backup.
- Presentation: test the actual outer v2 route gate and browser-facing metadata/
  tracks contracts. Direct handler success does not establish usable UI parity.
- Qualification and delivery: exclusive-target full workspace run including
  models and doctests, supported-platform limitations explicitly resolved or
  reported, and authorized native dependency/Motion branch publication. No live
  migration or deployment is implied by local fixtures or persisted source copies.
