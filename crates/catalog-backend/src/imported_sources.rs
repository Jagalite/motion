//! Immutable imported source descriptors. Preparation is journal-only; actual
//! activation receipts and source bindings are checked by the retained host.
//! Descriptors require migration86 and exact activated import provenance.
use crate::{motion_export::FrozenMotionSource, source_registry::NativeSourceDescriptor};
use anyhow::{Context, ensure};
use catabolic_runtime::host::HostSession;
use catabolic_store::{
    catalog_commands::CommandOutcome,
    catalog_import::{ImportReceipt, Reconciliation},
};
use playscale_core::{
    catalog_backend::Identity,
    catalog_cutover::{Checkpoint, Phase},
    catalog_source::Mode,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};
use std::collections::BTreeMap;

#[derive(Clone, Serialize, Deserialize, PartialEq)]
struct Control {
    single: Option<(String, String, String, String)>,
    set: Option<(String, String, String, i64, String)>,
    parts: Vec<(String, i64, String, String, String)>,
}
async fn control(db: &mut SqliteConnection) -> anyhow::Result<Control> {
    Ok(Control {
        single: sqlx::query_as("SELECT import_id,checkpoint_json,activation_envelope,phase FROM catabolic_cutover WHERE singleton=1").fetch_optional(&mut *db).await?,
        set: sqlx::query_as("SELECT root_import_id,manifest_digest,manifest_json,freeze_revision,phase FROM catabolic_cutover_sets WHERE singleton=1").fetch_optional(&mut *db).await?,
        parts: sqlx::query_as("SELECT import_id,ordinal,checkpoint_json,activation_envelope,phase FROM catabolic_cutover_parts ORDER BY ordinal").fetch_all(&mut *db).await?,
    })
}
/// No public fields or deserializer: callers cannot manufacture receipt admission.
pub struct PreparedImportedSources {
    control: Control,
    identity: Identity,
    root: String,
    freeze_revision: u64,
    checkpoints: Vec<(Checkpoint, String)>,
    sources: Vec<FrozenMotionSource>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ImportedSourceDescriptor {
    pub descriptor: NativeSourceDescriptor,
    pub proof: FrozenMotionSource,
    pub activation_digest: String,
}
/// Does not inspect or open the native database and creates no alternate gate.
pub async fn prepare_imported_sources(
    journal: &SqlitePool,
) -> anyhow::Result<PreparedImportedSources> {
    let mut read = journal.begin().await?;
    crate::source_freeze::verify(&mut read).await?;
    let captured = control(&mut read).await?;
    let identity: String =
        sqlx::query_scalar("SELECT identity_json FROM catabolic_binding WHERE id=1")
            .fetch_one(&mut *read)
            .await?;
    let identity: Identity = serde_json::from_str(&identity)?;
    read.commit().await?;
    let mut checkpoints = Vec::new();
    let (root, freeze_revision) = if captured.set.is_some() {
        let (manifest, hash, phase) = crate::partition_cutover::saved(journal).await?;
        let (root, captured_hash, raw, epoch, captured_phase) = captured.set.as_ref().unwrap();
        ensure!(
            root == &manifest.plan.root_import_id
                && captured_hash == &hash
                && serde_json::from_str::<Value>(raw)? == serde_json::to_value(&manifest)?
                && u64::try_from(*epoch)? == manifest.freeze_revision
                && captured_phase == &phase,
            "partition activation changed during source preparation"
        );
        ensure!(
            phase == "activated" && captured.parts.len() == manifest.plan.partitions.len(),
            "complete activated import set required"
        );
        identity
            .admit(&manifest.identity)
            .map_err(|e| anyhow::anyhow!("import identity: {e:?}"))?;
        for ((id, ordinal, raw, envelope, phase), part) in
            captured.parts.iter().zip(&manifest.plan.partitions)
        {
            let cp: Checkpoint = serde_json::from_str(raw)?;
            ensure!(
                id == &part.import_id
                    && *ordinal == part.index as i64
                    && phase == "activated"
                    && cp.valid()
                    && cp.phase == Phase::Activated
                    && cp.import_id == *id
                    && cp.input_digest == part.snapshot_digest
                    && cp.freeze_revision == manifest.freeze_revision
                    && cp.catalog_id == identity.catalog_id
                    && cp.incarnation == identity.incarnation,
                "activated source partition checkpoint mismatch"
            );
            checkpoints.push((cp, envelope.clone()));
        }
        (manifest.plan.root_import_id, manifest.freeze_revision)
    } else {
        ensure!(
            captured.parts.is_empty(),
            "unexpected partial activation set"
        );
        let (cp, envelope) = crate::cutover::recorded(journal).await?;
        let (id, raw, captured_envelope, captured_phase) = captured
            .single
            .as_ref()
            .context("source activation checkpoint missing")?;
        ensure!(
            id == &cp.import_id
                && serde_json::from_str::<Value>(raw)? == serde_json::to_value(&cp)?
                && captured_envelope == &envelope
                && captured_phase == "activated",
            "activated source checkpoint changed during preparation"
        );
        ensure!(
            cp.phase == Phase::Activated
                && cp.catalog_id == identity.catalog_id
                && cp.incarnation == identity.incarnation,
            "activated source checkpoint required"
        );
        let root = cp.import_id.clone();
        let epoch = cp.freeze_revision;
        checkpoints.push((cp, envelope));
        (root, epoch)
    };
    let mut sources: BTreeMap<String, FrozenMotionSource> = BTreeMap::new();
    for (cp, _) in &checkpoints {
        for source in crate::motion_export::frozen_motion_sources_from_journal(
            journal,
            &identity,
            &cp.import_id,
        )
        .await?
        {
            if let Some(old) = sources.get(&source.mapping.library_id) {
                let mut comparable = source.clone();
                comparable.import_id = old.import_id.clone();
                ensure!(&comparable == old, "partitioned source proofs disagree");
            } else {
                sources.insert(source.mapping.library_id.clone(), source);
            }
        }
    }
    Ok(PreparedImportedSources {
        control: captured,
        identity,
        root,
        freeze_revision,
        checkpoints,
        sources: sources.into_values().collect(),
    })
}

/// Invoke while retaining the supplied native host mutex. This function commits
/// its owned Motion writer itself, so acknowledgment cannot escape that custody.
pub async fn acknowledge(
    mut tx: Transaction<'static, Sqlite>,
    host: &mut HostSession,
    prepared: PreparedImportedSources,
) -> anyhow::Result<()> {
    ensure!(
        control(&mut tx).await? == prepared.control,
        "activation set changed during source preparation"
    );
    let freeze = crate::source_freeze::read(&mut tx).await?;
    ensure!(
        matches!(freeze.mode, Mode::Sealed { .. })
            && freeze.active_freeze() == Some((prepared.root.as_str(), prepared.freeze_revision)),
        "imported source seal mismatch"
    );
    crate::source_freeze::verify(&mut tx).await?;
    let current = host.identity()?;
    ensure!(
        current["catalog_id"] == prepared.identity.catalog_id
            && current["catalog_incarnation"] == prepared.identity.incarnation,
        "imported source retained host mismatch"
    );
    // Reconcile the COMPLETE set before inserting even one descriptor.
    for (checkpoint, raw) in &prepared.checkpoints {
        let command = crate::decode_envelope(raw)?;
        ensure!(
            command.key().operation_id()
                == catabolic_store::catalog_import::operation_id(
                    &checkpoint.import_id,
                    catabolic_store::catalog_import::ImportStep::Activate
                )?,
            "source activation operation mismatch"
        );
        let result = match host.reconcile_command_receipt(&command)? {
            Some(
                CommandOutcome::Committed { result, .. } | CommandOutcome::Replay { result, .. },
            ) => result,
            _ => anyhow::bail!("actual full import activation receipt unavailable"),
        };
        let receipt: ImportReceipt = serde_json::from_value(result)?;
        ensure!(
            catabolic_store::catalog_import::reconcile_activation(
                Some(&receipt),
                &crate::cutover::native_checkpoint(checkpoint),
                &prepared.identity.catalog_id,
                &prepared.identity.incarnation
            ) == Reconciliation::AdmitCatabolicOnly,
            "source activation receipt mismatch"
        );
    }
    let digest = catabolic_core::sha256(serde_json::to_vec(&prepared.control)?);
    let mut records = Vec::new();
    for proof in prepared.sources {
        let mapping = &proof.mapping;
        ensure!(
            current["profile"] == mapping.profile,
            "imported source profile mismatch"
        );
        // The disposable libraries row may be absent during reconstruction.
        // Authority is the immutable frozen proof, complete activation receipts
        // above, exact control mapping, and the retained native binding below.
        let saved_mapping: String = sqlx::query_scalar(
            "SELECT mapping_json FROM catabolic_source_mappings WHERE library_id=?",
        )
        .bind(&mapping.library_id)
        .fetch_one(&mut *tx)
        .await?;
        ensure!(
            serde_json::from_str::<crate::sources::SourceMapping>(&saved_mapping)? == *mapping,
            "imported immutable source mapping changed"
        );
        let binding = host.source_binding(&mapping.native_source)?;
        ensure!(
            binding == mapping.native_binding
                && host.source_observation_policy(&mapping.native_source)? == mapping.native_policy,
            "imported native source binding/policy changed"
        );
        crate::sources::check_root(&binding)?;
        let row: Option<String> = sqlx::query_scalar("SELECT json_object('id',id,'name',name,'root',root,'root_identity',root_identity,'managed',managed,'enabled',enabled,'revision',revision,'binding_revision',binding_revision,'exclusions_json',exclusions_json,'scan_barrier',scan_barrier) FROM libraries WHERE id=?")
            .bind(&mapping.library_id).fetch_optional(&mut *tx).await?;
        if let Some(row) = row {
            ensure!(
                serde_json::from_str::<Value>(&row)? == proof.source_row,
                "frozen source projection changed before acknowledgment"
            );
        }
        let source = &proof.source_row;
        ensure!(
            source["managed"] == 0
                && source["enabled"] == 1
                && source["revision"].as_i64().is_some_and(|r| r >= 0)
                && source["scan_barrier"].as_i64().is_some_and(|r| r >= 0),
            "unsupported imported source state"
        );
        let descriptor = NativeSourceDescriptor {
            id: mapping.library_id.clone(),
            logical_library_id: "motion:unassigned".into(),
            name: source["name"]
                .as_str()
                .context("frozen source name missing")?
                .into(),
            root: mapping.motion_root.clone(),
            root_identity: mapping.motion_root_identity.clone(),
            exclusions: mapping.exclusions.clone(),
        };
        let collision: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM catabolic_source_registry WHERE id=?)")
                .bind(&descriptor.id)
                .fetch_one(&mut *tx)
                .await?;
        ensure!(
            !collision,
            "imported source collides with native-created descriptor"
        );
        let record = ImportedSourceDescriptor {
            descriptor,
            proof,
            activation_digest: digest.clone(),
        };
        validate_descriptor(&record)?;
        records.push(record);
    }
    for record in records {
        sqlx::query("INSERT INTO catabolic_imported_source_descriptors VALUES(?,?,?,?,?) ON CONFLICT(source_id) DO NOTHING")
            .bind(&record.descriptor.id).bind(serde_json::to_string(&record.descriptor)?).bind(serde_json::to_string(&record.proof)?).bind(&record.activation_digest).bind(crate::now()).execute(&mut *tx).await?;
        let (descriptor,proof,digest):(String,String,String) = sqlx::query_as("SELECT descriptor_json,proof_json,activation_digest FROM catabolic_imported_source_descriptors WHERE source_id=?").bind(&record.descriptor.id).fetch_one(&mut *tx).await?;
        ensure!(
            serde_json::from_str::<NativeSourceDescriptor>(&descriptor)? == record.descriptor
                && serde_json::from_str::<FrozenMotionSource>(&proof)? == record.proof
                && digest == record.activation_digest,
            "imported source acknowledgment conflicts with retained proof"
        );
    }
    tx.commit().await?;
    Ok(())
}
/// A present disposable row can never override or weaken retained import facts.
pub fn validate_cached_row(record: &ImportedSourceDescriptor, row: &Value) -> anyhow::Result<()> {
    ensure!(
        row == &record.proof.source_row,
        "present imported source cache differs from immutable proof"
    );
    Ok(())
}
/// Pure descriptor/proof consistency shared by both SQLite adapters.
pub fn validate_descriptor(record: &ImportedSourceDescriptor) -> anyhow::Result<()> {
    let descriptor = &record.descriptor;
    let proof = &record.proof;
    let source = &proof.source_row;
    ensure!(
        source.as_object().is_some_and(|row| row.len() == 10)
            && source["id"] == descriptor.id
            && source["name"] == descriptor.name
            && source["root"] == descriptor.root
            && source["root_identity"] == descriptor.root_identity
            && source["managed"] == 0
            && source["enabled"] == 1
            && source["revision"].as_i64().is_some_and(|v| v >= 0)
            && source["binding_revision"].as_i64() == Some(proof.mapping.motion_binding_revision)
            && source["scan_barrier"].as_i64().is_some_and(|v| v >= 0)
            && proof.mapping.library_id == descriptor.id
            && descriptor.root == proof.mapping.motion_root
            && descriptor.root_identity == proof.mapping.motion_root_identity
            && descriptor.exclusions == proof.mapping.exclusions
            && descriptor.logical_library_id == "motion:unassigned",
        "imported source proof identity mismatch"
    );
    let exclusions: Vec<String> = serde_json::from_str(
        source["exclusions_json"]
            .as_str()
            .context("imported exclusions missing")?,
    )?;
    ensure!(
        playscale_core::sources::normalize_exclusions(&exclusions)
            .map_err(|e| anyhow::anyhow!("imported source exclusions: {e:?}"))?
            == descriptor.exclusions,
        "imported source exclusions mismatch"
    );
    Ok(())
}
fn resolve_rows(
    control: Control,
    rows: Vec<(String, String, String, String)>,
) -> anyhow::Result<Vec<ImportedSourceDescriptor>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let mut active = std::collections::BTreeSet::new();
    if let Some((root, _, manifest, epoch, phase)) = &control.set {
        let manifest: crate::partition_cutover::SetManifest = serde_json::from_str(manifest)?;
        ensure!(
            phase == "activated"
                && root == &manifest.plan.root_import_id
                && u64::try_from(*epoch)? == manifest.freeze_revision
                && control.parts.len() == manifest.plan.partitions.len()
                && !control.parts.is_empty(),
            "imported descriptor activation set incomplete"
        );
        for ((id, ordinal, raw, _, phase), part) in
            control.parts.iter().zip(&manifest.plan.partitions)
        {
            let cp: Checkpoint = serde_json::from_str(raw)?;
            ensure!(
                phase == "activated"
                    && id == &part.import_id
                    && *ordinal == part.index as i64
                    && cp.valid()
                    && cp.phase == Phase::Activated
                    && cp.import_id == *id
                    && cp.input_digest == part.snapshot_digest
                    && cp.freeze_revision == manifest.freeze_revision
                    && cp.catalog_id == manifest.identity.catalog_id
                    && cp.incarnation == manifest.identity.incarnation,
                "imported descriptor partition checkpoint mismatch"
            );
            active.insert(id.as_str());
        }
    } else {
        let (id, raw, _, phase) = control
            .single
            .as_ref()
            .context("imported descriptor activation missing")?;
        let cp: Checkpoint = serde_json::from_str(raw)?;
        ensure!(
            control.parts.is_empty()
                && phase == "activated"
                && cp.valid()
                && cp.phase == Phase::Activated
                && cp.import_id == *id,
            "imported descriptor activation incomplete"
        );
        active.insert(id.as_str());
    }
    let digest = catabolic_core::sha256(serde_json::to_vec(&control)?);
    rows.into_iter()
        .map(|(id, descriptor, proof, activation_digest)| {
            let record = ImportedSourceDescriptor {
                descriptor: serde_json::from_str(&descriptor)?,
                proof: serde_json::from_str(&proof)?,
                activation_digest,
            };
            validate_descriptor(&record)?;
            ensure!(
                record.descriptor.id == id
                    && record.activation_digest == digest
                    && active.contains(record.proof.import_id.as_str()),
                "imported descriptor acknowledgment is not for current complete activation"
            );
            Ok(record)
        })
        .collect()
}
/// Read-only descriptor resolution; registry membership is never library authority.
pub async fn imported_sources_in(
    db: &mut SqliteConnection,
) -> anyhow::Result<Vec<ImportedSourceDescriptor>> {
    let captured = control(db).await?;
    let rows = sqlx::query_as("SELECT source_id,descriptor_json,proof_json,activation_digest FROM catabolic_imported_source_descriptors ORDER BY source_id").fetch_all(&mut *db).await?;
    let records = resolve_rows(captured, rows)?;
    if records.is_empty() {
        return Ok(records);
    }
    let identity: String =
        sqlx::query_scalar("SELECT identity_json FROM catabolic_binding WHERE id=1")
            .fetch_one(&mut *db)
            .await?;
    let identity: Identity = serde_json::from_str(&identity)?;
    let mut frozen = BTreeMap::new();
    for record in &records {
        if !frozen.contains_key(&record.proof.import_id) {
            frozen.insert(
                record.proof.import_id.clone(),
                crate::motion_export::frozen_motion_sources_in(
                    db,
                    &identity,
                    &record.proof.import_id,
                )
                .await?,
            );
        }
        ensure!(
            frozen[&record.proof.import_id]
                .iter()
                .any(|proof| proof == &record.proof),
            "imported descriptor ACK differs from original frozen provenance"
        );
    }
    Ok(records)
}
/// Same proof checks for the isolated, transaction-owned projection connection.
pub fn imported_sources_sync(
    db: &rusqlite::Connection,
) -> anyhow::Result<Vec<ImportedSourceDescriptor>> {
    use rusqlite::OptionalExtension;
    let captured = Control {
        single: db.query_row("SELECT import_id,checkpoint_json,activation_envelope,phase FROM catabolic_cutover WHERE singleton=1", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?,
        set: db.query_row("SELECT root_import_id,manifest_digest,manifest_json,freeze_revision,phase FROM catabolic_cutover_sets WHERE singleton=1", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?,
        parts: db.prepare("SELECT import_id,ordinal,checkpoint_json,activation_envelope,phase FROM catabolic_cutover_parts ORDER BY ordinal")?.query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?.collect::<Result<Vec<_>,_>>()?,
    };
    let rows = db.prepare("SELECT source_id,descriptor_json,proof_json,activation_digest FROM catabolic_imported_source_descriptors ORDER BY source_id")?.query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?.collect::<Result<Vec<_>,_>>()?;
    let records = resolve_rows(captured, rows)?;
    if records.is_empty() {
        return Ok(records);
    }
    let identity: String = db.query_row(
        "SELECT identity_json FROM catabolic_binding WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    let identity: Identity = serde_json::from_str(&identity)?;
    let mut frozen = BTreeMap::new();
    for record in &records {
        if !frozen.contains_key(&record.proof.import_id) {
            frozen.insert(
                record.proof.import_id.clone(),
                crate::motion_export::frozen_motion_sources_from_connection(
                    db,
                    &identity,
                    &record.proof.import_id,
                )?,
            );
        }
        ensure!(
            frozen[&record.proof.import_id]
                .iter()
                .any(|proof| proof == &record.proof),
            "imported descriptor ACK differs from original frozen provenance"
        );
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn empty_import_requires_real_activation_and_never_invents_a_source() {
        activated_source_fixture(false).await;
    }
    #[tokio::test]
    async fn imported_descriptor_requires_actual_activation_and_exact_cache_proof() {
        activated_source_fixture(true).await;
    }
    async fn activated_source_fixture(with_source: bool) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("native.sqlite3");
        crate::initialize(path.clone()).await.unwrap();
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .after_connect(|connection, _| {
                Box::pin(crate::projection::configure_connection(connection))
            })
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(root.path().join("motion.sqlite3"))
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("../../migrations").run(&db).await.unwrap();
        let backend = crate::NativeCatalog::bind(
            db.clone(),
            path.clone(),
            crate::inspect(path.clone()).await.unwrap().identity,
        )
        .await
        .unwrap();
        if with_source {
            use std::os::unix::fs::MetadataExt;
            let media = root.path().join("media");
            std::fs::create_dir(&media).unwrap();
            let stat = std::fs::metadata(&media).unwrap();
            sqlx::query("INSERT INTO libraries(id,name,root,root_identity,managed,enabled,revision,binding_revision,exclusions_json,scan_barrier) VALUES('physical','Retained source',?,?,0,1,19,7,'[]',23)")
                .bind(media.to_str().unwrap()).bind(format!("{}:{}",stat.dev(),stat.ino())).execute(&db).await.unwrap();
            backend.register_motion_source("physical").await.unwrap();
        }
        backend
            .freeze_motion_source("empty-source-proof", 0)
            .await
            .unwrap();
        backend
            .capture_motion_import("empty-source-proof")
            .await
            .unwrap();
        backend.stage_import("empty-source-proof").await.unwrap();
        backend
            .prepare_motion_cutover("empty-source-proof")
            .await
            .unwrap();
        assert!(
            prepare_imported_sources(&db)
                .await
                .err()
                .unwrap()
                .to_string()
                .contains("activated")
        );
        backend
            .activate_motion_cutover(std::time::Duration::from_secs(30))
            .await
            .unwrap();
        let prepared = prepare_imported_sources(&db).await.unwrap();
        assert_eq!(prepared.sources.len(), usize::from(with_source));
        let mut admission = crate::cutover::open_server(db.clone(), path, "default".into())
            .await
            .unwrap();
        let tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        acknowledge(tx, admission.admission.admitted_host().unwrap(), prepared)
            .await
            .unwrap();
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_source_registry")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(count, 0);
        let mut read = db.begin().await.unwrap();
        let records = imported_sources_in(&mut read).await.unwrap();
        assert_eq!(records.len(), usize::from(with_source));
        if let Some(record) = records.first() {
            assert_eq!(record.proof.source_row["revision"], 19);
            assert_eq!(record.proof.source_row["binding_revision"], 7);
            assert_eq!(record.proof.source_row["scan_barrier"], 23);
            validate_cached_row(record, &record.proof.source_row).unwrap();
            for (field, changed_value) in [
                ("revision", serde_json::json!(20)),
                ("scan_barrier", serde_json::json!(24)),
                ("binding_revision", serde_json::json!(8)),
                ("enabled", serde_json::json!(0)),
                ("exclusions_json", serde_json::json!("[\"new-exclusion\"]")),
            ] {
                let mut changed = record.proof.source_row.clone();
                changed[field] = changed_value;
                assert!(
                    validate_cached_row(record, &changed).is_err(),
                    "changed {field} must not be adopted by matching root"
                );
            }
            let mut changed_descriptor = record.clone();
            changed_descriptor.descriptor.root.push_str("-other");
            assert!(validate_descriptor(&changed_descriptor).is_err());
            let captured = control(&mut read).await.unwrap();
            let row = (
                record.descriptor.id.clone(),
                serde_json::to_string(&record.descriptor).unwrap(),
                serde_json::to_string(&record.proof).unwrap(),
                record.activation_digest.clone(),
            );
            assert_eq!(
                resolve_rows(captured.clone(), vec![row.clone()]).unwrap(),
                records
            );
            // Copy only actual activated control/ACK values to an isolated read
            // model. There is deliberately no disposable libraries table here.
            let isolated = rusqlite::Connection::open_in_memory().unwrap();
            isolated
                .execute(
                    "ATTACH DATABASE ? AS original",
                    [root.path().join("motion.sqlite3").to_str().unwrap()],
                )
                .unwrap();
            let names = isolated.prepare("SELECT name FROM original.sqlite_master WHERE type='table' AND name LIKE 'catabolic_%'").unwrap().query_map([], |r| r.get::<_,String>(0)).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
            isolated.execute_batch("BEGIN").unwrap();
            for name in names {
                assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
                isolated
                    .execute_batch(&format!(
                        "CREATE TABLE {name} AS SELECT * FROM original.{name};"
                    ))
                    .unwrap();
            }
            isolated
                .execute_batch("COMMIT; DETACH DATABASE original;")
                .unwrap();
            assert_eq!(imported_sources_sync(&isolated).unwrap(), records);
            // Corrupt only the isolated disposable test copy, never the actual
            // activated journal. An ACK digest alone cannot attest source facts.
            let mut forged = record.clone();
            forged.descriptor.name = "invented name".into();
            forged.proof.source_row["name"] = serde_json::json!("invented name");
            validate_descriptor(&forged).unwrap();
            isolated.execute("UPDATE catabolic_imported_source_descriptors SET descriptor_json=?,proof_json=?",rusqlite::params![serde_json::to_string(&forged.descriptor).unwrap(),serde_json::to_string(&forged.proof).unwrap()]).unwrap();
            assert!(imported_sources_sync(&isolated).is_err());
            isolated.execute("UPDATE catabolic_imported_source_descriptors SET descriptor_json=?,proof_json=?",rusqlite::params![row.1,row.2]).unwrap();
            isolated
                .execute("UPDATE catabolic_cutover SET phase='validated'", [])
                .unwrap();
            assert!(imported_sources_sync(&isolated).is_err());
            let mut stale = row;
            stale.3 = "unrelated activation".into();
            assert!(resolve_rows(captured, vec![stale]).is_err());
        }
        read.rollback().await.unwrap();
        admission
            .admission
            .admitted_host()
            .unwrap()
            .close()
            .unwrap();
    }
}

pub(crate) mod archive;
pub mod historical;
