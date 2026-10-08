# Playscale design

Playscale is a self-hosted media library and streaming application inspired by Plex. It uses Rust for the server, Tailscale for private access, and [Demuxe](https://github.com/Jagalite/demuxe) for browser playback.

This document records the initial decisions from October 5, 2026. Implementation choices identified as proposed remain open.

The [feature roadmap](ROADMAP.md) covers the planned Plex-style functionality and proposed delivery phases. Live TV and DVR are excluded.

## Agreed decisions

- SQLite is the selected database, stored on the server's local disk (accepted October 6, 2026). Media files may reside on a NAS.
- The application server will be written in Rust.
- Axum is the selected HTTP framework (accepted October 6, 2026).
- Tokio is the selected async runtime, and Utoipa is the selected OpenAPI tooling (accepted October 6, 2026).
- Use a functional core / imperative shell architecture and dogfood Stateless against the production transition logic, initially in development and tests.
- Catabolic is a reference and future API integration, not a required backend. Support source-attributed metadata/tags and existing renditions, with future processing through local FFmpeg or delegated Catabolic jobs.
- Tailscale provides access to the server. Anyone permitted to reach Playscale through Tailscale can use it without a separate application login.
- The server serves a webpage containing Demuxe and its associated JavaScript and runtime assets.
- The server exposes media URLs that Demuxe can open from that webpage.
- A full documented public API supports third-party applications. The bundled webpage uses the same API; product capabilities must not depend on private UI-only endpoints.
- The roadmap includes library management, metadata, browsing, viewing history, profiles, playback conveniences, offline access, additional clients, other media types, discovery, and synchronized viewing. Live TV and DVR are out of scope.

Tailscale connectivity does not grant access to every device automatically: the tailnet policy must permit a connection to Playscale. The initial access model gives all admitted viewers the same media access. The core implements selectable profiles and a local admin token; profiles are not authentication identities. Enforcement of later parental restrictions remains open. [The implementation plan](IMPLEMENTATION_PLAN.md) and [API contract](API.md) describe the implemented boundary.

## Proposed architecture

Start with one authoritative server and a modular monolith. Keep catalog, media delivery, networking, playback sessions, and background jobs separate inside the application.

```text
Browser on a Tailscale-connected device
  |
  | HTTPS
  v
Playscale endpoint
  |-- Web interface and Demuxe assets
  |-- Catalog API ---------------------- SQLite
  |-- Media delivery ------------------- Media folders or NAS
  |-- Background jobs ------------------ FFprobe and FFmpeg
                                          |
                                          v
                                      Derived asset cache
```

The Rust server can embed the built webpage and player assets in its executable or serve a versioned asset directory beside it. Media files remain in configured library folders. Store the database and generated assets separately from original media.

Axum, Tokio, and Utoipa are selected for HTTP handling, asynchronous execution, and OpenAPI generation. SQLite is the selected database. Supervised native FFmpeg subprocesses remain proposed, and the frontend framework is undecided.

The [Axum, Poem, and Salvo review](FRAMEWORK_REVIEW.md) records the framework comparison and reproducible HTTP/OpenAPI probes that informed the Axum decision.

## Functional core and Stateless

Keep deterministic transition logic in small domain modules for jobs, playback sessions, and library scans. Inputs include relevant timestamps, IDs, and external outcomes; outputs describe effects as data. Tokio tasks, Axum requests, SQLite transactions, filesystem handles, and subprocesses belong in the imperative shell. Ordinary catalog queries and streamed media bytes need not pass through a state machine.

Dogfood the unpublished Stateless library from the first job lifecycle implementation. Its model adapter must exercise the same transition function used in production. Check cancellation, stale attempt results, concurrency limits, retries, and recovery using explicit invariants, generated sequences, bounded exploration, and minimized replayable failures. Independent oracles should derive expectations without duplicating the reducer's implementation.

Start with a development dependency and keep Stateless types out of the public API and core domain interfaces. Local co-development may use the sibling Stateless checkout. Reproducible builds use the exact MIT-licensed `statelessness` 0.1.1 registry package and Cargo.lock checksum; a machine-specific absolute path is not the shared build configuration. Playscale imports that development dependency as `stateless`. Feed reusable fixes back into Stateless and update the pinned source deliberately, retaining regression sequences and their model/build identities.

For durable work, commit state changes and pending effect records together before dispatch. Effects need retry-safe execution and restart reconciliation. Model checks establish properties within their declared bounds; retain integration tests for actual SQLite durability, process termination, HTTP streaming, and resource cleanup. Runtime trace recording can be added later with bounded storage and explicit handling of sensitive data.

## Networking and HTTPS

Serve the webpage, API, media, and player assets from one HTTPS origin. A representative URL is `https://playscale.<tailnet>.ts.net`; the actual name depends on the deployment.

Keep the application HTTP service independent of the Tailscale adapter. Two deployment options remain under consideration:

1. Rust listens on localhost behind Tailscale Serve. The installed Tailscale service provides private HTTPS access.
2. Rust embeds a Tailscale node. Certificate handling, connectivity, and required sharing features must be resolved for the selected library.

The proposed first deployment is option 1. It gives the browser a secure origin while keeping the application independent of the embedded networking implementation.

The official Rust library, `tailscale-rs`, supports TCP and UDP sockets, DERP relays, and direct connections in many cases. Its current documentation describes NAT traversal as incomplete and lists HTTPS certificates, MagicDNS, node sharing, and peer relays as unsupported. These are implementation-selection considerations, not limitations of Rust itself. Rust bindings to the Go-backed `libtailscale` also exist.

The viewing device needs a working route into the tailnet. Serving a webpage does not itself connect the browser to Tailscale. Public internet access is outside the initial design.

The service must not accidentally expose the same unauthenticated endpoints through an unrestricted LAN or public listener. In the Serve deployment, bind the application listener to loopback.

## Public API

The API is a core product interface from the first release. As features ship, expose their capabilities through the API alongside the bundled interface. Third-party applications should be able to browse and manage libraries, manage metadata and collections, use profiles and watch history, create playback sessions, request media and derived assets, and observe or control jobs where authorized.

The proposed contract is a versioned HTTP JSON API under `/api/v1`, documented with OpenAPI, plus streaming endpoints and a documented event interface. Media bytes and runtime assets need not use JSON or the API route prefix. Choose SSE or WebSocket transport according to event and interaction needs; the choice remains open.

Define stable resource IDs, pagination, filtering, sorting, structured errors, retry and idempotency behavior, source revisions, and compatibility/deprecation rules. Publish examples sufficient to build an independent client, including playback, progress updates, and job lifecycle handling. API coverage must include administrative functions once their authorization policy is defined.

Retain the Tailscale access model: an API consumer permitted to reach the service does not need a separate application login by default. Profile identity and any privileged operations require explicit semantics. Cross-origin browser clients need a deliberate origin/CORS policy and protection for state-changing requests; Tailscale reachability alone is not a browser-origin policy. External provider credentials and filesystem paths must remain server-side implementation details.

Use the bundled webpage as a consumer of the public API, with contract validation and a small independent example client to verify that the interface is usable outside the bundled application.

## HTTP surface

These routes illustrate the proposed interface; they are not a finalized API.

| Route | Purpose |
| --- | --- |
| `/` | Library interface and player |
| `/assets/demuxe/…` | Matching Demuxe JavaScript, Wasm, workers, and other assets |
| `/api/v1/libraries` | Library discovery and configuration where authorized |
| `/api/v1/items` | Catalog entries, metadata, and playable file IDs |
| `/media/<file-id>` | Original media bytes |
| `/artwork/<item-id>` | Cached posters and thumbnails |

The webpage opens a relative media URL through Demuxe, such as `/media/abc123`. The server resolves that opaque ID to a cataloged file within a configured media root. Requests must not accept arbitrary filesystem paths.

Media delivery needs HTTP range support, correct content lengths and range headers, HEAD requests, bounded buffering, and cancellation when the viewer disconnects or seeks. Define source revision handling so a file replacement cannot silently mix bytes from different versions during playback.

## Playback responsibilities

Demuxe owns browser playback-path selection, seeking, track selection, subtitles, and client fallback. Playscale owns source discovery, access, file delivery, catalog data, and any server-generated alternatives.

Prefer original-file delivery where the client and connection can sustain it. The read-only playback planner applies Auto, strict Original only, or Convert preferences, revision-bound client evidence, and an optional average-bitrate constraint. It selects an existing version or proposes an explicit fixed-recipe processing job. The bundled UI attempts unknown versions through Demuxe with bounded fallback. Processing admission still requires administrator authorization; outputs must finish and validate before playback. Streaming transcoding and viewer processing policy remain future work. See [the planning contract](API.md#playback-planning).

Rust can launch FFmpeg with hardware acceleration. Actual capability depends on the FFmpeg build, operating system, drivers, hardware, and source/output formats. NVIDIA, Intel, AMD, and Apple hardware require different backends. GPU encoding alone does not establish that decoding, scaling, subtitle burn-in, or tone mapping also stays on the GPU.

Use subprocesses initially if server processing is added. FFmpeg can read sources and write output directly; decoded frames do not need to pass through Rust. Jobs need concurrency limits, progress reporting, cancellation, and cleanup.

Coordination around FFmpeg is a core Playscale responsibility when server processing is introduced:

- **Capability negotiation:** Combine Demuxe's client capabilities, source tracks, connection constraints, and verified server capabilities into an explicit playback plan. Distinguish original delivery, remuxing, audio conversion, and video transcoding, with reasons for each choice.
- **Resource accounting:** Admit work against bounded CPU/GPU worker slots, bandwidth, and temporary-storage budgets. Track reservations per session or job and release them on completion, cancellation, failure, and restart reconciliation.
- **Buffering:** Coordinate client buffer needs, worker production, and output retention. Use actual segment timestamps and completion state; bound read-ahead and storage, and account for seeks and slow consumers.
- **Cancellation:** Propagate session stop, superseded playback requests, and expired session leases to the work they own. Stop subprocesses with a bounded shutdown deadline and clean up partial output without interrupting work still needed by another consumer.
- **Recovery:** Classify network, source, codec, and worker failures before choosing reconnect, retry, or a different playback plan. Bound retries and startup deadlines, preserve valid resume state, and reconcile interrupted jobs after restart. Progress reporting must have its own deadlines and must not indefinitely block media processing; represent unavailable speed or ETA explicitly.

Keep these decisions in the playback-session and job transition models, with subprocess, network, and storage effects executed by the imperative shell. Expose playback reasons, resource use, buffering state, and recovery outcomes through the public API and diagnostics.

Ship Demuxe's JavaScript package and runtime assets from the same verified build. Serve appropriate MIME types and configure cross-origin isolation when using threaded Wasm engines. Browser and media support must be checked against the selected runtime rather than assumed from file extensions.

## Catalog and storage

SQLite on the server's local disk is the selected database design. Read-only access to original media and a separate disposable cache for derived assets remain proposed.

Separate content identity from filesystem location:

```text
Movie or episode
  -> Edition or version
    -> Media file
      -> Video, audio, and subtitle tracks
```

Moving or replacing a file should preserve the title's metadata and viewing history where identity can be established. Metadata matching needs manual correction and a way to preserve user edits. Metadata providers and matching rules are undecided.

If scanning, probing, artwork generation, or conversion runs in the background, persist jobs so restarts do not lose work. Use bounded workers and periodic reconciliation to catch changes missed by filesystem notifications.

## Decisions still open

| Decision | Question to resolve |
| --- | --- |
| Client scope | Are desktop and mobile browsers sufficient for launch, or are TVs and casting required? |
| Networking | Use Tailscale Serve initially, or require embedded networking from the first release? |
| Administration | Can every admitted viewer add library roots and change settings, or should administration remain local or separately restricted? |
| API contract | Which event transport, compatibility policy, cross-origin client policy, and privileged-operation controls should the public API use? |
| Viewing identity | How should viewers select profiles without a separate application login, and how will restricted profiles be enforced? |
| Catalog scope | When should planned music, photos, and personal videos follow movies and television? |
| Metadata | Which providers and local metadata formats should be supported? |
| Processing scope | Ship original-file playback first, or include server remuxing and transcoding at launch? |
| Packaging | Which server operating systems, CPU architectures, and native/container packages come first? |
| Distribution | How are compatible Demuxe and FFmpeg builds versioned, bundled, updated, and accompanied by required license materials? |
| Recovery | What backup and restore flow covers the database, settings, and any networking credentials? |

## Proposed first milestone

Deliver one complete viewing loop:

1. Start the Rust server and expose its private HTTPS endpoint through Tailscale.
2. Configure a media folder and build a basic catalog.
3. Browse the catalog from the served webpage.
4. Open an original media URL in Demuxe.
5. Verify playback, seeking, track changes, and subtitles on the chosen launch browsers.
6. Persist resume position and reopen the item from another device using the selected viewing-identity model.

Verify that the application is unreachable through unintended network listeners. Exercise byte ranges, disconnects, and file changes. Test remote playback over both direct and relayed Tailscale connections before making high-bitrate streaming claims.

## References

- [Demuxe documentation](https://github.com/Jagalite/demuxe)
- [Official Rust Tailscale library and status](https://github.com/tailscale/tailscale-rs#status)
- [Rust bindings to libtailscale](https://github.com/messense/libtailscale-rs)
- [Tailscale Serve](https://tailscale.com/docs/features/tailscale-serve)
- [Tailscale connection types](https://tailscale.com/docs/reference/connection-types)
- [FFmpeg tools and libraries](https://ffmpeg.org/documentation.html)

## Correctness state and adapter contracts

The deterministic core owns decisions that depend on durable identity, revision,
ordering, or lifecycle state. Adapters reconstruct that state from SQLite while
holding the writer boundary, supply observations, and persist the returned
changes. Mutating transactions reserve the SQLite writer with `BEGIN IMMEDIATE`
before transaction-local observations, preventing unrelated diagnostic writes
from invalidating a deferred read snapshot. These are focused domain policies, not a second in-memory database.

- `core::processing` owns immutable request identity, source admission, output
  requirements, and completion admission. Completion observes the current source
  revision and validated output identity; cancelled, stale, replaced-source, and
  already-published results cannot publish. The returned job and output/rendition
  records must commit atomically. Duplicate and stale completions are complete
  no-ops, including timestamps and diagnostic fields.
- `core::scan` owns complete-inventory admission and identity reconciliation.
  Publication requires a current attempt, an enabled library, and matching root
  identities at traversal start, traversal end, and commit. Unique exact-revision
  moves retain identity; ambiguous copies receive new identities. Incomplete
  inventories never authorize marking missing paths unavailable.
- `core::viewing` owns session authority, ordering, exact retries, file-switch
  identity, rendition eligibility, sticky completion, manual overrides, and the
  legacy-write barrier. Session, progress, and viewing changes commit together.
  An invalidated session identity is retained to preserve the legacy-write barrier.
- `core::renditions`, `core::catalog`, and `core::revision` own revision-bound
  registrations, catalog relationship constraints, relocation revalidation,
  protected-profile policy, and optimistic revision advancement without overflow.
- `core::maintenance` owns retention eligibility, attempt-bound deletion
  acknowledgments, disk-reserve admission, and schedule advancement. Scheduled
  scan admission and schedule advancement commit in one transaction. Time is an
  observation supplied by the adapter. SQL may preselect candidates using core
  cutoff bounds; the core rechecks removal eligibility before effects execute.

Artwork pin/local/conflict resolution and event cursor reset eligibility also live
in the core. Existing metadata resolution, playback selection, and range policies remain
pure. HTTP syntax/authentication, SQL referential/uniqueness constraints and query
projections, locks/permits, filesystem containment, byte inspection/hashing,
FFprobe/FFmpeg execution, and actual transaction/rename operations remain adapter
responsibilities. Their outcomes feed core decisions; modeling those outcomes
alone does not certify OS behavior or adapter atomicity.

Stateless invokes production reducers. Its processing model includes source
replacement/unavailability, output validation outcomes, and commit/rollback as an
explicit adapter contract. SQLite fault-injection tests separately check that
scan and processing publication and scheduled admission obey that contract.
