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
| 0011 | reserved | A08 | access/identity (in progress in the A08 worktree) |
| 0012 | `0012_scan_coverage.sql` | A03 | per-attempt directory coverage |
| 0013 | `0013_matching.sql` | A04 | match proposals, item match state |
| 0014 | `0014_organization.sql` | A09 | saved filters, collections, playlists, queues |
| 0015 | `0015_timelines_versions.sql` | A01/A02 | timelines, media versions, version bindings; timeline FKs |
| 0016 | `0016_occurrence_index.sql` | A01/A02 | index for content-revision occurrence lookup |

Adding a column to an existing table breaks positional `INSERT ... VALUES`
statements; always name columns in inserts.
