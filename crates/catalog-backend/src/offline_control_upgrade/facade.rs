//! Fixed offline93→97 trusted-host facade. No normal41 admission or SQL escape.
use super::*;

/// Explicit host acknowledgments, not proof that external descriptors are idle.
/// The host retains actual shutdown/join/owner-drain/server-lock guards through
/// completion, including cancellation of any async waiter.
pub struct OfflineControlShutdownFacts {
    pub service_admission_stopped: bool,
    pub native_workers_joined: bool,
    pub application_owners_drained: bool,
    pub exclusive_operational_ownership_retained: bool,
}
pub struct OfflineControlShutdownAcknowledgment {
    target: PathBuf,
    device: u64,
    inode: u64,
}
impl OfflineControlShutdownAcknowledgment {
    pub fn for_stopped_target(
        target: &Path,
        facts: OfflineControlShutdownFacts,
    ) -> anyhow::Result<Self> {
        ensure!(
            facts.service_admission_stopped
                && facts.native_workers_joined
                && facts.application_owners_drained
                && facts.exclusive_operational_ownership_retained,
            "explicit completed shutdown and retained exclusive ownership are required"
        );
        ensure!(
            target.is_absolute()
                && target.components().all(|c| !matches!(
                    c,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )),
            "offline control acknowledgment requires exact absolute target"
        );
        let metadata = std::fs::symlink_metadata(target)?;
        ensure!(
            metadata.is_file() && metadata.nlink() == 1,
            "shutdown target must be an existing regular single-link file"
        );
        Ok(Self {
            target: std::fs::canonicalize(target)?,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}
pub struct OfflineControlUpgrade {
    reference: UpgradeReference,
}
impl OfflineControlUpgrade {
    pub async fn prepare() -> anyhow::Result<Self> {
        Ok(Self {
            reference: UpgradeReference::build().await?,
        })
    }
    /// Fixed recorded conversion operation. The trusted host retains selected
    /// pair/reservation/service custody. Pair and operation IDs are descriptive
    /// linkage, never independent authority; original bytes and inspected facts
    /// are consumed and reverified, and additions are chosen only by this owner.
    #[allow(clippy::too_many_arguments)]
    pub fn upgrade_recorded(
        self,
        file: File,
        path: &Path,
        original: &mut File,
        inspected: &crate::motion_snapshot::image_read::InspectedImage,
        record_parent: File,
        operation_id: &str,
        selected_pair_sha256: &str,
        acknowledgment: OfflineControlShutdownAcknowledgment,
        retained_pair_bytes: u64,
        cancel: &AtomicBool,
    ) -> anyhow::Result<UpgradedControlStore> {
        self.prepare_recorded(
            file,
            path,
            original,
            inspected,
            record_parent,
            operation_id,
            selected_pair_sha256,
            acknowledgment,
            retained_pair_bytes,
            cancel,
        )?
        .apply(cancel)
    }
    /// Complete immutable input preparation before fresh target authorization.
    /// The returned owner retains the exact93 writer through consuming apply.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_recorded(
        self,
        file: File,
        path: &Path,
        original: &mut File,
        inspected: &crate::motion_snapshot::image_read::InspectedImage,
        record_parent: File,
        operation_id: &str,
        selected_pair_sha256: &str,
        acknowledgment: OfflineControlShutdownAcknowledgment,
        retained_pair_bytes: u64,
        cancel: &AtomicBool,
    ) -> anyhow::Result<PreparedRecordedControlOperation> {
        use std::io::{Seek, SeekFrom};
        ensure!(
            !operation_id.is_empty()
                && operation_id.len() <= 256
                && !operation_id.chars().any(char::is_control)
                && selected_pair_sha256.len() == 64
                && selected_pair_sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "recorded control linkage exceeds exact contract"
        );
        verify_shutdown_target(&file, path, &acknowledgment)?;
        check(cancel)?;
        original.seek(SeekFrom::Start(0))?;
        let original = super::original::capture(
            &self.reference,
            original,
            inspected,
            retained_pair_bytes,
            cancel,
        )?;
        let directory = super::record::RecordDirectory::from_retained(record_parent)?;
        let metadata = file.metadata()?;
        let anchor = super::record::RecordAnchor {
            operation_id: operation_id.to_owned(),
            selected_pair_sha256: selected_pair_sha256.to_owned(),
            original93_image_sha256: original.image_sha256().to_owned(),
            original93_inventory_sha256: original.inventory_sha256().to_owned(),
            stage_device: metadata.dev(),
            stage_inode: metadata.ino(),
            frozen_binding_sha256: original.frozen_binding_sha256().to_owned(),
        };
        Ok(PreparedRecordedControlOperation {
            owner: self.reference.prepare_recorded(
                file,
                path,
                original,
                directory,
                anchor,
                retained_pair_bytes,
                cancel,
            )?,
        })
    }
    #[allow(clippy::too_many_arguments)]
    pub fn upgrade(
        self,
        file: File,
        path: &Path,
        expected_image_sha256: &str,
        expected_frozen_binding_sha256: &str,
        acknowledgment: OfflineControlShutdownAcknowledgment,
        retained_pair_bytes: u64,
        cancel: &AtomicBool,
    ) -> anyhow::Result<UpgradedControlStore> {
        verify_shutdown_target(&file, path, &acknowledgment)?;
        check(cancel)?;
        Ok(UpgradedControlStore {
            owner: self.reference.upgrade(
                file,
                path,
                expected_image_sha256,
                expected_frozen_binding_sha256,
                retained_pair_bytes,
                cancel,
            )?,
        })
    }
}
fn verify_shutdown_target(
    file: &File,
    path: &Path,
    acknowledgment: &OfflineControlShutdownAcknowledgment,
) -> anyhow::Result<()> {
    ensure!(
        std::fs::canonicalize(path)? == acknowledgment.target,
        "offline control shutdown target differs"
    );
    let named = std::fs::symlink_metadata(path)?;
    let held = file.metadata()?;
    ensure!(
        named.is_file()
            && held.is_file()
            && named.nlink() == 1
            && held.nlink() == 1
            && (named.dev(), named.ino()) == (acknowledgment.device, acknowledgment.inode)
            && (held.dev(), held.ino()) == (acknowledgment.device, acknowledgment.inode),
        "offline control shutdown target identity differs"
    );
    Ok(())
}
/// Prepared recorded97 operation. Target policy reads borrow this owner and
/// cannot outlive its actual93 writer. It grants no native admission or install.
pub struct PreparedRecordedControlOperation {
    owner: super::PreparedRecordedControlUpgrade,
}
impl PreparedRecordedControlOperation {
    pub async fn authorization_reader(
        &mut self,
    ) -> anyhow::Result<crate::native_bootstrap::TargetAuthorizationReader<'_>> {
        crate::native_bootstrap::TargetAuthorizationReader::open_control(&mut self.owner).await
    }
    pub fn apply(self, cancel: &AtomicBool) -> anyhow::Result<UpgradedControlStore> {
        Ok(UpgradedControlStore {
            owner: self.owner.apply(cancel)?,
        })
    }
}
/// Fixed consuming close/phase handoff only. No mutable database escape.
pub struct UpgradedControlStore {
    pub(super) owner: AdmittedControlStore,
}
impl UpgradedControlStore {
    pub fn close(self) -> anyhow::Result<()> {
        let AdmittedControlStore {
            db,
            witness,
            binding: _,
        } = self.owner;
        witness.verify(false)?;
        ensure!(!db.is_autocommit(), "upgraded control writer missing");
        db.execute_batch("ROLLBACK")?;
        witness.verify(false)?;
        db.close().map_err(|(_, error)| anyhow::anyhow!(error))?;
        witness.verify(false)?;
        Ok(())
    }
}
/// Exact read-only ordered97/native40 compatibility under a retained writer.
/// Actual native owner admission remains the caller's independent obligation.
pub async fn verify_ordered97_in(db: &mut sqlx::SqliteConnection) -> anyhow::Result<()> {
    let reference = UpgradeReference::build().await?;
    let mut lease = db.lock_handle().await?;
    let handle = lease.as_raw_handle().as_ptr();
    // SAFETY: read-only introspection and lexical nonowning wrapper under SQLx's
    // exclusive lease. No await/reentry/thread move while the wrapper is live.
    ensure!(
        unsafe { ffi::sqlite3_get_autocommit(handle) } == 0
            && unsafe { ffi::sqlite3_txn_state(handle, c"main".as_ptr()) } == 2,
        "ordered compatibility requires retained WRITE transaction"
    );
    let connection = unsafe { Connection::from_handle(handle) }?;
    ensure!(
        reference.0.verify_in(&connection)? == ControlVersion::Complete97,
        "ordered compatibility requires exact complete97"
    );
    crate::source_freeze::verify_sync(&connection)?;
    let size: i64 = connection.query_row(
        "SELECT length(CAST(identity_json AS BLOB)) FROM catabolic_binding WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    ensure!((1..=65536).contains(&size), "ordered binding exceeds bound");
    let frozen: String = connection.query_row(
        "SELECT identity_json FROM catabolic_binding WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    let frozen_hash = catabolic_core::sha256(frozen.as_bytes());
    verify_binding(&connection, &frozen_hash)?;
    let identity: playscale_core::catalog_backend::Identity = serde_json::from_str(&frozen)?;
    let root = crate::native_upgrade::effective_identity_sync(&connection, &identity)?;
    let restore = crate::paired_restore::authenticated_transitions(&connection, &identity)?;
    let mut transitions = restore.transitions().to_vec();
    transitions.extend(crate::native_bootstrap::authenticated_transitions(
        &connection,
        &frozen_hash,
    )?);
    match playscale_core::catalog_identity_lineage::resolve(&root, &transitions)
        .map_err(|e| anyhow::anyhow!("ordered compatibility lineage: {e:?}"))?
    {
        playscale_core::catalog_identity_lineage::Resolution::Current { identity, .. } => ensure!(
            identity.schema_version == 40,
            "normal native41 compatibility is not admitted"
        ),
        playscale_core::catalog_identity_lineage::Resolution::Pending { .. } => {
            anyhow::bail!("ordered compatibility requires exact pending transition recovery")
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn stopped() -> OfflineControlShutdownFacts {
        OfflineControlShutdownFacts {
            service_admission_stopped: true,
            native_workers_joined: true,
            application_owners_drained: true,
            exclusive_operational_ownership_retained: true,
        }
    }
    #[tokio::test]
    async fn actual_closed_fixture_upgrades_through_fixed_facade_and_closes() {
        let (_root, path, binding) = super::super::tests::sealed().await;
        let hash = catabolic_core::sha256(std::fs::read(&path).unwrap());
        OfflineControlUpgrade::prepare()
            .await
            .unwrap()
            .upgrade(
                File::open(&path).unwrap(),
                &path,
                &hash,
                &binding,
                OfflineControlShutdownAcknowledgment::for_stopped_target(&path, stopped()).unwrap(),
                0,
                &AtomicBool::new(false),
            )
            .unwrap()
            .close()
            .unwrap();
        let db = Connection::open(&path).unwrap();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert_eq!(
            UpgradeReference::build()
                .await
                .unwrap()
                .0
                .verify_in(&db)
                .unwrap(),
            ControlVersion::Complete97
        );
        crate::source_freeze::verify_sync(&db).unwrap();
        assert_eq!(
            db.query_row("SELECT name FROM profiles WHERE id='retained'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "Original profile"
        );
        db.execute_batch("ROLLBACK").unwrap();
    }
    #[tokio::test]
    async fn foreign_target_acknowledgment_refuses_before_effects() {
        let (_root, path, binding) = super::super::tests::sealed().await;
        let other = tempfile::NamedTempFile::new().unwrap();
        let ack = OfflineControlShutdownAcknowledgment::for_stopped_target(other.path(), stopped())
            .unwrap();
        let hash = catabolic_core::sha256(std::fs::read(&path).unwrap());
        assert!(
            OfflineControlUpgrade::prepare()
                .await
                .unwrap()
                .upgrade(
                    File::open(&path).unwrap(),
                    &path,
                    &hash,
                    &binding,
                    ack,
                    0,
                    &AtomicBool::new(false)
                )
                .is_err()
        );
        assert_eq!(hash, catabolic_core::sha256(std::fs::read(&path).unwrap()));
    }
    #[tokio::test]
    async fn cancelled_facade_refuses_without_advancement() {
        let (_root, path, binding) = super::super::tests::sealed().await;
        let hash = catabolic_core::sha256(std::fs::read(&path).unwrap());
        let ack =
            OfflineControlShutdownAcknowledgment::for_stopped_target(&path, stopped()).unwrap();
        assert!(
            OfflineControlUpgrade::prepare()
                .await
                .unwrap()
                .upgrade(
                    File::open(&path).unwrap(),
                    &path,
                    &hash,
                    &binding,
                    ack,
                    0,
                    &AtomicBool::new(true)
                )
                .is_err()
        );
        assert_eq!(hash, catabolic_core::sha256(std::fs::read(&path).unwrap()));
    }
    #[tokio::test]
    async fn incomplete_shutdown_or_target_replacement_cannot_authorize_upgrade() {
        let (root, path, binding) = super::super::tests::sealed().await;
        let mut facts = stopped();
        facts.native_workers_joined = false;
        assert!(OfflineControlShutdownAcknowledgment::for_stopped_target(&path, facts).is_err());
        let ack =
            OfflineControlShutdownAcknowledgment::for_stopped_target(&path, stopped()).unwrap();
        let file = File::open(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let hash = catabolic_core::sha256(&bytes);
        std::fs::rename(&path, root.path().join("original")).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        assert!(
            OfflineControlUpgrade::prepare()
                .await
                .unwrap()
                .upgrade(
                    file,
                    &path,
                    &hash,
                    &binding,
                    ack,
                    0,
                    &AtomicBool::new(false)
                )
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    #[tokio::test]
    async fn ordered_validation_refuses_autocommit_and_read_transaction() {
        use sqlx::Connection as _;
        let mut db = sqlx::SqliteConnection::connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE retained(value INTEGER)")
            .execute(&mut db)
            .await
            .unwrap();
        assert!(
            verify_ordered97_in(&mut db)
                .await
                .unwrap_err()
                .to_string()
                .contains("requires retained WRITE transaction")
        );
        sqlx::query("BEGIN").execute(&mut db).await.unwrap();
        sqlx::query("SELECT * FROM retained")
            .fetch_all(&mut db)
            .await
            .unwrap();
        assert!(
            verify_ordered97_in(&mut db)
                .await
                .unwrap_err()
                .to_string()
                .contains("requires retained WRITE transaction")
        );
        sqlx::query("ROLLBACK").execute(&mut db).await.unwrap();
    }
    #[tokio::test]
    async fn public_recorded_upgrade_consumes_real_original93_and_persists_intent() {
        use std::io::Write;
        let (root, bytes, facts) = super::super::original::tests::original().await;
        let path = root.path().join("recorded-stage.sqlite");
        std::fs::write(&path, &bytes).unwrap();
        let file = File::options().read(true).write(true).open(&path).unwrap();
        let acknowledgment =
            OfflineControlShutdownAcknowledgment::for_stopped_target(&path, stopped()).unwrap();
        let mut original = tempfile::tempfile().unwrap();
        original.write_all(&bytes).unwrap();
        let mut prepared = OfflineControlUpgrade::prepare()
            .await
            .unwrap()
            .prepare_recorded(
                file,
                &path,
                &mut original,
                &facts,
                File::open(root.path()).unwrap(),
                "actual-recorded-facade",
                &"a".repeat(64),
                acknowledgment,
                0,
                &AtomicBool::new(false),
            )
            .unwrap();
        let record_path = root.path().join(super::super::record::FILE_NAME);
        assert!(!record_path.exists());
        let mut reader = prepared.authorization_reader().await.unwrap();
        let db = reader.authorize_connection().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT max(version) FROM _sqlx_migrations")
                .fetch_one(&mut *db)
                .await
                .unwrap(),
            93
        );
        assert!(
            sqlx::query("DELETE FROM profiles")
                .execute(&mut *db)
                .await
                .is_err()
        );
        assert!(sqlx::query("COMMIT").execute(&mut *db).await.is_err());
        reader.verify().await.unwrap();
        drop(reader);
        let competing = rusqlite::Connection::open(&path).unwrap();
        competing.busy_timeout(std::time::Duration::ZERO).unwrap();
        assert!(competing.execute_batch("BEGIN IMMEDIATE").is_err());
        drop(competing);
        assert!(!record_path.exists());
        prepared
            .apply(&AtomicBool::new(false))
            .unwrap()
            .close()
            .unwrap();
        let record = std::fs::read(root.path().join(super::super::record::FILE_NAME)).unwrap();
        let record: serde_json::Value = serde_json::from_slice(&record).unwrap();
        assert_eq!(record["anchor"]["original93_image_sha256"], facts.sha256());
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert_eq!(
            super::super::reference::ClosedReference::build()
                .await
                .unwrap()
                .verify_in(&db)
                .unwrap(),
            super::super::reference::ControlVersion::Complete97
        );
    }

    #[tokio::test]
    async fn public_recorded_upgrade_rejects_existing_record_without_stage_effects() {
        use std::io::Write;
        let (root, bytes, facts) = super::super::original::tests::original().await;
        let path = root.path().join("recorded-stage.sqlite");
        std::fs::write(&path, &bytes).unwrap();
        let record_path = root.path().join(super::super::record::FILE_NAME);
        std::fs::write(&record_path, b"foreign record").unwrap();
        let file = File::options().read(true).write(true).open(&path).unwrap();
        let acknowledgment =
            OfflineControlShutdownAcknowledgment::for_stopped_target(&path, stopped()).unwrap();
        let mut original = tempfile::tempfile().unwrap();
        original.write_all(&bytes).unwrap();
        assert!(
            OfflineControlUpgrade::prepare()
                .await
                .unwrap()
                .upgrade_recorded(
                    file,
                    &path,
                    &mut original,
                    &facts,
                    File::open(root.path()).unwrap(),
                    "actual-recorded-facade",
                    &"a".repeat(64),
                    acknowledgment,
                    0,
                    &AtomicBool::new(false),
                )
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::read(record_path).unwrap(), b"foreign record");
    }
}
