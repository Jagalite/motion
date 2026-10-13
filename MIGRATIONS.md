# Migration registry

A02 (persistence) assigns production migration sequence numbers. Other
workstreams request a number here before adding a file under `migrations/`;
never renumber an applied migration. Each migration runs in its own SQLite
transaction, and `db::connect` takes a verified pre-upgrade backup before
applying any pending migration to an existing database.

| Version | File | Owner | Status |
|---|---|---|---|
| 0001–0009 | legacy Motion schema | — | applied baseline (`52491bf`) |
| 0010 | `0010_catalog_identity.sql` | A01/A02 | catalog revisions, aliases, receipts, legacy progress attribution, source binding revision |
| 0011 | `0011_access.sql` | A08 | access, identity, credentials and scoped events |
| 0012 | `0012_scan_coverage.sql` | A03 | per-attempt directory coverage |
| 0013 | `0013_matching.sql` | A04 | match proposals, item match state |
| 0014 | `0014_organization.sql` | A09 | saved filters, collections, playlists, queues |
| 0015 | `0015_timelines_versions.sql` | A01/A02 | timelines, media versions, version bindings; timeline FKs |
| 0016 | `0016_occurrence_index.sql` | A01/A02 | index for content-revision occurrence lookup |
| 0017 | `0017_library_sources.sql` | A02/A03 | logical libraries over storage sources, exclusions, scan binding revision |
| 0018 | `0018_delivery_sessions.sql` | A07 integration | delivery restart fencing; resolves the provisional 0015 collision before applying it in this integration |
| 0019 | `0019_job_api_identity.sql` | A08 integration | persisted job requester and revisions; legacy requester stays unknown |
| 0020 | `0020_scan_demands.sql` | A03 | scan requests/demands, freshness barrier, one running + one queued attempt |
| 0021 | `0021_search.sql` | A09 | FTS5 search projection with dirty queue |
| 0022 | `0022_scan_demand_facts.sql` | A03 | direct-request attempts; retained demand coverage |
| 0023 | `0023_nfo_origins.sql` | A04 | sidecar NFO contribution origin |
| 0024 | `0024_search_rows_coverage_backfill.sql` | A09/A03 | indexed FTS row map; demand coverage backfill |
| 0025 | `0025_structure.sql` | A01 | order groups, item relationships, multi-episode bindings |
| 0026 | `0026_sidecar_subtitles.sql` | A04 | sidecar subtitle observations |
| 0028 | `0028_delivery_admissions.sql` | A06/A07 | idempotent delivery admission receipts |
| 0029 | `0029_structure_events.sql` | A01/A09 | relationship update revisions; organization delete events |
| 0030 | `0030_markers.sql` | A04 | timeline markers |
| 0031 | `0031_search_origin_titles.sql` | A09 | reindex when an origin title is replaced |
| 0032 | `0032_timeline_viewing.sql` | A07 | v2 timeline-keyed viewing state, viewing sessions and acknowledged events; backfill only from exact attributions or single-timeline works |
| 0033 | `0033_delivery_owners.sql` | A07 | durable v2 delivery owner (principal, profile, timeline, version, source) |

| 0034 | `0034_scan_api_revisions.sql` | A02 data/catalog completion | public scan representation revisions |

| 0035 | `0035_catalog_adapter_state.sql` | A02 data/catalog completion | timeline representation revisions and unselected artwork ownership |

| 0036 | `0036_metadata_jobs.sql` | A02 data/catalog completion | durable local metadata refresh jobs and authorization witnesses |

| 0037 | `0037_smart_collection_revisions.sql` | A02 data/catalog completion | durable invalidation of evaluated smart collections |

| 0038 | `0038_completion_api_state.sql` | Data/catalog PR #17 | durable preparation identity |
| 0039 | `0039_logical_schedules.sql` | Data/catalog PR #17 | logical schedules and rendition representation invalidation |
| 0040 | `0040_backup_requests.sql` | Data/catalog PR #17 | durable backup jobs and receipts |

| 0041 | `0041_schedule_dispatch_progress.sql` | Data/catalog PR #17 | bounded resumable metadata schedule fanout |
| 0042 | `0042_operational_metrics.sql` | Server metrics | bounded operational samples and process-lifetime failed-job observation deduplication |
| 0043 | `0043_playback_monitoring.sql` | Playback monitoring | administrator session history with transfer counters, a viewing-by-delivery index, and three playback metric series (rebuilds `operational_metrics`) |
| 0044 | `0044_offline_downloads.sql` | A13 (N3 downloads, PR #31) | device downloads, download ticket link, offline sequences/events/lineage |
| 0045 | `0045_offline_event_retention.sql` | Completion item 1 (downloads) | offline event retention floor acknowledged by the device |
| 0046 | `0046_bounded_library_queries.sql` | Completion item 2 (library queries) | durable scan admission sequence (counter and trigger) and rendition-by-timeline index; finalized before merge (earlier branch states are development-only) |
| 0047 | `0047_restore_baselines.sql` | Offline restore | viewing baseline captured before restored service starts |
| 0048 | `0048_download_transfers.sql` | Downloads | ordered device transfer reports |
| 0049 | `0049_file_chapters.sql` | Chapters | revision-bound probed chapters |
| 0050 | `0050_download_subtitles.sql` | Downloads | selected subtitle identity |
| 0051 | `0051_previews.sql` | Previews | durable preview jobs and published sheets |
| 0052 | `0052_preview_jobs_unique.sql` | Previews | unique active revision admission |
| 0053 | `0053_review_download_inventory.sql` | Rewrite review U06 | bounded active download inventory independent of cancelled history |
| 0054 | `0054_preparation_dependencies.sql` | Rewrite review A05 | invalidate preparation revisions only for source, binding and output dependencies |
| 0055 | `0055_artwork_selection_revisions.sql` | Rewrite review C11 | advance role selection readback ETags when contribution facts change |
| 0056 | `0056_event_ownership.sql` | Rewrite review A07 | retain organization and logical schedule scope for authorized invalidation after deletion/moves |
| 0057 | `0057_access_management.sql` | TUI access | explicit user ownership, pairing denial, revision-bound invitations and durable redemption limits |
| 0058 | `0058_legacy_access_ownership.sql` | SSO/email A1 | unassign untouched generated legacy placeholders; preserve device grants/credentials and deliberate user associations |

| 0059 | `0059_signin.sql` | Authentication | identity, session family, sign-in transaction and audit facts |
| 0060 | `0060_preview_layout_identity.sql` | Review repairs | layout identity and bounded retention cursor |
| 0061 | `0061_issued_viewing_histories.sql` | Restore reconciliation | only known issued epochs may re-anchor offline sequences |
| 0062 | `0062_marker_creation_order.sql` | Marker corrections | persisted creation order for manual corrections |

| 0063 | `0063_download_subtitle_artifacts.sql` | Required download artifacts | durable subtitle digest, size and failure state |

The catalog migrations were renumbered when PR #4 merged (they had used 0018–0026 on the unreleased branch). Databases created by pre-merge `catalog-sources` builds are development-only and are not upgradable; recreate them. The released baseline (schema 9) and main's history upgrade normally. 0027 is unassigned.

Adding a column to an existing table breaks positional `INSERT ... VALUES`
statements; always name columns in inserts.

## Parallel development history reconciliation

The completion chain retains 0045–0052. The rewrite repair migrations are
0053–0056; access management, legacy ownership, and sign-in are 0057–0059.
Only their filenames/numbers changed; their SQL bytes remain unchanged.
0060 adds preview layout identity, 0061 records issued viewing histories,
0062 orders manual marker corrections, and 0063 records subtitle artifacts.

Main databases through 0044 upgrade normally with the integrated server.
Databases previously opened by the unmerged access/auth or rewrite branches
have conflicting SQLx migration numbers. Do not delete their migration rows,
change checksums, or start them with the integrated binary until reconciled.
Stop Motion and create a separate database copy:

```sh
python3 scripts/reconcile_development_migrations.py /path/to/old.sqlite3 /path/to/new.sqlite3
```

The destination must not exist. The tool verifies every applied version and
SHA-384 checksum against canonical SQL or the exact recorded branch mappings,
rejects failed/unknown/ambiguous histories, and remaps only known version
numbers in the copy. It preserves application rows and credentials and leaves
the source unchanged. Start the copied database with the integrated server to
apply missing migrations. This is an offline development-history repair, not
an automatic startup rewrite or a restore: restoring a backup separately
rotates viewing history and withdraws saved authority, including sign-in grants.
Unknown earlier development snapshots need explicit investigation; they are
not accepted by weakening SQLx checksum validation.

Repeated restores from the same old backup cannot prove epochs created only
after that snapshot. Offline logs claiming such unknown epochs are retained
and refused; they are never silently discarded or re-anchored. Recovery across
those absent histories requires a separately designed retained/signed proof.
