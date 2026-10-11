# Motion v2 cutover

Motion serves one v2 application. Topcoat is the default presentation in the
server, Go launcher and Electron-owned server. The accepted `--topcoat` flag is
now a compatibility no-op; it cannot select the retired web application.

Old clients must update. `/api/v1`, unversioned `/media` links, old stream and
subtitle URLs, the v1 OpenAPI document and old web application assets return
404 in every access mode, including for the operator. Refresh playback through
v2 plans and delivery sessions; old media links cannot bypass current grants.

Library data is retained and migrated. Historical item-keyed progress remains
recoverable; current viewing uses the timeline authority. Schema migrations,
attribution repair and canonical-history precedence remain in force. Retiring
HTTP writers does not delete historical tables or source media.

Before upgrading a real installation, retain its verified pre-upgrade backup
and matching older binary/assets. Rollback restores that pair to an isolated
state root. Never point an older binary at the upgraded database.

The full qualification status is recorded in
[the final consolidation ledger](qualification/consolidation/FINAL.md).
An implementation commit or merged lane does not itself qualify an installed
package, browser engine, hardware encoder or milestone.

Historical transport scaffolding under `tests/support/service_router.rs` and
browser fixtures under `tests/fixtures/legacy-web` retain service and failure
regressions. They are not installed routers or current-client acceptance. The
production absence and v2 authorization tests use the real application router.
