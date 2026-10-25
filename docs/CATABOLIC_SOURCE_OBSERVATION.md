# Motion source observation and companion packaging

Physical Motion `libraries.id` values identify source roots. Logical catalog
libraries and their memberships remain distinct. `motion-catalog register-source`
reads an existing unmanaged physical source, binds its exact canonical directory,
and persists a mapping keyed by that source ID. Its native source name is the
full SHA-256 of the Motion ID (64 hexadecimal characters). Registration is
repeatable only for the same catalog ID/incarnation/schema, native profile,
source name, Motion root identity/revision, native filesystem binding and effective
observation policy. Neither matching paths nor matching names authorize rebinding.

Operator commands (all paths are explicit; these examples do not execute them):

```
motion-catalog --catalog /path/native.sqlite3 register-source --motion-db /path/motion.sqlite3 --source-id SOURCE_ID
motion-catalog --catalog /path/native.sqlite3 audit-sources --motion-db /path/motion.sqlite3
motion-catalog --catalog /path/native.sqlite3 scan-source --motion-db /path/motion.sqlite3 --source-id SOURCE_ID --timeout-seconds 120
```

These commands do not deploy or activate cutover. Registration preserves Motion's
normalized exclusion prefixes and observes all extensions, including unknown
ones. Existing incompatible native policy is rejected rather than overwritten.
Changes to exclusions, filesystem binding or source identity require explicit
reconciliation, not automatic mutation of an immutable mapping. Audit rejects
missing mappings rather than silently omitting physical sources.

## Qualified companion boundary

The shipping Motion server and operator executable embed the same closed native
observation dispatcher. Before normal argument parsing or runtime initialization,
`--db` invocations are exclusively dispatched to the exact native v2 worker or
shared-worker protocol; malformed and other commands fail without starting the
application. The standalone `motion-catalog-observer` test/package binary uses
the same dispatcher. There is no shell, generic native CLI or external processor
branch in this entrypoint.

`observer::packaged_current_executable` is a packaging-owned API, not an operator
configuration API. It accepts no path or user-provided digest. Its grant pins and
hashes the running executable, which must be the shipped binary with the early
closed dispatcher. Calling it in an unrelated executable or test harness is not
qualification. The native helper retains its own exact file identity and digest
checks. Runtime enrollment must not substitute an environment-selected helper or
accept a user hash as evidence of closed behavior. Native packaging limits the
artifact to 128 MiB. Use a release build or a build with Cargo profile
`strip = "symbols"` (`CARGO_PROFILE_DEV_STRIP=symbols` for development
qualification) when symbol tables make a debug binary exceed that bound. The
packaging check hashes the final deployed executable after stripping; never raise
the native bound or substitute an arbitrary caller-supplied companion.

## Ownership and cancellation

Server scans use the already admitted, retained HostSession. The standalone
operator uses the backend's shared authorization. A scan retains both the worker
permit and an extra authorization lease across native subprocess exit and durable
cleanup acknowledgment. Cancellation and timeout request native cancellation,
then wait for exact cleanup; they are not permission to abandon the process.
Dropping an asynchronous operation signals its retained worker to cancel. Errors
that cannot acknowledge cleanup leave ownership draining and require recovery.
A successful observation rechecks the Motion mapping before exposing the result.

Disposable tests cover exact mappings, restart, real traversal/exclusions, native
policy drift, cancellation, dropped futures, gate revocation, forged retained-host
identity and non-directory/FIFO roots. They do not qualify deployment, actual
backup restoration on UUID-backed filesystems, or a user's live catalog. Report
those separately from disposable source-observation test results.

## New native-owned sources

After cutover, do not write frozen legacy `libraries`. Migration 77 decouples
source mappings from that legacy foreign key and adds immutable native source
descriptors. Operator provisioning uses explicit stable IDs:

```
motion-catalog --catalog /path/native.sqlite3 register-native-source --motion-db /path/motion.sqlite3 --source-id UUID --logical-library-id LIBRARY_UUID --name Media --root /media --exclude private
motion-catalog --catalog /path/native.sqlite3 native-sources --motion-db /path/motion.sqlite3
```

The server uses `register_with_host` on its retained admitted HostSession followed
by immutable control acknowledgment. It must not invoke an independent native
session while retaining its server owner. Native effect-before-ack interruption
can replay only the exact same identity, directory and observation policy.
Configured native sources resolve through the registry, not legacy fallback.

Legacy source registration now compares the real root's device/inode to Motion's
stored root identity before native mutation, and compares the returned native
binding to that same tuple. Running standalone scans retain a Motion writer fence
inside a cancellation-aware coordinator until native cleanup, so source-binding
and exclusion edits cannot race observation publication even if a caller aborts.

Physical migration maps native observations only when every captured Motion
fingerprint field agrees: size, mtime, device, inode and ctime. Durable mappings
retain Motion file/version IDs and content revisions separately from native
occurrence IDs/revisions. Projection of changed or missing native observations
invalidates availability; it never silently changes a version to new bytes.

## Native scan discovery

The server coordinator retains the source writer fence and actual storage permit
through native cleanup, including HTTP cancellation. It then processes supported
Motion media extensions using the existing opened-file SHA-256/ffprobe adapter,
not the legacy scan catalog writer. Durable discovery intents allocate IDs before
native effects; retries use the original IDs, probe and canonical command.

The native `fresh_media_discovery` command atomically creates the fresh work,
edition, version item, source association and typed identity through existing
native owners and their receipt transaction. A measured positive integral-us
whole-file duration may establish only that new file's own original timeline.
No equivalence to another timeline is inferred. Unknown or corrupt duration stays
provisional/draft without placement or readiness. Full raw probe facts remain in
the immutable intent and native metadata.

Immediately before dispatch, the adapter checks the current opened file's full
fingerprint, root identity, source binding/policy and native exact observation.
Lost acknowledgments replay the original native receipt; stale proof permits
receipt lookup only, never fresh dispatch. Committed discovery mappings use the
same exact occurrence projection records as imports, under a discovery-specific
provenance key. A later replacement gets a new explicit identity, not an inferred
continuation of the old version.

Native ExactFileRevision intentionally omits ctime. Therefore the adapter also
retains and compares the native observed ctime and full Motion fingerprint for
projection and serving. A ctime-only change under the same native revision is an
explicit reconciliation error, not permission to reuse old readiness or create
another intent with that same key. Original historical references remain intact
and unavailable rather than serving changed bytes.

### Remaining qualification boundaries

The coordinator is not itself authorization to expose an HTTP scan route. A
request must first become a durable, bounded scan demand under current principal
and source authorization. Native serving admits only its qualified finite route subset. Actual-process
bootstrap, basic catalog/source routes and restart have passed; public scan
endpoints and the production queue worker are now wired for end-to-end scan
qualification; their actual packaged-process result is tracked separately.

A canonical command's expected generation is immutable after journal commit.
Schema85 records a distinct bounded successor only from an opaque native witness
minted by an actual synchronous stale-generation decision for an older generation
in the same incarnation. The original payload, allocated entity IDs and every
prior envelope remain unchanged. The next envelope is committed before dispatch,
then demand, source and physical publication facts are checked again under the
writer and retained native owner. Future-generation requests, timeout, missing
receipts, restore uncertainty and expired receipts do not authorize supersession.
There are at most31 successors. Receipt-only startup recovery follows the exact
latest validated chain; it never executes an absent receipt.

Native occurrence revisions include location/path and source identity policy,
not just byte identity. The current adapter does not promise seamless move-follow
or content-equivalent version replacement. New revisions receive new provisional
identities while historical references remain retained and unavailable. Same
unchanged observations reuse existing identities; path or digest matching alone
never transfers a work or version identity.

## Durable native scan requests (explicit bounded service policy)

Native scan requests retain an explicit `accepted_bounded_service_v1` execution
policy. Successful authenticated admission transfers only the captured immutable
source mappings to a bounded service-owned demand. Captured principal facts are
admission audit, not a current permission grant. Later principal or logical
library changes cannot expand that accepted source set. New requests, status
reads and cancellation still use current authentication and library scope.

At both observation dispatch and discovery publication, the service checks the
same request ID, demand ID, source ID, attempt generation, exact mapping digest
and execution-policy version under the Motion writer custody retained through the
native effect. Durable cancellation revokes subsequent effects. A read-authorized
process-local cancellation signal can conservatively interrupt an active worker
while its source writer fence prevents cancellation from committing; no response
claims cancellation until reauthorization and durable acknowledgment complete.
Caller loss or restart before that acknowledgment leaves a pending interrupted
demand, not a fabricated cancellation. Retry retains original discovery IDs and
canonical command intents.

The initial queue has at most 100 sources per request, 1,024 pending demands and
32 attempts per demand. It does not coalesce unrelated requests implicitly.
Physical worker tests use the real packaged production executable and distinguish
that internal qualification from exposing the public scan endpoints.

A cancellation waiter holds exact-request, reference-counted reclaim inhibition
until it either commits the authorized cancellation or drops its custody handle.
Claim registration and inhibition checks share one registry critical section,
so a waiter cannot miss a claimed-but-not-yet-registered task. Completion tokens
are released only after native cleanup and the attempt acknowledgment. The
service coordinator retains those guards even if its awaiting caller disappears;
shutdown is an explicit token followed by actual draining. The HTTP adapter must
wait with a bounded deadline, reacquire the Motion writer with bounded SQLite
busy retry, and reauthorize before acknowledgment. A timeout is an error, never a
cancelled response. This process-local custody is not durable authority: restart
reconciles the original pending demand and its immutable scope.

Discovery has an explicit owned pending-acknowledgment phase after actual native
commit. It retains the Motion transaction and runtime; dropping it rolls back
only the host acknowledgment. Startup checks the latest exact canonical envelope in each validated attempt chain
with the retained native receipt reader before rebuilding projections. A full
matching receipt restores the original mapping, all atomic revision baselines,
and the receipt acknowledgment in one Motion transaction. No receipt leaves the
intent pending; unknown, expired, redacted or conflicting outcomes are not
success. Startup never dispatches the saved discovery or guesses a current head.
The real lost-ack test drops this production phase value, rather than deleting
proof rows or fabricating a committed native response.
A later cancellation cannot erase an already committed native discovery. Receipt
reconciliation may finish its host acknowledgment after cancellation, but performs
no new native mutation and cannot authorize another source or attempt.


## Lossless media-family and imported-source adapters

Motion stores only the media families video and audio. Native video maps exactly
to Motion video; generic audio is represented by the explicit native custom kind
custom:motion_audio, not inferred to be a musical recording. The shared core
mapping rejects unknown values and reverses exactly. Original Motion kind and
logical media_type are retained in metadata; generic discovered media remains
unclassified, so the existing public audio representation remains other.

Schema86 retains imported physical source descriptors separately from newly
registered native sources. Journal-only preparation verifies immutable frozen
source evidence; acknowledgment checks the entire activated receipt set and
current native binding/policy through the actual retained host before committing
any descriptor. Original source revision, binding revision, scan barrier and all
logical memberships remain evidence, not reconstructed defaults. Historical-only
source owners never become physical roots or access grants. Descriptor lookup is
not library authorization, and the unassigned marker does not imply membership.

## Explicit service lifecycle and public scan qualification

Native startup launches only the native accepted-demand coordinator alongside
Motion delivery/auth services. It does not start legacy scan/configure/watch or
catalog-maintenance writers. The public scan adapter performs current admission
and idempotency checks before enqueueing; the worker consumes only that durable
captured source scope.

On shutdown the native service cancels its token and waits up to120seconds for
actual coordinator cleanup, followed by the existing retained-host five-second
drain. The legacy service retains its five-second worker budget. Exceeding the
native coordinator deadline is an error with unresolved cleanup, not a clean
seal or a cancelled demand. Service managers should allow at least150seconds
before forced termination; a forced exit can require explicit native recovery
on restart. HTTP cancellation separately retains its exact-request cleanup and
durable acknowledgment custody.

The packaged-process scan regression covers real source and library creation,
WAV observation, excluded subtree handling, durable enqueue/status/list/cancel,
wrong-library paired-principal denial, native catalog publication/read, graceful
restart and idempotent original request replay. This source test is not itself
proof of qualification; only the recorded actual binary test result establishes
which gates have passed.

Native media probing uses the already opened, regular, read-only file descriptor.
On Unix, a bounded query of the configured ffprobe's input protocols selects its
seekable `fd` protocol when available; the protocol whitelist is exactly `fd`,
not `file` or any network protocol. Stream and chapter queries retain that same
opened descriptor and verify the full fingerprint, including ctime, before and
after probing. They do not reopen the source pathname. If `fd` is unavailable,
or on non-Unix platforms, the native adapter uses a `pipe`-only whitelist;
missing duration remains unknown rather than inferred. A failed capability query
is an error, not permission to broaden protocols. Cancellation and timeout kill
and reap native probe children before cleanup returns. Legacy probing remains
separate. The native probe regression target requires a real ffprobe with `fd`
input support to qualify measured PCM duration and secondary protocol denial.

Private generated-output observation is separate from public sources. The fixed
host identity `motion:generated-output` is journaled in
`catabolic_generated_source`; its mapping grants no logical-library membership
and is never returned by public source registration. Both public and private
registration reject overlapping roots. Projection must verify the retained
native binding/policy and exact private mapping before materializing a managed
source cache row; this is not an exemption for arbitrary managed libraries.

The processing owner commits its final locator before materialization. Completed
encoder output is copied into a distinct inode in a private sibling spool, with
read-only input, exact pre/post fingerprint, SHA-256, length and fsync checks. Only
that copied inode is linked no-clobber into the observed final root. A persistent
exact-locator exclusive lease prevents concurrent copier recovery. A retry may
remove only its deterministic spool alias after proving it is the same final
inode and matches expected bytes; it never removes unknown aliases or final
outputs. Partial pre-link spool replacement requires exclusive custody, no final
file and an unaliased regular spool leaf. Final ctime is measured after unlinking
the known spool alias. Native admission still requires an unaliased output.

Final output is re-probed and observed through the retained native host; its
opaque proof retains the read-only descriptor and exact native occurrence. A
second check of current source-relative locator, descriptor fingerprint, native
binding/policy and occurrence is mandatory at generated admission. Unmeasured
outputs do not qualify for the first playable generated-output contract. These
adapters do not grant an external native execution lifetime or manufacture a
cleanup receipt; Motion retains its existing render/supervisor ownership.
