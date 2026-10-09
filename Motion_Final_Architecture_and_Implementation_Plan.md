# Motion: Final Architecture and Implementation Plan
## Motion-owned Rust catalog · one server API · parallel implementation workstreams

**Design version:** 1.0.0  
**Date:** October 9, 2026  
**Status:** Finalized implementation baseline; the proposed changes are not yet implemented or release-qualified.  
**Primary repository:** `Jagalite/motion`  
**Public API contract:** [Motion Server API v2](contracts/Motion_Server_API_v2.yaml)  
**Audience:** Maintainer, implementation agents, reviewers, client developers, and release engineers.

> **Binding decision:** Motion owns its catalog, storage schema, scanning, metadata, processing, viewing, and delivery logic in Rust. Catabolic supplies selected reference behavior and regression scenarios—not a runtime, crate dependency, service, database, or required standalone rewrite. All ordinary clients use the same Motion API.

This document supersedes the architecture decisions in `Motion_Unified_Application_Architecture.md`, `Motion_Rust_Catabolic_Architecture_and_Implementation_Plan.md`, and their API draft. It retains the personal-media objectives and gaps from `Motion_Plex_Parity_and_Architecture_Review.md`. The previous full Catabolic rewrite/PyPI plan is **not** a prerequisite or a workstream in this implementation.

## Contents

1. [Decisions and scope](#1-decisions-and-scope)
2. [Evidence and retained foundations](#2-evidence-and-retained-foundations)
3. [Runtime topology and ownership](#3-runtime-topology-and-ownership)
4. [Repository and dependency structure](#4-repository-and-dependency-structure)
5. [Catalog domain and identities](#5-catalog-domain-and-identities)
6. [SQLite, transactions, assets, and events](#6-sqlite-transactions-assets-and-events)
7. [Scanning and filesystem reconciliation](#7-scanning-and-filesystem-reconciliation)
8. [Metadata, matching, artwork, and organization](#8-metadata-matching-artwork-and-organization)
9. [Jobs, FFmpeg, resource admission, and process safety](#9-jobs-ffmpeg-resource-admission-and-process-safety)
10. [Playback, viewing, and segmented delivery](#10-playback-viewing-and-segmented-delivery)
11. [Public server API](#11-public-server-api)
12. [Identity, authorization, and private networking](#12-identity-authorization-and-private-networking)
13. [Go CLI and Charm TUI](#13-go-cli-and-charm-tui)
14. [TypeScript frontend and Demuxe](#14-typescript-frontend-and-demuxe)
15. [Desktop shell and one-app lifecycle](#15-desktop-shell-and-one-app-lifecycle)
16. [Offline, TV, music, photos, and further parity](#16-offline-tv-music-photos-and-further-parity)
17. [Statelessness and the correctness corpus](#17-statelessness-and-the-correctness-corpus)
18. [Migration and compatibility](#18-migration-and-compatibility)
19. [Parallel-agent execution plan](#19-parallel-agent-execution-plan)
20. [Milestones and acceptance gates](#20-milestones-and-acceptance-gates)
21. [Qualification, operations, and release](#21-qualification-operations-and-release)
22. [Risks, change control, and completion](#22-risks-change-control-and-completion)
23. [Sources and accompanying artifacts](#23-sources-and-accompanying-artifacts)

## 1. Decisions and scope

### 1.1 Final decisions

| ID | Decision | Consequence |
|---|---|---|
| D01 | Rust remains the authoritative Motion backend, using the existing Axum/Tokio foundations. | The previously discussed Go server migration is superseded for this design. Go is a client language. |
| D02 | Implement a Motion-owned, Catabolic-inspired catalog in the Motion workspace. | No Python worker, PyO3, maturin, Catabolic HTTP listener, or internal Catabolic RPC protocol in the product. |
| D03 | Use one authoritative local SQLite database, owned by Motion. | One migration authority and one transaction protocol; module ownership does not require separate databases. |
| D04 | Keep a modular monolith with bounded native execution workers. | Ordinary domain operations are in-process Rust calls; FFmpeg/FFprobe remain supervised subprocesses. |
| D05 | One documented public API serves all clients. | Web, desktop, CLI, TUI, automation, and future clients cannot depend on private business endpoints or direct database access. |
| D06 | TypeScript is the shared web/desktop frontend; Demuxe is the browser playback implementation. | Do not rewrite Demuxe or its codecs as part of Motion. |
| D07 | Ship Electron as the initial desktop shell. | A thin host interface preserves a later Tauri option; do not build two production shells simultaneously. This is a design selection, not a claim of playback qualification. |
| D08 | Use Statelessness against production reducers and composed workflows. | Verification checks the real transition logic, not a separately maintained simulation. |
| D09 | Keep original media read-only. | Deleting or detaching a catalog object never authorizes deleting original files. |
| D10 | Separate viewing authority, active delivery, and durable preparation. | Session cancellation and background optimization have independent lifetimes, sharing resource admission and execution primitives. |
| D11 | Adopt `/api/v2` for logical catalog semantics; preserve v1 through compatibility adapters during migration. | Do not keep two authoritative catalog writers to support two API versions. |
| D12 | “One app” means one coherent installation and lifecycle, not one executable or OS process. | One installer, coordinated component versions, automatic local startup, explicit remote mode, and one data directory. |

Changes to these decisions require a recorded architecture decision and corresponding contract/migration review. Implementation agents must not silently reopen them.

### 1.2 Product scope

The target remains personal-media parity with Plex on a **published server/client matrix**, including applicable premium conveniences. Movies and television are the first complete slice. Music, photos, personal videos, downloads, offline reconciliation, household restrictions, operational tooling, and living-room use remain tracked requirements. A desktop wrapper is not automatically a TV client or mobile application. The retained gap mapping is in [PARITY_LEDGER.md](qualification/PARITY_LEDGER.md). [R1]

Live channels, tuner integration, electronic program guides, DVR, recording rules, and recording-specific commercial removal are excluded. HLS delivery of a personal movie is **not** live TV and remains in scope. Plex's hosted commercial catalog, rentals, advertising, DRM services, and cloud social graph are separate products, not prerequisites. External discovery and group viewing remain separately gated extensions. Read-only originals are a deliberate safety difference that must appear in eventual parity claims.

### 1.3 Non-goals

Do not port all of Catabolic, replace its Python release, publish a new Rust Catabolic package, or implement Python compatibility for Motion. Do not require distributed workers, PostgreSQL, a message broker, Kubernetes, general-purpose executable plugins, or embedded Tailscale networking. Do not route all read queries or media packets through a state machine. Do not promise a full Plex-compatible wire protocol or support for official Plex clients.

Development-only tooling may use Python to extract reference fixtures or validate documents. That is not a shipped Python runtime dependency.

## 2. Evidence and retained foundations

### 2.1 Source baselines

| Project | Reference commit | Use |
|---|---|---|
| Motion | `52491bf3e6ef323a33dd546687129bdb3b51d4d1` | Existing Rust application, API, schemas, delivery, viewing, packaging, and tests. |
| Catabolic | `8389e66b01a102b2107ee8ae9f0c00161d17d267` | Selected catalog, observation, components, recovery, and processing reference behavior. |
| Statelessness | `b97423d2bc01b61eee25b50a44e4b6416851b6b6` | Verification API/reference for the migration branch. |
| Demuxe | `f32afd60db1787600229823d13c668a812da4dca` | Latest checked source head for this document; runtime archives still need explicit selection and qualification. |

The Motion and Catabolic heads were rechecked for this document. Demuxe's source head is newer than the earlier review. A source commit is not equivalent to a published npm archive or an installed runtime. Existing Motion packaging pins a separate Demuxe distribution; do not inherit source-candidate capabilities by changing a version label. [M1] [D1]

No Motion application build, repository change, media benchmark, or new runtime qualification was performed to produce this plan. The accompanying validation report describes **document and schema checks only**.

### 2.2 Preserve existing behavior

Motion already has useful distinctions between items, editions, files, and revisions; source-attributed metadata; ordered viewing sessions; durable jobs; conditional/range delivery; configuration; snapshots; retention; and production-reducer tests. Its current UI is file-oriented and its conversions finish before playback. Preserve the implemented behavior and test evidence while replacing ownership and missing workflows—not merely the filenames. [M1] [M2] [M3] [R1]

Catabolic's reference model separates logical content, physical occurrences, file associations, output artifacts, and filesystem publication. Its guarded observation workflow includes freshness, completeness, source identity, and stale-worker rules. Those are the valuable behaviors to adapt. Its general-purpose SQL/GraphQL operator surfaces, third-party output layouts, notification system, and book/comic/document workflows are not automatically Motion requirements. [C1] [C2] [C3]

### 2.3 Reference selection register

Every adopted Catabolic behavior receives a record with: source commit/path/test; intended Motion outcome; required invariant; normalized fixture; Motion owner; and a disposition of `adapt`, `defer`, `not_applicable`, or `intentional_difference`. Preserve upstream attribution for copied/adapted code and test material. Do not label newly authored fixtures as copied or executed upstream evidence.

The initial selections are identity/association separation, guarded observations, component-to-file occurrence mapping, revision-bound lineage, request/attempt separation, filesystem journals, and bounded query selection. Correct source behavior outranks superficial API resemblance. Known reference defects become explicit corrected expectations, not compatibility obligations.

## 3. Runtime topology and ownership

```text
                        ONE MOTION PRODUCT

   Go CLI / Charm TUI     Web browser       Electron desktop
            |                 |              TypeScript + Demuxe
            +-----------------+---------------------+
                              |
                    Motion public HTTP API
                              |
                  motion-server (Rust/Axum)
                              |
            +-----------------+-------------------------+
            |                 |                         |
      Motion catalog    Viewing / delivery        Auth / operations
      scan / metadata   planning / sessions       events / settings
            |                 |                         |
            +--------- shared transaction services -----+
                              |
                  motion.sqlite + managed assets
                              |
                 Rust work/resource coordinator
                              |
                supervised FFprobe / FFmpeg workers
                              |
                 original roots / temporary segments

 Media data: safely opened original or prepared bytes -> Rust HTTP -> client.
 Decode/display: browser native / WebCodecs / Demuxe Wasm at the client.
 Server conversion: FFmpeg decodes, filters and re-encodes only when required.
 Catabolic: reference repository and development corpus; absent at runtime.
```

The Rust server normally does not render video into a display. It may decode through native FFmpeg while transcoding; the client presents the output. Direct play bypasses server decoding. Server-side adaptation and client-local fallback are coordinated choices, not mutually exclusive product architectures.

### 3.1 Component ownership

| Component | Sole responsibility | Forbidden ownership |
|---|---|---|
| Catalog services | Logical identity, locations, observed files, associations, metadata, effective search views | Viewer identity, UI state, uncontrolled process creation |
| Viewing services | Profile/timeline progress, manual overrides, session sequence authority | Segment generation or file discovery |
| Delivery services | Source selection, tracks, streaming generations, transport leases, timeline mapping | Inventing catalog identities or rewriting originals |
| Work coordinator | Durable demands, attempts, effect dispatch, recovery, shared budgets | A second catalog or an arbitrary shell-command service |
| Execution adapter | Qualified FFmpeg/FFprobe recipes, subprocess containment and evidence | User authorization decisions |
| API/identity layer | Request identity, policy enforcement, wire contract, scoped events | UI-specific alternate domain rules |
| Go and TypeScript clients | Presentation, user intent, transport retries, local caches | Direct SQLite writes, independent scans, server policy duplication |
| Desktop host | Native windows/tray, secure credential storage, dialogs, installed-server lifecycle, offline local assets | A second server implementation or unrestricted renderer IPC |

### 3.2 Deployment modes

**Local desktop:** installer includes the Rust server, Go terminal binary, web assets, Demuxe assets, FFmpeg/FFprobe, and Electron. Opening Motion attaches to the selected local server or explicitly starts it. Closing the window follows the configured background-service policy; it is not implicitly equivalent to shutting down the server.

**Headless:** the Rust server runs under the host service manager and serves the same web application/API. No Electron or Python installation is needed. A server-only package may omit the Go client, but the full desktop package includes it.

**Remote client:** desktop or terminal client connects to a selected existing server. Connection failure never silently starts a blank local library. All caches, credentials, and IDs are namespaced by `server_id`.

## 4. Repository and dependency structure

Use one Motion repository and one Rust workspace. The following boundaries are fixed; small submodules need not become additional crates.

```text
motion/
  crates/
    motion-domain/            # pure IDs, catalog/viewing/job/delivery policies
    motion-ports/             # typed persistence, filesystem, execution contracts
    motion-catalog/           # catalog/query/curation application use cases
    motion-store-sqlite/      # repositories, one migration registry, transactions
    motion-scanner/           # traversal, coverage, probing, revision observation
    motion-metadata/          # parsing, NFO, provider adapters, metadata/artwork
    motion-work/              # demands, attempts, reservations, effects, recovery
    motion-execution/         # native tools, process supervision, output validation
    motion-playback/          # planner, viewing, delivery, HLS/time-map services
    motion-application/       # composition of cross-domain operations and policy
    motion-server/            # Axum routes, auth ingress, events, asset/media routes
    motion-verification/      # Statelessness adapters, scenarios, oracle harness
  apps/
    terminal/                 # one Go module; motion CLI + motion tui
    web/                      # React + TypeScript + Vite application
    desktop/                  # Electron main/preload and packaged frontend host
  packages/
    api-ts/                   # generated models + typed fetch client
    playback/                 # Motion player coordinator + Demuxe adapter
    host/                     # narrow platform capability interface
    ui/                       # reusable accessible presentation components
  contracts/
    Motion_Server_API_v2.yaml
    fixtures/                 # public request/response and negative examples
  qualification/
    catalog-reference/        # selected Catabolic behavior fixtures
    models/                   # bounds, traces, seed/model/codec identity
    media/ api/ migration/ devices/ packaging/
  migrations/                 # one ordered registry, one integration owner
  packaging/ scripts/ docs/
```

Dependencies point inward: domain has no Axum, SQLx, Tokio runtime, Electron, or Demuxe types. Ports use domain types. Concrete adapters implement ports. Application services coordinate transactions and effects. The server is the composition root. `motion-catalog` does not depend on a `catabolic-*` crate.

Avoid a new giant shared `App` object exposing the entire database, admin secret, tool paths, mutable locks, and every service to every handler. Inject narrow service handles. One shared writer coordinator is acceptable for correctness initially; do not hold it across slow filesystem/network/encoder activity.

### 4.1 Internal service contracts

These are operation contracts, not wire DTOs or mandatory trait syntax:

| Service | Required operations | Transaction/effect boundary |
|---|---|---|
| `CatalogService` | browse, get, create, attach version, preview/commit merge/split, resolve timeline | Mutation, revision update, audit and event record commit together. |
| `ObservationService` | admit, join, start, stage batch, publish covered batch, cancel demand, recover | Admission and publication are separate commits around external traversal. |
| `MetadataService` | propose match, decide, replace contribution, refresh, select artwork | Fetch outside transactions; publish against exact proposal/source revisions. |
| `WorkCoordinator` | submit demand, claim attempt, heartbeat, cancel, validate, publish, retry | State and pending effects persist atomically; dispatch after commit. |
| `ExecutionService` | probe capability, start recipe, observe progress, terminate tree, validate output | Requires a reservation and owned output path; returns evidence, not catalog edits. |
| `PlaybackService` | plan, admit delivery, stage change, activate generation, close, renew lease | Plans have no execution effects; admission revalidates everything. |
| `ViewingService` | read, start authority, record ordered event, override, reconcile offline | Profile/timeline transaction protects manual epochs and sequence ordering. |
| `AccessPolicy` | authorize action/resource, build scoped query constraints, revoke | Evaluated before disclosure; cache tied to principal and policy revision. |

A persistence command may span catalog, viewing-reference, job, audit, and event tables in **one** SQLite transaction. Only the designated use case may make that combined change; direct cross-module table writes from unrelated code remain prohibited.

## 5. Catalog domain and identities

### 5.1 Canonical entities

| Entity | Meaning | Identity and revision rules |
|---|---|---|
| `Server` | Independent Motion installation | Stable server ID; distinct runtime/restore epoch invalidates stale sessions/cursors. |
| `Library` | Logical Movies, TV, Music, Photos, Personal Video or mixed library | Many storage sources; language/provider/access settings are library policy, not filesystem identity. |
| `StorageSource` | Registered read-only root with an expected volume/binding | Stable source ID; binding revision changes only on validated rebind. A path is not sufficient identity. |
| `FileOccurrence` | Observed file at source-relative path | Stable occurrence ID where a move is proven; availability belongs to observations. |
| `FileRevision` | Exact revision evidence and technical stream inventory | Opaque revision ID, optional verified SHA-256, evidence strength, stat/volume identity. Metadata revision is separate. |
| `CatalogItem` | Logical movie, series, episode, artist, album, photo, etc. | Server-local stable ID, external IDs with namespaces, curated fields. Not one item per encode. |
| `Edition` | Movie cut or complete series release | Separate from compression quality; series release determines an ordering domain. |
| `Timeline` | A playable cut/episode/track interval with its own viewing authority | Key for resume/watched state. Position never transfers across unknown timeline equivalence. |
| `MediaVersion` | Technical representation of a timeline | One or more ordered file bindings. Different encodings may share timeline only with explicit equivalence. |
| `FileBinding` | Version-to-file association, part number, optional interval | Supports multipart titles and multi-episode files without cloning file rows. Unknown boundaries remain null. |
| `MediaComponent` | Logical selected audio/subtitle/video role and its compatible occurrences | Occurrence pins the exact file revision and track ID; language alone is not a stable identity. |
| `DerivedAsset` / `Rendition` | Validated generated output | Input revisions, recipe digest, tool/build identity, validation result and owned storage record. |
| `MetadataContribution` | Provider, local/NFO, or scanner-attributed evidence | Source revision, field semantics, precedence, locks, withdrawn/intentional-empty distinction. |
| `Collection`, `Playlist`, `Queue` | Selection, reusable ordering, and current playback ordering | Distinct objects; adding membership grants no new access rights. |

Catabolic already distinguishes files, logical media, associations and relationships; Motion adapts that separation while making Plex-style editions/timelines first-class. This is **not** a claim that every Catabolic type or existing Motion table maps one-to-one. [C1] [C3] [C4]

### 5.2 Required identity behavior

A movie with two cuts, two encodes of one cut, and a backup copy should yield one logical work, two editions/timelines, distinct versions, and separate physical occurrences. A derived rendition remains associated with the chosen timeline. A backup copy is not a new edition. A filename, title/year pair, or byte hash alone must not merge works automatically.

For shows, series releases and episode-order groups retain specials, alternate orderings, and separate progression. A multi-episode file may bind to several episodes; playback/resume requires known intervals or an explicitly shared combined timeline. The implementation must never manufacture split points from episode count.

Originals and generated outputs carry explicit origin/provenance. A scanner must not rediscover the managed output tree and create duplicate original items. Source/data/output overlap checks enforce that boundary.

### 5.3 Merge, split, and correction

Use a two-step preview/commit workflow. The preview records all entity revisions, affected IDs, timeline consequences, provider conflicts, and expected policy revision. Commit either applies the complete authorized change or returns a typed conflict. A stale proposal does not partially apply.

Preserve alias/tombstone mappings so existing URLs and history remain explainable. Merging works does not merge their cuts or choose a resume timestamp. Explicit timeline reconciliation requires its own reviewed evidence. Preserve manual overrides, provenance, asset selections, and immutable audit receipts. A split moves selected associations; it does not copy or delete original files.

### 5.4 Catalog schema outline

The persistence team owns the exact SQL, indexes, triggers, and migration numbers. Required logical table families are:

```text
system_meta, schema_migrations, settings
libraries, sources, library_sources, source_bindings
file_occurrences, file_revisions, file_tracks, observation_batches, directory_coverage
catalog_items, external_identities, item_relationships, item_aliases
editions, timelines, order_groups, order_memberships, media_versions, version_files
media_components, component_occurrences, component_compatibility
metadata_contributions, effective_metadata, metadata_conflicts, match_proposals
assets, asset_references, renditions, artifact_inputs, filesystem_journal
profiles, principals, devices, grants, credentials, browser_sessions
viewing_state, viewing_sessions, viewing_events, offline_events
work_demands, work_attempts, reservations, effect_outbox, schedules
collections, saved_filters, playlists, playlist_entries, queues, queue_entries
delivery_sessions, delivery_generations, content_tickets, downloads
change_events, audit_records, backup_manifests, migration_receipts
```

Use foreign keys and unique constraints for identity tuples, source-relative occurrences, ordered memberships, event deduplication, and active-attempt ownership. Do not store a full media library inside one serialized reducer state. Effective metadata/search views are rebuildable projections of canonical rows, not another authority.

## 6. SQLite, transactions, assets, and events

### 6.1 One database and one migration authority

The server owns `motion.sqlite` on local storage. Media roots may be on a NAS; the SQLite database may not be placed on an unsupported shared network filesystem. SQLite WAL permits concurrent readers and one writer, not independent unlimited writers. Keep transactions short and bound read snapshots. [W1]

One application migration registry replaces competing schema owners. Only A02 assigns production migration sequence numbers. Other agents submit table/index change requests and tests. The deployment takes a verified pre-upgrade backup; schema changes apply only with exclusive instance/migration ownership. A failed migration must not leave clients using a partially upgraded schema.

Use foreign keys, WAL, explicit busy handling, and a durable write setting appropriate to the existing reliability contract. `BEGIN IMMEDIATE` or an equivalent reserved-writer transaction must protect read-modify-write decisions. Do not remove existing writer fencing just to increase apparent parallelism.

### 6.2 Durable effects and process boundaries

A transaction records next domain state, relevant revision increments, audit/event rows, and pending external effect together. Only after commit may the dispatcher execute the effect. Completion re-enters a revision/attempt-fenced use case.

Delivery is at least once across crashes. An effect ID and attempt generation make retries safe; they do not make a filesystem or encoder exactly-once. SQL commit plus file rename is not a single atomic operation. Use an owned staging path and a filesystem journal for publication and cleanup.

### 6.3 Assets and garbage collection

Store large generated media, previews, and new artwork in managed filesystem storage with immutable content identities and SQLite references. Preserve/import existing artwork blobs during migration; movement is not required before the first cutover. Keep data, originals, staging, and managed output roots disjoint.

Publication sequence: allocate intent and unique staging location; produce output; close and validate; flush as required by the platform contract; publish to an immutable owned path; then transactionally mark ready. Journal reconciliation handles crashes between these stages. No ready row points at an unvalidated partial file. A collision must not overwrite another attempt's output.

Garbage collection uses references and runtime leases. A playback lease, active preparation, download package, metadata selection, or backup pin can protect an asset. Cleanup first fences new admission and records intent; file deletion is a recoverable effect. Unknown files are quarantined/reported, not silently adopted. Deleting a managed asset never traverses arbitrary source paths.

### 6.4 Change events without a second catalog synchronization system

All relevant mutations append an event in the same database transaction. SSE events are **authorized invalidation hints**, not canonical entity payloads. Clients fetch the current permitted state. Keep opaque event cursors bound to server/restore epoch, principal/filter scope, and policy revision.

On initial connection or expired/invalid scope, emit `reset` with a fresh cursor. Client clears relevant cached state, opens/subscribes from that cursor, fetches pages, and buffers invalidations during fetch; then refetches affected entities before presenting a settled snapshot. This prevents the subscribe-after-snapshot race. Duplicate invalidations are harmless.

Page cursors pin sort/filter context, not a multi-request frozen database. Never use an ordinary partially paged list as complete destructive reconciliation evidence. Bulk export/removal planning must materialize a bounded snapshot or finish under its own declared complete-selection protocol. Slow subscribers receive a reset or disconnect under bounded queues; they cannot retain unlimited server memory.

## 7. Scanning and filesystem reconciliation

### 7.1 Pipeline and lifecycle

```text
scan demand -> compatible work admission -> owned attempt
            -> root/volume validation -> bounded traversal
            -> staged observations + per-directory coverage
            -> revision-checked publication -> asynchronous enrichment
```

A scan demand belongs to a requester; a physical scan attempt may satisfy compatible demands. Cancelling one demand does not cancel work still required by another. Admission compatibility includes source/binding revision, trust policy, exclusions, dirty generation, mode, completeness requirement, and minimum traversal start time. A new scan must not falsely report a previous observation as fresh. [C2]

One active physical traversal per source is the initial policy. Concurrent sources have bounded shared I/O admission. Filesystem notifications are hints: increment dirty generations, coalesce bursts, and schedule reconciliation. They do not prove absence or replace periodic scans.

### 7.2 Complete and incomplete coverage

Record completeness at the directory coverage actually proven by the attempt. Validated discoveries may publish after partial traversal. Previously known files may be marked missing **only** in covered directories whose enumeration and binding evidence remained valid. Unvisited directories retain previous observations.

Distinguish partial, unavailable, stale, failed, cancelled, and complete. An inaccessible mount, replaced root, resource budget exhaustion, or live but stalled syscall is not a successful empty scan. Coverage caps must produce explicit incomplete results. Do not convert an array length limit into an apparently complete inventory.

### 7.3 Revisions and hashing

Preserve the current exact-verification behavior for imported Motion files and the v1 compatibility route. Unchanged observations can reuse still-valid verification. New/changed files become `observed` first and `verified` only after the configured exact-content verification and probe checks. The first release uses verified revisions for immutable delivery and reusable artifact input; a future weaker/faster policy requires a separate contract, not a silent scanner optimization.

Separate the opaque revision ID, physical fingerprint, and content digest. Hashing is a budgeted task and can be deferred while metadata-only browsing is available. Do not claim that initial directory discovery has verified all bytes or automatically deduplicate because two observed files have equal size.

Open files beneath a validated root using a platform-safe handle-relative strategy. Check identity before and after probing/hashing. File descriptors keep path binding but do not make another process's in-place writes impossible. The serving guarantee is conditional on the documented source-stability assumptions and observed-change checks; do not claim continuous cryptographic immutability without a snapshot or verified copy.

### 7.4 Moves, duplicates, symlinks, and platforms

A unique, strong identity match can preserve a moved occurrence; retain source/path history. Ambiguous copies stay distinct and require reconciliation. Changed bytes at an old path create a new revision and stale affected derived assets. Volume rebind requires a preview, explicit operator action, and verification; never accept a new mounted disk solely because its mount path matches.

The default scanner does not follow symlinks. Do not port Catabolic projection layouts by weakening this rule. Support imported source mappings through validated source roots, not arbitrary URL/path grants. Preserve Unicode and case behavior; if a path cannot round-trip on a supported platform, report a bounded explicit error instead of a lossy conversion.

Windows root/file identity, sharing behavior, path rules, and process cleanup require native adapters and tests. Porting Unix `fcntl`/inode assumptions literally is not a cross-platform implementation. Catabolic's existing Python runtime is not being packaged, so its platform limitations are reference constraints rather than Motion dependencies. [C5]

### 7.5 Required scanner acceptance

The same fixture suite must cover additions, unchanged reuse, rename, copy ambiguity, replacement, root rebind, partial subtree failure, hidden mount, source mutation during hash/probe, cancellation at publication, dirty events during traversal, and restart with outstanding demand. Include real filesystem and SQLite tests alongside model scenarios. Performance campaigns must record files observed, verified, reused, bytes read, errors, memory, queue delay, and reader latency.

## 8. Metadata, matching, artwork, and organization

### 8.1 Metadata ingestion

Implement deterministic filename/tag parsing separately from provider matching. Start with movie and series/season/episode naming, years, common release tokens, specials, edition hints, audio/subtitle labels, and local assets. Parser results are evidence, not automatic write permission.

Use provider adapters for candidate search, exact-identity fetch, permitted artwork, rate-limit/retry handling, and provenance. The first online provider adapter is TMDB for movies/television; its registration/attribution/access terms are a release dependency to verify before enabling it. Local metadata and unmatched playback must work without provider credentials. NFO is a bounded, deliberately supported dialect; document unsupported fields and disable external entity/network expansion.

Each contribution records provider/source key, observation time, adapter version, external identity, language, fields, and errors. Local edits and field locks win under explicit precedence. Absence withdraws a source's contribution; an intentional local blank is distinct from no opinion. Provider conflicts are retained for diagnosis. Source refresh must not erase curation.

### 8.2 Identification workflow

Unmatched/ambiguous items appear in an inbox. Candidate acceptance rechecks file revision, proposal revision, library policy, and target identities. Expose match, fix match, unmatch, defer, manual identify, attach, merge, and split workflows without requiring handwritten SQL. Provider outage delays enrichment but does not invalidate already playable verified files.

Server-normalized external identity namespaces prevent accidental collisions across movie/TV providers. Multiple libraries can reference a work without making private-library membership visible to an unauthorized viewer. Counts, person pages, relations, and recommendations use the same scoped query rules as item browsing.

### 8.3 Artwork and subtitle components

Upload/import assets through bounded validation with safe MIME handling, decoded-size limits, and provenance. Artwork selection is separate from asset creation. Serve through authorized asset routes; do not return arbitrary remote image URLs as trusted local content. External provider fetching uses an allowlisted adapter and SSRF defenses, not a general URL downloader.

Index embedded and sidecar subtitles/audio with language, forced/default/SDH roles, codec, title, and timing evidence. Selection uses component identities and revision-bound occurrences. A choice of commentary must not become the first audio stream after transcoding. ASS/bitmap/fonts, attachment ownership, offsets, and HLS restrictions require route-specific qualification. [C4] [D1]

### 8.4 Browsing, search, and organization

Ship logical poster-grid home/library/detail/season views; one title appears once per relevant library presentation, not once per rendition. Show unavailable, verifying, unmatched, incomplete, and error states explicitly.

Build indexed effective metadata and a tested SQLite search strategy. Normalize searchable text deterministically while preserving display text and aliases. Cursor pages have stable tie-breakers. Search titles, aliases, cast/contributors, tags and relevant metadata; filter/sort under authorization before returning facets/counts.

Use typed saved filters for smart collections, not public arbitrary SQL. Manual collections are unordered curated membership; playlists preserve explicit entry IDs/order; a queue is a particular playback sequence with repeat and a reproducible shuffle seed. Edits are revision-checked. Do not store a player's current queue in a global mutable variable shared across profiles.

## 9. Jobs, FFmpeg, resource admission, and process safety

### 9.1 One execution infrastructure, different work classes

Motion's Rust code owns **both** live media execution and background preparation. Share the native tool registry, command construction/validation, reservations, process supervisor, progress parser, and diagnostics. Keep different domain lifecycles for scanning, preparation, interactive delivery, and maintenance.

Work classes are interactive playback, requested download/preparation, inventory/probe, metadata, speculative preview/analysis, and maintenance. Admission considers CPU/encoder capacity, GPU pipeline capability, source I/O, memory estimates, disk reservations, and fairness. Global concurrency is not separately configured by each agent's module.

Reserve some capacity for interactive work; background jobs use the remaining budget. Preemption means an explicit supported cancellation/restart policy, not assuming FFmpeg can pause/resume every codec safely. FFmpeg thread flags are not a complete OS CPU or memory limit. Publish which limits are hard-enforced on each platform and which are estimates/admission policy.

### 9.2 Durable lifecycle

```text
queued -> admitted -> running -> validating -> publishing -> completed
                     |             |              |
                 cancelling ------+--------------+-> cancelled
                     |
                  failed / interrupted -> explicit retry with new generation
```

Persist demands separately from attempts. Exactly one fenced attempt may publish a given logical result. Request retries with identical source/recipe parameters reuse demand; a changed source or recipe creates new work. Cancellation fences acceptance immediately, but capacity releases only after the execution owner confirms termination or classifies an unkillable worker.

Artifact identity includes exact input revisions, selected components/tracks, recipe version/digest, output policy, and producer build. A temporary live HLS segment is not a durable rendition. Promoting a completed session output requires the normal validation/publication workflow, not copying an entry into the rendition table.

### 9.3 Native subprocess rules

Launch pinned FFmpeg/FFprobe directly with validated argument arrays: no shell, command strings, user executable paths, arbitrary filtergraphs, or uncontrolled environment inheritance. Each execution has one owner, a job/delivery ID, attempt generation, working directory, reservation, process identity, bounded output logs, deadlines, and cancellation token. Restrict input protocols and output paths according to the recipe. Separate process execution provides crash containment but is **not an operating-system sandbox**. [W2] [W3]

Drain progress/stdout/stderr asynchronously and continuously. Bound retained buffers and parse work; a malicious file or tool flood cannot allocate unbounded memory. Startup timeout, no-progress timeout, expected-duration policy, and graceful/forced termination are separate controls. Validate output MIME/container/tracks/duration where required; exit code zero alone is insufficient.

Tokio does not terminate a child merely because its handle is dropped by default. Use explicit tree termination and `wait`/reaping; `kill_on_drop` is a fallback, not the lifecycle contract. Unix process groups and Windows Job Objects need tested adapters. Record process start identity and ownership; never kill an unrelated reused PID based on a stale database number. [W2]

### 9.4 Parent death and blocking I/O

For service mode, use OS service/container containment to own the process tree. For desktop-owned execution where that is insufficient, use a small bundled Rust execution guardian for a whole job, with an inherited control channel. Loss of the parent channel fences execution and terminates its owned FFmpeg tree; no per-frame RPC is involved. This optional same-product helper replaces neither the Rust catalog nor the HTTP server.

A blocked kernel filesystem operation may not stop immediately after timeout or signal. Mark it stuck, retain the capacity reservation, stop repeated replacements, and surface operator diagnostics. Expired leases alone are not permission to oversubscribe a still-live worker. Recovery must reconcile recorded ownership with actual OS execution before retrying.

### 9.5 Safety acceptance

Kill the server/guardian/encoder during admission, execution, validation and publication. Flood stderr, stall a pipe, simulate disk full, lose an acknowledgement, replay a stale completion, and attempt path/protocol escapes. Assert that originals remain unchanged, no stale artifact becomes ready, unrelated API/media serving continues, and resources are released only when justified. Tests must distinguish acceptance fencing from physical process termination.

## 10. Playback, viewing, and segmented delivery

### 10.1 Three separate objects

| Object | Key | State retained across restart |
|---|---|---|
| Viewing session/state | Principal-authorized profile + timeline | Valid history, manual epochs, last accepted sequence/position; old active authority becomes interrupted. |
| Delivery session/generation | Principal + timeline + exact sources/tracks + route | Diagnostic/recovery intent; live transports do not pretend to resume unchanged after restart. |
| Preparation demand/attempt | Exact sources + immutable recipe + authorized owner | Durable work, attempt fences, validation/publication intent and reusable results. |

Closing the last consumer of a temporary delivery normally stops its encoder. It does not cancel an independently requested background optimization. An encoder crash does not erase watched history. An edition change is a different timeline, not merely a quality switch.

### 10.2 Planning and admission

`POST /playback/plans` is a decision operation with no encoder, job, or viewing mutation. It evaluates the exact selected timeline/version, current permissions, available original/prepared sources, client observations, track selection, HDR/subtitle constraints, and resource policy.

The result contains an opaque authenticated short-lived plan token. It binds principal/profile, source and catalog revisions, selected tracks, recipe/pipeline, and expiry. Admission revalidates current authorization, file state, capabilities, and resource availability. A signed token is not a lasting grant or a substitute for rechecking policy.

Possible routes are original bytes, validated prepared bytes, server remux, audio conversion, and video transcode. Demuxe may perform admitted client-local adaptation for original-file routes. Prefer no unnecessary conversion, but do not insist on client software decoding when its measured cost, battery impact, or network load is unacceptable. Unknown support remains unknown and permits a bounded trial, not a fabricated capability claim.

Default automatic recovery is at most three distinct plan candidates per open attempt, with a retained failed-candidate set. An explicit edition/source/track selection never widens silently. Show why fallback is blocked: policy, capacity, missing source, incompatible tracks/HDR, unavailable assets, or failed route.

### 10.3 End-to-end open sequence

1. Read the timeline-specific viewing state and preferences.
2. Request a playback plan with explicit client observations and selected components.
3. Admit a delivery idempotently. Poll or consume invalidations until the first generation is ready.
4. Open the candidate with the client player without discarding a valid old player prematurely.
5. After candidate acceptance, create viewing authority with `expected_viewing_revision`; handle a conflict rather than silently taking over.
6. Play and report consecutive events in **logical timeline milliseconds**.
7. Renew the delivery lease; on close, record the final viewing event and retire delivery. A failed final save is surfaced and retained locally for reconciliation, not represented as saved.

### 10.4 Seek generations and route replacement

Changes stage one pending generation with a new source/track/recipe pin and expected active generation. Seek uses the requested absolute timeline time. Replanning cannot change the timeline of an existing delivery.

Where capacity permits, old playback remains active while the candidate initializes. The client activates the new generation only after it is ready and accepted. Activation fences the prior generation; bounded drain time allows already admitted transfers to complete. Failure leaves the old generation intact where possible. If overlap cannot be admitted, return an explicit disruptive-replacement outcome; never promise gapless or seamless switching without evidence.

Every manifest, initialization fragment, and segment URL includes `delivery_id` and `generation`. A completion or file from an old generation cannot be inserted into the active manifest. Retired-generation requests are denied after authorization even if the file still exists for cleanup.

### 10.5 Initial streaming contract

First qualify one H.264/AAC SDR fMP4 HLS pipeline with selected audio and explicit subtitle policy; then extend remux, audio-only conversion, hardware pipelines, and HDR handling. FFmpeg supports HLS/fMP4 muxing, but that fact does not qualify Motion's complete seek/control path. [W3]

Use distinct master, variant, init-fragment, and segment routes. The server exposes only completed, atomically published segments. A segment URL is an opaque owned object, not a filesystem path. Manifest URLs include explicit segment authorization; a query ticket on the master does not automatically propagate to child requests.

For playback-time VOD transcoding, the first transport may use a rolling HLS window. The Motion API remains authoritative for the **finite logical movie duration** and available interval. Do not label a truncated rolling window as a full static VOD playlist or use an EVENT playlist while deleting its listed segments. The shared player coordinator handles seeks outside the produced window by requesting a new generation, not by seeking to nonexistent HLS data. At actual completion, finish the playlist appropriately.

The generation reports `media_time_origin_ms`, requested start, and observed available interval. The adapter maps player time into logical time and performs any initial keyframe/preroll adjustment. UI duration, watch progress, markers, and seek requests use the logical timeline. A raw third-party HLS player without the Motion control contract can only seek within the offered transport window; the SDK must document this limitation.

Bound encode-ahead work, segment retention, playlist length, and disk consumption. Never expose a growing MP4 under an immutable full-file URL. True multi-rendition adaptive bitrate delivery is a later qualification increment; manually switching prepared files is not ABR.

### 10.6 Audio, subtitles, HDR, and source delivery

Bind chosen audio/subtitle components into each plan and recipe. Validate their occurrences against exact source revisions. Preserve forced/SDH/role metadata and explicit subtitle-off choices. Subtitle burn-in is a video transform, not an audio-only conversion. HDR-to-SDR requires a tested tone-map/color path or a clear unsupported response; do not silently produce washed-out SDR.

Direct media supports GET/HEAD, a verified revision, strong representation identity, conditional requests, and one byte range. Authorization is checked before metadata or existence is disclosed. HEAD has the same permission boundary as GET. Handle source replacement between plan and open explicitly; once serving starts, revalidate according to the source-stability contract rather than claiming another process cannot mutate the file.

### 10.7 Watch semantics

Store resume/watched state by profile and timeline. Alternate encodes with established equivalence share that state; distinct cuts/releases do not. Sessions accept exactly the next sequence; an exact duplicate returns the recorded acknowledgement, a conflicting duplicate rejects, and gaps/stale sessions cannot overwrite current state.

Manual watched/unwatched/reset advances a manual epoch and fences older authority. Completion policy includes explicit ended evidence or the configured progress threshold, plus later marker-aware handling. Do not suppress post-credit viewing merely because an item became watched. Rewatch, season/show bulk actions, next-episode ordering, and autoplay must preserve the selected release and profile policy.

## 11. Public server API

### 11.1 Authority and completeness

The companion [OpenAPI document](contracts/Motion_Server_API_v2.yaml) is the authoritative **wire-shape baseline**. This document defines lifecycle, authorization, migration, and semantic constraints not expressible by JSON Schema alone. The [endpoint index](contracts/API_ENDPOINTS.md) assigns each operation an owner and earliest milestone. These are implementation targets, not evidence of existing handlers.

A00 owns changes to the normative contract. A08 implements HTTP adapters; the named subsystem owner implements the operation. Runtime-generated OpenAPI must be compared with the reviewed contract, not silently replace it. Generated SDKs must be built and exercised against the real server before release. Schema validation does not prove code-generator compatibility or HTTP behavior. [W4]

The API covers the architecture foundation, complete movie/TV workflows, delivery, organization, selected offline operations, and operations/import boundaries. Later music/photo-specific UX, external discovery, cast protocols, and group synchronization have owned extension gates in Section 16. Their complete wire expansions must be added and reviewed **before those agents implement them**; a generic item schema is not claimed to implement every future feature.

### 11.2 Common rules

| Area | Contract |
|---|---|
| Base | JSON API under `/api/v2`; documented media/stream/asset paths under the same prefix. |
| IDs | Opaque server-scoped strings. Clients do not parse UUIDs or embed physical paths. |
| Revisions | Unsigned decimal strings, distinct from content digests and file revision IDs. Do not serialize arbitrary 64-bit counters as JavaScript numbers. |
| Time | UTC RFC3339 timestamps for reporting; safe nonnegative integer milliseconds for media positions. Monotonic runtime time for local deadlines. |
| Pages | `items`, `next_cursor`, `read_revision`, `event_cursor`; limit defaults to 50 and caps at 200. A page is a committed snapshot, not an unlimited stable enumeration. |
| Reads | Deterministic sort with ID tie-breaker; authorization precedes counts, filtering, relationships and pagination. |
| Mutation preconditions | Strong `If-Match` from the target resource; missing 428, stale 412. Subresource creation explicitly uses its owning aggregate ETag. Multi-entity commands carry reviewed expected revisions or a plan token; conflict 409. |
| Retry identity | `Idempotency-Key` on creation/command operations unless intrinsic event identity is specified. Same key+body returns saved acknowledgement; different body 409. |
| Body semantics | Reject unknown structural request fields. Metadata extension maps intentionally allow validated JSON values. Enforce domain combinations even when the schema permits their individual fields. |
| Errors | RFC 9457 `application/problem+json`: stable `code`, request ID, actionable redacted detail, retryable flag. Permission and compatibility errors are not silent fallback. [W5] |
| Events | SSE plus ordinary reads; explicit reset/replay and bounded queues. No client infers success only from an event arriving. |
| Capabilities | `implemented`, `enabled`, and qualification are separate. Exact binary/assets and receipt identities accompany claims. |
| Defaults | Source originals read-only; missing evidence unknown; unsupported feature denied; no implicit admin or remote exposure. |

`If-Match` resource ETags use an opaque strong representation such as `"r-7"`; clients must retain the returned value, not construct it. Collections have aggregate revisions. A `PUT` to a metadata contribution uses the owning metadata ETag, and adding editions/timelines/versions uses the documented parent aggregate ETag. Plain filesystem digests are not metadata ETags.

Default JSON body cap is 256 KiB; artwork permits 8 MiB compressed input with a separate decoded-image cap. List limits do not increase body limits. Larger imports require bounded staged files and a preview protocol. Domain-level validation enforces exactly one content-ticket resource family, correct match decision/candidate combinations, manual versus smart collection fields, same-timeline generation activation, and immutable source/track bindings before effects occur.

### 11.3 API groups and implementation ownership

| Group | Main resources/operations | Service owner |
|---|---|---|
| Identity/system | capabilities, current principal, browser sessions, device pairing/revocation, profile policy | A08 |
| Catalog/storage | libraries, source registration/relocation, file inventory, logical items, relations, editions, timelines, versions | A01/A03 |
| Observation | library scan admission, per-source coverage, demand cancellation | A03 |
| Metadata | contributions, refresh, proposals/decisions, artwork, markers | A04 |
| Viewing | timeline state, manual override, continue watching, next episode, session events | A07 |
| Playback | read-only plan, delivery create/heartbeat/change/activate/close, media and HLS generation routes | A07 with A06 execution |
| Background processing | capability offers, immutable recipes, preparation, jobs/cancel/retry, renditions/schedules | A06 |
| Organization | search, saved filters, collections, playlists, queues | A09 |
| Offline | device download demand/manifest, sync event reconciliation | A13 |
| Operations | diagnostics, storage, backup, interchange preview/apply | A14/A09 |

There is no public arbitrary `run_ffmpeg`, SQL-write, shell, Catabolic proxy, or filesystem-read endpoint. A viewer requests an authorized outcome, not a native command.

### 11.4 Idempotency and concurrency details

Persist the key, principal, operation/target, canonical request digest, status, and enough response data to replay the acknowledgement. Authenticate and authorize before returning a cached result, so revocation cannot leak a prior success response. Domain side effects and the idempotency record share one transaction. Minimum retention is seven days and, for active durable requests, through termination plus seven days. The server exposes the effective limit; clients do not assume indefinite replay after expiration.

Viewing and offline events use their explicit event ID and sequence rather than arbitrary retries that create new events. Creation of viewing authority uses `expected_viewing_revision`, returning 409 on conflict. Delivery changes use `expected_generation`; activation requires the expected previous active generation. Do not use wall-clock timestamps to resolve authority races.

A read-only playback plan may be an authenticated self-contained token rather than a database entity. Its expiry is initially 60 seconds. Resource reservation happens only on admission. An old plan cannot bypass changed library/rating/track policy or source revisions. Replaying a delivery creation with an already retired result returns its prior acknowledgement; it does not recreate a fresh encoder under the old key.

### 11.5 Media protocol

GET and HEAD enforce identical resource authorization and source-generation checks. Byte routes honor the documented strong preconditions, one byte range, and `If-Range`. A failed `If-Match` returns 412; an outdated requested file revision returns 409. An unsatisfiable single range or deliberately unsupported multiple range returns 416 with an appropriate `Content-Range`. HEAD ignores Range and never emits a body/206. Conditional HTTP ordering must follow the tested RFC 9110 contract. [W6]

Media-ticket authentication is an alternative only on media routes; it does not authenticate arbitrary JSON operations. Tickets pin one file revision, download, or delivery generation and intended purpose. HLS child URIs must carry an eligible ticket or a supported cookie/bearer path independently. No ticket authorizes a filesystem prefix. Queries containing secrets are redacted; referrer policy prohibits leakage.

File/asset routes return an appropriate content type after validation. Mutable HLS manifests and credential-bearing responses are not public immutable cache objects. Segment paths are generation-bound and cannot be accidentally shared between principals through a public CDN cache.

### 11.6 Event reset and restore epochs

`Last-Event-ID` takes precedence over `after`. A policy change or restored database invalidates old cursor scope even if numeric event positions overlap. Emit a reset, clear stale client rows, and rebuild authorized views using the overlap protocol in Section 6.4. The server rechecks authorization while streaming and ends revoked sessions. The event loop must not poll SQLite independently per client without a bounded shared notification strategy.

### 11.7 Concrete fixture coverage

The contract package includes machine-checked example objects for a playback request, scan result, viewing event, content ticket request, and offline synchronization. These are authored conformance fixtures, not results obtained from the current server. Implementers must add end-to-end request/response tests for each operation and negative tests for 401/403/404, 409/412/428, source mutation, expired tickets, event resets, and conditional media behavior.

## 12. Identity, authorization, and private networking

### 12.1 Principals are not profiles

A principal is an authenticated person/device/integration identity. A profile is a viewing identity that the principal is permitted to use. Enforce allowed profile switches, library visibility, ratings/labels, unrated policy, processing/download rights, and administration across every route.

Never trust `profile_id`, device name, Host, Origin, or an unverified proxy header as identity. Hiding a restricted title in the UI does not protect its original, subtitle, artwork, thumbnail, segment, search count, or event record.

### 12.2 Local and private access

Default bind is loopback. The packaged desktop establishes a scoped local connection using a protected bootstrap capability delivered through an inherited handle or protected local channel. The capability is short-lived and one-use; it is not placed in argv, a media URL, logs, or a committed config. Loopback is not automatically admin on a multi-user machine.

Preserve a no-login trusted-household mode **only as an explicit deployment setting**. Its synthetic viewer has no administration privileges. It cannot coexist with pretending that household profiles are enforceably restricted. Restricted mode requires paired credentials or verified ingress identity mapped to an explicit principal; missing identity fails closed.

Use external Tailscale Serve as the initial private HTTPS adapter. It may establish upstream identity only through a configured trusted transport/peer boundary; arbitrary loopback or forwarded requests cannot supply a trusted identity header. Tailscale's documentation notes that user identity headers are not populated for tagged devices, so those devices need explicit pairing or another configured identity path. No login-prompt convenience may turn a missing identity into unrestricted access. [W7]

Public internet ingress is not enabled automatically. It requires separate authentication, TLS, exposure, abuse-limit and recovery qualification. Tailscale itself remains an operator-installed networking option, not a reason the application requires a second media-server service.

### 12.3 Browser and native credentials

Browser sessions use HttpOnly, SameSite cookies, Secure over HTTPS, exact trusted-origin handling, and a CSRF token on unsafe requests. The credential/trusted-ingress session exchange also enforces origin and bootstrap policy. A bearer API client is not exempt from authorization merely because it supplies no Origin header.

Desktop/CLI long-lived credentials live in OS-protected storage where available; unsupported storage must fail explicitly or use a user-approved restrictive file with documented limitations. Issue shorter-lived, narrower API tokens for application sessions; do not expose reusable administrator secrets to a remote webpage. Revoke parent credentials and active delivery authorization together.

Initial third-party browser CORS is disabled. Explicitly approved origins may use bearer tokens with a narrow method/header/exposed-header policy. Wildcard credentialed CORS is forbidden. The desktop application's registered local origin is a deliberate approved origin, not a rule allowing arbitrary `file:` or `null` origins.

### 12.4 Revocation guarantees

Revocation denies new API/media admissions and signals active deliveries to stop. Some bytes already admitted to an OS/network transfer may finish before cancellation takes effect. Define and test that interval; do not promise retroactive deletion of already transferred data. Unencrypted offline copies cannot be erased by server revocation alone. Explicitly disclose that limitation in sharing/download UX.

## 13. Go CLI and Charm TUI

Use one Go module and a shared generated/typed Motion client. The terminal binary is `motion`; `motion tui` starts the Charm Bubble Tea/Lip Gloss interface. Command parsing can use Cobra. These are selected implementation tools, with exact versions committed by the release integrator.

```text
motion --server <selected-server> libraries list --json
motion libraries add ...
motion sources add ...
motion libraries scan <library-id>
motion catalog search <text>
motion catalog show <item-id>
motion jobs list
motion jobs cancel <job-id>
motion play <timeline-id>
motion tui
motion serve
motion server status
motion server stop
```

The command spelling is the target interface; current binaries do not yet implement it. `motion serve` locates and delegates to the bundled Rust server; it does not contain a Go server implementation. `server stop` uses authorized lifecycle control and cannot terminate an unrelated process. Local offline maintenance commands delegate to the Rust binary only after it has obtained exclusive data-directory ownership.

The CLI must support stable JSON output, stderr diagnostics, explicit timeouts, cancellation, server/profile selection, and distinct success/invalid-input/auth/conflict/unavailable exit codes. Retries preserve operation identity. JSON mode never embeds terminal control sequences or progress spinners. Machine output schema is versioned separately from terminal formatting.

The TUI provides libraries, logical catalog, search, profiles, scans/jobs, diagnostics, and selected playback control. It does not decode and display video in the terminal. `play` opens the authorized web/desktop player or sends a command to an explicitly paired receiver once that capability ships. It must not place a long-lived token into a browser URL.

TUI reconnect uses event reset plus fresh queries. Slow screens cannot block event draining. Terminal resize, Unicode, keyboard focus, cancellation, server-unavailable state, and credential redaction are acceptance requirements. Agent-owned tests must prove no direct SQLite, scan, provider, or FFmpeg dependencies exist in the Go client.

## 14. TypeScript frontend and Demuxe

### 14.1 Shared application

Select React + TypeScript + Vite for the new application. Share API models/client, domain-neutral UI components, route screens, and the playback coordinator between browser and desktop. Keep a small host-capability interface rather than conditionals scattered throughout features.

Initial screens: onboarding, home/continue watching, libraries, movie/show/season/episode detail, versions/editions, search, matches/corrections, profiles/preferences, processing, sources/scans, and diagnostics. Later add collections/playlists/queues, offline, markers, music/photos, and qualified living-room controls. Loading, empty, partial, denied, verifying, unavailable, retry, and conflict states are real UI states—not one generic spinner/error.

All business operations go through the public API. A host-only file picker returns an explicit user selection; adding a server source is still an authorized server operation. A folder selected on a remote client's machine is not automatically a server-local media source.

### 14.2 Playback coordinator versus player implementation

`packages/playback` owns Motion workflow: read preferences/history, plan, admit, obtain media access, open candidate, create viewing authority, renew lease, submit ordered events, stage/activate replacement, and tear down. Demuxe owns local player state, browser route execution, tracks, subtitles, and decoder lifecycle. Do not fork its reducers or wrap it in a contradictory second decoder state machine.

Use a playback adapter with open/close, play/pause, absolute seek through the Motion timeline mapper, track/quality selection, state/error subscription, and bounded disposal. Serialize critical source changes and use generation ownership to ignore stale callbacks. Each asynchronous operation is cancelled on navigation or ownership replacement; late completion cannot change a new page/player.

### 14.3 Assets and exact-build qualification

Package JavaScript and runtime assets from the same verified Demuxe archive. Store a manifest with source/build identity, file hashes, enabled providers, licensing materials, and browser qualification. Do not dynamically download unpinned codecs into a signed production app. Native browser, WebCodecs, Wasm and HLS paths remain separate capability records. [D1]

Qualify localhost/HTTPS and Electron's chosen custom/packaged origin for workers, fetch/ranges, CSP, WASM MIME, audio, and isolation. Cross-origin isolation is an observed runtime fact, not a configuration assumption. Non-isolated JSPI/Asyncify support is restricted to the selected assets and tested cases; it does not establish Safari, HDR, surround, or unlimited-format support. [D1]

Do not route entire movie payloads through a small custom-source staging API or Electron JSON IPC. Use normal revision-bound HTTP sources or a separately qualified lazy native byte adapter. The earlier Demuxe extension documentation has a bounded custom playback staging path; do not assume it is a general large-file native bridge. [D2]

### 14.4 Client cost and accessibility

Measure client CPU, memory, dropped frames, audible output and stalls as well as server cost. The player can request a server fallback when local execution is inappropriate, but only the server admits it. Route labels and progressing timestamps are not proof of hardware decode or correct sound/color.

Responsive and keyboard/screen-reader behavior is a release requirement. Use explicit focus management, accessible track/quality controls, readable conflict messages, and reduced-motion handling. Native PiP/fullscreen/background behavior requires testing on the actual host, not inference from a web API existing.

## 15. Desktop shell and one-app lifecycle

### 15.1 Shell decision

Electron is the initial production shell because it allows the release to control a bundled Chromium version. That is an engineering choice for qualifying a complex browser media stack, not a claim that Electron makes every codec work. Tauri uses platform webviews and remains an option after its actual target engines pass the required corpus. [W8] [W9] [D1]

Keep Node integration disabled in the renderer, context isolation and renderer sandbox enabled, navigation/popups restricted, and IPC sender/origin validated. Expose task-specific host capabilities, never `exec`, arbitrary filesystem read/write, a raw Electron IPC object, or reusable privileged credentials. An Electron renderer compromise must not imply unrestricted host command execution. [W9]

### 15.2 Origins and connections

Load the bundled TypeScript UI from a registered secure application origin with an explicit protocol handler and immutable local assets. Browser deployment serves the same build from the Rust origin. Desktop domain requests use the same public HTTP API through the selected server connection and a scoped in-memory token; approved CORS is explicit. Media elements receive narrow tickets where headers are unavailable. Disable cross-origin token forwarding and reject unapproved redirects.

The host retains the persistent device credential and obtains short-lived API access for the selected server. The bridge only supplies connection/auth bootstrap and native capabilities; it does not implement alternative catalog or playback use cases. Switching servers clears server-scoped player state, caches and credentials. Remote content is never loaded into a privileged preload context.

The custom origin must pass actual worker, media, isolation, audio and source-network tests. A failing media-origin proof blocks the shell release; do not disable browser security flags to make tests pass. A local HTTP asset-host alternative is an ADR with the same API/auth requirements, not a second domain backend.

### 15.3 Single-instance and server ownership

Use an OS-level exclusive data-directory lock for the authoritative server. A private descriptor records server ID, runtime epoch, endpoint, binary version and startup ownership. It is not itself sufficient authentication. A second desktop/CLI process verifies and attaches to the live instance; a stale descriptor never authorizes PID killing or database writes.

Support explicit `desktop_owned`, `service_owned`, and `remote` attachment modes. Only the actor that owns a local process may terminate it through local lifecycle controls. Closing a desktop window does not stop a service-owned or remote server. “Quit Motion” clearly distinguishes closing the UI from stopping the local server and its active viewers.

Server startup sequence: acquire ownership; validate paths/config; verify binaries/assets; complete migration/recovery; initialize services; publish readiness; accept viewers. Shutdown: stop admission; fence deliveries; flush accepted state/effects; terminate/reap owned executions; close resources; retire descriptor/ownership. Deadlines never justify reporting live workers as dead.

### 15.4 Packaging and updates

One full installer includes native server/terminal tools, Electron, frontend, matching Demuxe providers, FFmpeg/FFprobe, notices, and a release manifest. No Python, pip, cargo, Node development server, or manually managed Catabolic process is required to run it. Headless packages exclude the GUI components.

Sign/notarize installers where applicable; verify update signatures and exact artifacts before execution. Coordinate server, frontend, protocol, tool and schema compatibility. Do not auto-update one embedded component independently unless the declared manifest compatibility range and qualification allow it. Retain a rollback package and verified database/asset backup; restoring a backup is not a general schema downgrade.

Launch targets are macOS arm64 local desktop/server and Linux x86_64 headless server, each gated by tests. Windows and additional architectures are separate build/qualification milestones. This target list is not a claim the new packages already exist.

## 16. Offline, TV, music, photos, and further parity

### 16.1 Managed downloads and offline events

A download is a device-owned demand for exact media plus selected subtitles, artwork and metadata. The manifest pins timeline/source revisions, file sizes and checksums. The client owns local transfer staging, resume, quotas, verified completion and deletion; the server owns authorization and optional durable preparation. Do not call a downloaded source URL a complete offline feature.

The first offline client is desktop, using a private managed asset store; browser offline follows only after storage quota/eviction and lifecycle testing. Offline local data is a cache plus unsynchronized events, not a second authoritative media catalog. It may have its own local storage engine without violating the single server database decision.

Each offline event carries device sequence, event ID, timeline revision, base viewing revision and base manual epoch. Exact duplicates are idempotent. Per-device order is required; clocks are recorded but not used as global authority. Apply progress to current state only when it is causally compatible and no newer manual action/session has precedence. Otherwise retain history and return `history_only` with the current state. A long offline rewatch may validly move backward in its own sequence; do not reduce all progress to a maximum timestamp or maximum position.

On revoke or remove, stop new server access and remove local data only when the client receives and executes that instruction. Never promise remote erasure of unencrypted bytes on a disconnected device.

### 16.2 TV, casting, and remote control

Run an early proof on one physical living-room receiver before promising a broad app matrix. The receiver—not just the controlling phone—must reach authorized media. A sender's Tailscale route does not magically give the receiver that route.

Client/receiver sessions need capabilities, pairing, controller permissions, queues, handoff and reconnect. Remote control is distinct from delivery and viewing authority. Do not conflate casting with screen mirroring or claim Chromecast/AirPlay/DLNA support from a loaded webpage. A13 owns the dedicated device/control API extension and physical qualification; it must be contract-reviewed before implementation.

### 16.3 Music

A15 builds artist/album/album-artist/disc/track relationships, compilations, embedded tags, album art, favorites/ratings, persistent queues, shuffle/repeat, loudness policy, and listening history. Background/offline listening, gapless transitions and output fidelity need audio-specific qualification. Do not treat playing an MP3 in the movie player as music parity.

Generic catalog, assets, queues and viewing primitives are reused; dedicated queries and listening semantics receive explicit contract additions. Preserve potentially different recording, release, track and physical-file identities. Music-specific analysis is low-priority work under the global coordinator.

### 16.4 Photos and personal videos

A15 also owns photo dates/orientation/EXIF handling, album membership, thumbnails, slideshows, and personal-video organization independent of movie-provider matching. Original EXIF/GPS metadata is private by default; derivatives and public metadata apply the disclosure policy. Photo equality, edited versions, and RAW-derived previews require explicit relationships.

Untrusted image decoders and metadata parsers receive size limits and isolation appropriate to the platform. A file extension or server-generated thumbnail does not establish safe decoded dimensions or correct orientation.

### 16.5 Convenience and ecosystem backlog

Keep chapters, server-generated previews, intro/credits markers, correction, dialog/loudness enhancements, subtitle alignment, richer optimization, extras, recommendations and local watchlists in the parity ledger. Marker timestamps and automatic analysis are bound to the correct timeline/revision; uncertainty is visible.

External discovery/availability and group viewing remain M6+ extensions, not local-playback dependencies. Any provider terms, account model, DRM, regional behavior, or commercial rights require separate qualification. Group viewing needs a coordinator, per-member grants and delivery choices, reconnect/drift handling and explicit authority; it cannot be implemented by broadcasting a play button alone.

Catabolic/Plex/Jellyfin interoperability is a previewable interchange import/export feature, not direct reads/writes to their internal SQLite schemas in production. Retain provenance and ID mappings; leave unsupported state untouched and report it.

## 17. Statelessness and the correctness corpus

### 17.1 What using Statelessness means

Production domain code exposes deterministic transitions and decisions with external observations, clocks, identifiers and completion outcomes supplied as inputs. Outputs describe requested effects and accepted domain changes. Tokio, SQL, network, filesystem and native process execution live in adapters. Statelessness wraps the **same** production reducer/decision functions for checks, exploration, fuzzing, shrinking and replay. [S1]

It is not a runtime job scheduler, database, distributed transaction system, or replacement for OS process supervision. Keep normal read queries and media byte transport direct. Model each aggregate/workflow separately and compose only interacting state necessary to check cross-domain invariants.

Use the pinned Statelessness commit from Section 2 for the migration branch and record it in the verification lock/receipts. The package name is `statelessness`, the library name `stateless`; do not assume a newer repository version is published on crates.io. Updating the dependency is a controlled compatibility change. Runtime audit support is optional and bounded; verification may remain a dev/test dependency for the initial production package. [S1]

### 17.2 Required models and property IDs

| Model family | Stable property examples | Required interactions |
|---|---|---|
| Catalog identity | `catalog.originals_immutable`, `catalog.no_implicit_timeline_merge`, `catalog.aliases_resolve` | Merge/split, version attachment, external identity conflict, history references. |
| Observation | `scan.absence_requires_coverage`, `scan.binding_fenced`, `scan.freshness_preserved` | Partial traversal, new dirty event, source replacement, cancellation, shared demand. |
| Jobs/reservations | `job.stale_attempt_cannot_publish`, `job.capacity_tracks_live_owner`, `job.cancel_no_resurrection` | Retry, lease expiry, stuck process, duplicate completion, shutdown. |
| Artifacts/journal | `asset.ready_requires_validation`, `asset.no_unowned_delete`, `asset.pinned_not_collected` | File write/rename/commit failure, restart, download/backup pin. |
| Viewing | `viewing.sequence_monotonic`, `viewing.manual_epoch_wins`, `viewing.timeline_isolation` | Duplicate/out-of-order events, session supersession, edition switch, offline merge. |
| Delivery | `delivery.generation_fenced`, `delivery.track_choice_preserved`, `delivery.closed_denies_admission` | Seek, staged switch, old segment, missing source, cancellation. |
| Access/events | `access.no_profile_impersonation`, `events.policy_reset`, `ticket.scope_not_widened` | Revoke, source URLs, search counts, stale cursor, restore epoch. |
| Cross-system | `system.restart_reconciles_effects`, `system.no_double_resource_release` | Job completion racing scan revision change, GC racing playback, backup racing asset publication. |

A failed search must retain model/codec/property versions, build digest, seed, bounds, original/minimized trace, and expected disposition. A bounded pass proves only the supplied properties within that model and bounds. A liveness claim additionally states progress/fairness assumptions; a permanently blocked filesystem cannot satisfy unconditional eventual-completion assertions. [S1]

### 17.3 Independent oracles and Catabolic fixtures

Select Catabolic test scenarios and translate inputs/observable outcomes into a neutral corpus. Preserve distinctions that matter: scope, order, completeness, attempts, identity equivalence, expected effects and rejected operations. Normalize synthetic paths, timestamps and generated IDs only through declared mappings; do not normalize away stale attempts or ordering defects.

Run the frozen Python reference during development where practical and separately run the new Rust adapter on equivalent fixtures. A reference fixture can be adapted rather than byte-identical when Motion intentionally changes semantics; record the difference and the approved invariant. Do not demand a total Catabolic product port to call the Motion catalog complete.

Statelessness `WithOracle` can compare an independent event ledger/reference implementation with observed production transitions. Do not derive the oracle from the very reducer it is checking. If observing a transition already executed at runtime, do not execute it again just to record it. [S1]

### 17.4 Verification ownership

A05 owns shared corpus formats, model scaffolding, oracle helpers and cross-system campaigns. Every implementation team owns its own production invariants and tests. The verification team must not become a separate owner of a second set of business rules.

Pure models are complemented by real SQLite migrations/concurrency, filesystem faults, child-process crashes/stalls, HTTP media, and browser/device tests. Qualification records must separate source-unit tests, model search, packaged-binary acceptance, audiovisual observations, and performance measurements.

## 18. Migration and compatibility

### 18.1 Scope of replacement

Refactor the existing server and preserve useful reducers/adapters/tests. Replace or extend the file-oriented catalog with the Motion-owned domain described here; do not delete the current implementation first and reconstruct behavior from README claims.

The migration input is an existing **Motion** database and configuration. A Catabolic database import is optional later interchange work, not the primary migration path. Existing Catabolic Python users, CLI contracts and PyPI releases are untouched by this project.

### 18.2 Migration sequence

| Step | Action | Required evidence |
|---|---|---|
| 1 | Freeze the current Motion commit, schemas, v1 request/response behavior and packaged assets. | Baseline tests run against a disposable installation; failures recorded, not hidden. |
| 2 | Snapshot database, immutable asset inventory, config and relevant ownership data. | Actual restoration succeeds before schema cutover is attempted. |
| 3 | Add the new schema/identity structures and compatibility ID map under one migrator. | Foreign-key and uniqueness checks; no loss of original data or provenance. |
| 4 | Convert library-root records into logical libraries plus sources. | Old IDs/URLs map deterministically; same roots are not rescanned as duplicates. |
| 5 | Attach existing items, editions, files and derived outputs to explicit timelines/versions. | Conflicts and ambiguous histories are preserved in a receipt. No guessed cut equivalence. |
| 6 | Run new reads and selected scan/publication operations against fixture copies. | Old/new semantic comparison and intentional differences approved. |
| 7 | Quiesce old writer paths and enable one new authority. | No simultaneous old/new catalog mutation. v1 routes call new services. |
| 8 | Ship incremental client adoption and recovery/rollback instructions. | Real web/CLI/TUI workflows, source replacement, restart and restore pass. |
| 9 | Retire legacy code only after compatibility gates and deprecation notice. | No hidden endpoint, import dependency, or live old-writer call remains. |

Shadow comparisons use read-only snapshots or a disposable cloned database; they do not run two live scanners publishing to the same source of truth.

### 18.3 Legacy viewing and file revisions

Preserve existing file/content identities and digest evidence. If old progress can be attributed to an exact edition/source, migrate to the corresponding timeline. If multiple cuts share an ambiguous legacy progress record, preserve it separately with a migration warning; do not silently copy it to every cut. User correction can resolve the ambiguity.

Old numeric metadata revisions remain mapped; file revision strings and old range URLs stay supported where their semantics can be preserved. Source replacement must still invalidate old URLs. Archive old viewing/job history rather than fabricate new execution events.

### 18.4 API compatibility and security

`/api/v1` is an adapter layer over the new authority until its documented deprecation gate. Preserve supported v1 schemas and behavior or return explicit typed incompatibility when the new timeline model cannot faithfully project a result. Do not return a random edition merely to make an old response nonempty.

Security is not frozen at an unsafe boundary: once restricted mode is enabled, v1 routes apply the same effective authorization as v2. Existing unauthenticated clients may need pairing under that mode; document the intentional change rather than preserving a bypass.

### 18.5 Failure and rollback

A failed upgrade restores the verified pre-upgrade database and compatible assets/binary. Do not assume old code can open a newer schema. Preserve both backup and migration receipt. Restore generates a fresh runtime/cursor epoch and revokes ephemeral deliveries/tickets; outstanding durable work reconciles before retry. No migration cleanup deletes original files.

## 19. Parallel-agent execution plan

### 19.1 Workstreams

The package contains [individual agent briefs](agents/README.md). Each agent receives this document, the API contract, the parity ledger, and its brief. These are **work assignments**, not claims that agents have already been launched.

| Agent | Owned subsystem and paths | Initial dependencies | Completion proof |
|---|---|---|---|
| A00 | Architecture/contracts; `docs/adr`, `contracts`, contract change control | Source baselines | Frozen IDs/errors/lifecycles, reviewed OpenAPI, typed contract fixtures. |
| A01 | Catalog domain/use cases; catalog modules in `motion-domain`, `motion-catalog` | A00 contracts | Timeline/edition/move/merge semantics and golden fixtures. |
| A02 | Persistence/migrations; `motion-store-sqlite`, `migrations` | A00/A01 identity model | Verified Motion upgrade, transactions, indexes, restore and conflict receipts. |
| A03 | Scanning/filesystem; `motion-scanner`, source ports | A00/A02 ports | Real partial/unavailable/move/stale/cancel tests and model adapter. |
| A04 | Metadata/artwork/markers; `motion-metadata` | A00/A01 interfaces | Fix-match survives refresh; bounded NFO/provider/assets; selected component evidence. |
| A05 | Statelessness/corpus; `motion-verification`, qualification models | Production transition contracts | Independent oracles, minimized replay traces, cross-workflow fault cases. |
| A06 | Jobs/admission/process execution; `motion-work`, `motion-execution` | A00/A02 contracts | Native tree cleanup, stuck-worker accounting, durable publication and fairness. |
| A07 | Viewing/delivery; `motion-playback`, relevant domain modules | A00/A01/A06 | Timeline state, generation changes, HLS before-complete, exact tracks, cleanup. |
| A08 | HTTP/auth/events; `motion-server`, policy application modules | A00 ports | Real contract conformance, profile isolation, no v1/ticket/event bypass. |
| A09 | Search/collections/queries/interchange; organization modules | A00/A01/A02 | Scoped counts, queues, saved filters, preview/apply import receipts. |
| A10 | Go client; `apps/terminal` | A00 mock server/SDK shapes | CLI/TUI workflows, retry identity, cancellation, reconnect; no DB dependency. |
| A11 | Shared TS app and Demuxe; `apps/web`, `packages/ui`, `packages/playback` | A00 mocks, A07 adapter contract | Real open/seek/switch/resume with exact assets; accessible complete screens. |
| A12 | Desktop host; `apps/desktop`, `packages/host` | A00 connection/auth contract | One-app lifecycle, sandbox, credentials, native dialogs, packaged media proof. |
| A13 | Offline and qualified external clients; offline modules/client cache | A00/A07/A08 | Verified offline media, causal history merge, early physical receiver proof. |
| A14 | Operations/release integration; packaging, CI, release manifests | All artifact owners | One-install, headless, signing, recovery, upgrade, inventory and qualification. |
| A15 | Music/photos/personal media; dedicated feature modules | A01/A04/A09/A11 | Media-type-specific metadata, audio/visual lifecycle/privacy acceptance. |

A08 owns the HTTP adapter, not every service body. A01/A07/A06 own distinct subdirectories inside domain crates. Shared-file changes go through the designated owner; do not assign every agent ownership of `lib.rs`, `Cargo.lock`, and the entire migration folder.

### 19.2 Parallelism and integration order

**Wave 0:** A00, A01, A02 and A05 establish identities, ports, schema change protocol, and fixtures. A11/A12 run the exact Demuxe/shell proof; A13 investigates receiver reachability; A14 establishes clean package baselines.

**Wave 1:** Once interface baseline 1 is committed, A03/A04/A06/A07/A08/A09 implement independently against ports; A10/A11/A12 build against schema-valid mocks. Mocks must identify themselves and cannot be counted as backend completion.

**Wave 2:** Integrate source -> scan -> logical browse -> direct play -> viewing state across real clients. Advance durable preparation and generation-based streaming with real FFmpeg.

**Wave 3:** Complete advanced catalog organization, offline, clients, convenience features, music/photos, operations and broader parity qualification. API additions precede dependent implementation, rather than agents inventing incompatible private endpoints.

### 19.3 Ownership rules

A00 approves wire changes and shared type decisions. A02 owns migration numbering and cross-table transaction changes. A14 owns root lockfiles, build manifests and installer integration. Agents use separate branches/worktrees and merge small tested vertical changes; no parallel blind pushes to the same main-branch files.

No agent may change a shared contract by editing generated clients directly. Propose a contract diff, update fixtures, regenerate SDKs, then coordinate affected owners. Changes to invariants need a rationale and new regression case. Tests must not weaken assertions merely to make an implementation pass.

### 19.4 Required handoff from every agent

Each handoff includes owned changes, public/internal contract changes, dependencies, migrations, executable test commands, actual results with environment identity, known limitations, negative/fault cases, and rollback impact. “Implemented” means code exists with evidence; “qualified” additionally means the exact delivered combination passed its declared matrix. Unexecuted tests are labeled unexecuted.

The acceptance commands in briefs are **target commands to add** where the repository does not already provide them. No brief assumes a build system or test harness exists merely because it is named in this plan.

## 20. Milestones and acceptance gates

| Gate | Required outcome | Release-blocking checks |
|---|---|---|
| M0 — Contracts and proof | Final contract/version in repo, source fixtures, one schema owner, selected Demuxe/Electron media proof | No unresolved canonical ownership; no duplicate runtime catalog; source/asset mismatch fails clearly. |
| M1 — Rust catalog foundation | Motion-owned single DB, source/library split, identity/timelines, guarded scans, basic auth/v2, migration adapters | Incomplete scan cannot erase unseen media; migration/restore preserve IDs/history; restricted raw URLs fail. |
| M2 — One-app vertical slice | Installer opens desktop; Go/TUI/browser see same catalog; original playback and ordered resume | No Python/Catabolic service; one server instance; clean machine install; cross-surface restart/resume. |
| M3 — Complete library and preparation | Matching/fix-match/NFO/artwork, search/organization, durable renditions and schedules | Curation survives scans; wrong match corrected; selected tracks retained; stale attempt rejected; no duplicate browse rows. |
| M4 — Playback service | Uncached streaming conversion, generation seeks/switches, shared resource budget, selected subtitle/HDR policy | Playback begins before full encode; far seek works; cancel/source change/restart do not expose stale segments or leak capacity. |
| M5 — Convenience/offline/client breadth | Markers/previews, managed offline, desktop polish, selected TV/cast workflows and advanced playback | Actual offline output and conflict merge; physical receiver route; exact audio/subtitle/display qualification. |
| M6 — Broader personal media/operations | Dedicated music/photos, scoped integrations/imports, supported platform expansion, parity ledger completion | Media-specific fidelity/privacy, signed upgrades/restore, documented exceptions; no broad parity claim with missing rows. |

Milestones describe dependency and acceptance order, not delivery dates. A feature can be developed ahead against a contract, but cannot bypass prerequisite safety gates. Optional external/commercial/social work remains separately approved; “all M6 complete” does not imply those products exist.

### First integration demonstration

For M2, install Motion on a clean target. Launch desktop, open the web client, and attach the TUI. Add a source/library through one surface, scan, inspect a discovered logical item, and see it elsewhere. Play a qualified original, change its supported tracks, stop, resume on another client, restart the server, and recover the same authoritative state. M3 extends this demonstration with automatic matching, correction and background conversion visible on all surfaces. No manual Python process, package-manager setup, second authoritative database, or private UI endpoint is allowed.

## 21. Qualification, operations, and release

### 21.1 Evidence layers

Use separate receipts for: pure unit tests; Statelessness models; persistence/filesystem/process integration; public HTTP contract; generated SDK builds; browser/player behavior; installed package; physical device/audio/HDR; and performance. A pass in one layer is not automatically transferred to another.

Every receipt records Motion commit, schema level, contract digest, Statelessness/model versions where applicable, native tool hashes, Demuxe archive and enabled providers, browser/shell/OS/hardware, fixture IDs, network topology, commands, observed outcomes and limitations. A changed media provider or player archive invalidates affected previous qualification.

### 21.2 Required fault and behavior matrix

| Family | Minimum cases |
|---|---|
| Catalog | Copies versus versions versus cuts; external ID collision; duplicate/split reconciliation; multipart/multi-episode; retained aliases/history. |
| Scanning | Partial directory; unmounted/replaced root; same-path changed bytes; moves; notification loss; locked/stuck I/O; cancellation before publish. |
| Persistence | Concurrent readers/writer; busy retry; migration failure; journal crash points; live-backup restore; policy/cursor epoch after restore. |
| Process | Parent/child crash; orphan attempt; PID reuse; stdout/stderr flood; full disk; hung encoder; outdated completion; hard/soft budget evidence. |
| Media | Original/range/conditions; remux; audio/video conversion; long/far seeks; track switching; subtitle render/burn; explicit HDR behavior. |
| Access | Restricted profile guessing; list/facet/count leak; media HEAD; sidecar/artwork; HLS child URLs; expired ticket; stale v1 path; CSRF and origin. |
| Client | CLI JSON/exit codes; TUI cancellation/reconnect; web focus/accessibility; desktop sandbox/lifecycle; real media open/fallback/dispose. |
| Offline | Partial download; checksum mismatch; quota; device restart; disconnected playback; reconnect after manual override; revoked credentials. |
| Operations | Clean install; relocated package; service attach; incompatible component; signed update rejection; supported rollback/restore. |
| Personal media | Music album/disc/compilation ordering; gapless/background audio; photo orientation/time/GPS policy; personal video without matching. |

### 21.3 Performance budgets

Proposed first qualification budgets: p95 ordinary indexed browse/search at or below 250 ms on the frozen 10,000-item local reference catalog; p95 direct-play startup within 2 seconds; simple uncached SDR conversion startup within 8 seconds; seek recovery within 3 seconds on the selected pipeline. These are **targets, not measurements**. Define hardware, fixture, cache state, topology, client and concurrency before collecting results.

Test 10,000 logical items and a source corpus near the current 100,000-file boundary before increasing caps. Publish bytes hashed, source I/O, memory, writer delay, API latency and simultaneous playback cost separately. Do not claim a full-file rescan is cheap based only on metadata reuse cases.

### 21.4 Backups and restoration

Use SQLite's supported snapshot/backup mechanism, not an arbitrary copy of a live main database file while ignoring WAL state. [W10] Pin immutable assets referenced by the snapshot until the backup manifest completes, or briefly quiesce publishers while establishing a snapshot and pin barrier. Newly published assets after that barrier need not be included; referenced assets must not disappear during copying.

A backup includes database snapshot, schema/contract/build versions, configuration, asset inventory and checksums, source-binding identities and a receipt. Secrets use a separately documented encrypted/export policy; sharing a support bundle must not export credentials. Original media is not automatically backed up by a catalog snapshot. Regenerable unpinned cache may be omitted; user-uploaded irreplaceable assets require inclusion or an explicit incomplete-backup warning.

Restore into a new destination, validate integrity/references/assets, establish a fresh epoch, and only then switch the active installation. Test actual restore before claiming backup support. Restoring a snapshot does not make missing NAS files reappear.

### 21.5 Observability and privacy

Expose active deliveries, actual selected routes, reasons, selected components, measured transfer/stall counters, attempts, reservations, queue age, source health and disk pressure. Unknown CPU/hardware/throughput observations stay unknown. Logs are bounded and redacted; diagnostic bundles require explicit export. Paths, title history, credentials and metadata may be sensitive.

The server health endpoint means process liveness; readiness and per-feature capability are separate. A working HTTP listener with a failed migration is not ready. Probe dependencies before announcing unavailable hardware as supported. Long-running work never blocks diagnostics or unrelated media delivery behind a global lock.

## 22. Risks, change control, and completion

| Risk | Mitigation and stop condition |
|---|---|
| Repeated architectural reversals | D01-D12 are binding for this version. New Catabolic integration/rewrite/PyPI work requires a separate proposal, not an agent side quest. |
| Porting only happy paths | Reference register, negative fixtures, partial coverage, journals and actual crash tests. No deletion of old writer before migration proof. |
| Timeline corruption | Explicit cuts/order domains, recoverable legacy history, no inferred position equivalence. |
| Duplicate rule implementations | Shared production reducers; one catalog and one job/FFmpeg execution infrastructure; API clients own no business policy. |
| Process-isolation overconfidence | Subprocess is not sandbox; tested containment, argument/IO limits, live-process accounting and parent-death handling. |
| API/model drift across agents | A00 contract ownership, A02 migrations, generated-client comparison and real HTTP conformance. |
| Device/codec overclaims | Exact package/asset/device receipts; shell proof; distinguish implemented, enabled, and qualified. |
| Event/security race | Transactional events, scoped cursor epochs, reset/overlap resync, revocation before cached response disclosure. |
| Product scope disappearing during rewrite | All original G01-G45 items mapped to owners/gates or explicit exclusions in the parity ledger. |
| Catabolic/Motion divergence | Pin selected reference scenarios and record intended differences; do not imply ongoing automatic parity with all upstream behavior. |

A module is complete only when its owned behavior, public/internal contract, persistence/recovery, authorization, user surface where applicable, and negative tests are present. A release capability is qualified only on its named delivered matrix. A finalized design document does not certify an implementation.

**Final acceptance statement:** Motion is one installed product with a Motion-owned Rust catalog and media server, one authoritative SQLite database, one public API, supervised native media workers, shared TypeScript/Demuxe presentation, Go terminal clients, and a thin Electron host. Catabolic remains a separate reference project. Statelessness exercises actual production transitions. The full personal-media goal remains visible and testable throughout the migration.

## 23. Sources and accompanying artifacts

### 23.1 Package contents

| File/folder | Purpose |
|---|---|
| `Motion_Final_Architecture_and_Implementation_Plan.md` | This self-contained normative design and implementation plan. |
| `contracts/Motion_Server_API_v2.yaml` / `.json` | Machine-readable implementation contract; all operations carry owner/milestone metadata. |
| `contracts/API_ENDPOINTS.md` | Generated operation index. |
| `contracts/CONTRACT_EXAMPLES.json` | Positive and negative schema fixtures, authored for this design. |
| `agents/README.md`, `agents/A00-*.md` … `A15-*.md` | Ownership, dependencies, tasks and acceptance brief per workstream. |
| `qualification/PARITY_LEDGER.md` | G01-G45 traceability retained from the earlier review, with final ownership/milestones. |
| `qualification/CATALOG_REFERENCE_REGISTER.md` | Selected Catabolic behaviors and explicit non-goals. |
| `qualification/DOCUMENT_VALIDATION.json` | Checks actually run on this deliverable and their limitations. |
| `validate_design.py`, `validation_requirements.txt` | Reproducible static contract/package checks and pinned validator dependencies; not a Motion runtime test. |
| `SHA256SUMS` | Integrity inventory for the delivered bundle. |

### 23.2 Source notes

Repository facts are pinned below. Prior comparisons are used as a requirements input, not presented as a newly executed Plex/Jellyfin/Emby benchmark. The architectural decisions, proposed budgets, agent assignments and endpoint shapes are this document's design. External documentation supports the narrow implementation constraints cited, not the completeness of Motion.

[M1]: https://github.com/Jagalite/motion/blob/52491bf3e6ef323a33dd546687129bdb3b51d4d1/README.md
[M2]: https://github.com/Jagalite/motion/blob/52491bf3e6ef323a33dd546687129bdb3b51d4d1/DESIGN.md
[M3]: https://github.com/Jagalite/motion/blob/52491bf3e6ef323a33dd546687129bdb3b51d4d1/API.md
[C1]: https://github.com/Jagalite/catabolic/blob/8389e66b01a102b2107ee8ae9f0c00161d17d267/docs/DESIGN.md
[C2]: https://github.com/Jagalite/catabolic/blob/8389e66b01a102b2107ee8ae9f0c00161d17d267/docs/OBSERVATIONS.md
[C3]: https://github.com/Jagalite/catabolic/blob/8389e66b01a102b2107ee8ae9f0c00161d17d267/docs/MEDIA_MODEL.md
[C4]: https://github.com/Jagalite/catabolic/blob/8389e66b01a102b2107ee8ae9f0c00161d17d267/docs/COMPONENTS.md
[C5]: https://github.com/Jagalite/catabolic/blob/8389e66b01a102b2107ee8ae9f0c00161d17d267/src/catabolic/database_io.py
[S1]: https://github.com/Jagalite/statelessness/blob/b97423d2bc01b61eee25b50a44e4b6416851b6b6/README.md
[D1]: https://github.com/Jagalite/demuxe/blob/f32afd60db1787600229823d13c668a812da4dca/README.md
[D2]: https://github.com/Jagalite/demuxe/blob/fb52a6b00df7abf862a988c895ad159fc11aca47/docs/API-EXTENSIONS.md
[W1]: https://www.sqlite.org/wal.html
[W2]: https://docs.rs/tokio/latest/tokio/process/index.html
[W3]: https://ffmpeg.org/ffmpeg-formats.html
[W4]: https://spec.openapis.org/oas/v3.1.0.html
[W5]: https://www.rfc-editor.org/rfc/rfc9457.html
[W6]: https://www.rfc-editor.org/rfc/rfc9110.html
[W7]: https://tailscale.com/docs/features/tailscale-serve
[W8]: https://v2.tauri.app/reference/webview-versions/
[W9]: https://www.electronjs.org/docs/latest/tutorial/security
[W10]: https://www.sqlite.org/backup.html
[R1]: qualification/PARITY_LEDGER.md
