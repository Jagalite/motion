# Private native HLS candidate (not qualified)

The public native HLS planner gate, HTTP allowlist and advertised capability
remain closed. Root's source67p1 qualification passed all **five private HLS
application fixtures and four real terminal-owner tests**, plus the supporting
core/output/witness and three existing export/service regressions. This is finite
private candidate evidence, not public acceptance. The post68 joined-timeout
HLS test below is **uncompiled and unrun**. No build, test or encoder fixture
was run by this lane.

## Deliberately finite VOD cohort

A private acceptance entry verifies a signed whole-original or whole-prepared
byte-plan identity, then selects one software H.264/AAC, SDR rendition. It
requires Linux, an O_TMPFILE-capable scratch filesystem, a currently observed
source of at most **60 seconds and 64 MiB**, and an actual `ftyp` header read
from the genuine descriptor. This is the MP4-style subset of the MOV family,
not every QuickTime file. Unknown/long/large inputs are refused before dispatch.
No edited clips, multipart, subtitles, copy/remux, hardware, tone map, adaptive
ladder or bitrate variants are admitted. MOV external data references are
explicitly disabled. No plan/catalog pathname becomes filesystem authority.

This cohort is **completion-before-ready VOD**, not progressive live HLS and
not movie-scale support. FFmpeg's ordinary non-VOD file playlists force a
pathname rename; the private recipe instead explicitly uses `playlist_type=vod`
and `single_file+independent_segments` with inherited output descriptors. The
existing supervisor/reducer runs unchanged lifecycle, cancellation and pacing
control. Native publication waits for successful supervisor exit **and actual
witness/descendant confirmation**, then checks one final manifest and all ranges.
It never treats a size-limited/truncated stream as whole-file success: successful
exit, ENDLIST and duration coverage within 250 ms are all required. Existing
startup/no-progress deadlines may refuse expensive inputs even within the size
and duration cohort; there is no throughput guarantee.

## Byte authority, publication and drain

* A current native occurrence/binding/policy opens the source through the native
  guarded root and exact observation, with native ctime joined to that same
  observation tuple. Projection mappings only select native identities.
* Each generation reopens through that retained capability into an independent
  open-file description. It never duplicates another encoder's seek offset.
  Actual source currentness is checked after preparation, before child dispatch,
  before publication, after output reads and at final request admission.
* Prepared playback checks the generated output and current receipt/version.
  All-scoped prepared authority acquires no new dependency on its original
  source. Actual prepared HLS independence still needs its acceptance fixture.
* Ordinary UUID validation requiring an unowned child fails closed. There is no
  enrolled-volume support or schema41-admission claim, and the separate native
  structural 64 KiB/2 MiB budgets are not whole-media scalability evidence.
* Preparation creates anonymous media and manifest files, retained by the
  generation and inherited as fd5/fd6 by the real supervisor/encoder. Readers
  use positioned I/O from those exact descriptors. No output directory/leaf,
  projected path or manifest URI is ever adopted as byte authority, even at
  first publication. Manifest URIs must name the known inherited fd, but are
  never opened or exposed to clients.
* The native execution witness is created exclusively and anchored by reopening
  its actual owned Linux descriptor into an independent lock probe. Lease
  registration clones that probe rather than looking up `.owner` again after
  asynchronous preparation. Directory/leaf substitution cannot redirect drain
  evidence to a different inode. Legacy witness behavior is unchanged.
* Final manifest ranges must be explicit, contiguous, bounded and zero-based.
  The actual init identifies the video track; each fragment is structurally
  bounded and begins with an IDR. Byte-range identity plus SHA-256 is committed
  before reducer publication. Reads compare the commitment and buffer immutable
  bytes. Replaced directory/leaf names have no authority; same-FD mutation fails
  the digest check.
* Native observation bypasses all legacy path-based metadata, playlist and IDR
  reads. Actual blocking owners retain Session/native authority, output
  descriptors, execution lease and their I/O slot across timeout/cancellation.
  Cleanup and stderr owners also retain native Session custody. The actual
  exit-confirmation callback owns final inspection, including late reaping.
* Final HTTP admission rechecks the **actual request credential or ticket** and
  native source/representation under one application writer, then rechecks
  time-based authorization after native I/O. That read-only transaction ends
  before returning the response. Already-admitted bounded bodies may drain
  after subsequent revocation, while new/pending admissions fail; a slow body
  never holds the global application writer. Media and native master/variant
  playlist bytes carry their Session/native
  lease, stream slot and buffer slot in a shared `Bytes::from_owner` allocation.
  Exhausting/dropping the body producer cannot release those guards while a
  yielded/cloned/sliced frame still retains the allocation. Native Input also
  retains the actual Motion Runtime, not just the source's shared gate lease.
* Only the Motion terminal server owner revokes the native shared gate. It polls
  the actual already-admitted scan supervisor before waiting for that scan's
  lease, releases RuntimeAdmission while genuine leases drain, then reacquires
  it to close the host. New work remains refused throughout. Export admission
  alone is still nonrevoking. No global HostSession close semantics changed.
  A timeout refuses acknowledgment and keeps admission closed; existing
  NativeService Retained/quarantine custody keeps the actual App, native owner,
  server lock and I/O ownership. No new permanent owner registry is introduced.
  A dropped async waiter cannot discard the actual blocking close owner.
* Admission reauthorizes before receipt lookup. Exact retries replay without
  reopening the source; new native preparation runs only after initial expiry
  checks, with authorization/expiry checked again afterward and before commit.

Blocking I/O is not magically cancelable. A deadline is not proof of drain;
actual owners and native authorization remain retained until that I/O ends.
Independent sessions sharing a source have independent readers/cancellation,
not a shared encoder or cache. Public HLS/seek/variant promises do not follow
from this candidate.

## Explicit resource bounds

* Eight native output generations, with permits following actual descriptor
  owners. A Linux RLIMIT_FSIZE of 64 MiB applies to **each** child output file,
  so worst-case malformed media plus manifest storage is 128 MiB/generation,
  1 GiB across eight slots. The accepted manifest is much smaller, 64 KiB.
  Existing 512 MiB preparation reserve/free-space policy remains applicable.
* At most 16 manifest entries, 1 MiB init and 8 MiB per media fragment.
* Two owned publication readers, each using under 10 MiB for these media buffers
  and the IDR parser: at most 64 top-level boxes and one <=256 KiB moof bound its
  internal allocations too. Finalizations can wait within the existing 5-second
  observation deadline; transient contention alone is not immediate failure.
* Four separate at-most-8 MiB response buffers (native playlists are capped
  at 64 KiB too); permits survive buffered I/O and the last retained data-frame
  clone/slice, not merely the HTTP body producer. Thus these native media
  buffer/parser allocations are below
  52 MiB in total, excluding catalog/SQLite, FFmpeg and general runtime memory.
  This is not a total-process RSS guarantee.

Pin/cohort/duration/whole-completion policy lives in the production pure core.
Descriptors, native/application authorization, child status/witness observations,
cryptographic commitments, resource permits and I/O custody remain adapter facts.
This boundary reuses the actual delivery reducer rather than inventing a model
whose passing tests would be mistaken for real execution. Terminal admission and
lease counts reuse the native core HostAuthorization state. Real child reaping,
mutex release, retained service ownership and backend close remain adapter effects.

## Qualification evidence and next finite checks

Root's source65p1 historical results were:

* Native exact-source readers: 2 passed; HLS pure cohort rules: 2 passed.
* Native output/byte-owner mechanisms: 7 passed; retained witness tests: 2 passed.
* Actual private identity fences/independent sessions, ticket/credential release
  recheck, and unsupported/expired claims before dispatch: 3 passed.
* The deterministic preparation test inspected a raw admission ApiError through
  the generic playback mapper, which remapped unlisted 409 errors to `conflict`.
  The repair checks exact raw 409/`plan_expired`, still requires zero effects
  before preparation, and preserves post-preparation expiry and receipt replay.
* The retained-frame test failed because native shutdown closed the host without
  waiting for shared native leases. Passing Bytes-owner mechanism tests did not
  establish real backend drain. Root's subsequent source67p1 run passed the
  repaired actual four-response final-clone/slice assertion and expiry ordering.

Root's source67p1 run passed all five private HLS tests (73 seconds), all four
terminal-owner tests (35.8 seconds), core HLS rules (2), delivery mechanisms (7),
retained witnesses (2), and the three named existing regressions below. The
original dropped-waiter HLS test Weak-upgrades Runtime while the first blocking
worker may still retain RuntimeAdmission. That worker cannot keep Weak<Runtime>
alive by itself, but this test did not prove physical-backend survival after
that temporary admission owner had exited. This is an additional coverage gap,
not a new demonstrated production failure.

Root-owned revised qualification should run these exact filters with the actual
installed FFmpeg and verified packaged observer/supervisor (`MOTION_TEST_OBSERVER`):

* `catabolic-store`: `catalog_source_read::tests`
* `playscale-core`: `delivery::native_whole_hls_tests`
* `playscale --lib`: `delivery::native_hls::tests` (including the focused
  `owned_response_allocation_keeps_capacity_after_body_and_original_frame_drop`
  mechanism test, not an actual native-session test)
* `playscale --lib`: `execution::tests::retained_native_witness`
  (replacement/drop and actual descendant outliving its parent)
* `playscale --lib`: `v2::playback::native_hls_candidate::tests` (now six tests;
  the new exact filter is
  `native_yielded_frames_retain_backend_after_joined_shutdown_timeout_until_last_alias_drop`)
* `playscale --lib`: `native_catalog::terminal::tests` (four new real-owner tests:
  production bounded timeout/same-owner retry, cancelled ordinary/terminal
  waiters, nonrevoking verified export, and actual packaged scan cleanup)
* Existing regressions: `terminal_runtime_refuses_retained_clone_and_export_failure_cannot_reopen`,
  `real_terminal_pair_is_anonymous_verified_and_retains_motion_writer`, and
  `actual_native_service_runs_application_retention_and_joins_it`.

The private application fixtures cover actual observer/encoder/supervisor
construction, two independent sessions, source replacement, signed-plan expiry,
missing-source exact replay, admission/preparation ordering, actual ticket and
credential expiry/revocation after awaited reads, and already-admitted response
semantics. Anonymous output/parser fixtures cover directory replacement before
first pin, same-FD byte mutation, independent positioned reads, malformed or
oversized manifest ranges, fragment parser limits and MP4/header bounds.

The focused production byte-owner mechanism test retains cloned/sliced frames
after body EOF/drop; this alone is not evidence of actual native terminal drain.
Separately, the authored application test
`native_yielded_frames_retain_custody_after_body_eof_until_last_clone_and_slice_drop`
uses actual v2 init/media/master/variant handlers, stops the real delivery worker,
then requires actual native shutdown to remain pending while frame aliases own
Session/stream custody. It releases aliases one allocation at a time and checks
permit return and final native close. The revised fixture also checks retained
buffer capacity, actual gate revocation and a free Motion writer while bodies
are retained. It drops the App and async shutdown waiter Runtime owners, then
requires the actual frame-retained Runtime to support a terminal retry; all
original clone/slice hold and final-close assertions remain in place. **This
abort-waiter case and the four terminal-owner tests passed in source67p1.**

The separate post68 test
`native_yielded_frames_retain_backend_after_joined_shutdown_timeout_until_last_alias_drop`
uses the same genuine four-response setup and per-alias release assertions. It
drops the ordinary App, keeps all response aliases through the first production
five-second shutdown refusal, and joins that task normally with the exact
`native authorization did not drain: Timeout` error. Only after both temporary
shutdown owners are gone does it Weak-upgrade the original frame-retained
Runtime for the same-owner retry. No timer-only sleep, reconstructed backend,
registry, fake lease or Arc-count substitute supplies this evidence. The
original abort-waiter case remains separate. **The shared test refactor and new
joined-timeout case are uncompiled and unrun.**

The terminal wait shares a five-second lease/scan budget. Mutex acquisition and
existing bounded Store reconnection are not magically interruptible, so this is
not a strict five-second end-to-end shutdown guarantee. Failed production close
uses the existing service quarantine; direct runtime fixtures must retain their
runtime to retry. No timeout is labeled successful close or permission to reopen.

Remaining acceptance work includes prepared independence, representation/policy
changes, deterministic cancellation during actual blocked I/O, executing the
new joined-timeout retained-frame case and rerunning the refactored abort case,
burst finalization, real encoder output/disk-limit failure, unchanged public
gates, and full existing delivery regressions. These authored
private fixtures are not public-route end-to-end acceptance. Nothing here opens
native HLS publicly or claims the integration is complete.
