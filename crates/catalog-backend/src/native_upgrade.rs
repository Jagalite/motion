//! Offline operator-only native39→40 companion. A durable intent precedes the
//! native migration; only the native exact locked reconciliation owner can ACK.
//! Neither a schema number nor absence of a receipt permits a new binding.
use anyhow::{Context, ensure};
use catabolic_store::migration::{self, NativeUpgradeQuiescence};
use playscale_core::catalog_backend::Identity;
use rusqlite::{Connection, OptionalExtension, ffi};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    ffi::CStr,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    contract_version: u32,
    from: Identity,
    to: Identity,
    native_path: PathBuf,
    plan: Value,
}
struct ExactInsert {
    kind: String,
    json: String,
}
unsafe extern "C" fn exact_insert(
    ctx: *mut ffi::sqlite3_context,
    argc: i32,
    argv: *mut *mut ffi::sqlite3_value,
) {
    let permit = unsafe { ffi::sqlite3_user_data(ctx) }.cast::<ExactInsert>();
    let allowed = if argc == 2 && !argv.is_null() && !permit.is_null() {
        let a = unsafe { ffi::sqlite3_value_text(*argv) };
        let b = unsafe { ffi::sqlite3_value_text(*argv.add(1)) };
        !a.is_null()
            && !b.is_null()
            && unsafe { CStr::from_ptr(a.cast()) }.to_bytes() == unsafe { &*permit }.kind.as_bytes()
            && unsafe { CStr::from_ptr(b.cast()) }.to_bytes() == unsafe { &*permit }.json.as_bytes()
    } else {
        false
    };
    unsafe { ffi::sqlite3_result_int(ctx, i32::from(allowed)) };
}
unsafe extern "C" fn destroy_insert(raw: *mut std::ffi::c_void) {
    if !raw.is_null() {
        drop(unsafe { Box::from_raw(raw.cast::<ExactInsert>()) });
    }
}
fn insert(db: &Connection, kind: &str, text: String) -> anyhow::Result<()> {
    let grant = Box::into_raw(Box::new(ExactInsert {
        kind: kind.into(),
        json: text.clone(),
    }));
    let rc = unsafe {
        ffi::sqlite3_create_function_v2(
            db.handle(),
            c"motion_native_upgrade".as_ptr(),
            2,
            ffi::SQLITE_UTF8 | ffi::SQLITE_INNOCUOUS,
            grant.cast(),
            Some(exact_insert),
            None,
            None,
            Some(destroy_insert),
        )
    };
    ensure!(
        rc == ffi::SQLITE_OK,
        "cannot install exact isolated upgrade insertion"
    );
    let sql = match kind {
        "intent" => "INSERT INTO catabolic_native_upgrade_intent VALUES(1,?)",
        "ack" => "INSERT INTO catabolic_native_upgrade_ack VALUES(1,?)",
        _ => anyhow::bail!("invalid private upgrade insertion"),
    };
    let result = db.execute(sql, [&text]);
    // Revoke immediately on either SQL outcome, before returning to caller.
    let revoked = unsafe {
        ffi::sqlite3_create_function_v2(
            db.handle(),
            c"motion_native_upgrade".as_ptr(),
            2,
            ffi::SQLITE_UTF8,
            std::ptr::null_mut(),
            None,
            None,
            None,
            None,
        )
    };
    ensure!(
        revoked == ffi::SQLITE_OK,
        "cannot revoke isolated upgrade insertion"
    );
    result?;
    Ok(())
}
fn expected_schema() -> anyhow::Result<Vec<(String, String, String)>> {
    let db = Connection::open_in_memory()?;
    db.execute_batch(include_str!(
        "../../../migrations/0089_catabolic_native_upgrade.sql"
    ))?;
    let mut q = db.prepare(
        "SELECT type,name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type,name",
    )?;
    let rows = q
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
fn verify_schema(db: &Connection) -> anyhow::Result<()> {
    let mut q=db.prepare("SELECT type,name,sql FROM sqlite_schema WHERE tbl_name IN ('catabolic_native_upgrade_intent','catabolic_native_upgrade_ack') AND sql IS NOT NULL ORDER BY type,name")?;
    let actual = q
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<Vec<(String, String, String)>, _>>()?;
    ensure!(
        actual == expected_schema()?,
        "native upgrade journal schema or capability guards changed"
    );
    Ok(())
}
fn open(path: &Path) -> anyhow::Result<Connection> {
    ensure!(
        std::fs::symlink_metadata(path)?.is_file(),
        "Motion upgrade requires existing regular database"
    );
    let db = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.pragma_update(None, "foreign_keys", true)?;
    db.pragma_update(None, "synchronous", "FULL")?;
    db.execute_batch("BEGIN IMMEDIATE")?;
    verify_schema(&db)?;
    let state: String = db.query_row(
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
        "native upgrade requires sealed catalog ownership"
    );
    let admitted:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM catabolic_cutover WHERE phase='activated') OR EXISTS(SELECT 1 FROM catabolic_cutover_sets WHERE phase='activated')",[],|r|r.get(0))?;
    ensure!(
        admitted,
        "native upgrade requires completed host activation"
    );
    let schema: i64 = db.query_row(
        "SELECT max(version) FROM _sqlx_migrations WHERE success=1",
        [],
        |r| r.get(0),
    )?;
    let mut guards=db.prepare("SELECT g.table_name,g.trigger_sql,s.sql FROM catabolic_source_guards g LEFT JOIN sqlite_schema s ON s.type='trigger' AND s.name=g.trigger_name")?;
    for row in guards.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })? {
        let (table, recorded, actual) = row?;
        ensure!(
            crate::source_freeze::guard_definition_matches(
                schema,
                &table,
                &recorded,
                actual.as_deref()
            ),
            "native upgrade source guard changed"
        );
    }
    drop(guards);
    Ok(db)
}
fn binding(db: &Connection) -> anyhow::Result<Identity> {
    let text: String = db.query_row(
        "SELECT identity_json FROM catabolic_binding WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    Ok(serde_json::from_str(&text)?)
}
fn saved(db: &Connection) -> anyhow::Result<Intent> {
    let text: String = db.query_row(
        "SELECT intent_json FROM catabolic_native_upgrade_intent WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        text.len() <= 1024 * 1024 + 4096,
        "oversized retained upgrade intent"
    );
    let intent: Intent = serde_json::from_str(&text)?;
    validate_intent(&binding(db)?, &intent)?;
    Ok(intent)
}
fn validate_intent(original: &Identity, intent: &Intent) -> anyhow::Result<()> {
    ensure!(
        intent.contract_version == 1
            && original == &intent.from
            && intent.from.schema_version == 39
            && intent.to.schema_version == 40
            && intent.from.catalog_id == intent.to.catalog_id
            && intent.from.incarnation == intent.to.incarnation
            && intent.plan["from_schema"] == 39
            && intent.plan["to_schema"] == 40
            && intent.plan["input"]["database"] == serde_json::to_value(&intent.native_path)?
            && intent.plan["input"]["catalog_identity"]["catalog_id"] == intent.from.catalog_id
            && intent.plan["input"]["catalog_identity"]["incarnation"] == intent.from.incarnation,
        "retained native upgrade identity changed"
    );
    let mut unsigned = intent.plan.clone();
    let fingerprint = unsigned
        .as_object_mut()
        .context("upgrade plan object missing")?
        .remove("plan_fingerprint");
    ensure!(
        fingerprint
            == Some(Value::String(catabolic_core::sha256(
                catabolic_core::encode(&unsigned)
            ))),
        "retained native upgrade plan digest changed"
    );
    Ok(())
}
/// Explicit operator preparation only. Does not migrate either database. The
/// caller must already have stopped service admission and joined every worker.
pub fn prepare(
    motion: &Path,
    native: &Path,
    quiescence: NativeUpgradeQuiescence,
) -> anyhow::Result<Value> {
    require_quiescence(quiescence)?;
    let db = open(motion)?;
    let path = std::fs::canonicalize(native)?;
    let existing: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM catabolic_native_upgrade_intent)",
        [],
        |r| r.get(0),
    )?;
    if existing {
        let intent = saved(&db)?;
        ensure!(intent.native_path == path, "upgrade path changed");
        return Ok(serde_json::to_value(intent)?);
    }
    let from = binding(&db)?;
    ensure!(
        from.schema_version == 39,
        "only explicit bound native39→40 upgrade is supported"
    );
    let plan = migration::plan_native_39_to_40(&path)?;
    ensure!(
        catabolic_core::encode(&plan).len() <= 1024 * 1024,
        "native upgrade plan exceeds retained proof budget"
    );
    ensure!(
        plan["from_schema"] == 39 && plan["to_schema"] == 40,
        "native catalog not at exact upgrade source"
    );
    ensure!(
        plan["work_settled"] == true,
        "native pending ownership must be settled before saving upgrade intent"
    );
    ensure!(
        matches!(
            plan["input"]["journal_mode"].as_str(),
            Some("delete" | "truncate" | "persist")
        ),
        "offline journal-mode conversion must precede the saved native upgrade plan"
    );
    let to = Identity {
        schema_version: 40,
        ..from.clone()
    };
    let intent = Intent {
        contract_version: 1,
        from,
        to,
        native_path: path,
        plan,
    };
    validate_intent(&binding(&db)?, &intent)?;
    let value = serde_json::to_value(&intent)?;
    insert(&db, "intent", serde_json::to_string(&value)?)?;
    db.execute_batch("COMMIT")?;
    Ok(value)
}
fn require_quiescence(q: NativeUpgradeQuiescence) -> anyhow::Result<()> {
    ensure!(
        q.admission_stopped && q.readers_and_workers_stopped && q.exclusive_operational_ownership,
        "explicit operator shutdown and exclusive operational ownership are required"
    );
    Ok(())
}
/// Execute the exact saved plan or reconcile its exact native commit. On any
/// uncertainty the durable intent remains pending and ordinary startup refuses.
pub fn execute(
    motion: &Path,
    native: &Path,
    quiescence: NativeUpgradeQuiescence,
) -> anyhow::Result<Value> {
    require_quiescence(quiescence)?;
    let db = open(motion)?;
    let intent = saved(&db)?;
    ensure!(
        std::fs::canonicalize(native)? == intent.native_path,
        "upgrade path changed"
    );
    let existing: Option<String> = db
        .query_row(
            "SELECT proof_json FROM catabolic_native_upgrade_ack WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(text) = existing {
        let proof: Value = serde_json::from_str(&text)?;
        validate_ack(&intent, &proof)?;
        return Ok(proof);
    }
    let actual = migration::plan_native_39_to_40(&intent.native_path)?;
    match actual["from_schema"].as_u64() {
        Some(39) => {
            migration::upgrade_native_39_to_40(
                &intent.native_path,
                &intent.plan,
                quiescence,
                None,
                &|_, _, _| Ok(()),
            )?;
        }
        Some(40) => {}
        _ => anyhow::bail!("native upgrade outcome requires operator investigation"),
    }
    let proof = migration::with_reconciled_native_upgrade(
        &intent.native_path,
        &intent.plan,
        &intent.from.catalog_id,
        &intent.from.incarnation,
        quiescence,
        |proof| {
            let value = proof.proof().clone();
            validate_ack(&intent, &value).map_err(|e| catabolic_store::Error(e.to_string()))?;
            insert(
                &db,
                "ack",
                serde_json::to_string(&value).map_err(|e| catabolic_store::Error(e.to_string()))?,
            )
            .map_err(|e| catabolic_store::Error(e.to_string()))?;
            // Native locks and positive proof are still held at this commit.
            db.execute_batch("COMMIT")?;
            Ok(value)
        },
    )?;
    Ok(proof)
}
fn validate_ack(intent: &Intent, proof: &Value) -> anyhow::Result<()> {
    ensure!(
        proof["contract_version"] == 1
            && proof["catalog_id"] == intent.from.catalog_id
            && proof["incarnation"] == intent.from.incarnation
            && proof["from_schema"] == 39
            && proof["to_schema"] == 40
            && proof["plan_fingerprint"] == intent.plan["plan_fingerprint"]
            && proof["input_fingerprint"] == intent.plan["input_fingerprint"]
            && proof["native_schema_fingerprint"] == intent.plan["native_schema_fingerprint"],
        "native upgrade ACK does not match exact saved plan"
    );
    Ok(())
}
/// Existing activation/import provenance retains the original binding. Only
/// actual server admission uses this reviewed append-only effective identity.
pub async fn effective_identity(
    db: &sqlx::SqlitePool,
    original: &Identity,
) -> anyhow::Result<Identity> {
    let mut tx = db.begin().await?;
    effective_identity_in(&mut tx, original).await
}
pub(crate) async fn effective_identity_in(
    db: &mut sqlx::SqliteConnection,
    original: &Identity,
) -> anyhow::Result<Identity> {
    let actual:Vec<(String,String,String)>=sqlx::query_as("SELECT type,name,sql FROM sqlite_schema WHERE tbl_name IN ('catabolic_native_upgrade_intent','catabolic_native_upgrade_ack') AND sql IS NOT NULL ORDER BY type,name").fetch_all(&mut *db).await?;
    if actual.is_empty() {
        let schema: i64 =
            sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations WHERE success=1")
                .fetch_one(&mut *db)
                .await?;
        ensure!(
            (69..=88).contains(&schema),
            "native upgrade journal missing from reviewed current schema"
        );
        return Ok(original.clone());
    }
    ensure!(
        actual == expected_schema()?,
        "native upgrade journal schema or capability guards changed"
    );
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT intent_json FROM catabolic_native_upgrade_intent WHERE singleton=1",
    )
    .fetch_optional(&mut *db)
    .await?;
    let Some(raw) = raw else {
        return Ok(original.clone());
    };
    let intent = effective_intent(original, &raw)?;
    let proof: Option<String> =
        sqlx::query_scalar("SELECT proof_json FROM catabolic_native_upgrade_ack WHERE singleton=1")
            .fetch_optional(&mut *db)
            .await?;
    effective_ack(intent, proof.as_deref())
}
fn effective_intent(original: &Identity, raw: &str) -> anyhow::Result<Intent> {
    ensure!(
        raw.len() <= 1024 * 1024 + 4096,
        "oversized retained upgrade intent"
    );
    let intent: Intent = serde_json::from_str(raw)?;
    validate_intent(original, &intent)?;
    Ok(intent)
}
fn effective_ack(intent: Intent, proof: Option<&str>) -> anyhow::Result<Identity> {
    let proof =
        proof.context("native upgrade is pending; use explicit offline operator reconciliation")?;
    validate_ack(&intent, &serde_json::from_str(proof)?)?;
    Ok(intent.to)
}
/// Read an independently opened copied Motion image with the exact same upgrade
/// schema and provenance rules as live admission. The caller owns snapshot and
/// complete-image validation; this read issues no restore or mutation authority.
pub(crate) fn effective_identity_sync(
    db: &Connection,
    original: &Identity,
) -> anyhow::Result<Identity> {
    let mut query = db.prepare("SELECT type,name,sql FROM sqlite_schema WHERE tbl_name IN ('catabolic_native_upgrade_intent','catabolic_native_upgrade_ack') AND sql IS NOT NULL ORDER BY type,name")?;
    let actual = query
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if actual.is_empty() {
        let schema: i64 = db.query_row(
            "SELECT max(version) FROM _sqlx_migrations WHERE success=1",
            [],
            |r| r.get(0),
        )?;
        ensure!(
            (69..=88).contains(&schema),
            "native upgrade journal missing from reviewed current schema"
        );
        return Ok(original.clone());
    }
    ensure!(
        actual == expected_schema()?,
        "native upgrade journal schema or capability guards changed"
    );
    let raw: Option<String> = db
        .query_row(
            "SELECT intent_json FROM catabolic_native_upgrade_intent WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let Some(raw) = raw else {
        return Ok(original.clone());
    };
    let intent = effective_intent(original, &raw)?;
    let proof: Option<String> = db
        .query_row(
            "SELECT proof_json FROM catabolic_native_upgrade_ack WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    effective_ack(intent, proof.as_deref())
}

#[cfg(test)]
mod effective_image_tests;
