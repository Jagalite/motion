//! Append-only deployment lifecycle. Historical mapping validity is deliberately
//! separate from current admission; an absent lifecycle row never means active.
mod renewal;
use crate::{source_registry::NativeSourceDescriptor, sources::SourceMapping};
use anyhow::{Context, ensure};
pub use playscale_core::source_lifecycle::{LifecycleProof, State, WithdrawalFacts};
use rusqlite::{OptionalExtension, ffi};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqliteConnection;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Baseline {
    descriptor: NativeSourceDescriptor,
    mapping: SourceMapping,
    source_revision: i64,
    binding_revision: i64,
    imported: Option<crate::imported_sources::ImportedSourceDescriptor>,
}
fn baseline(
    mapping: SourceMapping,
    descriptor: Option<String>,
    imported: Vec<crate::imported_sources::ImportedSourceDescriptor>,
) -> anyhow::Result<Baseline> {
    let imported = imported
        .into_iter()
        .find(|row| row.descriptor.id == mapping.library_id);
    ensure!(
        descriptor.is_none() || imported.is_none(),
        "source descriptor owners collide"
    );
    let (descriptor, source_revision, binding_revision) = if let Some(record) = &imported {
        ensure!(
            record.proof.mapping == mapping,
            "imported lifecycle mapping differs"
        );
        (
            record.descriptor.clone(),
            record.proof.source_row["revision"]
                .as_i64()
                .context("source revision missing")?,
            record.proof.source_row["binding_revision"]
                .as_i64()
                .context("binding revision missing")?,
        )
    } else {
        (
            serde_json::from_str::<NativeSourceDescriptor>(
                &descriptor.context("qualified source descriptor missing")?,
            )?,
            1,
            1,
        )
    };
    ensure!(
        descriptor.id == mapping.library_id
            && descriptor.root == mapping.motion_root
            && descriptor.root_identity == mapping.motion_root_identity
            && descriptor.exclusions == mapping.exclusions,
        "source lifecycle descriptor differs"
    );
    ensure!(
        source_revision >= 0 && binding_revision >= 0,
        "invalid source revision"
    );
    Ok(Baseline {
        descriptor,
        mapping,
        source_revision,
        binding_revision,
        imported,
    })
}
async fn baseline_in(db: &mut SqliteConnection, id: &str) -> anyhow::Result<Baseline> {
    let raw: String =
        sqlx::query_scalar("SELECT mapping_json FROM catabolic_source_mappings WHERE library_id=?")
            .bind(id)
            .fetch_one(&mut *db)
            .await?;
    let descriptor =
        sqlx::query_scalar("SELECT descriptor_json FROM catabolic_source_registry WHERE id=?")
            .bind(id)
            .fetch_optional(&mut *db)
            .await?;
    let imported = crate::imported_sources::imported_sources_in(db).await?;
    baseline(serde_json::from_str(&raw)?, descriptor, imported)
}
fn baseline_sync(db: &rusqlite::Connection, id: &str) -> anyhow::Result<Baseline> {
    let raw: String = db.query_row(
        "SELECT mapping_json FROM catabolic_source_mappings WHERE library_id=?",
        [id],
        |r| r.get(0),
    )?;
    let descriptor = db
        .query_row(
            "SELECT descriptor_json FROM catabolic_source_registry WHERE id=?",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    baseline(
        serde_json::from_str(&raw)?,
        descriptor,
        crate::imported_sources::imported_sources_sync(db)?,
    )
}
pub fn mapping_digest(mapping: &SourceMapping) -> anyhow::Result<String> {
    Ok(catabolic_core::sha256(serde_json::to_vec(
        &serde_json::to_value(mapping)?,
    )?))
}
type Row = (i64, String, String, String, i64, i64, String, String);
fn resolve(id: &str, base: &Baseline, row: Row) -> anyhow::Result<LifecycleProof> {
    let (epoch, catalog_id, incarnation, digest, source_revision, binding_revision, state, proof) =
        row;
    ensure!(
        serde_json::from_str::<Baseline>(&proof)? == *base,
        "lifecycle baseline provenance changed"
    );
    let state: State = serde_json::from_value(json!(state))?;
    ensure!(
        epoch >= 0
            && source_revision.checked_sub(epoch) == Some(base.source_revision)
            && binding_revision == base.binding_revision
            && catalog_id == base.mapping.catalog.catalog_id
            && incarnation == base.mapping.catalog.incarnation
            && digest == mapping_digest(&base.mapping)?
            && (epoch != 0 || state == State::Active)
            && (epoch == 0 || state != State::Active),
        "lifecycle chain differs from baseline"
    );
    Ok(LifecycleProof {
        source_id: id.into(),
        epoch,
        catalog_id,
        incarnation,
        mapping_digest: digest,
        source_revision,
        binding_revision,
        state,
    })
}
pub async fn current_in(db: &mut SqliteConnection, id: &str) -> anyhow::Result<LifecycleProof> {
    Ok(current_with_mapping_in(db, id).await?.0)
}
/// Current descriptive state under one actual writer, not Caller authority.
pub async fn current_mapping_in(
    db: &mut SqliteConnection,
    id: &str,
) -> anyhow::Result<(LifecycleProof, SourceMapping)> {
    require_retained_writer_in(db).await?;
    current_with_mapping_in(db, id).await
}
/// Historical receipt provenance only. Complete chain precedes exact membership.
pub async fn require_historical_mapping_in(
    db: &mut SqliteConnection,
    saved: &SourceMapping,
) -> anyhow::Result<()> {
    require_retained_writer_in(db).await?;
    let base = baseline_in(db, &saved.library_id).await?;
    let _ = current_with_mapping_in(db, &saved.library_id).await?;
    if base.mapping == *saved {
        return Ok(());
    }
    ensure!(
        renewal::enabled_in(db).await?,
        "saved mapping absent from history"
    );
    let canonical = serde_json::to_string(&serde_json::to_value(saved)?)?;
    let found:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM catabolic_source_relocation_admissions i JOIN catabolic_source_relocation_acks a USING(operation_id) JOIN catabolic_source_lifecycle l ON l.source_id=i.source_id AND l.operation_id=i.operation_id AND l.state='active' AND l.mapping_digest=i.new_mapping_digest WHERE i.source_id=? AND i.new_mapping_json=?)").bind(&saved.library_id).bind(canonical).fetch_one(&mut *db).await?;
    ensure!(found, "saved mapping absent from authenticated history");
    Ok(())
}
pub fn current_sync(db: &rusqlite::Connection, id: &str) -> anyhow::Result<LifecycleProof> {
    Ok(current_with_mapping_sync(db, id)?.0)
}
/// Current mapping is interpreted from original immutable provenance plus every
/// authenticated94 hop, never inferred from a new path or descriptor absence.
pub(crate) async fn current_with_mapping_in(
    db: &mut SqliteConnection,
    id: &str,
) -> anyhow::Result<(LifecycleProof, SourceMapping)> {
    let base = baseline_in(db, id).await?;
    if renewal::enabled_in(db).await? {
        return renewal::current_in(db, id, &base).await;
    }
    let row=sqlx::query_as("SELECT epoch,catalog_id,incarnation,mapping_digest,source_revision,binding_revision,state,proof_json FROM catabolic_source_lifecycle WHERE source_id=? ORDER BY epoch DESC LIMIT 1").bind(id).fetch_one(db).await?;
    Ok((resolve(id, &base, row)?, base.mapping))
}
pub(crate) fn current_with_mapping_sync(
    db: &rusqlite::Connection,
    id: &str,
) -> anyhow::Result<(LifecycleProof, SourceMapping)> {
    let base = baseline_sync(db, id)?;
    if renewal::enabled_sync(db)? {
        return renewal::current_sync(db, id, &base);
    }
    let row=db.query_row("SELECT epoch,catalog_id,incarnation,mapping_digest,source_revision,binding_revision,state,proof_json FROM catabolic_source_lifecycle WHERE source_id=? ORDER BY epoch DESC LIMIT 1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?)))?;
    Ok((resolve(id, &base, row)?, base.mapping))
}
pub async fn require_active_in(
    db: &mut SqliteConnection,
    id: &str,
) -> anyhow::Result<LifecycleProof> {
    let proof = current_in(db, id).await?;
    ensure!(proof.state == State::Active, "source admission withdrawn");
    Ok(proof)
}
pub fn require_active_sync(db: &rusqlite::Connection, id: &str) -> anyhow::Result<LifecycleProof> {
    let proof = current_sync(db, id)?;
    ensure!(proof.state == State::Active, "source admission withdrawn");
    Ok(proof)
}

struct Grant {
    live: Arc<AtomicBool>,
    values: Vec<Value>,
}
struct Revoke(Arc<AtomicBool>);
impl Drop for Revoke {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
unsafe extern "C" fn destroy(raw: *mut std::ffi::c_void) {
    unsafe {
        drop(Box::from_raw(raw.cast::<Grant>()));
    }
}
unsafe extern "C" fn exact(
    context: *mut ffi::sqlite3_context,
    argc: i32,
    argv: *mut *mut ffi::sqlite3_value,
) {
    let allowed = (|| {
        if argc != 11 || argv.is_null() {
            return false;
        }
        let raw = unsafe { ffi::sqlite3_user_data(context) };
        if raw.is_null() {
            return false;
        }
        let grant = unsafe { &*raw.cast::<Grant>() };
        if !grant.live.load(Ordering::Acquire) {
            return false;
        }
        for (i, wanted) in grant.values.iter().enumerate() {
            let arg = unsafe { *argv.add(i) };
            let actual = match unsafe { ffi::sqlite3_value_type(arg) } {
                ffi::SQLITE_INTEGER => json!(unsafe { ffi::sqlite3_value_int64(arg) }),
                ffi::SQLITE_TEXT => {
                    let len = unsafe { ffi::sqlite3_value_bytes(arg) };
                    let ptr = unsafe { ffi::sqlite3_value_text(arg) };
                    if len < 0 || ptr.is_null() {
                        return false;
                    }
                    let Ok(value) = std::str::from_utf8(unsafe {
                        std::slice::from_raw_parts(ptr, len as usize)
                    }) else {
                        return false;
                    };
                    json!(value)
                }
                _ => return false,
            };
            if &actual != wanted {
                return false;
            }
        }
        true
    })();
    unsafe {
        ffi::sqlite3_result_int(context, i32::from(allowed));
    }
}
async fn insert_exact(
    db: &mut SqliteConnection,
    proof: &LifecycleProof,
    operation: &str,
    base: &Baseline,
    created_at: i64,
) -> anyhow::Result<()> {
    let state = serde_json::to_value(proof.state)?
        .as_str()
        .context("state string missing")?
        .to_owned();
    let base = serde_json::to_string(base)?;
    let values = vec![
        json!(proof.source_id),
        json!(proof.epoch),
        json!(proof.catalog_id),
        json!(proof.incarnation),
        json!(proof.mapping_digest),
        json!(proof.source_revision),
        json!(proof.binding_revision),
        json!(state),
        json!(operation),
        json!(base),
        json!(created_at),
    ];
    let live = Arc::new(AtomicBool::new(true));
    let _revoke = Revoke(live.clone());
    {
        let mut handle = db.lock_handle().await?;
        let raw = Box::into_raw(Box::new(Grant { live, values })).cast();
        let code = unsafe {
            ffi::sqlite3_create_function_v2(
                handle.as_raw_handle().as_ptr(),
                c"motion_source_lifecycle_insert_allowed".as_ptr(),
                11,
                ffi::SQLITE_UTF8 | ffi::SQLITE_INNOCUOUS,
                raw,
                Some(exact),
                None,
                None,
                Some(destroy),
            )
        };
        ensure!(
            code == ffi::SQLITE_OK,
            "cannot install exact lifecycle owner"
        );
    }
    sqlx::query("INSERT INTO catabolic_source_lifecycle(source_id,epoch,catalog_id,incarnation,mapping_digest,source_revision,binding_revision,state,operation_id,proof_json,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?)")
      .bind(&proof.source_id).bind(proof.epoch).bind(&proof.catalog_id).bind(&proof.incarnation).bind(&proof.mapping_digest).bind(proof.source_revision).bind(proof.binding_revision).bind(state).bind(operation).bind(base).bind(created_at).execute(db).await?;
    Ok(())
}

/// Check retained SQL writer custody only. This does not grant source, file, or
/// catalog mutation authority, and never acquires a second connection.
pub async fn require_retained_writer_in(db: &mut SqliteConnection) -> anyhow::Result<()> {
    let mut handle = db.lock_handle().await?;
    ensure!(
        unsafe { ffi::sqlite3_get_autocommit(handle.as_raw_handle().as_ptr()) } == 0
            && unsafe { ffi::sqlite3_txn_state(handle.as_raw_handle().as_ptr(), c"main".as_ptr()) }
                == 2,
        "source lifecycle requires retained writer transaction"
    );
    Ok(())
}
fn verify_host(
    host: &catabolic_runtime::host::HostSession,
    mapping: &SourceMapping,
) -> anyhow::Result<()> {
    let actual = host.identity()?;
    let identity = playscale_core::catalog_backend::Identity {
        catalog_id: actual["catalog_id"]
            .as_str()
            .context("catalog identity missing")?
            .into(),
        incarnation: actual["catalog_incarnation"]
            .as_str()
            .context("incarnation missing")?
            .into(),
        schema_version: actual["schema_version"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .context("schema missing")?,
    };
    playscale_core::catalog_backend::admit_retained_source_record(&identity, &mapping.catalog)
        .map_err(|e| anyhow::anyhow!("source lifecycle native identity: {e:?}"))?;
    ensure!(
        actual["profile"].as_str() == Some(mapping.profile.as_str()),
        "source profile differs"
    );
    let binding = host.source_binding(&mapping.native_source)?;
    ensure!(
        binding == mapping.native_binding
            && host.source_observation_policy(&mapping.native_source)? == mapping.native_policy,
        "source binding/policy differs"
    );
    // Administrative lifecycle is independent of current root availability.
    // Byte/scan admission retains its own exact live-root check.
    Ok(())
}
/// Called only after admitted source ACK reconstruction, while its actual
/// writer and retained native host are held. Never initializes historical-only
/// roots or the independently owned private generated output source.
pub async fn initialize_with_host(
    db: &mut SqliteConnection,
    host: &catabolic_runtime::host::HostSession,
    id: &str,
    created_at: i64,
) -> anyhow::Result<LifecycleProof> {
    require_retained_writer_in(db).await?;
    ensure!(
        id != crate::generated_source::SOURCE_ID,
        "private generated source has no public lifecycle"
    );
    let base = baseline_in(db, id).await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM catabolic_source_lifecycle WHERE source_id=?)",
    )
    .bind(id)
    .fetch_one(&mut *db)
    .await?;
    if exists {
        let (proof, mapping) = current_with_mapping_in(db, id).await?;
        verify_host(host, &mapping)?;
        return Ok(proof);
    }
    verify_host(host, &base.mapping)?;
    let proof = LifecycleProof {
        source_id: id.into(),
        epoch: 0,
        catalog_id: base.mapping.catalog.catalog_id.clone(),
        incarnation: base.mapping.catalog.incarnation.clone(),
        mapping_digest: mapping_digest(&base.mapping)?,
        source_revision: base.source_revision,
        binding_revision: base.binding_revision,
        state: State::Active,
    };
    let operation = format!(
        "source-baseline:{}",
        catabolic_core::sha256(serde_json::to_vec(&serde_json::to_value(&base)?)?)
    );
    insert_exact(db, &proof, &operation, &base, created_at).await?;
    Ok(proof)
}
/// The route supplies complete current logical membership under its retained
/// authorization writer. All database/native execution blockers are observed
/// here rather than accepted from request fields. This does not delete bytes,
/// native observations, mappings, or free a physical locator.
pub async fn withdraw_in(
    db: &mut SqliteConnection,
    host: &catabolic_runtime::host::HostSession,
    id: &str,
    expected_revision: i64,
    operation: &str,
    logical_memberships: u64,
    created_at: i64,
) -> anyhow::Result<LifecycleProof> {
    withdraw_native_in(
        db,
        WithdrawalNative::Host(host, logical_memberships),
        id,
        expected_revision,
        operation,
        created_at,
    )
    .await
}
pub async fn withdraw_completed_in(
    db: &mut SqliteConnection,
    proof: &catabolic_store::migration::offline_bootstrap::CompletedSourceWithdrawal<'_>,
    id: &str,
    expected_revision: i64,
    operation: &str,
    created_at: i64,
) -> anyhow::Result<LifecycleProof> {
    withdraw_native_in(
        db,
        WithdrawalNative::Completed(proof),
        id,
        expected_revision,
        operation,
        created_at,
    )
    .await
}
// This fixed verifier accepts a genuine, borrow-scoped completed native owner,
// never a caller-provided schema number as upgrade authority. That owner proves
// the exact unrotated40→41 transition and historical bootstrap on the SAME
// retained connection. Source94 deliberately preserves the original mapping
// envelope, so its schema40 label must not be sent through ordinary admission.
// Equal41 remains strict;39 and every other historical schema stay unsupported.
pub(crate) fn verify_completed_withdrawal(
    proof: &catabolic_store::migration::offline_bootstrap::CompletedSourceWithdrawal<'_>,
    mapping: &SourceMapping,
) -> anyhow::Result<()> {
    mapping
        .catalog
        .validate()
        .map_err(|e| anyhow::anyhow!("withdrawal identity: {e:?}"))?;
    ensure!(
        matches!(mapping.catalog.schema_version, 40 | 41)
            && proof.catalog_id() == mapping.catalog.catalog_id
            && proof.incarnation() == mapping.catalog.incarnation,
        "completed withdrawal identity differs"
    );
    ensure!(
        proof.motion_source_id() == mapping.library_id
            && proof.profile() == mapping.profile
            && proof.native_source() == mapping.native_source
            && *proof.binding() == mapping.native_binding
            && *proof.policy() == mapping.native_policy,
        "completed withdrawal source differs"
    );
    // Cheap mismatches deny before the full recapture. Successful comparison
    // still requires the actual current owner, worker and complete source facts.
    proof.revalidate()?;
    Ok(())
}
enum WithdrawalNative<'a, 'b> {
    Host(&'a catabolic_runtime::host::HostSession, u64),
    Completed(&'a catabolic_store::migration::offline_bootstrap::CompletedSourceWithdrawal<'b>),
}
impl WithdrawalNative<'_, '_> {
    fn verify(&self, m: &SourceMapping) -> anyhow::Result<()> {
        match self {
            Self::Host(h, _) => verify_host(h, m),
            Self::Completed(p) => verify_completed_withdrawal(p, m),
        }
    }
    fn facts(&self, m: &SourceMapping) -> anyhow::Result<(u64, u64, bool)> {
        match self {
            Self::Completed(p) => Ok((
                p.logical_memberships(),
                p.retained_files(),
                p.native_ownership_unresolved(),
            )),
            Self::Host(h, n) => {
                let inventory = h.worker_inventory()?;
                let snapshot = h.source_occurrences(&m.native_source)?;
                ensure!(
                    snapshot["binding"] == m.native_binding
                        && snapshot["policy"] == m.native_policy,
                    "withdrawal snapshot differs"
                );
                let files = snapshot["occurrences"]
                    .as_array()
                    .context("complete occurrences missing")?
                    .len();
                Ok((*n, u64::try_from(files)?, !inventory.admits_new_work()))
            }
        }
    }
}
async fn withdraw_native_in(
    db: &mut SqliteConnection,
    native: WithdrawalNative<'_, '_>,
    id: &str,
    expected_revision: i64,
    operation: &str,
    created_at: i64,
) -> anyhow::Result<LifecycleProof> {
    require_retained_writer_in(db).await?;
    let base = baseline_in(db, id).await?;
    // Authenticate the complete merged history before interpreting either a
    // historical replay or a new withdrawal. The original baseline is retained
    // only for append provenance, never substituted for the current binding.
    let (current, mapping) = current_with_mapping_in(db, id).await?;
    // Withdrawal can safely deny an offline source. Current immutable binding
    // and native identity remain mandatory, but filesystem availability is not.
    native.verify(&mapping)?;
    let previous: Option<(String, i64)> = sqlx::query_as(
        "SELECT source_id,epoch FROM catabolic_source_lifecycle WHERE operation_id=?",
    )
    .bind(operation)
    .fetch_optional(&mut *db)
    .await?;
    if let Some((source, epoch)) = previous {
        ensure!(
            source == id,
            "withdrawal operation belongs to another source"
        );
        let row:Row=sqlx::query_as("SELECT epoch,catalog_id,incarnation,mapping_digest,source_revision,binding_revision,state,proof_json FROM catabolic_source_lifecycle WHERE source_id=? AND epoch=?").bind(id).bind(epoch).fetch_one(&mut *db).await?;
        // Every row, including this epoch, was authenticated by the complete
        // walk above under this SAME retained writer. Do not run the91-only
        // baseline resolver against a withdrawal after an authenticated94 hop.
        let saved = if renewal::enabled_in(db).await? {
            LifecycleProof {
                source_id: id.into(),
                epoch: row.0,
                catalog_id: row.1,
                incarnation: row.2,
                mapping_digest: row.3,
                source_revision: row.4,
                binding_revision: row.5,
                state: serde_json::from_value(json!(row.6))?,
            }
        } else {
            resolve(id, &base, row)?
        };
        ensure!(
            saved.state != State::Active
                && saved.source_revision.checked_sub(1) == Some(expected_revision),
            "withdrawal replay CAS differs"
        );
        return Ok(saved);
    }
    let active_demands:i64=sqlx::query_scalar("SELECT count(*) FROM catabolic_scan_demands WHERE source_id=? AND (phase IN ('queued','running','cancelling') OR status='pending')").bind(id).fetch_one(&mut *db).await?;
    let historical_demands: i64 =
        sqlx::query_scalar("SELECT count(*) FROM catabolic_scan_demands WHERE source_id=?")
            .bind(id)
            .fetch_one(&mut *db)
            .await?;
    let pending_discovery_effects:i64=sqlx::query_scalar("SELECT count(*) FROM catabolic_discovery_intents WHERE source_id=? AND command_json IS NOT NULL AND state<>'committed'").bind(id).fetch_one(&mut *db).await?;
    let (logical_memberships, native_files, native_ownership_unresolved) =
        native.facts(&mapping)?;
    let retained_files: i64 =
        sqlx::query_scalar("SELECT count(*) FROM media_files WHERE library_id=?")
            .bind(id)
            .fetch_one(&mut *db)
            .await?;
    // Conservatively block any actual reserved publication until its native
    // effect is reconciled; an incomplete ancestry capture never grants delete.
    let reserved_publications:i64=sqlx::query_scalar("SELECT count(*) FROM catabolic_generated_publications WHERE command_json IS NOT NULL AND acknowledged=0").fetch_one(&mut *db).await?;
    let active_processing: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM processing_jobs WHERE phase IN ('queued','running','cancelling')",
    )
    .fetch_one(&mut *db)
    .await?;
    let facts = WithdrawalFacts {
        logical_memberships,
        active_demands: active_demands.try_into()?,
        active_processing: active_processing.try_into()?,
        reserved_publications: reserved_publications.try_into()?,
        pending_discovery_effects: pending_discovery_effects.try_into()?,
        native_ownership_unresolved,
        private_generated_source: id == crate::generated_source::SOURCE_ID,
        retained_files: u64::try_from(retained_files)?.max(native_files),
        historical_demands: historical_demands.try_into()?,
    };
    let next = playscale_core::source_lifecycle::withdraw(&current, expected_revision, facts)
        .map_err(|e| anyhow::anyhow!("source withdrawal refused: {e:?}"))?;
    insert_exact(db, &next, operation, &base, created_at).await?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn db() -> SqliteConnection {
        use sqlx::Connection;
        let mut db = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        crate::projection::configure_connection(&mut db)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/0091_catabolic_source_lifecycle.sql"
        ))
        .execute(&mut db)
        .await
        .unwrap();
        db
    }
    fn fixture() -> (LifecycleProof, Baseline) {
        let mapping = SourceMapping {
            library_id: "source".into(),
            profile: "default".into(),
            native_source: "native-source".into(),
            catalog: playscale_core::catalog_backend::Identity {
                catalog_id: "catalog".into(),
                incarnation: "incarnation".into(),
                schema_version: 40,
            },
            motion_root: "/test".into(),
            motion_root_identity: "1:2".into(),
            motion_binding_revision: 7,
            native_binding: json!({}),
            exclusions: vec![],
            native_policy: json!({}),
        };
        let base = Baseline {
            descriptor: NativeSourceDescriptor {
                id: "source".into(),
                logical_library_id: "motion:unassigned".into(),
                name: "test".into(),
                root: "/test".into(),
                root_identity: "1:2".into(),
                exclusions: vec![],
            },
            mapping: mapping.clone(),
            source_revision: 19,
            binding_revision: 7,
            imported: None,
        };
        let proof = LifecycleProof {
            source_id: "source".into(),
            epoch: 0,
            catalog_id: "catalog".into(),
            incarnation: "incarnation".into(),
            mapping_digest: mapping_digest(&mapping).unwrap(),
            source_revision: 19,
            binding_revision: 7,
            state: State::Active,
        };
        (proof, base)
    }
    #[tokio::test]
    async fn lifecycle_insert_is_exact_and_revoked_after_owner_returns() {
        let mut db = db().await;
        let (proof, base) = fixture();
        let denied = "INSERT INTO catabolic_source_lifecycle VALUES('source',0,'catalog','incarnation',?,19,7,'active','untrusted','{}',0)";
        assert!(
            sqlx::query(denied)
                .bind(&proof.mapping_digest)
                .execute(&mut db)
                .await
                .is_err()
        );
        insert_exact(&mut db, &proof, "baseline", &base, 1)
            .await
            .unwrap();
        let next = playscale_core::source_lifecycle::withdraw(
            &proof,
            19,
            WithdrawalFacts {
                retained_files: 1,
                ..Default::default()
            },
        )
        .unwrap();
        // The connection retains callback storage, never live permission.
        assert!(sqlx::query("INSERT INTO catabolic_source_lifecycle SELECT source_id,1,catalog_id,incarnation,mapping_digest,20,binding_revision,'withdrawn','forged',proof_json,2 FROM catabolic_source_lifecycle").execute(&mut db).await.is_err());
        insert_exact(&mut db, &next, "withdraw", &base, 2)
            .await
            .unwrap();
        assert!(
            sqlx::query("UPDATE catabolic_source_lifecycle SET state='active'")
                .execute(&mut db)
                .await
                .is_err()
        );
        assert!(
            sqlx::query("DELETE FROM catabolic_source_lifecycle")
                .execute(&mut db)
                .await
                .is_err()
        );
        let mut skipped = next.clone();
        skipped.epoch = 3;
        skipped.source_revision = 22;
        assert!(
            insert_exact(&mut db, &skipped, "skip", &base, 3)
                .await
                .is_err()
        );
        let enabled:i64=sqlx::query_scalar("SELECT motion_source_lifecycle_insert_allowed('source',3,'catalog','incarnation',?,22,7,'withdrawn','skip',?,3)").bind(&proof.mapping_digest).bind(serde_json::to_string(&base).unwrap()).fetch_one(&mut db).await.unwrap();
        assert_eq!(enabled, 0);
    }
    #[tokio::test]
    async fn dropping_pending_insert_revokes_exact_connection_grant() {
        use sqlx::Connection;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lifecycle.sqlite3");
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .busy_timeout(std::time::Duration::from_secs(5));
        let mut owner = SqliteConnection::connect_with(&options).await.unwrap();
        crate::projection::configure_connection(&mut owner)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/0091_catabolic_source_lifecycle.sql"
        ))
        .execute(&mut owner)
        .await
        .unwrap();
        let (proof, base) = fixture();
        insert_exact(&mut owner, &proof, "baseline", &base, 1)
            .await
            .unwrap();
        let next = playscale_core::source_lifecycle::withdraw(
            &proof,
            19,
            WithdrawalFacts {
                retained_files: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let mut other = SqliteConnection::connect_with(&options).await.unwrap();
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut other)
            .await
            .unwrap();
        let attempt = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            insert_exact(&mut owner, &next, "cancelled", &base, 2),
        )
        .await;
        assert!(
            attempt.is_err(),
            "actual competing writer must hold the insert pending"
        );
        sqlx::query("ROLLBACK").execute(&mut other).await.unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_source_lifecycle")
            .fetch_one(&mut owner)
            .await
            .unwrap();
        assert_eq!(
            rows, 1,
            "cancelled exact-row authority cannot execute later on the pooled connection"
        );
    }
    #[test]
    fn administrative_state_does_not_follow_native_generation_or_cache_revision() {
        let (proof, base) = fixture();
        let encoded = serde_json::to_string(&base).unwrap();
        let row = (
            1,
            proof.catalog_id.clone(),
            proof.incarnation.clone(),
            proof.mapping_digest.clone(),
            20,
            7,
            "withdrawn".into(),
            encoded.clone(),
        );
        let current = resolve("source", &base, row).unwrap();
        assert_ne!(current, proof);
        assert_eq!(current.binding_revision, proof.binding_revision);
        assert!(
            resolve(
                "source",
                &base,
                (
                    1,
                    proof.catalog_id,
                    proof.incarnation,
                    proof.mapping_digest,
                    19,
                    7,
                    "active".into(),
                    encoded
                )
            )
            .is_err()
        );
    }
}

/// ACK has been inserted under its exact grant; its deferred endpoint FK remains
/// outstanding until this exact lifecycle successor is appended.
pub(crate) async fn append_relocation_in(
    db: &mut SqliteConnection,
    expected: &LifecycleProof,
    successor: &LifecycleProof,
    old: &SourceMapping,
    operation: &str,
) -> anyhow::Result<()> {
    require_retained_writer_in(db).await?;
    let base = baseline_in(db, &expected.source_id).await?;
    let(epoch,digest):(i64,String)=sqlx::query_as("SELECT epoch,mapping_digest FROM catabolic_source_lifecycle WHERE source_id=? ORDER BY epoch DESC LIMIT 1").bind(&expected.source_id).fetch_one(&mut *db).await?;
    ensure!(
        epoch == expected.epoch && digest == expected.mapping_digest,
        "relocation lifecycle predecessor changed"
    );
    let _ = crate::source_relocations::history::mapping_in(db, operation, expected, old, successor)
        .await?;
    insert_exact(db, successor, operation, &base, crate::now()).await
}
