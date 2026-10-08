# Running Playscale as a service

The macOS deployment uses a per-user LaunchAgent: startup at login and automatic
restart after exit. It does not start before user login. Tailscale must also be
running; the private Serve mapping is managed separately. The installer copies
the executable and matching Demuxe assets onto the internal disk so the service
does not depend on the checkout or build directory. This is a managed development
build, not a signed/notarized release package.

## Configuration

`--config config.local.json` loads strict JSON. Start by copying
`config.example.json`; real config and `.env` files are Git-ignored. Unknown fields are errors.
Paths inside a configuration are relative to that file; explicit CLI flags override
file values and retain their normal working-directory-relative meaning. Supplying
any `--library` flags replaces the file's entire startup library list. Bare `ffprobe`
uses PATH; a path with directory components is relative to the configuration.

```json
{
  "listen": "127.0.0.1:8787",
  "data_dir": "data",
  "public_origin": "https://your-server.your-tailnet.ts.net:8443",
  "libraries": [],
  "demuxe_dir": "/absolute/path/to/matching/demuxe",
  "ffprobe": "/opt/homebrew/bin/ffprobe"
}
```

`playscale --config config.json --check-config` validates and prints the effective
configuration without opening the database or starting a server. Defaults still
support the original CLI. `libraries` registers and queues those roots at startup;
an empty list preserves libraries already stored in SQLite without rescanning them
at every restart. Media on external/NAS storage must be mounted when accessed.
Storage identity changes can require operator review under the existing root guard.

## Install, stop and inspect on macOS

Stop any manually started server using the same port/data before first installation.
The installer does not migrate a database: configure an existing stopped data
directory or restore a snapshot to the intended new directory first. Build and test
before installation, then use an explicit configuration:

```sh
cargo build --locked -p playscale
python3 scripts/install_macos_service.py \
  --binary target/debug/playscale --config /absolute/path/config.json
```

The installer refuses an existing service/configuration unless `--replace` is
supplied. It pins FFprobe's executable path, verifies copied binary/asset hashes,
and installs:

- `~/Library/LaunchAgents/local.playscale.server.plist`
- `~/Library/Application Support/Playscale/config.json`
- `~/Library/Application Support/Playscale/releases/BUILD_ID/`
- `~/Library/Application Support/Playscale/logs/`
- `~/Library/Application Support/Playscale/tools/backup.py`

The database uses the configured data directory. The current local deployment puts
it under `~/Library/Application Support/Playscale/data`; the three generated test
videos are copied to `test-media` beneath the same install directory. Their hashes
and catalog identities were preserved when relocating this test library. Only one process can own that directory.

```sh
launchctl print "gui/$(id -u)/local.playscale.server"
# Gracefully stop and unload; killing the process alone causes a restart.
launchctl bootout "gui/$(id -u)/local.playscale.server"
# Start again; RunAtLoad and KeepAlive are in the plist.
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/local.playscale.server.plist"
```

Wait for the previous process to exit before bootstrapping it again; the upgrade
installer waits for this explicitly.

Logs are `stdout.log` and `stderr.log` in the installed logs directory. Retention/
rotation is not automated yet. SIGINT and SIGTERM both initiate bounded graceful
shutdown, cancelling the worker and draining HTTP before closing SQLite. A final
two-second runtime shutdown limit prevents a blocked filesystem task from keeping
the process alive indefinitely; this does not make the underlying I/O cancellable.
Launchd throttles repeated restarts to avoid a tight failure loop.

## Health and diagnostics

`GET /health` is liveness. `GET /ready` returns 200 only when the migrated database
responds, the supervised worker is running, and shutdown has not begun; otherwise
503. It does not prove media availability, codec support, writable/free disk space,
or end-to-end Tailscale connectivity. Both routes use the configured Host policy.

Admin `GET /api/v1/admin/diagnostics` reports readiness, uptime, package version,
counts of libraries/files/unavailable files, jobs grouped by phase, available stream
slots, and SQLite page metrics. It includes whether Demuxe was present at startup
and a bounded current FFprobe availability check. It exposes no token or filesystem
paths. Database counts are read in a transaction; filesystem availability remains
based on catalog observations. These are diagnostics, not a metrics/alerting system.

The existing private Serve mapping remains:

```sh
tailscale serve --bg --https=8443 http://127.0.0.1:8787
tailscale serve status
```

Use `https://your-server.your-tailnet.ts.net:8443/` while connected to the tailnet.
Do not reset Serve: this Mac has other applications on its other HTTPS ports.
To remove only Playscale's mapping: `tailscale serve --https=8443 off`.

## Backup and restore

Requires Python 3.11 or newer. Never copy a live `.sqlite3` file alone: committed
transactions may be in its WAL. Use the SQLite online snapshot tool, which works
while the server is running:

```sh
python3 scripts/backup.py backup \
  --data-dir "$HOME/Library/Application Support/Playscale/data" \
  --output /path/to/backups/NEW_BACKUP_DIRECTORY
```

The installed copy is at
`~/Library/Application Support/Playscale/tools/backup.py`. The output directory must
not exist; its parent must exist. The backup contains a self-contained database,
SHA-256/size/schema manifest, catalog, artwork blobs, profiles, viewing history,
sessions, preferences, and jobs. It runs SQLite integrity and foreign-key checks.
Output directories use mode 0700 and the database uses 0600. An interrupted operation
may leave an incomplete directory; it has no completed manifest and is not a backup.
Snapshot work is bounded by a 120-second progress deadline.

Source media, Demuxe assets, configuration, and the admin token are **not included**.
Back those up separately as appropriate. No automatic schedule or retention policy
is installed; run backups before upgrades and arrange off-device copies separately.

Restore validates the manifest/hash and database, then writes to a new directory:

```sh
python3 scripts/backup.py restore \
  --backup /path/to/backups/COMPLETED_BACKUP \
  --data-dir /path/to/NEW_DATA_DIRECTORY
```

It refuses every existing destination, even an empty one. Restored servers generate
a fresh admin token. Media roots/identities remain as recorded; restoring the DB does
not relocate media. To switch a service to restored data: stop/unload it, change
`data_dir` in its configuration, then bootstrap it again. Keep the original data
and configuration until the replacement has passed readiness and content checks.

## Upgrades and rollback

Take a backup first. Run the tests/smoke scripts and install the new binary using
`--replace`. Older executable/asset directories remain available. The installer
saves the preceding configuration and plist as `config.previous.json` and
`service.previous.plist` in the install directory. Check readiness, diagnostics,
and a media request after activation. A successful launchctl bootstrap alone does
not prove the process stayed healthy.

SQLite migrations run on startup and have no automatic down-migration. Rolling back
the executable alone after a schema change may fail. For rollback, restore the
pre-upgrade snapshot to a new directory, stop/unload the service, restore the matching
old configuration/plist with its data path changed to that restored directory, and
bootstrap the old service. Preserve the failed upgraded data for investigation.


## Processing configuration

FFmpeg is configured alongside FFprobe. JSON accepts this optional section:

```json
"processing": {
  "ffmpeg": "/opt/homebrew/bin/ffmpeg",
  "cache_bytes": 21474836480,
  "max_output_bytes": 2147483648,
  "retention_seconds": 2592000,
  "timeout_seconds": 7200
}
```

These are the defaults, except the default executable is `ffmpeg` resolved on PATH.
File-relative executable paths containing a directory resolve beside the config;
the macOS installer resolves FFmpeg to an absolute path. `timeout_seconds` applies
to each encoding/validation subprocess. Retention is 60 seconds to one year;
timeouts are 1 second to one day. Output budgets must be at least 1 MiB and no
larger than the cache budget. Cache inspection has its own bounded admission.

Generated files live under `data_dir/generated`; keep the entire data directory
outside source libraries. Treat that directory as server-owned. The byte budget
includes source snapshots and outputs. It does not reserve filesystem free space
or protect against unrelated disk users. Full disks and exhausted budgets produce
failed jobs, never a partially published rendition. Cleanup only removes known
job directories and defers during conversion or recent playback.

SQLite snapshots include processing jobs, schedules, and event cursors, but exclude
generated bytes. After a DB-only restore to a different data directory, old generated
entries are unavailable; originals retain the usual external-path requirements.
Recreate expired outputs with new idempotency keys. Preserve the admin token only
for an intentional deployment move, as described above.

Use `scripts/processing_smoke.py` to exercise real conversions, hardware encoding on
macOS, cancellation/retry, SIGTERM/SIGKILL recovery, source replacement, budget
rejection, schedules, and cleanup using disposable media. The successful hardware
case proves this H.264 encode/decode pipeline, not HDR or every hardware backend.

## Automatic storage maintenance

The optional `storage` configuration has these defaults:

```json
{
  "storage": {
    "backup_interval_seconds": 86400,
    "backups_keep": 7,
    "history_retention_seconds": 7776000,
    "min_free_bytes": 1073741824,
    "log_bytes": 8388608,
    "log_files": 5,
    "incremental_scans": true
  }
}
```

The maintenance worker checks every 30 seconds. A new installation takes its first
snapshot on the first check. Zero disables automatic snapshots; manual admin
snapshots remain available. Backups use SQLite `VACUUM INTO`, validate integrity
and foreign keys, record schema versions and SHA-256, then atomically publish a
manifest directory under `data_dir/backups`. Restore with the existing
`scripts/backup.py restore` command. These are local database snapshots: copy them
to another device for protection against loss of the server disk. Source media,
generated files, configuration, credentials, and player assets are not included.

Retention removes only marked `auto-*` snapshot directories belonging to this
scheduler. Unfinished directories are not published as backups; partials older than
24 hours are removed before the next backup space check, even if disk pressure
prevents that backup from completing. Complete snapshots are retained until a
replacement snapshot has been published. Manually
created backups are left alone. At least one complete snapshot is retained.

Free-space admission reserves the configured minimum in addition to estimated
snapshot space or source-plus-maximum-output conversion space. It is a preflight
check, not a disk quota: another process can consume space after admission.
Snapshot and cache failures are recorded by `GET /api/v1/admin/storage` and logged;
they do not deliberately stop the HTTP server. Actual SQLite write failures can
still prevent mutations; the reserve does not guarantee database availability on
a full or failing volume.

Application logs rotate in `data_dir/logs`: one current file and up to `log_files`
rotated files, each bounded by `log_bytes`. Logging uses a bounded nonblocking
queue, which may drop entries under overload. LaunchAgent stdout/stderr files
remain in the installation's `logs` directory and contain startup failures; they
are separate from application log rotation.

History retention removes up to 500 old records per category per check. It prunes
terminal scan jobs, cleaned terminal processing requests, and obsolete closed
playback sessions after 90 days by default. It preserves catalog provenance,
viewing progress, preferences, and the most recent authoritative session for each
profile/item. Processing idempotency keys and old session-event retries are only
guaranteed while their records remain retained.

## Library administration and incremental scans

Configured roots are registered asynchronously: an unavailable root is reported in
storage status without blocking HTTP readiness. Startup registration preserves
administrator-assigned names and never reactivates detached roots. A scan of an
unavailable or replaced root fails without marking the entire library missing.

On Unix, ordinary scans reuse SHA-256/probe results when the securely opened file's
fingerprint (device/inode, size, modification and change timestamps) is unchanged.
New or changed files are fully inspected. Other platforms use full inspection.
Set `storage.incremental_scans` false, or request `POST
/api/v1/libraries/{id}/scans?full=true`, to rehash/reprobe everything. Full scans are
useful after FFprobe upgrades and for periodic verification of filesystems whose
attribute behavior is unreliable. A cached failed probe also needs a full scan to
be retried. This is incremental inspection, not a filesystem watcher; traversal
and catalog publication still occur for each scan.

Admin library rename, detach, and relocation require `expected_revision`. Detach
retains originals on disk and all catalog/viewing data, disables scans, removes
its scan schedule, and makes its original files unavailable. Independent imported
or generated renditions remain usable where their revision rules allow it.
Relocation verifies candidate bytes against recorded hashes before changing the
stored root; it does not move files. It preserves file/item IDs and source
revisions. An enabled library may retain known missing files; reactivating a
detached library conservatively requires every retained path to be present and
unchanged. Active scan/processing jobs must finish or be cancelled first.
