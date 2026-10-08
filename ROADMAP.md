# Playscale roadmap

Playscale will provide a Plex-style media experience using a Rust server, Tailscale access, and Demuxe playback. The feature scope below is agreed; phase ordering is proposed and carries no release dates. Checked items are implemented; broader unchecked items may have partial core support.

The first implementation covers local cataloging, original/rendition delivery, selectable profiles and progress, source-attributed metadata/tags, and a minimal Demuxe UI. [Implementation plan](IMPLEMENTATION_PLAN.md), [API contract](API.md), and [validation](VALIDATION.md) define the precise boundary. Fixed local FFmpeg processing recipes are implemented; delegated Catabolic processing/callbacks remain planned.

See [the design](DESIGN.md) for architecture and access decisions. Anyone permitted to reach Playscale through Tailscale can access the application without a separate application login. Profiles personalize viewing; administrative and parental restrictions need an explicit enforcement design.

Live TV, tuner integration, electronic program guides, DVR recording, and recording-specific commercial detection are out of scope. Cataloged television series and episodes remain in scope.

## Public API across every phase

A full public API is an agreed requirement. The bundled webpage and third-party applications use the same documented interface. Each feature's API ships with that feature, starting in phase 1.

- [x] Publish a versioned HTTP API and OpenAPI specification with runnable examples.
- [ ] Cover libraries, items, metadata, artwork, search, profiles, preferences, watch history, and playback sessions.
- [ ] Add collections, playlists, versions, editions, preview and skip markers, processing, downloads, other media, discovery, and group viewing as those features ship.
- [ ] Expose administration, settings, jobs, and server diagnostics under the chosen authorization policy.
- [ ] Document media delivery, range requests, source revisions, and playback fallback for independent clients.
- [x] Provide events for job progress, catalog changes, playback sessions, and other live state.
- [ ] Define pagination, filtering, sorting, errors, idempotency, concurrency behavior, and compatibility/deprecation rules.
- [ ] Define browser-origin/CORS handling and protection for state-changing requests while preserving default Tailscale access without a separate application login.
- [ ] Validate contracts and maintain an independent example client proving that core flows require no private frontend endpoints.

Third-party application support is a release criterion, not a later wrapper around the bundled UI. Client SDKs can be added after the contract is established; SDK languages remain open.

## Phase 1 Core library and viewing

- [x] Implement small functional cores with imperative adapters; dogfood Stateless against the production job transition logic.
- [x] Pin a reproducible Stateless source dependency and retain replayable regression cases for cancellation, stale results, retries, and recovery.
- [x] Configure multiple media folders with read-only access to originals.
- [ ] Scan for new and changed files; reconcile moved, missing, and replaced files without silently losing metadata or history.
- [x] Expose explicit movie/series/season/episode structure, edition management, metadata validation, and artwork import/selection APIs.
- [ ] Build the library UI for movies, shows, seasons, episodes, and specials.
- [ ] Probe technical metadata and model content, editions, versions, files, and tracks separately.
- [ ] Match titles to metadata providers and obtain descriptions, posters, cast, genres, and release dates.
- [ ] Correct matches manually, edit metadata and artwork, and preserve edits across refreshes.
- [ ] Browse poster grids, recently added items, and detail pages.
- [ ] Search titles and cast; filter and sort the library.
- [ ] Serve the webpage, matching Demuxe assets, API, and range-enabled media URLs over private HTTPS.
- [x] Build the bundled webpage on the public API and publish its initial specification and independent client example.
- [ ] Play original files with seeking, audio-track selection, and embedded or external subtitles through Demuxe.
- [x] Persist preferred audio and subtitle languages, subtitle behavior, and playback quality hints through the profile API.
- [x] Apply saved preferences in the bundled player.
- [ ] Provide viewer profiles with separate resume positions, watched status, and preferences.
- [x] Expose continue watching, manual watched/unwatched overrides, and next-episode lookup APIs.
- [x] Persist playback sessions with sequence ordering, duplicate handling, and superseded-session protection.
- [x] Integrate session APIs, continue watching, watched controls, and autoplay into the bundled UI.
- [ ] Show basic scan, job, active-stream, and playback-error diagnostics.

Completion means a viewer can discover a title, play it, change tracks, stop, and resume on another device under the same profile. Validate the selected browser/device matrix, byte ranges, seeking, disconnects, and direct versus relayed Tailscale playback.

## Phase 2 Organization and playback conveniences

- [ ] Create manual collections for franchises, themes, and curated groups.
- [ ] Create smart collections driven by saved filters.
- [ ] Build ordered playlists with playback queues.
- [ ] Select between quality versions and distinct editions, including theatrical and director's cuts.
- [ ] Define how progress and watched state relate across versions and editions with different timelines.
- [ ] Browse chapters and generate cached timeline preview thumbnails.
- [ ] Detect intro and credit boundaries and expose skip controls through the player integration.
- [ ] Allow correction of generated skip markers and preserve post-credit viewing behavior.
- [ ] Add household parental restrictions and library visibility controls without changing the default Tailscale access policy.

Completion means organization and preferences survive rescans, version selection is explicit, and generated preview/skip data is tied to the correct file revision. Restricted profiles must not be bypassable merely by selecting an unrestricted profile.

## Phase 3 Server processing and operations

- [x] Add strict JSON configuration, readiness and private diagnostics.
- [x] Provide a per-user macOS login service with automatic restart and local binary/assets.
- [x] Provide consistent SQLite snapshot backups and non-overwriting, integrity-checked restore.
- [x] Add automatic backup scheduling/retention and bounded application log rotation.
- [ ] Add signed release packaging.

- [ ] Negotiate explicit server fallback with Demuxe for compatibility, bandwidth, or client processing limits.
- [x] Support remuxing, selected-audio conversion, and video transcoding as distinct operations.
- [x] Plan Auto, strict Original only, and Convert playback with profile defaults, per-view overrides, revision-bound client evidence, and explicit preparation proposals.
- [ ] Stream transcoding output during encoding, with seek-aware sessions and bounded segment retention.
- [x] Validate explicit macOS VideoToolbox H.264 processing and expose successful-job evidence.
- [ ] Expand hardware discovery/validation to additional platforms and codecs.
- [ ] Add output quality selection and adaptive delivery where appropriate.
- [ ] Define HDR handling, tone mapping, subtitle burn-in, and GPU-to-CPU fallback behavior.
- [x] Persist jobs with bounded concurrency, progress, cancellation, retries, and restart recovery.
- [ ] Manage generated renditions, previews, and artwork with cache budgets and cleanup policies.
- [ ] Expand the server dashboard with active streams, delivery mode, transcoding reasons, resource use, and bandwidth.
- [x] Persist scheduled scans and generated-cache maintenance, with job status and bounded encoder diagnostics.
- [x] Add automated backup retention, terminal-history retention, and free-space admission.
- [ ] Expand cache maintenance to previews/artwork and additional disk-pressure policies.
- [ ] Provide backup, restore, database migrations, and versioned application/runtime updates.

Completion means representative conversions work on the supported hardware matrix, stopped sessions release resources, and failures cannot leave unbounded workers or cache growth. Hardware support must be measured for the actual processing pipeline.

## Phase 4 Offline access and client expansion

- [ ] Download originals or prepared versions for offline viewing.
- [ ] Manage download progress, cancellation, local storage limits, and removal.
- [ ] Reconcile offline watch history and resume updates after reconnecting.
- [ ] Expand desktop and mobile client experiences, including native packaging where needed.
- [ ] Add TV clients with remote-friendly navigation.
- [ ] Support casting and remote playback control on selected platforms.
- [ ] Define Tailscale connectivity, secure-origin, and playback capability requirements per client.

Completion is platform-specific: offline playback must work without server connectivity, and cast receivers must be able to reach their media source. A desktop browser pass does not establish TV or mobile support.

## Phase 5 Music and personal media

- [ ] Add music libraries with artists, albums, tracks, tags, artwork, search, and playlists.
- [ ] Provide a dedicated music listening experience inspired by Plexamp, with a persistent queue, shuffle, repeat, gapless playback where supported, and offline listening.
- [ ] Add photo libraries with thumbnails, albums, metadata, browsing, and slideshows.
- [ ] Add personal video libraries that work without movie or television metadata matching.
- [ ] Apply profiles, collections, access policy, and job/cache management consistently across media types.

Completion means each media type has an appropriate browsing and playback experience while originals remain unchanged. Music-specific and photo-specific behavior must be validated independently from movie playback.

## Phase 6 Discovery and shared viewing

- [ ] Add a universal watchlist spanning local titles and externally discovered titles.
- [ ] Show recommendations and related titles using available metadata and viewer preferences.
- [ ] Integrate external streaming availability and clearly distinguish locally playable titles from provider links.
- [ ] Explore an external on-demand catalog, including free or ad-supported provider integrations where provider agreements and playback APIs permit. This does not include live channels.
- [ ] Add synchronized group viewing with shared play/pause/seek, participant status, reconnect handling, and playback drift correction.

External catalog delivery depends on provider permissions, available APIs, and any DRM requirements; it does not imply access to Plex's own catalog. Synchronized viewing is a Playscale goal regardless of Plex's current support for its historical Watch Together feature.

## Decisions before implementation

- Choose the launch browsers, server platforms, and Tailscale deployment method.
- Choose metadata providers and local metadata import formats.
- Define profile selection, administration, and enforcement of parental restrictions.
- Specify the Demuxe integration and server-fallback contract.
- Define the public API's event transport, versioning, compatibility, browser-origin policy, and privileged-operation model.
- Establish stable content identities and source revisions before adding derived assets and offline synchronization.
- Select the first hardware-transcoding targets and later TV/casting platforms.
- Define packaging, update, licensing-material, backup, and recovery requirements.

The phases organize delivery rather than imply that every feature is needed for launch. Persistent jobs, identity, profiles, and the networking boundary should be designed early so later phases can build on them.

## Core operations milestone

- [x] Incremental file inspection with explicit full verification and inspection counts.
- [x] Nonblocking startup registration for unavailable roots.
- [x] Revision-checked library rename, safe detach and hash-verified relocation.
- [x] Profile rename/removal with default-profile protection.
- [x] Missing-file inspection without deleting originals.
- [ ] Qualify larger real libraries and stalled/network filesystems on additional hosts.
