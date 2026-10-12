# Lane N1 — M3 library workflows ledger

Plan: `MOTION_CONSOLIDATION_AND_COMPLETION_PLAN.md` section 11.2.
Worktree `/Volumes/seed1/Projects/motion-m3-workflows`, branch `m3-workflows`.
Starting SHA: `c6d6e266c76f0ea7ec1274bca10e30f9b4bdf658` (origin/main, 2026-10-11).

This ledger keeps implementation, missing wiring, unsupported cases, focused
checks, review findings and deferred release checks separate. "Usable" means an
ordinary user with the right grant can reach the operation from the Motion web
UI and see its result. It does not mean it has passed release qualification.

## 1. Workflow matrix

The API column was taken from `contracts/served_operations.txt` and the handlers
at the starting SHA. Every operation in M3 scope was already served except
markers. The gaps were in the UI.

| Workflow | API operations (permission) | UI before N1 | UI after N1 | Status |
|---|---|---|---|---|
| Register a source | createSource (sources:manage) | /sources form | unchanged | Usable |
| Create a library | createLibrary (sources:manage) | /sources form | unchanged | Usable |
| Edit a library (name, kind, language, sources) | replaceLibrary, If-Match library | none | /sources "Edit library", If-Match from the list revision | Usable |
| Relocate a source | previewSourceRelocation (If-Match source) → commitSourceRelocation (plan token) | none | /sources "Moved this folder?": preview shows affected count and warnings, then "Apply this change" commits that plan | Usable |
| Start a scan | createScan (sources:manage) | form hidden: the facade hard-coded `scans_available: false` | "Scan now" (changes only or verify) | Usable |
| Scan progress and result | listScans, getScan | hidden (same cause) | newest 20 scans, per-source counts, partial and unavailable explained | Usable. Refresh by reloading; there is no live push |
| Cancel a scan | cancelScan | hidden | "Cancel scan" while queued or running | Usable |
| Browse a library | listCatalogItems | /library/{id} | unchanged; selected posters now shown | Usable |
| Search | searchCatalog | /search | unchanged | Usable |
| Correct metadata (title, year, description, tags) | getMetadata, replaceMetadataContribution `local` (catalog:write, If-Match metadata) | none | /item/{id}/manage "Details": effective values with their sources, edit of the `local` contribution. Local fields the form does not show (release_date, cast) are carried unchanged | Usable |
| NFO / local metadata | read during scans; refreshMetadata re-reads `.nfo` | none | "Re-read .nfo files"; attribution shows ".nfo file", "file name" or "your edit" | Usable |
| Rescan keeps corrections | scans withdraw only `nfo`; `local` is untouched | n/a | verified end to end (section 3) | Usable |
| Manual identify / match correction | createMatch (provider null, exact FileRef), decideMatch (If-Match) | /matches could decide existing proposals only | manage page "Find matches" per exact file revision; /matches accepts, rejects or defers | Usable for local candidates. Online providers are deferred |
| Artwork upload | uploadArtwork (octet-stream, Idempotency-Key) | none | manage page upload (PNG/JPEG/WebP ≤ 8 MB); the bridge sends the raw file | Usable |
| Artwork selection | listArtwork, selectArtwork (If-Match item) | none | manage page "Use as poster"; posters render on cards, detail and continue-watching | Usable. Poster role only (see section 2) |
| Collections | list/create/replace/deleteCollection (collections:write, If-Match) | none | /organize create, list, remove, delete; manage page add/remove this title | Usable for manual collections |
| Playlists and order | list/create/replace/deletePlaylist (If-Match) | none | /organize create, remove entry, "Move up", delete; manage page add/remove the title's default timeline | Usable |
| Preparation | listRecipes, createPreparation (processing:request, Idempotency-Key) | none | manage page per timeline and exact original file: recipe and audio track (`a{n}`) | Usable for remux_mp4, h264720p, audio_aac |
| Inspect a prepared result | listRenditions | none | manage page lists each timeline's renditions and their status | Usable |
| Job retry | retryJob (processing:request, Idempotency-Key; the handler reads no If-Match) | cancel only | /processing "Retry" on failed or cancelled jobs (the server's retryable phases) | Usable. A stale page can retry a job someone else already retried; the server, not the page, bounds attempts |
| Schedules | list/create/replace/deleteSchedule (sources:manage, If-Match) | none | /sources Schedules: add, change interval, pause, remove | Usable for `scan` and `metadata_refresh` |

## 2. Still missing or explicitly unsupported

These are named on the page or listed here. Nothing is silently substituted.

- **Subtitles in prepared versions.** The server accepts only
  `subtitle_policy: off` with null subtitle selections
  (`preparation_tracks_unsupported`). The manage page states this, and the test
  shows `require` is refused with 422. The fix belongs to M4/N2 (subtitle
  pipeline), not this lane.
- **Audio component selection** (`audio_component_id`) is unsupported by the
  server. The UI offers only the exact file's audio track `a{n}`.
- **`video_profile` recipe.** It needs a full VideoProfile body. It remains
  an API operation; the UI does not offer it.
- **`preview` and `optimize` schedules** are refused by the server
  (`schedule_task_unavailable`) and not offered.
- **Artwork roles other than poster** (backdrop, banner, thumbnail) are
  selectable through the API only. Online/provider artwork is deferred by plan.
- **Online provider matching** is deferred by plan (`provider_unavailable`).
- **Smart collections and saved filters.** The API exists; the UI lists smart
  collections but cannot create or edit filters.
- **Episode order groups** (createOrderGroup, placeTimelineInOrder). API only;
  there is no UI for alternate episode orders.
- **Markers** (listMarkers/createMarker) are not served (M5 scope).
- **Merge/split reconciliation** (previewMerge/previewSplit/commitReconciliation).
  API only. Accepting a local match performs the common merge.
- **Library and source deletion.** API only, deliberately not added: both are
  destructive and need a confirmation design.
- **Match subjects** are shown as file IDs on /matches. The v2 contract has no
  scoped file→title read for a proposal; the manage page is the readable path.
- **Scan progress** is pull-only (reload). The event stream exists but the
  Topcoat pages have no live runtime by design.
- **Lists are bounded.** Lists are ordered by random ID with no server filter
  or recency order, so the pages read them whole, up to 1000 rows (5 pages).
  Past that the section says it cannot be shown rather than truncating:
  - Scan history: "Scan now" stays offered. `scan_requests` is never pruned,
    so an hourly schedule passes 1000 rows within weeks. This needs server-side
    recent ordering or pruning: a follow-up for the scans owner.
  - Renditions: "Prepared versions" lists as unknown; preparing stays offered.
    This needs a `timeline_id` filter on listRenditions.
  - /organize reads titles one by one, up to 300 reads. Beyond that the page is
    unavailable. A profile organizing more than about 300 distinct titles needs
    a batched title read.

## 3. Focused checks

| Check | Covers | Result |
|---|---|---|
| `node --test packages/ui-bridge/bridge.test.mjs` | nested/optional/minutes encoding; fixed JSON bodies sent verbatim with If-Match; raw-file upload with a retry-stable key; plan preview never commits, and confirm commits that plan once | 13/13 pass |
| `cargo test -p motion-ui` (compose) | markup, permissions and exact operations/validators for the new pages against the self-labelled mock | see receipts |
| `cargo test --test m3_workflows` | real SQLite + scan worker + ffmpeg media + processing worker through the real facade: register, scan, browse/search, metadata correction, stale If-Match → 412, artwork upload/selection, verify rescan keeps both, match proposal, collection add + stale replay → 412, preparation keeping audio `a1` → ready rendition shown, unsupported subtitle request → 422, schedules, library edit, restricted viewer/preparer/outsider grants (outsider 404 identical to missing), relocation preview → commit | see receipts |
| `cargo test --test access_api topcoat_` | real facade reads; the sources page now offers scans | see receipts |

Receipts (kyoto job IDs and results) are appended below as they complete.

## 4. Correctness boundary

No new state-dependent rule was added. Every decision exercised here is already
made by the server and its core: revision preconditions, idempotency, scope,
match validity, preparation admission and fencing, relocation plan ownership and
expiry, and the hidden-member preservation in collections and playlists. The UI
only carries the exact validators and bodies it rendered. The bridge keeps its
transport-only role (CSRF, idempotency-key lifecycle, If-Match). A precomputed
replacement body is guarded by the If-Match of the revision it was computed
from, so a concurrent change yields 412 instead of a lost update. The
integration test shows this for metadata and collections. No Stateless model
changed, because no core transition changed.

## 5. Review findings

An independent Claude review of the full lane diff (read-only, 2026-10-12)
reported 9 findings. Dispositions:

| # | Finding | Disposition |
|---|---|---|
| 1 | High: saving Details dropped local `release_date`/`cast`, because the form sent `values: {}` and the PUT replaces the whole contribution | Fixed. The form carries the unshown local values (`preserved_values`). The end-to-end test sets a release date, edits the title through the page's form and asserts the date survives |
| 2 | /organize returned 404 when a playlist entry's timeline was unreadable (edition visibility is narrower than item visibility) | Fixed. The entry is shown as "Unavailable title" and stays removable; other errors still fail |
| 3 | A stale relocation plan stayed applicable after the field changed or a new preview failed | Fixed. A preview submit and any field edit withdraw the plan, and the plan names the folder it applies to. Bridge test added |
| 4 | Bounded reads hid "Scan now" and "Prepared versions" once history grew; the organize budget was undocumented | Fixed for the UI: "Scan now" is independent of the history read, renditions report unknown separately, and the bounds are documented in section 2. Server-side ordering and filters are follow-ups |
| 5 | Retry offered for a nonexistent phase, missed `cancelled`, and claimed an If-Match the server ignores | Fixed: failed or cancelled, no precondition claimed in the UI or the ledger |
| 6 | Schedule edit rounded non-minute intervals | Fixed: whole minutes are edited in minutes, others in seconds |
| 7 | Collection toggle used the URL ID, which can be a merged alias | Fixed: uses the item's live `id` |
| 8 | Identification listed generated renditions and could send an empty `library_id` | Fixed: only files bound to original versions are listed, and the form is hidden when no manageable library holds the file |
| 9 | Ledger overclaimed (retry If-Match, untested playlists and organize removal, empty receipts) | Fixed. The end-to-end test now covers playlist add (two titles), distinct entry IDs, Move up, stale-replay 412, playlist Remove, and collection Remove from /organize. Receipts recorded below. The remaining evidence gaps are listed in section 6 |

The reviewer confirmed these correct: If-Match guards every precomputed body;
`entry_id` generation is valid and collision-safe; profile and scope filtering;
identical 404s; no render-time writes; bridge DOM writes use textContent only;
the plan commits only on click with a per-token key; JSON-in-attribute escaping;
no `style=` attributes; same-origin asset URLs under COEP; `list_all` cursor
handling; and request bodies matching the src/v2 handlers.

GPT-6.1 Sol review is required by the plan. It is not available from this
agent and is **outstanding**.

## 6. Deferred release checks

- Browser execution of the bridge flows (upload, plan confirm) in Chromium,
  WebKit and Electron, in the release matrix. In `m3_workflows`, preparation,
  relocation commit and the first metadata save send hand-written JSON
  equivalent to the form; the form encoding itself is covered by the node
  bridge tests, not by a browser.
- A poster `<img>` request authenticated by the browser session cookie only.
  The integration test fetches the asset with a bearer header.
- Installed-package run of the same workflow against packaged ffmpeg.
- The full-M3 release gate: the remaining items in section 2 that the release
  requires (at minimum subtitle-inclusive preparation and provider metadata, if
  they stay in M3 scope) plus the exhaustive matrix.

## Receipts
All on this worktree at the starting SHA plus the lane diff, 2026-10-12, darwin-arm64,
toolchain 1.98.0. Compilation ran through kyoto; tests ran outside it (kyoto does not run tests).

Final state after the review fixes:

| Command | Kyoto job | Result |
|---|---|---|
| `cargo test --locked -p motion-ui -p playscale --test compose --test m3_workflows --test access_api --no-run`, then run each | f5a251fa | compose 21 passed; m3_workflows 1 passed (Homebrew ffmpeg/ffprobe 8.1.2 on PATH); `access_api topcoat presentation` 8 passed |
| `node --test packages/ui-bridge/bridge.test.mjs` | — | 14 passed |
| `cargo clippy --locked -p motion-ui -p playscale --all-targets -- -D warnings -A clippy::too_many_arguments -A clippy::result_large_err` (CI flags) | 5fd2096e | clean |
| `cargo fmt --all` (1.98) | — | applied |

Not run in this lane: the rest of the workspace test suite (the other
integration test binaries) and the full CI matrix. The lane touches
`src/presentation.rs` and the shared UI crate only, but CI on the PR is the
broader check.

Problems found while building the end-to-end test, and their fixes:
- The fixture's processing supervisor defaulted to the test binary, so the encoder
  failed (`Unrecognized option: 'internal-ffmpeg-supervisor'`). This was a test setup
  error, not a product defect. The fixture now uses `CARGO_BIN_EXE_playscale`, like
  the delivery and playback tests. Before this lane no test had run a v2
  preparation to completion; this one does (remux_mp4, audio `a1`, rendition `ready`).
- The idempotency keys in the test were shorter than 16 characters (rejected
  correctly with `invalid_idempotency_key`). Test fix only.
