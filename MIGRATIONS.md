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
| 0028 | `0028_delivery_admissions.sql` | A06/A07 | requested: idempotent delivery admission receipts (0020–0026 are claimed by open PR #4) |
| 0032 | `0032_timeline_viewing.sql` | A07 | v2 timeline-keyed viewing state, viewing sessions and acknowledged events; backfill only from exact attributions or single-timeline works (allocated by the PR #4 owner; 0029–0031 are PR #4's) |
| 0033 | `0033_delivery_owners.sql` | A07 | durable v2 delivery owner (principal, profile, timeline, version, source) |

Adding a column to an existing table breaks positional `INSERT ... VALUES`
statements; always name columns in inserts.
