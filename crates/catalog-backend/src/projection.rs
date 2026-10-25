//! Disposable native-authoritative compatibility projection. The SQL function
//! defaults to denial on every normal Motion connection. Only an isolated,
//! never-pooled connection owned by this module can write explicit projection
//! tables; no SQL value, journal row, or application request enables it.
use anyhow::{Context, ensure};
use rusqlite::ffi;
use std::{ffi::CStr, path::Path};

/// This allowlist excludes app history, auth, source identity, cutover and every
/// control table. Trigger side effects on search_dirty are disposable only.
pub(crate) const TABLES: &[&str] = &[
    "items",
    "item_origins",
    "search_dirty",
    "editions",
    "timelines",
    "media_versions",
    "media_files",
    "version_files",
    "catabolic_projection_file_libraries",
    "catabolic_projection_generated_libraries",
];
unsafe extern "C" fn deny(
    context: *mut ffi::sqlite3_context,
    _argc: i32,
    _argv: *mut *mut ffi::sqlite3_value,
) {
    // SAFETY: SQLite supplies a live result context for this invocation.
    unsafe { ffi::sqlite3_result_int(context, 0) };
}
unsafe extern "C" fn permit(
    context: *mut ffi::sqlite3_context,
    argc: i32,
    argv: *mut *mut ffi::sqlite3_value,
) {
    let allowed = if argc == 1 && !argv.is_null() {
        // SAFETY: argc verifies SQLite supplied one argument; text is valid for
        // this callback and is not retained. Null/nontext requests fail closed.
        let raw = unsafe { ffi::sqlite3_value_text(*argv) };
        if raw.is_null() {
            false
        } else {
            unsafe { CStr::from_ptr(raw.cast()) }
                .to_str()
                .ok()
                .is_some_and(|name| TABLES.contains(&name))
        }
    } else {
        false
    };
    unsafe { ffi::sqlite3_result_int(context, i32::from(allowed)) };
}
unsafe fn install(handle: *mut ffi::sqlite3, privileged: bool) -> anyhow::Result<()> {
    // Not DETERMINISTIC: a statement must never constant-fold authority. No
    // SQL-callable setter exists; registration requires the private raw handle.
    let code = unsafe {
        ffi::sqlite3_create_function_v2(
            handle,
            c"motion_native_projection".as_ptr(),
            1,
            ffi::SQLITE_UTF8 | ffi::SQLITE_INNOCUOUS,
            std::ptr::null_mut(),
            Some(if privileged { permit } else { deny }),
            None,
            None,
            None,
        )
    };
    ensure!(
        code == ffi::SQLITE_OK,
        "cannot install native projection denial"
    );
    let code = unsafe {
        ffi::sqlite3_create_function_v2(
            handle,
            c"motion_native_source_projection".as_ptr(),
            10,
            ffi::SQLITE_UTF8 | ffi::SQLITE_INNOCUOUS,
            std::ptr::null_mut(),
            Some(deny),
            None,
            None,
            None,
        )
    };
    ensure!(
        code == ffi::SQLITE_OK,
        "cannot install source projection denial"
    );
    let code = unsafe {
        ffi::sqlite3_create_function_v2(
            handle,
            c"motion_native_upgrade".as_ptr(),
            2,
            ffi::SQLITE_UTF8 | ffi::SQLITE_INNOCUOUS,
            std::ptr::null_mut(),
            Some(deny),
            None,
            None,
            None,
        )
    };
    ensure!(
        code == ffi::SQLITE_OK,
        "cannot install native upgrade denial"
    );
    // Lifecycle authority is never conferred by the disposable projection
    // permit, including on its isolated connection. The lifecycle owner must
    // install an exact-row, cancellation-safe insert capability separately.
    let code = unsafe {
        ffi::sqlite3_create_function_v2(
            handle,
            c"motion_source_lifecycle_insert_allowed".as_ptr(),
            11,
            ffi::SQLITE_UTF8 | ffi::SQLITE_INNOCUOUS,
            std::ptr::null_mut(),
            Some(deny),
            None,
            None,
            None,
        )
    };
    ensure!(
        code == ffi::SQLITE_OK,
        "cannot install source lifecycle denial"
    );
    Ok(())
}
/// Install through SqlitePoolOptions::after_connect for every ordinary Motion
/// connection, including later pool growth. Missing function also fails closed.
pub async fn configure_connection(
    connection: &mut sqlx::SqliteConnection,
) -> Result<(), sqlx::Error> {
    let mut handle = connection.lock_handle().await?;
    unsafe { install(handle.as_raw_handle().as_ptr(), false) }
        .map_err(|error| sqlx::Error::Protocol(error.to_string()))
}

fn isolated(path: &Path) -> anyhow::Result<rusqlite::Connection> {
    isolated_with_timeout(path, std::time::Duration::from_secs(5))
}
fn isolated_with_timeout(
    path: &Path,
    timeout: std::time::Duration,
) -> anyhow::Result<rusqlite::Connection> {
    ensure!(
        std::fs::symlink_metadata(path)?.file_type().is_file(),
        "projection database must be a regular file"
    );
    let db = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.busy_timeout(timeout)?;
    db.pragma_update(None, "foreign_keys", true)?;
    db.pragma_update(None, "synchronous", "FULL")?;
    // SAFETY: this connection is local, newly opened and never pooled/exposed.
    unsafe { install(db.handle(), true) }?;
    Ok(db)
}

// Separate insert-only source descriptor capability. Generic catalog projection
// can never enable writes to libraries, and no SQL-callable setter exists.
unsafe extern "C" fn destroy_source(value: *mut std::ffi::c_void) {
    if !value.is_null() {
        unsafe {
            drop(Box::from_raw(value.cast::<Vec<serde_json::Value>>()));
        }
    }
}
unsafe extern "C" fn permit_source(
    context: *mut ffi::sqlite3_context,
    argc: i32,
    argv: *mut *mut ffi::sqlite3_value,
) {
    let allowed = (|| {
        if argc != 10 || argv.is_null() {
            return false;
        }
        let data = unsafe { ffi::sqlite3_user_data(context) };
        if data.is_null() {
            return false;
        }
        let expected = unsafe { &*data.cast::<Vec<serde_json::Value>>() };
        if expected.len() != 10 {
            return false;
        }
        for (index, wanted) in expected.iter().enumerate() {
            let arg = unsafe { *argv.add(index) };
            let actual = match unsafe { ffi::sqlite3_value_type(arg) } {
                ffi::SQLITE_INTEGER => serde_json::json!(unsafe { ffi::sqlite3_value_int64(arg) }),
                ffi::SQLITE_TEXT => {
                    let bytes = unsafe { ffi::sqlite3_value_bytes(arg) };
                    let text = unsafe { ffi::sqlite3_value_text(arg) };
                    if bytes < 0 || text.is_null() {
                        return false;
                    }
                    let bytes = unsafe { std::slice::from_raw_parts(text, bytes as usize) };
                    let Ok(text) = std::str::from_utf8(bytes) else {
                        return false;
                    };
                    serde_json::json!(text)
                }
                _ => return false,
            };
            if actual != *wanted {
                return false;
            }
        }
        true
    })();
    unsafe {
        ffi::sqlite3_result_int(context, i32::from(allowed));
    }
}
fn source_capability(
    db: &rusqlite::Connection,
    values: Option<Vec<serde_json::Value>>,
) -> anyhow::Result<()> {
    let (data, callback, destructor) = match values {
        Some(values) => (
            Box::into_raw(Box::new(values)).cast(),
            Some(permit_source as unsafe extern "C" fn(_, _, _)),
            Some(destroy_source as unsafe extern "C" fn(_)),
        ),
        None => (
            std::ptr::null_mut(),
            Some(deny as unsafe extern "C" fn(_, _, _)),
            None,
        ),
    };
    // SQLite calls the supplied destructor even if registration fails.
    let code = unsafe {
        ffi::sqlite3_create_function_v2(
            db.handle(),
            c"motion_native_source_projection".as_ptr(),
            10,
            ffi::SQLITE_UTF8 | ffi::SQLITE_INNOCUOUS,
            data,
            callback,
            None,
            None,
            destructor,
        )
    };
    ensure!(
        code == ffi::SQLITE_OK,
        "cannot scope native source projection"
    );
    Ok(())
}
/// Materialize only the physical FK descriptor of an independently proven native
/// source. No logical library membership, permission or scan demand is granted.
pub(super) fn materialize_source(
    db: &rusqlite::Connection,
    host: &catabolic_runtime::host::HostSession,
    mapping: &crate::sources::SourceMapping,
) -> anyhow::Result<()> {
    ensure!(
        !db.is_autocommit(),
        "source descriptor requires held projection transaction"
    );
    let isolated: bool = db.query_row("SELECT motion_native_projection('items')=1", [], |r| {
        r.get(0)
    })?;
    ensure!(
        isolated,
        "source descriptor requires private projection connection"
    );
    let identity = host.identity()?;
    ensure!(
        identity["catalog_id"] == mapping.catalog.catalog_id
            && identity["catalog_incarnation"] == mapping.catalog.incarnation
            && identity["profile"] == mapping.profile,
        "source descriptor identity mismatch"
    );
    if mapping.library_id == crate::generated_source::SOURCE_ID {
        use rusqlite::OptionalExtension;
        let saved = crate::generated_source::mapping_sync(db)?
            .context("private generated source has no immutable ACK")?;
        ensure!(
            saved == *mapping,
            "private generated source ACK differs from occurrence mapping"
        );
        crate::generated_source::verify_with_host(host, mapping)?;
        let collision: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM catabolic_source_registry WHERE id=?) OR EXISTS(SELECT 1 FROM catabolic_source_mappings WHERE library_id=?) OR EXISTS(SELECT 1 FROM catabolic_imported_source_descriptors WHERE source_id=?)",
            rusqlite::params![crate::generated_source::SOURCE_ID,crate::generated_source::SOURCE_ID,crate::generated_source::SOURCE_ID], |row| row.get(0))?;
        ensure!(
            !collision,
            "private generated source collides with public source authority"
        );
        let cached: Option<String> = db.query_row("SELECT json_object('id',id,'name',name,'root',root,'root_identity',root_identity,'managed',managed,'enabled',enabled,'revision',revision,'binding_revision',binding_revision,'exclusions_json',exclusions_json,'scan_barrier',scan_barrier) FROM libraries WHERE id=?",
            [crate::generated_source::SOURCE_ID], |row| row.get(0)).optional()?;
        if let Some(cached) = cached {
            return crate::generated_source::validate_cached_row(
                mapping,
                &serde_json::from_str(&cached)?,
            );
        }
        let row = crate::generated_source::descriptor_row(mapping)?;
        let values = [
            "id",
            "name",
            "root",
            "root_identity",
            "managed",
            "enabled",
            "revision",
            "binding_revision",
            "exclusions_json",
            "scan_barrier",
        ]
        .into_iter()
        .map(|field| row[field].clone())
        .collect();
        return insert_source_descriptor(db, values);
    }
    let saved: String = db.query_row(
        "SELECT mapping_json FROM catabolic_source_mappings WHERE library_id=?",
        [&mapping.library_id],
        |r| r.get(0),
    )?;
    ensure!(
        serde_json::from_str::<crate::sources::SourceMapping>(&saved)? == *mapping,
        "source descriptor acknowledgment changed"
    );
    let observed = host.source_occurrences(&mapping.native_source)?;
    ensure!(
        observed["binding"] == mapping.native_binding
            && observed["policy"] == mapping.native_policy,
        "source descriptor native authority changed"
    );
    crate::sources::check_root(&observed["binding"])?;
    let imported = crate::imported_sources::imported_sources_sync(db)?
        .into_iter()
        .find(|record| record.descriptor.id == mapping.library_id);
    if let Some(imported) = imported {
        ensure!(
            imported.proof.mapping == *mapping,
            "imported descriptor mapping differs from current native proof"
        );
        let collision: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM catabolic_source_registry WHERE id=?)",
            [&mapping.library_id],
            |row| row.get(0),
        )?;
        ensure!(
            !collision,
            "imported descriptor collides with native registration"
        );
        let row = &imported.proof.source_row;
        let values = [
            "id",
            "name",
            "root",
            "root_identity",
            "managed",
            "enabled",
            "revision",
            "binding_revision",
            "exclusions_json",
            "scan_barrier",
        ]
        .into_iter()
        .map(|field| row[field].clone())
        .collect();
        return insert_source_descriptor(db, values);
    }
    let raw: String = db.query_row(
        "SELECT descriptor_json FROM catabolic_source_registry WHERE id=?",
        [&mapping.library_id],
        |r| r.get(0),
    )?;
    let descriptor: crate::source_registry::NativeSourceDescriptor = serde_json::from_str(&raw)?;
    ensure!(
        descriptor.id == mapping.library_id
            && descriptor.root == mapping.motion_root
            && descriptor.root_identity == mapping.motion_root_identity
            && descriptor.exclusions == mapping.exclusions,
        "source descriptor differs from proven native mapping"
    );
    let exclusions = serde_json::to_string(&descriptor.exclusions)?;
    let values = vec![
        serde_json::json!(descriptor.id),
        serde_json::json!(descriptor.name),
        serde_json::json!(descriptor.root),
        serde_json::json!(descriptor.root_identity),
        serde_json::json!(0),
        serde_json::json!(1),
        serde_json::json!(0),
        serde_json::json!(mapping.motion_binding_revision),
        serde_json::json!(exclusions),
        serde_json::json!(0),
    ];
    insert_source_descriptor(db, values)
}

/// The caller has established native/source provenance. Keep the exact row
/// capability limited to this insertion, including nonzero imported revisions.
fn insert_source_descriptor(
    db: &rusqlite::Connection,
    values: Vec<serde_json::Value>,
) -> anyhow::Result<()> {
    ensure!(
        values.len() == 10,
        "source descriptor must contain every field"
    );
    let sql_values = values
        .iter()
        .map(|value| match value {
            serde_json::Value::String(text) => Ok(rusqlite::types::Value::Text(text.clone())),
            serde_json::Value::Number(number) => number
                .as_i64()
                .map(rusqlite::types::Value::Integer)
                .context("source descriptor integer is out of range"),
            _ => anyhow::bail!("unsupported source descriptor field"),
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    source_capability(db, Some(values))?;
    let inserted = db.execute(
        "INSERT INTO libraries(id,name,root,root_identity,managed,enabled,revision,binding_revision,exclusions_json,scan_barrier) VALUES(?,?,?,?,?,?,?,?,?,?)",
        rusqlite::params_from_iter(sql_values),
    );
    let revoked = source_capability(db, None);
    inserted?;
    revoked?;
    Ok(())
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ProjectionStatus {
    pub catalog_id: String,
    pub incarnation: String,
    pub generation: i64,
    pub work_count: u64,
}

/// Opaque private-connection writer reservation. Reserve Motion first, then
/// acquire the retained native owner. No SQL/connection/capability is exposed.
pub struct ProjectionWriter {
    db: rusqlite::Connection,
}
impl ProjectionWriter {
    pub fn reserve(path: &Path) -> anyhow::Result<Self> {
        let db = isolated(path).context("projection reservation isolated setup")?;
        db.execute_batch("BEGIN IMMEDIATE")
            .context("projection reservation BEGIN IMMEDIATE")?;
        Ok(Self { db })
    }
    /// Read-path admission only: never queue behind an active publication.
    /// None means SQLite BUSY/LOCKED, not malformed identity or an I/O error.
    /// The isolated connection is dropped on refusal, with no native admission
    /// acquired and no projection effects. Blocking callers remain unchanged.
    pub fn try_reserve(path: &Path) -> anyhow::Result<Option<Self>> {
        let attempt = || -> anyhow::Result<Self> {
            let db = isolated_with_timeout(path, std::time::Duration::ZERO)
                .context("projection try-reservation isolated setup")?;
            db.execute_batch("BEGIN IMMEDIATE")
                .context("projection try-reservation BEGIN IMMEDIATE")?;
            Ok(Self { db })
        };
        match attempt() {
            Ok(writer) => Ok(Some(writer)),
            Err(error) if error.downcast_ref::<rusqlite::Error>().is_some_and(|error| {
                matches!(error, rusqlite::Error::SqliteFailure(code, _) if matches!(code.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
            }) => Ok(None),
            Err(error) => Err(error),
        }
    }
    /// Rebuild under the caller's exclusive admitted native host custody.
    pub fn rebuild(
        self,
        host: &mut catabolic_runtime::host::HostSession,
    ) -> anyhow::Result<ProjectionStatus> {
        let identity = host.identity()?;
        let catalog = identity["catalog_id"]
            .as_str()
            .context("native catalog identity missing")?
            .to_owned();
        let incarnation = identity["catalog_incarnation"]
            .as_str()
            .context("native incarnation missing")?
            .to_owned();
        let generation = host.catalog_generation()?;
        let tx = &self.db;
        let bound: String = tx.query_row(
            "SELECT identity_json FROM catabolic_binding WHERE id=1",
            [],
            |r| r.get(0),
        )?;
        let bound: playscale_core::catalog_backend::Identity = serde_json::from_str(&bound)?;
        ensure!(
            bound.catalog_id == catalog && bound.incarnation == incarnation,
            "projection native binding changed"
        );
        let state: String = tx.query_row(
            "SELECT state_json FROM catabolic_source_state WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let state: playscale_core::catalog_source::State = serde_json::from_str(&state)?;
        ensure!(
            state.valid()
                && matches!(
                    state.mode,
                    playscale_core::catalog_source::Mode::Sealed { .. }
                ),
            "projection requires sealed native ownership"
        );
        let admitted:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM catabolic_cutover WHERE phase='activated') OR EXISTS(SELECT 1 FROM catabolic_cutover_sets WHERE phase='activated')",[],|r|r.get(0))?;
        ensure!(
            admitted,
            "projection requires completed host acknowledgment"
        );
        let schema: i64 = tx.query_row(
            "SELECT max(version) FROM _sqlx_migrations WHERE success=1",
            [],
            |r| r.get(0),
        )?;
        let mut guards = tx.prepare("SELECT g.table_name,g.trigger_sql,s.sql FROM catabolic_source_guards g LEFT JOIN sqlite_schema s ON s.type='trigger' AND s.name=g.trigger_name")?;
        for guard in guards.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })? {
            let (table, recorded, actual) = guard?;
            ensure!(
                crate::source_freeze::guard_definition_matches(
                    schema,
                    &table,
                    &recorded,
                    actual.as_deref()
                ),
                "projection source guard changed"
            );
        }
        drop(guards);
        let previous: Option<(String, String, i64)> = {
            use rusqlite::OptionalExtension;
            tx.query_row("SELECT catalog_id,incarnation,generation FROM catabolic_projection_state WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?
        };
        if let Some((old_catalog, old_incarnation, old_generation)) = previous {
            ensure!(
                old_catalog == catalog
                    && old_incarnation == incarnation
                    && old_generation <= generation,
                "projection restore/replacement mismatch"
            );
        }
        let mut after: Option<String> = None;
        let mut work_count = 0;
        let mut physical_items = Vec::new();
        loop {
            let page = host.projection_item_page(generation, after.as_deref(), 1000)?;
            ensure!(
                page["catalog_id"] == catalog
                    && page["incarnation"] == incarnation
                    && page["generation"].as_i64() == Some(generation),
                "projection page identity changed"
            );
            for item in page["items"]
                .as_array()
                .context("projection page items missing")?
            {
                let id = item["item_id"].as_str().context("native item ID missing")?;
                let metadata = &item["metadata"];
                if [
                    "motion_library",
                    "motion_edition",
                    "motion_version",
                    "motion_historical_file",
                    "motion_historical_timeline",
                    "motion_historical_version",
                    "motion_source_history",
                    "motion_reference_tombstone",
                ]
                .iter()
                .any(|field| !metadata[*field].is_null())
                {
                    physical_items.push(item.clone());
                }
                let model = &metadata["motion_catalog"];
                // Unknown native kinds are not silently converted into Motion works.
                if model.is_null() {
                    continue;
                }
                ensure!(
                    model["contract_version"] == 1 && model["item_id"] == id,
                    "unsupported native work projection"
                );
                let kind = model["kind"].as_str().context("Motion work kind missing")?;
                ensure!(
                    playscale_core::catalog_kind::to_native(kind) == item["kind"].as_str(),
                    "native work kind does not match the preserved Motion family"
                );
                let title = metadata["title"]
                    .as_str()
                    .context("native resolved work title missing")?;
                let revision = item["item_revision"]
                    .as_i64()
                    .context("native work revision missing")?;
                ensure!(revision > 0, "invalid native revision");
                let (bridge_catalog,bridge_incarnation,base,native_base):(String,String,i64,i64)=tx.query_row("SELECT catalog_id,incarnation,motion_base,native_base FROM catabolic_revision_bridges WHERE native_id=?",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
                ensure!(
                    bridge_catalog == catalog && bridge_incarnation == incarnation,
                    "revision bridge identity changed"
                );
                let wire_revision = playscale_core::catalog_projection::bridged_revision(
                    base,
                    native_base,
                    revision,
                )
                .context("invalid native revision bridge")?;
                let match_state = model["match_state"].as_str().unwrap_or("unmatched");
                ensure!(
                    ["unmatched", "ambiguous", "matched", "manual"].contains(&match_state),
                    "unsupported matching state"
                );
                // Never delete/reinsert: existing application FK targets and history
                // retain their exact IDs. Explicit retirement stays in native cache.
                tx.execute("INSERT INTO items(id,title,kind,catalog_revision,match_state) VALUES(?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET title=excluded.title,kind=excluded.kind,catalog_revision=excluded.catalog_revision,match_state=excluded.match_state WHERE title<>excluded.title OR kind<>excluded.kind OR catalog_revision<>excluded.catalog_revision OR match_state<>excluded.match_state",rusqlite::params![id,title,kind,wire_revision,match_state])?;
                tx.execute("INSERT INTO item_origins(item_id,title) VALUES(?,?) ON CONFLICT(item_id) DO UPDATE SET title=excluded.title WHERE title<>excluded.title",rusqlite::params![id,title])?;
                tx.execute("INSERT INTO catabolic_projection_items VALUES(?,?,?,?) ON CONFLICT(native_id) DO UPDATE SET native_revision=excluded.native_revision,metadata_json=excluded.metadata_json,generation=excluded.generation",rusqlite::params![id,revision,serde_json::to_string(item)?,generation])?;
                work_count += 1;
            }
            let next = page["next_after_id"].as_str().map(str::to_owned);
            if let Some(next) = next {
                ensure!(
                    after.as_ref().is_none_or(|old| next > *old),
                    "projection cursor did not advance"
                );
                after = Some(next)
            } else {
                break;
            }
        }
        crate::physical_projection::rebuild(tx, host, generation, &physical_items)?;
        crate::physical_library_projection::rebuild(tx, host, generation, &physical_items)?;
        ensure!(
            host.catalog_generation()? == generation,
            "native generation changed before projection commit"
        );
        tx.execute("INSERT INTO catabolic_projection_state VALUES(1,?,?,?,1) ON CONFLICT(singleton) DO UPDATE SET catalog_id=excluded.catalog_id,incarnation=excluded.incarnation,generation=excluded.generation,model_version=1",rusqlite::params![catalog,incarnation,generation])?;
        tx.execute_batch("COMMIT")?;
        // self drops here: privileged capability never returns to an ordinary pool.
        Ok(ProjectionStatus {
            catalog_id: catalog,
            incarnation,
            generation,
            work_count,
        })
    }
}
impl Drop for ProjectionWriter {
    fn drop(&mut self) {
        let _ = self.db.execute_batch("ROLLBACK");
    }
}
#[cfg(test)]
fn rebuild(
    host: &mut catabolic_runtime::host::HostSession,
    path: &Path,
) -> anyhow::Result<ProjectionStatus> {
    ProjectionWriter::reserve(path)?.rebuild(host)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn try_reserve_reports_real_contention_and_releases_without_effects() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("motion.sqlite3");
        let held = rusqlite::Connection::open(&path).unwrap();
        held.execute_batch(
            "CREATE TABLE retained(value); INSERT INTO retained VALUES(7); BEGIN IMMEDIATE",
        )
        .unwrap();
        let before = std::time::Instant::now();
        assert!(ProjectionWriter::try_reserve(&path).unwrap().is_none());
        assert!(before.elapsed() < std::time::Duration::from_secs(1));
        held.execute_batch("ROLLBACK").unwrap();
        let admitted = ProjectionWriter::try_reserve(&path).unwrap().unwrap();
        assert_eq!(
            admitted
                .db
                .query_row("SELECT value FROM retained", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            7
        );
        drop(admitted);
        held.execute_batch("BEGIN IMMEDIATE; ROLLBACK").unwrap();
    }
    #[test]
    fn try_reserve_does_not_hide_non_contention_failures() {
        let root = tempfile::tempdir().unwrap();
        assert!(ProjectionWriter::try_reserve(&root.path().join("missing")).is_err());
        assert!(ProjectionWriter::try_reserve(root.path()).is_err());
        let corrupt = root.path().join("corrupt");
        std::fs::write(&corrupt, b"not a SQLite database").unwrap();
        assert!(ProjectionWriter::try_reserve(&corrupt).is_err());
    }
    #[tokio::test]
    async fn ordinary_pool_has_no_sql_way_to_arm_projection() {
        use sqlx::Connection;
        let mut db = sqlx::SqliteConnection::connect(":memory:").await.unwrap();
        configure_connection(&mut db).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT motion_native_projection('items')")
                .fetch_one(&mut db)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT motion_native_projection('catabolic_cutover')")
                .fetch_one(&mut db)
                .await
                .unwrap(),
            0
        );
        assert!(
            sqlx::query("SELECT motion_native_projection('items',1)")
                .execute(&mut db)
                .await
                .is_err()
        );
    }
    #[test]
    fn source_descriptor_capability_is_exact_insert_only_and_revocable() {
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.sqlite3");
        drop(rusqlite::Connection::open(&path).unwrap());
        let db = isolated(&path).unwrap();
        db.execute_batch("CREATE TABLE libraries(id TEXT PRIMARY KEY,name TEXT,root TEXT,root_identity TEXT,managed INTEGER,enabled INTEGER,revision INTEGER,binding_revision INTEGER,exclusions_json TEXT,scan_barrier INTEGER); CREATE TRIGGER guarded_insert BEFORE INSERT ON libraries WHEN motion_native_source_projection(NEW.id,NEW.name,NEW.root,NEW.root_identity,NEW.managed,NEW.enabled,NEW.revision,NEW.binding_revision,NEW.exclusions_json,NEW.scan_barrier)<>1 BEGIN SELECT RAISE(ABORT,'denied'); END; CREATE TRIGGER guarded_update BEFORE UPDATE ON libraries BEGIN SELECT RAISE(ABORT,'denied'); END;").unwrap();
        let insert = "INSERT INTO libraries VALUES('source','Name','/root','1:2',0,1,0,1,'[]',0)";
        assert!(db.execute(insert, []).is_err());
        let exact = vec![
            json!("source"),
            json!("Name"),
            json!("/root"),
            json!("1:2"),
            json!(0),
            json!(1),
            json!(0),
            json!(1),
            json!("[]"),
            json!(0),
        ];
        source_capability(&db, Some(exact)).unwrap();
        assert!(
            db.execute(
                "INSERT INTO libraries VALUES('other','Name','/root','1:2',0,1,0,1,'[]',0)",
                []
            )
            .is_err()
        );
        assert!(
            db.execute(
                "INSERT INTO libraries VALUES('source','Different','/root','1:2',0,1,0,1,'[]',0)",
                []
            )
            .is_err()
        );
        db.execute(insert, []).unwrap();
        assert!(
            db.execute("UPDATE libraries SET root='/replacement'", [])
                .is_err()
        );
        assert_eq!(
            db.query_row("SELECT motion_native_projection('libraries')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        source_capability(&db, None).unwrap();
        assert_eq!(db.query_row("SELECT motion_native_source_projection('source','Name','/root','1:2',0,1,0,1,'[]',0)",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        drop(db);
        let reopened = isolated(&path).unwrap();
        assert_eq!(reopened.query_row("SELECT motion_native_source_projection('source','Name','/root','1:2',0,1,0,1,'[]',0)",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    }
    #[test]
    fn imported_source_insert_preserves_full_row_and_revokes_after_error() {
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("imported-source.sqlite3");
        drop(rusqlite::Connection::open(&path).unwrap());
        let db = isolated(&path).unwrap();
        db.execute_batch("CREATE TABLE libraries(id TEXT PRIMARY KEY,name TEXT,root TEXT,root_identity TEXT,managed INTEGER,enabled INTEGER,revision INTEGER,binding_revision INTEGER,exclusions_json TEXT,scan_barrier INTEGER); CREATE TRIGGER guarded_insert BEFORE INSERT ON libraries WHEN motion_native_source_projection(NEW.id,NEW.name,NEW.root,NEW.root_identity,NEW.managed,NEW.enabled,NEW.revision,NEW.binding_revision,NEW.exclusions_json,NEW.scan_barrier)<>1 BEGIN SELECT RAISE(ABORT,'denied'); END;").unwrap();
        let row = vec![
            json!("imported"),
            json!("Imported source"),
            json!("/source"),
            json!("11:12"),
            json!(0),
            json!(1),
            json!(19),
            json!(7),
            json!("[\"ignored\"]"),
            json!(23),
        ];
        insert_source_descriptor(&db, row.clone()).unwrap();
        assert_eq!(db.query_row("SELECT revision,binding_revision,scan_barrier,exclusions_json FROM libraries WHERE id='imported'", [], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?))).unwrap(), (19,7,23,"[\"ignored\"]".into()));
        assert!(insert_source_descriptor(&db, row).is_err());
        assert_eq!(db.query_row(r#"SELECT motion_native_source_projection('imported','Imported source','/source','11:12',0,1,19,7,'["ignored"]',23)"#, [], |r| r.get::<_,i64>(0)).unwrap(),0);
        assert!(db.execute("INSERT INTO libraries SELECT 'other',name,root,root_identity,managed,enabled,revision,binding_revision,exclusions_json,scan_barrier FROM libraries", []).is_err());
    }
    #[test]
    fn isolated_capability_is_allowlisted_and_does_not_survive_close() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("motion.sqlite3");
        let db = rusqlite::Connection::open(&path).unwrap();
        drop(db);
        let privileged = isolated(&path).unwrap();
        assert_eq!(
            privileged
                .query_row("SELECT motion_native_projection('items')", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        for table in [
            "profiles",
            "credentials",
            "libraries",
            "catabolic_cutover",
            "catabolic_source_mappings",
        ] {
            assert_eq!(
                privileged
                    .query_row("SELECT motion_native_projection(?)", [table], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        drop(privileged);
        let ordinary = rusqlite::Connection::open(&path).unwrap();
        assert!(
            ordinary
                .query_row("SELECT motion_native_projection('items')", [], |r| r
                    .get::<_, i64>(0))
                .is_err()
        );
    }
    async fn fixture() -> (
        tempfile::TempDir,
        sqlx::SqlitePool,
        crate::cutover::ServerAdmission,
        std::path::PathBuf,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("motion.sqlite3");
        let native = dir.path().join("native.sqlite3");
        crate::initialize(native.clone()).await.unwrap();
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .after_connect(|conn, _| Box::pin(configure_connection(conn)))
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("../../migrations").run(&db).await.unwrap();
        let backend = crate::NativeCatalog::bind(
            db.clone(),
            native.clone(),
            crate::inspect(native.clone()).await.unwrap().identity,
        )
        .await
        .unwrap();
        backend
            .freeze_motion_source("projection-test", 0)
            .await
            .unwrap();
        backend
            .capture_motion_import("projection-test")
            .await
            .unwrap();
        backend.stage_import("projection-test").await.unwrap();
        backend
            .prepare_motion_cutover("projection-test")
            .await
            .unwrap();
        backend
            .activate_motion_cutover(std::time::Duration::from_secs(2))
            .await
            .unwrap();
        let admission = crate::cutover::open_server(db.clone(), native, "default".into())
            .await
            .unwrap();
        (dir, db, admission, path)
    }
    // Projection tests seed their explicit fixture baseline immediately after
    // the fixture's first synchronous native put. Production uses receipts only.
    async fn fixture_bridge(
        db: &sqlx::SqlitePool,
        host: &catabolic_runtime::host::HostSession,
        id: &str,
    ) {
        let identity = host.identity().unwrap();
        let page = host
            .projection_item_page(host.catalog_generation().unwrap(), None, 1000)
            .unwrap();
        let head = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["item_id"] == id)
            .unwrap()["item_revision"]
            .as_i64()
            .unwrap();
        sqlx::query("INSERT INTO catabolic_revision_bridges VALUES(?,?,?,?,?,?)")
            .bind(id)
            .bind(identity["catalog_id"].as_str().unwrap())
            .bind(identity["catalog_incarnation"].as_str().unwrap())
            .bind(9i64)
            .bind(head)
            .bind("{\"kind\":\"explicit_projection_test_fixture\"}")
            .execute(db)
            .await
            .unwrap();
    }
    fn work(id: &str, title: &str) -> serde_json::Value {
        serde_json::json!({"id":id,"kind":"video","metadata":{"title":title,"motion_catalog":{"contract_version":1,"item_id":id,"kind":"video","match_state":"unmatched"}}})
    }
    #[tokio::test]
    async fn private_generated_descriptor_requires_exact_native_proof_and_full_cache_row() {
        use serde_json::json;
        let (directory, pool, mut admission, path) = fixture().await;
        let root = directory.path().join("generated-final");
        std::fs::create_dir(&root).unwrap();
        let host = admission.admission.admitted_host().unwrap();
        let prepared = crate::source_registry::register_with_host(
            host,
            crate::generated_source::SOURCE_ID,
            "motion:private-generated",
            "Generated media",
            root,
            vec![],
        )
        .unwrap();
        let mapping = prepared.mapping().clone();
        sqlx::query(
            "INSERT INTO catabolic_generated_source(id,mapping_json,created_at) VALUES(?,?,1)",
        )
        .bind(crate::generated_source::SOURCE_ID)
        .bind(serde_json::to_string(&mapping).unwrap())
        .execute(&pool)
        .await
        .unwrap();
        let expected = crate::generated_source::descriptor_row(&mapping).unwrap();
        let db = isolated(&path).unwrap();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert!(db.execute("INSERT INTO libraries(id,name,root,root_identity,managed,enabled,revision,binding_revision,exclusions_json,scan_barrier) VALUES('ordinary','Generated media','/other','0:0',1,1,0,1,'[]',0)", []).is_err());
        materialize_source(&db, host, &mapping).unwrap();
        materialize_source(&db, host, &mapping).unwrap();
        let cached: String = db.query_row("SELECT json_object('id',id,'name',name,'root',root,'root_identity',root_identity,'managed',managed,'enabled',enabled,'revision',revision,'binding_revision',binding_revision,'exclusions_json',exclusions_json,'scan_barrier',scan_barrier) FROM libraries WHERE id=?", [crate::generated_source::SOURCE_ID], |row|row.get(0)).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&cached).unwrap(),
            expected
        );
        assert_eq!(
            db.query_row("SELECT motion_native_projection('libraries')", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            0
        );
        assert_eq!(db.query_row("SELECT motion_native_source_projection(id,name,root,root_identity,managed,enabled,revision,binding_revision,exclusions_json,scan_barrier) FROM libraries WHERE id=?", [crate::generated_source::SOURCE_ID], |row|row.get::<_,i64>(0)).unwrap(), 0);
        assert!(
            db.execute(
                "UPDATE libraries SET enabled=0 WHERE id=?",
                [crate::generated_source::SOURCE_ID]
            )
            .is_err()
        );
        db.execute_batch("ROLLBACK").unwrap();
        // Deliberately corrupt ONLY this disposable test cache through the
        // internal test fixture insertion, then exercise real owner validation.
        for (field, wrong) in [
            ("name", json!("Different")),
            ("managed", json!(0)),
            ("enabled", json!(0)),
            ("revision", json!(2)),
            ("binding_revision", json!(2)),
            ("exclusions_json", json!("[\"hidden\"]")),
            ("scan_barrier", json!(5)),
        ] {
            db.execute_batch("BEGIN IMMEDIATE").unwrap();
            let mut cached = expected.clone();
            cached[field] = wrong;
            let values = [
                "id",
                "name",
                "root",
                "root_identity",
                "managed",
                "enabled",
                "revision",
                "binding_revision",
                "exclusions_json",
                "scan_barrier",
            ]
            .into_iter()
            .map(|key| cached[key].clone())
            .collect();
            insert_source_descriptor(&db, values).unwrap();
            assert!(materialize_source(&db, host, &mapping).is_err(), "{field}");
            db.execute_batch("ROLLBACK").unwrap();
        }
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        let mut changed = mapping.clone();
        changed.native_binding["inode"] = json!(0);
        assert!(materialize_source(&db, host, &changed).is_err());
        db.execute_batch("ROLLBACK").unwrap();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        db.execute(
            "INSERT INTO catabolic_source_registry(id,descriptor_json,created_at) VALUES(?,'{}',1)",
            [crate::generated_source::SOURCE_ID],
        )
        .unwrap();
        assert!(
            materialize_source(&db, host, &mapping).is_err(),
            "private source cannot acquire public registry authority"
        );
        db.execute_batch("ROLLBACK").unwrap();
    }
    #[tokio::test]
    async fn native_projection_preserves_fk_history_and_rejects_ordinary_writer() {
        let (_dir, db, mut admission, path) = fixture().await;
        let host = admission.admission.admitted_host().unwrap();
        let cancel = catabolic_runtime::host::Cancellation::default();
        host.put_items(&[work("new-work", "First")], &cancel)
            .unwrap();
        fixture_bridge(&db, host, "new-work").await;
        let first = rebuild(host, &path).unwrap();
        assert_eq!(first.work_count, 1);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT catalog_revision FROM items WHERE id='new-work'")
                .fetch_one(&db)
                .await
                .unwrap(),
            9
        );
        sqlx::query("INSERT INTO progress VALUES('default','new-work',42,1)")
            .execute(&db)
            .await
            .unwrap();
        assert!(
            sqlx::query("UPDATE items SET title='Legacy writer' WHERE id='new-work'")
                .execute(&db)
                .await
                .is_err()
        );
        host.put_items(&[work("new-work", "Native updated")], &cancel)
            .unwrap();
        let second = rebuild(host, &path).unwrap();
        assert!(second.generation > first.generation);
        assert!(
            sqlx::query_scalar::<_, i64>("SELECT catalog_revision FROM items WHERE id='new-work'")
                .fetch_one(&db)
                .await
                .unwrap()
                > 9
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT title FROM items WHERE id='new-work'")
                .fetch_one(&db)
                .await
                .unwrap(),
            "Native updated"
        );
        assert_eq!(
            sqlx::query_scalar::<_, f64>(
                "SELECT position_seconds FROM progress WHERE item_id='new-work'"
            )
            .fetch_one(&db)
            .await
            .unwrap(),
            42.0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT motion_native_projection('items')")
                .fetch_one(&db)
                .await
                .unwrap(),
            0
        );
        host.close().unwrap();
    }
    #[tokio::test]
    async fn failed_rebuild_rolls_back_rows_and_generation_as_one_transaction() {
        let (_dir, db, mut admission, path) = fixture().await;
        let host = admission.admission.admitted_host().unwrap();
        let cancel = catabolic_runtime::host::Cancellation::default();
        host.put_items(&[work("a", "Before")], &cancel).unwrap();
        fixture_bridge(&db, host, "a").await;
        let before = rebuild(host, &path).unwrap();
        let mut bad = work("z", "Bad");
        bad["metadata"]["motion_catalog"]["contract_version"] = serde_json::json!(99);
        host.put_items(
            &[
                work("a", "Not committed to cache"),
                work("b", "Also rolled back"),
                bad,
            ],
            &cancel,
        )
        .unwrap();
        fixture_bridge(&db, host, "b").await;
        assert!(rebuild(host, &path).is_err());
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT title FROM items WHERE id='a'")
                .fetch_one(&db)
                .await
                .unwrap(),
            "Before"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM items WHERE id='b'")
                .fetch_one(&db)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT generation FROM catabolic_projection_state")
                .fetch_one(&db)
                .await
                .unwrap(),
            before.generation
        );
        assert!(
            sqlx::query("INSERT INTO items(id,title,kind) VALUES('legacy','Legacy','video')")
                .execute(&db)
                .await
                .is_err()
        );
        host.close().unwrap();
    }
    // Process-kill coverage for the isolated SQLite capability/transaction, not
    // a claim that a host-generation race is reproduced by this helper.
    #[test]
    #[ignore = "child process fixture"]
    fn interrupted_projection_child() {
        let Some(path) = std::env::var_os("MOTION_PROJECTION_TEST_DB") else {
            return;
        };
        let barrier = std::env::var_os("MOTION_PROJECTION_TEST_BARRIER").unwrap();
        let mut db = isolated(Path::new(&path)).unwrap();
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        tx.execute(
            "UPDATE items SET title='Uncommitted child' WHERE id='crash-work'",
            [],
        )
        .unwrap();
        tx.execute(
            "UPDATE catabolic_projection_state SET generation=generation+1",
            [],
        )
        .unwrap();
        std::fs::write(barrier, "inside projection transaction").unwrap();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    }
    #[tokio::test]
    async fn killed_projection_process_rolls_back_capability_and_bookkeeping() {
        let (dir, db, mut admission, path) = fixture().await;
        let host = admission.admission.admitted_host().unwrap();
        host.put_items(
            &[work("crash-work", "Before crash")],
            &catabolic_runtime::host::Cancellation::default(),
        )
        .unwrap();
        fixture_bridge(&db, host, "crash-work").await;
        let before = rebuild(host, &path).unwrap();
        let barrier = dir.path().join("child-entered");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "projection::tests::interrupted_projection_child",
                "--ignored",
                "--nocapture",
            ])
            .env("MOTION_PROJECTION_TEST_DB", &path)
            .env("MOTION_PROJECTION_TEST_BARRIER", &barrier)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        while !barrier.exists() && tokio::time::Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let entered = barrier.exists();
        let _ = child.kill();
        let _ = child.wait();
        assert!(entered, "child did not enter write transaction");
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT title FROM items WHERE id='crash-work'")
                .fetch_one(&db)
                .await
                .unwrap(),
            "Before crash"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT generation FROM catabolic_projection_state")
                .fetch_one(&db)
                .await
                .unwrap(),
            before.generation
        );
        assert!(
            sqlx::query("UPDATE items SET title='Unauthorized after crash'")
                .execute(&db)
                .await
                .is_err()
        );
        host.close().unwrap();
    }
    #[tokio::test]
    async fn stale_or_replaced_projection_checkpoint_cannot_be_adopted() {
        let (_dir, db, mut admission, path) = fixture().await;
        let host = admission.admission.admitted_host().unwrap();
        let before = rebuild(host, &path).unwrap();
        sqlx::query("UPDATE catabolic_projection_state SET incarnation='unrelated-incarnation'")
            .execute(&db)
            .await
            .unwrap();
        assert!(rebuild(host, &path).is_err());
        sqlx::query("UPDATE catabolic_projection_state SET incarnation=?,generation=?")
            .bind(&before.incarnation)
            .bind(before.generation + 1)
            .execute(&db)
            .await
            .unwrap();
        assert!(rebuild(host, &path).is_err());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT generation FROM catabolic_projection_state")
                .fetch_one(&db)
                .await
                .unwrap(),
            before.generation + 1
        );
        host.close().unwrap();
    }
}
