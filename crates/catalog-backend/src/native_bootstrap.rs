//! Distinct offline40→41 intent/bootstrap/ACK owner. Original89 binding evidence
//! is never rewritten. Normal startup and generic native41 dispatch stay closed.
mod accounting;
mod authorization_reader;
pub(crate) mod prepare_operation;
pub use authorization_reader::TargetAuthorizationReader;
mod imported_archive;
mod journal;
pub(crate) use imported_archive::{
    ImportedLibraryBridge, RetainedImportedArchive, TransferredImportedArchive,
};
mod source_session;
use anyhow::{Context, ensure};
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey};
use catabolic_store::{
    catalog_commands::ReceiptPolicy,
    migration::{
        NativeUpgradeQuiescence,
        offline_bootstrap::{HostSourceEvidence, OfflineBootstrap, PreparedOfflineBootstrap},
    },
};
use playscale_core::{
    catalog_backend::Identity,
    catalog_identity_lineage::{self, Resolution, Transition, TransitionKind, TransitionStatus},
};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub(crate) use source_session::{RetainedProofAdmission, SourcePairCommitments, SourcePairProof};
use std::path::{Path, PathBuf};
const MAX_RECORD: usize = 4 * 1024 * 1024;
struct EncodedBudget {
    bytes: usize,
}
impl std::io::Write for EncodedBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .filter(|n| *n <= MAX_RECORD - 65536)
            .ok_or_else(|| std::io::Error::other("native41 approved plan exceeds journal bound"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn admit_plan(plan: &Value) -> anyhow::Result<()> {
    serde_json::to_writer(&mut EncodedBudget { bytes: 0 }, plan)?;
    Ok(())
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    contract_version: u32,
    transition_id: String,
    predecessor: Option<String>,
    from: Identity,
    to: Identity,
    frozen_binding_sha256: String,
    native_path: PathBuf,
    native_file_identity: (u64, u64),
    source_generation: i64,
    plan: Value,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Acknowledgment {
    contract_version: u32,
    transition_id: String,
    intent_sha256: String,
    native_transition_sha256: String,
    command_key: CommandKey,
    payload_digest: String,
    inventory_digest: String,
    committed_generation: i64,
    state_digest: String,
    source_count: i64,
}
fn hash(text: &str) -> String {
    catabolic_core::sha256(text.as_bytes())
}
fn identifier(text: &str) -> bool {
    !text.is_empty() && text.len() <= 256 && !text.chars().any(char::is_control)
}
fn decode<T: serde::de::DeserializeOwned + Serialize>(text: &str) -> anyhow::Result<T> {
    ensure!(text.len() <= MAX_RECORD, "native41 record exceeds bound");
    let value: T = serde_json::from_str(text)?;
    ensure!(
        serde_json::to_string(&value)? == text,
        "native41 journal is not canonical"
    );
    Ok(value)
}
fn frozen(db: &Connection) -> anyhow::Result<(Identity, String)> {
    let bytes: i64 = db.query_row(
        "SELECT length(CAST(identity_json AS BLOB)) FROM catabolic_binding WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        (1..=65536).contains(&bytes),
        "native41 frozen binding exceeds bound"
    );
    let raw: String = db.query_row(
        "SELECT identity_json FROM catabolic_binding WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    Ok((serde_json::from_str(&raw)?, hash(&raw)))
}
fn validate_intent(intent: &Intent) -> anyhow::Result<()> {
    ensure!(
        intent.contract_version == 1
            && identifier(&intent.transition_id)
            && intent.predecessor.as_deref().is_none_or(identifier)
            && intent.from.schema_version == 40
            && intent.to.schema_version == 41
            && intent.from.catalog_id == intent.to.catalog_id
            && intent.from.incarnation == intent.to.incarnation
            && intent.source_generation >= 0
            && intent.plan["from_schema"] == 40
            && intent.plan["to_schema"] == 41,
        "invalid exact native41 transition intent"
    );
    intent
        .from
        .validate()
        .map_err(|e| anyhow::anyhow!("invalid original identity: {e:?}"))?;
    admit_plan(&intent.plan)?;
    let mut unsigned = intent.plan.clone();
    let fingerprint = unsigned
        .as_object_mut()
        .context("native41 plan object missing")?
        .remove("plan_fingerprint");
    ensure!(
        fingerprint
            == Some(Value::String(catabolic_core::sha256(
                catabolic_core::encode(&unsigned)
            ))),
        "native41 plan fingerprint changed"
    );
    Ok(())
}
/// Begins only with the exact ordered control schema already installed. It does
/// not install reserved migrations or silently advance a retained93 database.
fn open(path: &Path) -> anyhow::Result<Connection> {
    open_reserved(path, 0)
}
fn open_reserved(path: &Path, external_retained_bytes: u64) -> anyhow::Result<Connection> {
    accounting::Allocation::new(external_retained_bytes)?;
    ensure!(
        std::fs::symlink_metadata(path)?.is_file(),
        "native41 control requires existing regular Motion database"
    );
    let db = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    admit_owned_connection_reserved(db, external_retained_bytes)
}
fn admit_owned_connection(db: Connection) -> anyhow::Result<Connection> {
    admit_owned_connection_reserved(db, 0)
}
fn admit_owned_connection_reserved(
    db: Connection,
    external_retained_bytes: u64,
) -> anyhow::Result<Connection> {
    let mut allocation = accounting::Allocation::new(external_retained_bytes)?;
    ensure!(
        db.is_autocommit(),
        "native41 control requires an unborrowed connection"
    );
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.pragma_update(None, "foreign_keys", true)?;
    db.pragma_update(None, "recursive_triggers", true)?;
    db.pragma_update(None, "synchronous", "FULL")?;
    db.execute_batch("BEGIN IMMEDIATE")?;
    allocation.reservation(&db, None, None)?;
    journal::verify(&db)?;
    crate::source_freeze::verify_sync(&db)?;
    let activated: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM catabolic_cutover WHERE phase='activated') OR EXISTS(SELECT 1 FROM catabolic_cutover_sets WHERE phase='activated')", [], |r| r.get(0))?;
    ensure!(
        activated,
        "native41 transition requires completed Motion activation"
    );
    Ok(db)
}
fn initial_identity(db: &Connection) -> anyhow::Result<(Identity, Option<String>, String)> {
    let (binding, binding_hash) = frozen(db)?;
    let root = crate::native_upgrade::effective_identity_sync(db, &binding)?;
    let restore = crate::paired_restore::authenticated_transitions(db, &binding)?;
    let mut transitions = restore.transitions().to_vec();
    transitions.extend(authenticated_transitions(db, &binding_hash)?);
    match catalog_identity_lineage::resolve(&root, &transitions)
        .map_err(|e| anyhow::anyhow!("native transition lineage: {e:?}"))?
    {
        Resolution::Current { identity, ordered } => Ok((
            identity.clone(),
            ordered.last().map(|t| t.id.clone()),
            binding_hash,
        )),
        Resolution::Pending { .. } => {
            anyhow::bail!("native transition pending; exact recovery is required")
        }
    }
}

/// Owns both real retained preparations; serialized intent alone cannot call apply.
pub(crate) struct PreparedTransition {
    allocation: accounting::Allocation,
    imported_archive: Option<imported_archive::RetainedArchiveCapture>,
    db: Connection,
    native: PreparedOfflineBootstrap,
    intent: Intent,
    raw: String,
}
impl PreparedTransition {
    pub(crate) fn refresh_external_reservation(
        &mut self,
        external_retained_bytes: u64,
    ) -> anyhow::Result<()> {
        let raised = self.allocation.advance(&self.db, external_retained_bytes)?;
        self.native.refresh_external_reservation(raised)?;
        let reservation = self
            .allocation
            .reservation(&self.db, Some(&self.intent.plan), None)?;
        self.native.refresh_external_reservation(reservation)?;
        Ok(())
    }

    pub(crate) fn prepare(
        motion: &Path,
        native: &Path,
        q: NativeUpgradeQuiescence,
        id: &str,
    ) -> anyhow::Result<Self> {
        Self::prepare_reserved(motion, native, q, id, 0)
    }
    pub(crate) fn prepare_reserved(
        motion: &Path,
        native: &Path,
        q: NativeUpgradeQuiescence,
        id: &str,
        external_retained_bytes: u64,
    ) -> anyhow::Result<Self> {
        Self::prepare_admitted(
            open_reserved(motion, external_retained_bytes)?,
            native,
            q,
            id,
            external_retained_bytes,
            None,
        )
    }
    /// Fixed converted-pair seam. The caller retains independent selected-pair
    /// custody; this owner repeats all control, freeze and lineage checks before
    /// consuming the mutable Motion image. No native Connection is exposed.
    pub(crate) fn prepare_owned_image(
        db: Connection,
        native: &Path,
        q: NativeUpgradeQuiescence,
        id: &str,
    ) -> anyhow::Result<Self> {
        Self::prepare_admitted(admit_owned_connection(db)?, native, q, id, 0, None)
    }
    pub(crate) fn prepare_retained_control(
        db: Connection,
        native: &Path,
        q: NativeUpgradeQuiescence,
        id: &str,
    ) -> anyhow::Result<Self> {
        Self::prepare_retained_control_reserved(db, native, q, id, 0)
    }
    pub(crate) fn prepare_retained_control_reserved(
        db: Connection,
        native: &Path,
        q: NativeUpgradeQuiescence,
        id: &str,
        external_retained_bytes: u64,
    ) -> anyhow::Result<Self> {
        Self::prepare_retained_control_impl(db, native, q, id, external_retained_bytes, None)
    }
    /// Optional genuine archive custody, captured before native acquisition.
    /// This does not populate bootstrap evidence or enable library admission.
    pub(crate) fn prepare_retained_control_with_archive_reserved(
        db: Connection,
        native: &Path,
        q: NativeUpgradeQuiescence,
        id: &str,
        external_retained_bytes: u64,
        deadline: std::time::Instant,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> anyhow::Result<Self> {
        Self::prepare_retained_control_impl(
            db,
            native,
            q,
            id,
            external_retained_bytes,
            Some((deadline, cancel)),
        )
    }
    fn prepare_retained_control_impl(
        db: Connection,
        native: &Path,
        q: NativeUpgradeQuiescence,
        id: &str,
        external_retained_bytes: u64,
        capture: Option<(
            std::time::Instant,
            std::sync::Arc<std::sync::atomic::AtomicBool>,
        )>,
    ) -> anyhow::Result<Self> {
        accounting::Allocation::new(external_retained_bytes)?.reservation(&db, None, None)?;
        ensure!(
            !db.is_autocommit(),
            "native41 handoff requires retained control writer"
        );
        journal::verify(&db)?;
        crate::source_freeze::verify_sync(&db)?;
        let activated: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM catabolic_cutover WHERE phase='activated') OR EXISTS(SELECT 1 FROM catabolic_cutover_sets WHERE phase='activated')", [], |r| r.get(0))?;
        ensure!(
            activated,
            "native41 transition requires completed Motion activation"
        );
        Self::prepare_admitted(db, native, q, id, external_retained_bytes, capture)
    }
    fn prepare_admitted(
        db: Connection,
        native: &Path,
        q: NativeUpgradeQuiescence,
        id: &str,
        external_retained_bytes: u64,
        capture: Option<(
            std::time::Instant,
            std::sync::Arc<std::sync::atomic::AtomicBool>,
        )>,
    ) -> anyhow::Result<Self> {
        let mut allocation = accounting::Allocation::new(external_retained_bytes)?;
        allocation.reservation(&db, None, None)?;
        ensure!(identifier(id), "invalid native41 transition ID");
        let (from, predecessor, binding_hash) = initial_identity(&db)?;
        ensure!(
            from.schema_version == 40,
            "native41 transition already completed"
        );
        let imported_archive = match capture {
            Some((deadline, cancel)) => {
                let remaining = allocation.initial_archive_remaining(&db)?;
                let archive = imported_archive::RetainedArchiveCapture::capture_in(
                    &db, &from, remaining, deadline, cancel,
                )?;
                allocation.retain_archive(archive.retained_allocations()?)?;
                // Recheck target cache growth before acquiring the native owner.
                allocation.reservation(&db, None, None)?;
                Some(archive)
            }
            None => None,
        };
        let mut native = PreparedOfflineBootstrap::prepare_reserved(
            native,
            q,
            allocation.reservation(&db, None, None)?,
        )?;
        ensure!(
            native.catalog_id() == from.catalog_id && native.incarnation() == from.incarnation,
            "native40 preparation differs from authenticated Motion identity"
        );
        // Bound serialization before retaining a second owned plan copy.
        admit_plan(native.approved_plan())?;
        let reservation = allocation.reservation(&db, Some(native.approved_plan()), None)?;
        native.refresh_external_reservation(reservation)?;
        let intent = Intent {
            contract_version: 1,
            transition_id: id.into(),
            predecessor,
            to: Identity {
                schema_version: 41,
                ..from.clone()
            },
            from,
            frozen_binding_sha256: binding_hash,
            native_path: native.native_path().to_owned(),
            native_file_identity: native.file_identity(),
            source_generation: native.source_generation(),
            plan: native.approved_plan().clone(),
        };
        validate_intent(&intent)?;
        let raw = serde_json::to_string(&intent)?;
        allocation.retain_raw(&raw)?;
        journal::insert(&db, "intent", id, &raw)?;
        native.refresh_external_reservation(allocation.reservation(
            &db,
            Some(&intent.plan),
            None,
        )?)?;
        if let Some(archive) = &imported_archive {
            archive.check()?;
        }
        db.execute_batch("COMMIT")?;
        Ok(Self {
            allocation,
            imported_archive,
            db,
            native,
            intent,
            raw,
        })
    }
    pub(crate) fn recover_before_migration(
        motion: &Path,
        native_path: &Path,
        q: NativeUpgradeQuiescence,
        id: &str,
    ) -> anyhow::Result<Self> {
        Self::recover_before_migration_reserved(motion, native_path, q, id, 0)
    }
    pub(crate) fn recover_before_migration_reserved(
        motion: &Path,
        native_path: &Path,
        q: NativeUpgradeQuiescence,
        id: &str,
        external_retained_bytes: u64,
    ) -> anyhow::Result<Self> {
        let mut allocation = accounting::Allocation::new(external_retained_bytes)?;
        let db = open_reserved(motion, external_retained_bytes)?;
        allocation.reservation(&db, None, None)?;
        let (intent, raw) = saved_for_recovery(&db, native_path, id)?;
        allocation.retain_raw(&raw)?;
        ensure!(
            journal::record(&db, "command", id)?.is_none()
                && journal::record(&db, "ack", id)?.is_none(),
            "native41 migration already advanced"
        );
        let native = PreparedOfflineBootstrap::prepare_reserved(
            native_path,
            q,
            allocation.reservation(&db, Some(&intent.plan), None)?,
        )?;
        ensure!(
            native.approved_plan() == &intent.plan
                && native.catalog_id() == intent.from.catalog_id
                && native.incarnation() == intent.from.incarnation
                && native.file_identity() == intent.native_file_identity
                && native.source_generation() == intent.source_generation,
            "native40 source no longer matches saved transition"
        );
        db.execute_batch("COMMIT")?;
        Ok(Self {
            allocation,
            imported_archive: None,
            db,
            native,
            intent,
            raw,
        })
    }
    fn verify_prepared_writer(&self) -> anyhow::Result<()> {
        if let Some(archive) = &self.imported_archive {
            archive.check()?;
        }
        self.allocation
            .check(&self.db, Some(&self.intent.plan), None)?;
        verify_intent_writer(&self.db, &self.intent, &self.raw)?;
        ensure!(
            journal::record(&self.db, "command", &self.intent.transition_id)?.is_none()
                && journal::record(&self.db, "ack", &self.intent.transition_id)?.is_none()
                && self.native.approved_plan() == &self.intent.plan
                && self.native.catalog_id() == self.intent.from.catalog_id
                && self.native.incarnation() == self.intent.from.incarnation
                && self.native.file_identity() == self.intent.native_file_identity
                && self.native.source_generation() == self.intent.source_generation,
            "native41 prepared phase or retained native plan changed"
        );
        Ok(())
    }
    pub(crate) fn apply(
        mut self,
        backup_parent: Option<&Path>,
    ) -> anyhow::Result<PendingTransition> {
        self.refresh_external_reservation(self.allocation.external())?;
        retain_writer(&self.db)?;
        self.verify_prepared_writer()?;
        let native = self.native.apply(backup_parent)?;
        ensure!(
            native.approved_plan() == &self.intent.plan
                && native.catalog_id() == self.intent.from.catalog_id
                && native.incarnation() == self.intent.from.incarnation,
            "native41 migrated owner differs"
        );
        if let Some(archive) = &self.imported_archive {
            archive.check()?;
        }
        self.db.execute_batch("COMMIT")?;
        Ok(PendingTransition {
            allocation: self.allocation,
            imported_archive: self.imported_archive,
            db: self.db,
            native,
            intent: self.intent,
            raw: self.raw,
        })
    }
}
pub(crate) struct PendingTransition {
    allocation: accounting::Allocation,
    imported_archive: Option<imported_archive::RetainedArchiveCapture>,
    db: Connection,
    native: OfflineBootstrap<'static>,
    intent: Intent,
    raw: String,
}
/// This value can only be returned after actual measured native Complete and the
/// exact Motion ACK commit, with native session custody retained throughout.
pub(crate) struct AcknowledgedNativeBootstrap {
    allocation: accounting::Allocation,
    imported_archive: Option<imported_archive::RetainedArchiveCapture>,
    db: Connection,
    native: OfflineBootstrap<'static>,
    command: CommandEnvelope,
    intent: Intent,
    acknowledgment: Acknowledgment,
    ack_digest: String,
}
impl PendingTransition {
    pub(crate) fn refresh_external_reservation(
        &mut self,
        external_retained_bytes: u64,
    ) -> anyhow::Result<()> {
        let raised = self.allocation.advance(&self.db, external_retained_bytes)?;
        self.native.refresh_external_reservation(raised)?;
        let reservation = self
            .allocation
            .reservation(&self.db, Some(&self.intent.plan), None)?;
        self.native.refresh_external_reservation(reservation)?;
        Ok(())
    }

    fn verify_pending_writer(&self) -> anyhow::Result<()> {
        if let Some(archive) = &self.imported_archive {
            archive.check()?;
        }
        self.allocation
            .check(&self.db, Some(&self.intent.plan), None)?;
        verify_intent_writer(&self.db, &self.intent, &self.raw)?;
        ensure!(
            journal::record(&self.db, "ack", &self.intent.transition_id)?.is_none()
                && self.native.approved_plan() == &self.intent.plan
                && self.native.catalog_id() == self.intent.from.catalog_id
                && self.native.incarnation() == self.intent.from.incarnation,
            "native41 pending phase or retained native plan changed"
        );
        let transition = self.native.transition()?;
        ensure!(
            transition.approved_plan_sha256()
                == self.intent.plan["plan_fingerprint"]
                    .as_str()
                    .context("native41 pending plan fingerprint missing")?
                && transition.source_generation() == self.intent.source_generation
                && transition.catalog_id() == self.intent.from.catalog_id
                && transition.incarnation() == self.intent.from.incarnation,
            "native41 pending permanent migration differs"
        );
        Ok(())
    }
    pub(crate) fn verified_backup_path(&self) -> anyhow::Result<&Path> {
        Ok(self.native.verified_backup_path()?)
    }
    pub(crate) fn bootstrap(
        self,
        operation_id: &str,
        evidence: &[HostSourceEvidence<'_>],
        policy: ReceiptPolicy,
    ) -> anyhow::Result<AcknowledgedNativeBootstrap> {
        self.save_command(operation_id, evidence)?
            .resume_saved(evidence, policy)
    }
    /// A durable pre-effect phase, recoverable without inventing another key.
    pub(crate) fn save_command(
        mut self,
        operation_id: &str,
        evidence: &[HostSourceEvidence<'_>],
    ) -> anyhow::Result<Self> {
        self.refresh_external_reservation(self.allocation.external())?;
        retain_writer(&self.db)?;
        self.verify_pending_writer()?;
        ensure!(
            journal::record(&self.db, "command", &self.intent.transition_id)?.is_none(),
            "native41 bootstrap command already saved; use receipt-first recovery"
        );
        let payload = self.native.prepare(crate::HOST_NAMESPACE, evidence)?;
        self.native
            .refresh_external_reservation(self.allocation.with_extra(
                &self.db,
                &self.intent.plan,
                &payload,
            )?)?;
        let command = CommandEnvelope::new(
            1,
            CommandKey::new(
                &self.intent.from.catalog_id,
                crate::HOST_NAMESPACE,
                operation_id,
            )?,
            &self.intent.from.incarnation,
            self.native.generation()?,
            &serde_json::to_string(&payload)?,
        )?;
        journal::insert(
            &self.db,
            "command",
            &self.intent.transition_id,
            command.canonical_payload(),
        )?;
        self.refresh_external_reservation(self.allocation.external())?;
        if let Some(archive) = &self.imported_archive {
            archive.check()?;
        }
        self.db.execute_batch("COMMIT")?;
        Ok(self)
    }
    /// Compatibility convenience for internal fixtures. Production callers must
    /// retain the executed phase through final current authorization before ACK.
    pub(crate) fn resume_saved(
        self,
        evidence: &[HostSourceEvidence<'_>],
        policy: ReceiptPolicy,
    ) -> anyhow::Result<AcknowledgedNativeBootstrap> {
        self.execute_saved(evidence, policy)?.acknowledge()
    }
    /// Execute only the exact saved native command. This deliberately does not
    /// acknowledge it in Motion: the same writer and native custody remain held
    /// while the trusted host reauthorizes the current caller.
    pub(crate) fn execute_saved(
        mut self,
        evidence: &[HostSourceEvidence<'_>],
        policy: ReceiptPolicy,
    ) -> anyhow::Result<ExecutedTransition> {
        self.refresh_external_reservation(self.allocation.external())?;
        retain_writer(&self.db)?;
        self.verify_pending_writer()?;
        let command = crate::decode_envelope(
            &journal::record(&self.db, "command", &self.intent.transition_id)?
                .context("native41 saved command missing")?,
        )?;
        ensure!(
            command.key().catalog_id() == self.intent.to.catalog_id
                && command.admission_incarnation() == self.intent.to.incarnation
                && command.key().host_namespace() == crate::HOST_NAMESPACE,
            "native41 saved command scope differs"
        );
        self.native
            .refresh_external_reservation(self.allocation.reservation(
                &self.db,
                Some(&self.intent.plan),
                Some(&command),
            )?)?;
        if self
            .native
            .reconcile(&command, crate::HOST_NAMESPACE)?
            .is_none()
        {
            self.native
                .execute(&command, crate::HOST_NAMESPACE, evidence, policy)?;
        }
        ExecutedTransition::measured(
            self.db,
            self.native,
            self.intent,
            self.raw,
            command,
            self.allocation,
            self.imported_archive,
        )
    }
    pub(crate) fn recover_pending(
        motion: &Path,
        native_path: &Path,
        q: NativeUpgradeQuiescence,
        id: &str,
        backup: &Path,
    ) -> anyhow::Result<Self> {
        Self::recover_pending_reserved(motion, native_path, q, id, backup, 0)
    }
    pub(crate) fn recover_pending_reserved(
        motion: &Path,
        native_path: &Path,
        q: NativeUpgradeQuiescence,
        id: &str,
        backup: &Path,
        external_retained_bytes: u64,
    ) -> anyhow::Result<Self> {
        let mut allocation = accounting::Allocation::new(external_retained_bytes)?;
        let db = open_reserved(motion, external_retained_bytes)?;
        allocation.reservation(&db, None, None)?;
        let (intent, raw) = saved_for_recovery(&db, native_path, id)?;
        allocation.retain_raw(&raw)?;
        ensure!(
            journal::record(&db, "ack", id)?.is_none(),
            "native41 ACK exists; use completed receipt recovery"
        );
        admit_plan(&intent.plan)?;
        let reservation = allocation.reservation(&db, Some(&intent.plan), None)?;
        let native = OfflineBootstrap::recover_owned_reserved(
            native_path,
            q,
            intent.plan.clone(),
            catabolic_store::migration::offline_bootstrap::Recovery::Pending {
                backup_path: backup,
            },
            reservation,
        )?;
        db.execute_batch("COMMIT")?;
        Ok(Self {
            allocation,
            imported_archive: None,
            db,
            native,
            intent,
            raw,
        })
    }
}

/// Post-native, pre-ACK phase. It owns the original Motion writer and native
/// session; there is no transaction extraction, operation substitution or raw
/// connection callback. Dropping it leaves the permanent native receipt for
/// exact receipt-only recovery, never an implicit Motion ACK.
pub(crate) struct ExecutedTransition {
    allocation: accounting::Allocation,
    imported_archive: Option<imported_archive::RetainedArchiveCapture>,
    db: Connection,
    native: OfflineBootstrap<'static>,
    intent: Intent,
    raw: String,
    command: CommandEnvelope,
}
impl ExecutedTransition {
    pub(crate) fn refresh_external_reservation(
        &mut self,
        external_retained_bytes: u64,
    ) -> anyhow::Result<()> {
        let raised = self.allocation.advance(&self.db, external_retained_bytes)?;
        self.native.refresh_external_reservation(raised)?;
        let reservation =
            self.allocation
                .reservation(&self.db, Some(&self.intent.plan), Some(&self.command))?;
        self.native.refresh_external_reservation(reservation)?;
        Ok(())
    }

    fn measured(
        db: Connection,
        native: OfflineBootstrap<'static>,
        intent: Intent,
        raw: String,
        command: CommandEnvelope,
        allocation: accounting::Allocation,
        imported_archive: Option<imported_archive::RetainedArchiveCapture>,
    ) -> anyhow::Result<Self> {
        let mut owner = Self {
            allocation,
            imported_archive,
            db,
            native,
            intent,
            raw,
            command,
        };
        owner.refresh_external_reservation(owner.allocation.external())?;
        owner.verify_current()?;
        Ok(owner)
    }
    fn verify_current(&self) -> anyhow::Result<()> {
        if let Some(archive) = &self.imported_archive {
            archive.check()?;
        }
        self.allocation
            .check(&self.db, Some(&self.intent.plan), Some(&self.command))?;
        verify_saved_writer(&self.db, &self.intent, &self.raw, &self.command)?;
        let complete = self.native.complete(&self.command)?;
        ensure!(
            complete.catalog_id() == self.intent.to.catalog_id
                && complete.incarnation() == self.intent.to.incarnation
                && complete.command_key() == self.command.key()
                && complete.payload_digest() == self.command.digest().as_str(),
            "native41 executed Complete differs from saved command"
        );
        Ok(())
    }
    /// Called only after final current authorization. The fixed finish owner
    /// revalidates the saved writer and remeasures Complete before ACK COMMIT.
    pub(crate) fn acknowledge(self) -> anyhow::Result<AcknowledgedNativeBootstrap> {
        finish(
            self.db,
            self.native,
            self.intent,
            self.raw,
            self.command,
            self.allocation,
            self.imported_archive,
        )
    }
}

impl ExecutedTransition {
    /// Only exact saved receipt recovery. Missing receipts/effects never cause
    /// migration or bootstrap redispatch, and no archive/backup is required.
    pub(crate) fn recover_completed(
        motion: &Path,
        native_path: &Path,
        q: NativeUpgradeQuiescence,
        id: &str,
    ) -> anyhow::Result<Self> {
        Self::recover_completed_reserved(motion, native_path, q, id, 0)
    }
    pub(crate) fn recover_completed_reserved(
        motion: &Path,
        native_path: &Path,
        q: NativeUpgradeQuiescence,
        id: &str,
        external_retained_bytes: u64,
    ) -> anyhow::Result<Self> {
        let mut allocation = accounting::Allocation::new(external_retained_bytes)?;
        let db = open_reserved(motion, external_retained_bytes)?;
        allocation.reservation(&db, None, None)?;
        let (intent, raw) = saved_for_recovery(&db, native_path, id)?;
        allocation.retain_raw(&raw)?;
        let command = crate::decode_envelope(
            &journal::record(&db, "command", id)?
                .context("native41 saved bootstrap command missing")?,
        )?;
        ensure!(
            command.key().catalog_id() == intent.to.catalog_id
                && command.admission_incarnation() == intent.to.incarnation
                && command.key().host_namespace() == crate::HOST_NAMESPACE,
            "native41 recovery command differs"
        );
        admit_plan(&intent.plan)?;
        let reservation = allocation.reservation(&db, Some(&intent.plan), None)?;
        let native = OfflineBootstrap::recover_owned_reserved(
            native_path,
            q,
            intent.plan.clone(),
            catabolic_store::migration::offline_bootstrap::Recovery::CompletedReceiptOnly {
                command: &command,
            },
            reservation,
        )?;
        Self::measured(db, native, intent, raw, command, allocation, None)
    }
}

fn retain_writer(db: &Connection) -> anyhow::Result<()> {
    if db.is_autocommit() {
        db.execute_batch("BEGIN IMMEDIATE")?;
    }
    ensure!(
        !db.is_autocommit()
            && db.transaction_state(Some("main"))? == rusqlite::TransactionState::Write,
        "native41 requires retained Motion writer"
    );
    Ok(())
}
fn verify_intent_writer(db: &Connection, intent: &Intent, raw: &str) -> anyhow::Result<()> {
    ensure!(
        !db.is_autocommit()
            && db.transaction_state(Some("main"))? == rusqlite::TransactionState::Write,
        "native41 requires retained Motion writer"
    );
    journal::verify(db)?;
    crate::source_freeze::verify_sync(db)?;
    let (_, current_raw) = saved_for_recovery(db, &intent.native_path, &intent.transition_id)?;
    ensure!(current_raw == raw, "native41 retained intent changed");
    Ok(())
}
fn verify_saved_writer(
    db: &Connection,
    intent: &Intent,
    raw: &str,
    command: &CommandEnvelope,
) -> anyhow::Result<()> {
    verify_intent_writer(db, intent, raw)?;
    ensure!(
        journal::record(db, "command", &intent.transition_id)?.as_deref()
            == Some(command.canonical_payload()),
        "native41 retained exact command changed"
    );
    Ok(())
}

fn finish(
    db: Connection,
    mut native: OfflineBootstrap<'static>,
    intent: Intent,
    raw: String,
    command: CommandEnvelope,
    mut allocation: accounting::Allocation,
    imported_archive: Option<imported_archive::RetainedArchiveCapture>,
) -> anyhow::Result<AcknowledgedNativeBootstrap> {
    if let Some(archive) = &imported_archive {
        archive.check()?;
    }
    native.refresh_external_reservation(allocation.reservation(
        &db,
        Some(&intent.plan),
        Some(&command),
    )?)?;
    verify_saved_writer(&db, &intent, &raw, &command)?;
    let transition = native.transition()?;
    ensure!(
        transition.approved_plan_sha256()
            == intent.plan["plan_fingerprint"]
                .as_str()
                .context("native41 plan fingerprint missing")?
            && transition.source_generation() == intent.source_generation
            && transition.catalog_id() == intent.from.catalog_id
            && transition.incarnation() == intent.from.incarnation,
        "native41 permanent migration differs from saved intent"
    );
    drop(transition);
    let complete = native.complete(&command)?;
    ensure!(
        complete.catalog_id() == intent.to.catalog_id
            && complete.incarnation() == intent.to.incarnation
            && complete.command_key() == command.key()
            && complete.payload_digest() == command.digest().as_str(),
        "native41 Complete differs from saved command"
    );
    let acknowledgment = Acknowledgment {
        contract_version: 1,
        transition_id: intent.transition_id.clone(),
        intent_sha256: hash(&raw),
        native_transition_sha256: complete.transition_digest().into(),
        command_key: complete.command_key().clone(),
        payload_digest: complete.payload_digest().into(),
        inventory_digest: complete.inventory_digest().into(),
        committed_generation: complete.committed_generation(),
        state_digest: complete.state_digest().into(),
        source_count: complete.source_count(),
    };
    let ack = serde_json::to_string(&acknowledgment)?;
    validate_ack(&intent, &raw, &command, &acknowledgment)?;
    match journal::record(&db, "ack", &intent.transition_id)? {
        Some(prior) => ensure!(
            prior == ack,
            "retained native41 ACK differs from measured completion"
        ),
        None => journal::insert(&db, "ack", &intent.transition_id, &ack)?,
    }
    allocation.check(&db, Some(&intent.plan), Some(&command))?;
    if let Some(archive) = &imported_archive {
        archive.check()?;
    }
    // Native Complete and its original lifecycle/writer remain held at commit.
    db.execute_batch("COMMIT")?;
    db.execute_batch("BEGIN IMMEDIATE")
        .context("native41 ACK committed; retained Motion custody must be recovered")?;
    journal::verify(&db)?;
    crate::source_freeze::verify_sync(&db)?;
    allocation.check(&db, Some(&intent.plan), Some(&command))?;
    let (_, current_raw) = saved_for_recovery(&db, &intent.native_path, &intent.transition_id)?;
    ensure!(
        current_raw == raw
            && journal::record(&db, "command", &intent.transition_id)?.as_deref()
                == Some(command.canonical_payload()),
        "native41 ACK committed but current terminal intent or command changed"
    );
    ensure!(
        journal::record(&db, "ack", &intent.transition_id)?.as_deref() == Some(ack.as_str()),
        "native41 ACK committed but changed before retained image custody"
    );
    drop(complete);
    native.refresh_external_reservation(allocation.reservation(
        &db,
        Some(&intent.plan),
        Some(&command),
    )?)?;
    Ok(AcknowledgedNativeBootstrap {
        allocation,
        imported_archive,
        db,
        native,
        command,
        intent,
        acknowledgment,
        ack_digest: hash(&ack),
    })
}
/// Fixed paired conversion handoff, retaining the actual Motion writer and the
/// separately verified native image/source session. Only96 can construct it.
pub(crate) struct AcknowledgedCompletedImage {
    image: catabolic_store::migration::offline_bootstrap::CompletedOfflineImage,
    db: Connection,
    proof: AcknowledgedBootstrapProof,
}
pub(crate) struct AcknowledgedBootstrapProof {
    transition_id: String,
    predecessor: Option<String>,
    identity: Identity,
    frozen_binding_sha256: String,
    ack_sha256: String,
    native_transition_sha256: String,
}
impl AcknowledgedBootstrapProof {
    pub(crate) fn transition_id(&self) -> &str {
        &self.transition_id
    }
    pub(crate) fn predecessor(&self) -> Option<&str> {
        self.predecessor.as_deref()
    }
    pub(crate) fn identity(&self) -> &Identity {
        &self.identity
    }
    pub(crate) fn frozen_binding_sha256(&self) -> &str {
        &self.frozen_binding_sha256
    }
    pub(crate) fn ack_sha256(&self) -> &str {
        &self.ack_sha256
    }
    pub(crate) fn native_transition_sha256(&self) -> &str {
        &self.native_transition_sha256
    }
}
impl AcknowledgedCompletedImage {
    /// Internal owner-to-owner transfer, never a public raw native connection.
    pub(crate) fn into_restore_parts(
        self,
    ) -> (
        Connection,
        catabolic_store::migration::offline_bootstrap::CompletedOfflineImage,
        AcknowledgedBootstrapProof,
    ) {
        (self.db, self.image, self.proof)
    }
}
impl AcknowledgedNativeBootstrap {
    pub(crate) fn refresh_external_reservation(
        &mut self,
        external_retained_bytes: u64,
    ) -> anyhow::Result<()> {
        let raised = self.allocation.advance(&self.db, external_retained_bytes)?;
        self.native.refresh_external_reservation(raised)?;
        let reservation =
            self.allocation
                .reservation(&self.db, Some(&self.intent.plan), Some(&self.command))?;
        self.native.refresh_external_reservation(reservation)?;
        Ok(())
    }

    pub(crate) fn into_completed_image(
        mut self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<AcknowledgedCompletedImage> {
        self.refresh_external_reservation(self.allocation.external())?;
        ensure!(
            !self.db.is_autocommit(),
            "native41 paired handoff lost Motion writer"
        );
        journal::verify(&self.db)?;
        ensure!(
            journal::record(&self.db, "ack", &self.intent.transition_id)?
                .as_deref()
                .is_some_and(|raw| hash(raw) == self.ack_digest),
            "native41 paired handoff ACK differs"
        );
        let image = self.native.into_completed_image(&self.command, cancel)?;
        let proof = AcknowledgedBootstrapProof {
            transition_id: self.intent.transition_id,
            predecessor: self.intent.predecessor,
            identity: self.intent.to,
            frozen_binding_sha256: self.intent.frozen_binding_sha256,
            ack_sha256: self.ack_digest,
            native_transition_sha256: self.acknowledgment.native_transition_sha256,
        };
        Ok(AcknowledgedCompletedImage {
            image,
            db: self.db,
            proof,
        })
    }
    /// Internal compatibility convenience. Production recovery retains the
    /// explicit executed phase until final authorization before ACK.
    pub(crate) fn recover_completed(
        motion: &Path,
        native_path: &Path,
        q: NativeUpgradeQuiescence,
        id: &str,
    ) -> anyhow::Result<Self> {
        Self::recover_completed_reserved(motion, native_path, q, id, 0)
    }
    pub(crate) fn recover_completed_reserved(
        motion: &Path,
        native_path: &Path,
        q: NativeUpgradeQuiescence,
        id: &str,
        external_retained_bytes: u64,
    ) -> anyhow::Result<Self> {
        ExecutedTransition::recover_completed_reserved(
            motion,
            native_path,
            q,
            id,
            external_retained_bytes,
        )?
        .acknowledge()
    }
    pub(crate) fn transition_id(&self) -> &str {
        &self.intent.transition_id
    }
    pub(crate) fn predecessor(&self) -> Option<&str> {
        self.intent.predecessor.as_deref()
    }
    pub(crate) fn acknowledgment_digest(&self) -> &str {
        &self.ack_digest
    }
    pub(crate) fn native_transition_digest(&self) -> &str {
        &self.acknowledgment.native_transition_sha256
    }
    pub(crate) fn identity(&self) -> &Identity {
        &self.intent.to
    }
}

fn saved_for_recovery(
    db: &Connection,
    native_path: &Path,
    id: &str,
) -> anyhow::Result<(Intent, String)> {
    let raw = journal::record(db, "intent", id)?.context("native41 transition intent missing")?;
    let intent: Intent = decode(&raw)?;
    validate_intent(&intent)?;
    ensure!(
        intent.transition_id == id && crate::destination(native_path)? == intent.native_path,
        "native41 recovery target differs"
    );
    let (binding, binding_hash) = frozen(db)?;
    ensure!(
        binding_hash == intent.frozen_binding_sha256,
        "native41 recovery frozen binding differs"
    );
    let root = crate::native_upgrade::effective_identity_sync(db, &binding)?;
    let restores = crate::paired_restore::authenticated_transitions(db, &binding)?;
    let mut transitions = restores.transitions().to_vec();
    transitions.extend(authenticated_transitions(db, &binding_hash)?);
    match catalog_identity_lineage::resolve(&root, &transitions)
        .map_err(|e| anyhow::anyhow!("native41 recovery lineage: {e:?}"))?
    {
        Resolution::Pending { transition, .. } => {
            ensure!(transition.id == id, "different transition is pending")
        }
        Resolution::Current { identity, ordered } => ensure!(
            *identity == intent.to && ordered.last().is_some_and(|step| step.id == id),
            "native41 recovery is not the current terminal transition"
        ),
    }
    Ok((intent, raw))
}

fn hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn validate_ack(
    intent: &Intent,
    raw: &str,
    command: &CommandEnvelope,
    ack: &Acknowledgment,
) -> anyhow::Result<()> {
    ensure!(
        ack.contract_version == 1
            && ack.transition_id == intent.transition_id
            && ack.intent_sha256 == hash(raw)
            && ack.command_key == *command.key()
            && ack.payload_digest == command.digest().as_str()
            && command.key().catalog_id() == intent.to.catalog_id
            && command.admission_incarnation() == intent.to.incarnation
            && command.key().host_namespace() == crate::HOST_NAMESPACE
            && command.payload()["operation"] == "nfo_bootstrap"
            && command.payload()["contract_version"] == 1
            && command.payload()["inventory_digest"] == ack.inventory_digest
            && ack.committed_generation >= command.expected_generation()
            && ack.source_count >= 0
            && [
                &ack.native_transition_sha256,
                &ack.inventory_digest,
                &ack.state_digest
            ]
            .into_iter()
            .all(|s| hex(s)),
        "native41 acknowledgment differs from exact bootstrap intent"
    );
    Ok(())
}
pub(crate) fn authenticated_transitions(
    db: &Connection,
    binding_hash: &str,
) -> anyhow::Result<Vec<Transition>> {
    ensure!(
        !db.is_autocommit(),
        "native41 lineage requires retained transaction"
    );
    journal::verify(db)?;
    let row: Option<(String, String)> = db
        .query_row(
            "SELECT transition_id,intent_json FROM catabolic_native_bootstrap_intents",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((id, raw)) = row else {
        return Ok(Vec::new());
    };
    let intent: Intent = decode(&raw)?;
    validate_intent(&intent)?;
    ensure!(
        intent.transition_id == id && intent.frozen_binding_sha256 == binding_hash,
        "native41 frozen binding differs"
    );
    let status = if let Some(ack) = journal::record(db, "ack", &id)? {
        let command = crate::decode_envelope(
            &journal::record(db, "command", &id)?.context("native41 ACK has no saved command")?,
        )?;
        let acknowledgment: Acknowledgment = decode(&ack)?;
        validate_ack(&intent, &raw, &command, &acknowledgment)?;
        TransitionStatus::Acknowledged
    } else {
        TransitionStatus::Pending
    };
    Ok(vec![Transition {
        id,
        predecessor: intent.predecessor,
        from: intent.from,
        to: intent.to,
        kind: TransitionKind::Native41Bootstrap,
        status,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_journal_retains_every_outer_field_and_exact_canonical_bytes() {
        let command=CommandEnvelope::new(1,CommandKey::new("catalog",crate::HOST_NAMESPACE,"operation").unwrap(),
            "incarnation",7,r#"{"operation":"nfo_bootstrap","contract_version":1,"inventory_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#).unwrap();
        let wire: Value = serde_json::from_str(command.canonical_payload()).unwrap();
        assert_eq!(wire["key"]["operation_id"], "operation");
        assert_eq!(wire["admission_incarnation"], "incarnation");
        assert_eq!(wire["expected_generation"], 7);
        assert_eq!(
            crate::decode_envelope(command.canonical_payload()).unwrap(),
            command
        );
        let changed = CommandEnvelope::new(
            1,
            command.key().clone(),
            "incarnation",
            8,
            &command.payload().to_string(),
        )
        .unwrap();
        assert_ne!(changed.digest(), command.digest());
        assert_ne!(changed.canonical_payload(), command.canonical_payload());
    }
    #[test]
    fn exact_journal_denies_unowned_writes_and_retains_rows() {
        let db = Connection::open_in_memory().unwrap();
        db.pragma_update(None, "foreign_keys", true).unwrap();
        db.execute_batch(include_str!("native_bootstrap/schema96.sql"))
            .unwrap();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        journal::verify(&db).unwrap();
        let raw = r#"{"contract_version":1,"transition_id":"one","from":{"schema_version":40},"to":{"schema_version":41}}"#;
        assert!(
            db.execute(
                "INSERT INTO catabolic_native_bootstrap_intents VALUES('one',?)",
                [raw]
            )
            .is_err()
        );
        journal::insert(&db, "intent", "one", raw).unwrap();
        assert_eq!(
            journal::record(&db, "intent", "one").unwrap().as_deref(),
            Some(raw)
        );
        assert!(
            db.execute(
                "UPDATE catabolic_native_bootstrap_intents SET intent_json=intent_json",
                []
            )
            .is_err()
        );
        assert!(
            db.execute("DELETE FROM catabolic_native_bootstrap_intents", [])
                .is_err()
        );
        assert!(
            db.execute(
                "INSERT INTO catabolic_native_bootstrap_commands VALUES('one','{}')",
                []
            )
            .is_err()
        );
        assert!(journal::insert(&db, "command", "foreign", "{}").is_err());
        db.execute_batch("ROLLBACK").unwrap();
        assert!(
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM catabolic_native_bootstrap_intents)",
                [],
                |r| r.get::<_, bool>(0)
            )
            .unwrap()
                == false
        );
    }
    #[test]
    fn plan_copy_is_precharged_with_escaped_wire_bytes() {
        admit_plan(&serde_json::json!({"small":"value"})).unwrap();
        assert!(admit_plan(&Value::String("\n".repeat(MAX_RECORD / 2))).is_err());
        let mut budget = EncodedBudget { bytes: usize::MAX };
        assert!(std::io::Write::write(&mut budget, b"x").is_err());
    }
}
