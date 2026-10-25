//! Consumed stopped-source/restored-copy conversion. Only independently proved
//! empty NFO obligations are supported. No final installation or ordinary41
//! Runtime is created, and initial stage hashes are not post-mutation evidence.
//! Recorded97 and native96 receive measured external reservations before long
//! phases. The native owner adds its actual retained data and operation charges.
//! This is logical accounting, not hard RSS. Subsequent94 actor/SQLx retention
//! and copied-image continuation still require connected refresh before claiming
//! an end-to-end converted-pair allocation bound.
//! Recorded97 preparation retains its actual writer through distinct target
//! authentication and final temporal checks before consuming apply. The original
//! operation deadline is retained across authorization, never restarted.
pub(in crate::native_service::pair) mod retained_native_apply;
pub(in crate::native_service::pair) mod retained_native_bootstrap;
pub(in crate::native_service::pair) mod retained_native_intent;
pub(in crate::native_service::pair) mod retained_source_relocation;

use super::*;
use crate::v2::auth::{
    Caller,
    retained::{CurrentSourceAuthentication, TargetAuthentication},
};
use motion_catalog_backend::{
    OfflineControlShutdownAcknowledgment, OfflineControlShutdownFacts, OfflineControlUpgrade,
};
use playscale_core::access::Permission;
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::os::unix::fs::OpenOptionsExt;

// Two independently bounded readers plus retained-source auth/schema growth
// and Rust metadata are precharged, not claimed as a process-wide RSS limit.
const AUTHORIZATION_RESERVE: u64 = 64 * 1024 * 1024;
fn conversion_reservation(source: u64, native: u64) -> anyhow::Result<u64> {
    let reserved = source
        .checked_add(native)
        .and_then(|n| n.checked_add(OVERHEAD))
        .and_then(|n| n.checked_add(AUTHORIZATION_RESERVE))
        // The same source writer later serves borrowed service reservation.
        // Admit its opening/close custody before that first retained acquisition.
        .and_then(|n| n.checked_add(motion_catalog_backend::motion_snapshot::FreshWriter::authorization_storage_reservation()))
        .context("conversion allocation overflow")?;
    ensure!(
        reserved <= 256 * 1024 * 1024,
        "conversion retained allocation cohort exceeded"
    );
    Ok(reserved)
}
async fn refreshed_reservation(
    current: &mut CurrentSourceAuthentication<'_>,
    pinned_native: u64,
) -> anyhow::Result<u64> {
    // The original native export is terminal and cannot grow through ordinary
    // work. Source writer/reader caches are remeasured; the reader is also inside
    // the conservative64MiB reserve, intentionally double-counted. Pinned bytes
    // also include selected-archive metadata when it differs from current. The facade
    // separately adds current target Motion connection and retained plan data.
    let source = current
        .retained_allocations()
        .await
        .map_err(|e| anyhow::anyhow!("current source allocation measurement: {e:?}"))?;
    conversion_reservation(source, pinned_native)
}
#[derive(Debug)]
struct CurrentConversionAuthorizationError(crate::v2::Problem);
impl std::fmt::Display for CurrentConversionAuthorizationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "current conversion authorization: {:?}", self.0)
    }
}
impl std::error::Error for CurrentConversionAuthorizationError {}
fn current_authorization_error(error: crate::v2::Problem) -> anyhow::Error {
    CurrentConversionAuthorizationError(error).into()
}
#[derive(Default)]
struct ConversionObservation {
    #[cfg(test)]
    expiry: Option<(i64, Arc<AtomicBool>)>,
}
impl ConversionObservation {
    async fn before_final_authorization(&self, target: &TargetAuthentication<'_>) {
        #[cfg(test)]
        if let Some((expiry, _seen)) = &self.expiry {
            let now = crate::now();
            assert!(
                now < *expiry,
                "actual credential must still be valid at prepared97 admission"
            );
            tokio::time::sleep(std::time::Duration::from_secs(
                u64::try_from(*expiry - now).unwrap(),
            ))
            .await;
            let error = target
                .revalidate_temporal()
                .err()
                .expect("real declared credential expiry must refuse");
            assert_eq!(error.status, axum::http::StatusCode::UNAUTHORIZED);
        }
        let _ = target;
    }
}
pub(super) struct SourceActorCustody {
    pub(super) session: motion_catalog_backend::SourceRelocationSession,
    pub(super) pool: SqlitePool,
    pub(super) stage: NativeConversionStage,
}
pub(in crate::native_service::pair) struct SourceConversionActor {
    pub(super) held: Option<SourceActorCustody>,
}
static ACTOR_RECOVERY: std::sync::OnceLock<std::sync::Mutex<Vec<SourceActorCustody>>> =
    std::sync::OnceLock::new();
impl Drop for SourceConversionActor {
    fn drop(&mut self) {
        if let Some(held) = self.held.take() {
            tracing::error!(
                "unfinished copied source conversion retains stage/native/source custody for recovery"
            );
            ACTOR_RECOVERY
                .get_or_init(Default::default)
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(held);
        }
    }
}
impl NativeConversionStage {
    pub(super) fn source_writer(&mut self) -> anyhow::Result<&mut FreshWriter> {
        Ok(&mut self
            .held
            .as_mut()
            .context("conversion custody missing")?
            .target
            .held
            .as_mut()
            .context("target custody missing")?
            .input
            .current
            ._writer)
    }
    pub(super) fn verify_phase_custody(&mut self) -> anyhow::Result<()> {
        let held = self.held.as_mut().context("conversion custody missing")?;
        let target = held
            .target
            .held
            .as_mut()
            .context("target custody missing")?;
        verify_target_lock(&target.parent, &target.lock)?;
        let opened = super::super::super::super::open_parent(&target.path)?;
        ensure!(
            identity(&opened)? == identity(&target.parent)?,
            "conversion target parent changed"
        );
        target.input.current._stopped.verify_lock()?;
        held.record
            .as_mut()
            .context("conversion record missing")?
            .read_verified(|_| Ok(()))?;
        held.motion_stage
            .as_ref()
            .context("Motion stage missing")?
            .verify_mutable_stage_identity()?;
        held.stage
            .as_ref()
            .context("native stage missing")?
            .verify_mutable_stage_identity()?;
        Ok(())
    }
    pub(in crate::native_service::pair) async fn into_source_actor(
        self,
        caller: Caller,
        flag: Arc<AtomicBool>,
    ) -> anyhow::Result<SourceConversionActor> {
        self.launch_source_conversion(caller, flag, ConversionObservation::default())
            .await
    }
    async fn launch_source_conversion(
        self,
        caller: Caller,
        flag: Arc<AtomicBool>,
        observation: ConversionObservation,
    ) -> anyhow::Result<SourceConversionActor> {
        let mut cancellation = PairCancellation(flag.clone(), true);
        let runtime = tokio::runtime::Handle::current();
        let result = tokio::task::spawn_blocking(move || {
            let mut stage = self;
            let input = &mut stage
                .held
                .as_mut()
                .context("conversion custody missing")?
                .target
                .held
                .as_mut()
                .context("target custody missing")?
                .input;
            let archive = input.archive_reservation();
            let pair = &mut input.current;
            // Measure before entering block_on: this counter itself leases SQLx
            // through the retained writer's runtime, without a nested runtime.
            let pinned_native = pair
                .retained_native_allocations()?
                .checked_add(archive)
                .context("selected archive reservation overflow")?;
            conversion_reservation(pair._writer.retained_allocations_blocking()?, pinned_native)?;
            runtime.block_on(convert(stage, caller, flag, pinned_native, observation))
        })
        .await
        .context("copied conversion owner panicked; durable stages require recovery")?;
        cancellation.1 = false;
        result
    }
}
fn exact_file(path: &Path, expected: &Evidence) -> anyhow::Result<File> {
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    verify_named_file(&file, path, expected)?;
    Ok(file)
}
/// Initial published bytes only, before native96 may legitimately mutate them.
/// Anonymous input validation still requires nlink0; this create-only published
/// stage instead requires the recorded held/named inode, nlink1 and original
/// exact bytes. Never use this initial commitment as post-migration evidence.
fn verify_initial_named_file(
    file: &mut File,
    path: &Path,
    expected: &Evidence,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    use std::os::unix::fs::FileExt;
    cancelled(cancel)?;
    verify_named_file(file, path, expected)?;
    let before = file.metadata()?;
    ensure!(
        before.len() == expected.artifact.bytes,
        "initial linked stage length differs"
    );
    let mut digest = Sha256::new();
    let mut offset = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        cancelled(cancel)?;
        let n = file.read_at(&mut buffer, offset)?;
        if n == 0 {
            break;
        }
        offset = offset
            .checked_add(n as u64)
            .context("initial linked stage size overflow")?;
        ensure!(
            offset <= expected.artifact.bytes,
            "initial linked stage grew"
        );
        digest.update(&buffer[..n]);
    }
    ensure!(
        offset == expected.artifact.bytes
            && format!("{:x}", digest.finalize()) == expected.artifact.sha256,
        "initial linked stage digest differs"
    );
    verify_named_file(file, path, expected)?;
    let after = file.metadata()?;
    ensure!(
        after.len() == before.len()
            && (
                after.mtime(),
                after.mtime_nsec(),
                after.ctime(),
                after.ctime_nsec()
            ) == (
                before.mtime(),
                before.mtime_nsec(),
                before.ctime(),
                before.ctime_nsec()
            ),
        "initial linked stage changed during verification"
    );
    cancelled(cancel)
}

fn verify_named_file(file: &File, path: &Path, expected: &Evidence) -> anyhow::Result<()> {
    let held = file.metadata()?;
    let named = std::fs::symlink_metadata(path)?;
    ensure!(
        held.is_file()
            && named.is_file()
            && held.nlink() == 1
            && named.nlink() == 1
            && (held.dev(), held.ino()) == (expected.file_device, expected.file_inode)
            && (named.dev(), named.ino()) == (expected.file_device, expected.file_inode),
        "copied conversion stage identity differs"
    );
    Ok(())
}
async fn target_pool(path: &Path) -> anyhow::Result<SqlitePool> {
    Ok(SqlitePoolOptions::new()
        .max_connections(1)
        .after_connect(|db, _| {
            Box::pin(motion_catalog_backend::projection::configure_connection(db))
        })
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(false)
                .foreign_keys(true),
        )
        .await?)
}
async fn authorize_next_effect(
    reader: motion_catalog_backend::TargetAuthorizationReader<'_>,
    caller: &Caller,
    current: &mut CurrentSourceAuthentication<'_>,
    flag: &AtomicBool,
) -> anyhow::Result<()> {
    authorize_next_effect_observed(
        reader,
        caller,
        current,
        flag,
        &ConversionObservation::default(),
    )
    .await
}
async fn authorize_next_effect_observed(
    reader: motion_catalog_backend::TargetAuthorizationReader<'_>,
    caller: &Caller,
    current: &mut CurrentSourceAuthentication<'_>,
    flag: &AtomicBool,
    observation: &ConversionObservation,
) -> anyhow::Result<()> {
    let mut target = TargetAuthentication::prepare(reader, caller, Permission::SystemAdmin)
        .await
        .map_err(|e| anyhow::anyhow!("target pre-effect authorization: {e:?}"))?;
    observation.before_final_authorization(&target).await;
    let validation = target.revalidate().await;
    #[cfg(test)]
    if let Some((_, seen)) = &observation.expiry {
        let error = validation
            .as_ref()
            .expect_err("normal target verification must refuse expired authority");
        assert_eq!(
            error.status,
            axum::http::StatusCode::UNAUTHORIZED,
            "deadline/owner failure is not expiry evidence"
        );
        seen.store(true, Ordering::Release);
    }
    validation.map_err(|e| anyhow::anyhow!("target pre-effect authorization: {e:?}"))?;
    current
        .revalidate()
        .await
        .map_err(current_authorization_error)?;
    cancelled(flag)?;
    target
        .revalidate_temporal()
        .map_err(|e| anyhow::anyhow!("target pre-effect temporal authorization: {e:?}"))?;
    current
        .revalidate_temporal()
        .map_err(current_authorization_error)?;
    Ok(())
}
async fn convert(
    mut stage: NativeConversionStage,
    caller: Caller,
    flag: Arc<AtomicBool>,
    pinned_native: u64,
    observation: ConversionObservation,
) -> anyhow::Result<SourceConversionActor> {
    cancelled(&flag)?;
    stage.verify_phase_custody()?;
    stage
        .held
        .as_mut()
        .context("conversion missing")?
        .target
        .held
        .as_mut()
        .context("target missing")?
        .input
        .verify_archive(&flag)?;
    let (motion_path, native_path, motion_evidence, native_evidence, transition, operation) = {
        let held = stage.held.as_mut().context("conversion missing")?;
        held.motion_stage
            .as_mut()
            .context("Motion stage missing")?
            .read_verified(|_| Ok(()))?;
        held.stage
            .as_mut()
            .context("native stage missing")?
            .read_verified(|_| Ok(()))?;
        let target = held.target.held.as_mut().context("target missing")?;
        let motion = held.motion_stage.as_ref().unwrap().evidence().clone();
        let native = held.stage.as_ref().unwrap().evidence().clone();
        (
            target.path.join(&motion.name),
            target.path.join(&native.name),
            motion,
            native,
            format!("paired-upgrade-{}", target.install_id),
            format!("paired-bootstrap-{}", target.install_id),
        )
    };
    let upgrade = OfflineControlUpgrade::prepare().await?;
    let target_custody = stage
        .held
        .as_mut()
        .context("conversion custody missing")?
        .target
        .held
        .as_mut()
        .context("target custody missing")?;
    let record_parent = target_custody.parent.try_clone()?;
    let selected_pair = target_custody.input.pair_sha256().to_owned();
    // Disjoint fields retain original inspected bytes while the live source
    // writer is exclusively borrowed for the entire conversion authorization.
    let input = &mut target_custody.input;
    let original = &mut input.motion;
    let inspected = &input._motion_facts;
    let source_reader = input.current._writer.legacy_authorization_reader().await?;
    let mut current = CurrentSourceAuthentication::prepare_legacy(
        source_reader,
        &caller,
        Permission::SystemAdmin,
    )
    .await
    .map_err(current_authorization_error)?;
    current
        .revalidate()
        .await
        .map_err(current_authorization_error)?;
    cancelled(&flag)?;
    let file = exact_file(&motion_path, &motion_evidence)?;
    let reserved = refreshed_reservation(&mut current, pinned_native).await?;
    let mut prepared_control = upgrade.prepare_recorded(
        file,
        &motion_path,
        original,
        inspected,
        record_parent,
        &transition,
        &selected_pair,
        OfflineControlShutdownAcknowledgment::for_stopped_target(
            &motion_path,
            OfflineControlShutdownFacts {
                service_admission_stopped: true,
                native_workers_joined: true,
                application_owners_drained: true,
                exclusive_operational_ownership_retained: true,
            },
        )?,
        reserved,
        &flag,
    )?;
    authorize_next_effect_observed(
        prepared_control.authorization_reader().await?,
        &caller,
        &mut current,
        &flag,
        &observation,
    )
    .await?;
    let upgraded = prepared_control.apply(&flag)?;
    current
        .revalidate()
        .await
        .map_err(current_authorization_error)?;
    cancelled(&flag)?;
    let mut native_file = exact_file(&native_path, &native_evidence)?;
    verify_initial_named_file(&mut native_file, &native_path, &native_evidence, &flag)?;
    let reserved = refreshed_reservation(&mut current, pinned_native).await?;
    let mut prepared = upgraded.prepare_native_reserved(
        &native_path,
        catabolic_store::migration::NativeUpgradeQuiescence {
            admission_stopped: true,
            readers_and_workers_stopped: true,
            exclusive_operational_ownership: true,
        },
        &transition,
        reserved,
    )?;
    prepared
        .refresh_external_reservation(refreshed_reservation(&mut current, pinned_native).await?)?;
    authorize_next_effect(
        prepared.authorization_reader().await?,
        &caller,
        &mut current,
        &flag,
    )
    .await?;
    let mut pending = prepared.apply(None)?;
    pending
        .refresh_external_reservation(refreshed_reservation(&mut current, pinned_native).await?)?;
    authorize_next_effect(
        pending.authorization_reader().await?,
        &caller,
        &mut current,
        &flag,
    )
    .await?;
    verify_named_file(&native_file, &native_path, &native_evidence)?;
    // This is not an assumed absence. Native preparation independently measures
    // its complete NFO source/tag/origin obligations and refuses nonempty inputs.
    let mut pending = pending.save_command(&operation, &[])?;
    current
        .revalidate()
        .await
        .map_err(current_authorization_error)?;
    cancelled(&flag)?;
    pending
        .refresh_external_reservation(refreshed_reservation(&mut current, pinned_native).await?)?;
    authorize_next_effect(
        pending.authorization_reader().await?,
        &caller,
        &mut current,
        &flag,
    )
    .await?;
    let mut executed = pending.execute_unacknowledged(
        &[],
        catabolic_store::catalog_commands::ReceiptPolicy {
            retention: std::time::Duration::from_secs(3600),
            max_receipts: 100,
            max_hold_records: 100,
            max_logical_bytes: 16 * 1024 * 1024,
        },
    )?;
    // Native effect completion does not write the Motion ACK. Both independent
    // current-source and target temporal policies are checked after actual work.
    executed
        .refresh_external_reservation(refreshed_reservation(&mut current, pinned_native).await?)?;
    let mut target = TargetAuthentication::prepare(
        executed.authorization_reader().await?,
        &caller,
        Permission::SystemAdmin,
    )
    .await
    .map_err(|e| anyhow::anyhow!("target completed conversion authorization: {e:?}"))?;
    target
        .revalidate()
        .await
        .map_err(|e| anyhow::anyhow!("target completed conversion authorization: {e:?}"))?;
    current
        .revalidate()
        .await
        .map_err(current_authorization_error)?;
    cancelled(&flag)?;
    verify_named_file(&native_file, &native_path, &native_evidence)?;
    target
        .revalidate_temporal()
        .map_err(|e| anyhow::anyhow!("target completed temporal authorization: {e:?}"))?;
    current
        .revalidate_temporal()
        .map_err(current_authorization_error)?;
    drop(target);
    let mut acknowledged = executed.acknowledge()?;
    acknowledged
        .refresh_external_reservation(refreshed_reservation(&mut current, pinned_native).await?)?;
    let session = acknowledged.into_source_session().await?;
    drop(current);
    stage.verify_phase_custody()?;
    stage
        .held
        .as_mut()
        .context("conversion missing")?
        .target
        .held
        .as_mut()
        .context("target missing")?
        .input
        .verify_archive(&flag)?;
    cancelled(&flag)?;
    let pool = target_pool(&motion_path).await?;
    if let Err(error) = stage.verify_phase_custody() {
        pool.close().await;
        return Err(error);
    }
    Ok(SourceConversionActor {
        held: Some(SourceActorCustody {
            session,
            pool,
            stage,
        }),
    })
}

#[cfg(test)]
impl SourceConversionActor {
    pub(in crate::native_service::pair) async fn exercise_converted_fixture(mut self) {
        let held = self.held.as_mut().unwrap();
        held.stage.verify_phase_custody().unwrap();
        assert_eq!(held.session.identity().schema_version, 41);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT max(version) FROM _sqlx_migrations")
                .fetch_one(&held.pool)
                .await
                .unwrap(),
            97
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM catabolic_native_bootstrap_acks")
                .fetch_one(&held.pool)
                .await
                .unwrap(),
            1
        );
        let target = held
            .stage
            .held
            .as_ref()
            .unwrap()
            .target
            .held
            .as_ref()
            .unwrap();
        assert_eq!(target.input.manifest.native_identity.schema_version, 40);
        assert_eq!(target.input.manifest.motion_schema_version, 93);
        let pair = &target.input.current;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT max(version) FROM _sqlx_migrations")
                .fetch_one(&pair._stopped.source_db().unwrap())
                .await
                .unwrap(),
            93
        );
        assert!(!target.path.join("playscale.sqlite3").exists());
        assert!(!target.path.join("catabolic.sqlite3").exists());
        // Test-only complete cleanup; production abandonment retains recovery custody.
        let SourceActorCustody {
            session,
            pool,
            mut stage,
        } = self.held.take().unwrap();
        pool.close().await;
        session.close().unwrap();
        drop(stage.held.take());
    }
}

#[cfg(test)]
mod accounting_tests {
    use super::*;
    #[test]
    fn initial_linked_stage_requires_exact_recorded_bytes_and_named_inode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("native-stage.sqlite3");
        let bytes = b"genuine initially published stage bytes";
        std::fs::write(&path, bytes).unwrap();
        let stat = std::fs::metadata(&path).unwrap();
        let parent = std::fs::metadata(dir.path()).unwrap();
        let evidence = Evidence {
            parent_device: parent.dev(),
            parent_inode: parent.ino(),
            file_device: stat.dev(),
            file_inode: stat.ino(),
            name: "native-stage.sqlite3".into(),
            artifact: Artifact {
                bytes: bytes.len() as u64,
                sha256: format!("{:x}", Sha256::digest(bytes)),
            },
        };
        let mut file = exact_file(&path, &evidence).unwrap();
        let flag = AtomicBool::new(false);
        let parent_fd = File::open(dir.path()).unwrap();
        let mut anonymous_file = anonymous(&parent_fd).unwrap();
        anonymous_file.write_all(bytes).unwrap();
        assert_eq!(anonymous_file.metadata().unwrap().nlink(), 0);
        assert!(verify_initial_named_file(&mut anonymous_file, &path, &evidence, &flag).is_err());
        verify_file(&mut anonymous_file, &evidence.artifact, &flag).unwrap();
        verify_initial_named_file(&mut file, &path, &evidence, &flag).unwrap();
        assert!(
            verify_file(&mut file, &evidence.artifact, &flag).is_err(),
            "anonymous input contract must remain unchanged"
        );
        assert!(
            verify_initial_named_file(&mut file, &path, &evidence, &AtomicBool::new(true)).is_err()
        );
        let alias = dir.path().join("alias");
        std::fs::hard_link(&path, &alias).unwrap();
        assert!(verify_initial_named_file(&mut file, &path, &evidence, &flag).is_err());
        std::fs::remove_file(alias).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
        assert!(verify_initial_named_file(&mut file, &path, &evidence, &flag).is_err());
        std::fs::write(&path, vec![b'x'; bytes.len()]).unwrap();
        assert!(verify_initial_named_file(&mut file, &path, &evidence, &flag).is_err());
        std::fs::write(&path, bytes).unwrap();
        verify_initial_named_file(&mut file, &path, &evidence, &flag).unwrap();
        std::fs::rename(&path, dir.path().join("original")).unwrap();
        std::fs::write(&path, bytes).unwrap();
        assert!(verify_initial_named_file(&mut file, &path, &evidence, &flag).is_err());
    }
    #[test]
    fn conversion_precharges_readers_and_refuses_checked_overflow() {
        assert_eq!(
            conversion_reservation(0, 0).unwrap(),
            OVERHEAD + AUTHORIZATION_RESERVE
                + motion_catalog_backend::motion_snapshot::FreshWriter::authorization_storage_reservation()
        );
        assert!(conversion_reservation(u64::MAX, 1).is_err());
        assert!(conversion_reservation(256 * 1024 * 1024, 0).is_err());
    }
}

#[cfg(test)]
impl NativeConversionStage {
    pub(in crate::native_service::pair) async fn exercise_actual_prepared_expiry(
        self,
        caller: Caller,
        expiry: i64,
    ) {
        let evidence = self
            .held
            .as_ref()
            .unwrap()
            .record
            .as_ref()
            .unwrap()
            .evidence()
            .clone();
        let seen = Arc::new(AtomicBool::new(false));
        let result = self
            .launch_source_conversion(
                caller,
                Arc::new(AtomicBool::new(false)),
                ConversionObservation {
                    expiry: Some((expiry, seen.clone())),
                },
            )
            .await;
        assert!(
            result.is_err(),
            "expired authority cannot consume prepared97"
        );
        assert!(
            seen.load(Ordering::Acquire),
            "genuine target was admitted then expired"
        );
        Self::assert_no_effects_and_close(&evidence).await;
    }
    pub(in crate::native_service::pair) async fn exercise_actual_current_revocation(
        mut self,
        caller: Caller,
    ) {
        self.verify_phase_custody().unwrap();
        let held = self.held.as_ref().unwrap();
        let evidence = held.record.as_ref().unwrap().evidence().clone();
        let path = held
            .target
            .held
            .as_ref()
            .unwrap()
            .path
            .join(&held.motion_stage.as_ref().unwrap().evidence().name);
        // Archived permission is real and remains granted. It must never
        // substitute for the separately retained current source credential.
        let pool = target_pool(&path).await.unwrap();
        let mut writer = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        caller
            .reauthorize(&mut writer, Some(Permission::SystemAdmin))
            .await
            .unwrap();
        writer.rollback().await.unwrap();
        pool.close().await;
        let error = self
            .into_source_actor(caller, Arc::new(AtomicBool::new(false)))
            .await
            .err()
            .expect("current revoked source must refuse historical target grant");
        let denied = error
            .downcast_ref::<CurrentConversionAuthorizationError>()
            .expect("refusal must originate from exact current-source authorization");
        assert_eq!(denied.0.status, axum::http::StatusCode::UNAUTHORIZED);
        Self::assert_no_effects_and_close(&evidence).await;
    }
    async fn assert_no_effects_and_close(evidence: &Evidence) {
        let mut held = {
            let mut recovery = super::RECOVERY.get().unwrap().lock().unwrap();
            let index = recovery
                .iter()
                .position(|h| h.record.as_ref().is_some_and(|r| r.evidence() == evidence))
                .expect("failed conversion retains exact stage custody");
            recovery.remove(index)
        };
        let target = held.target.held.as_ref().unwrap();
        let path = target
            .path
            .join(&held.motion_stage.as_ref().unwrap().evidence().name);
        let pool = target_pool(&path).await.unwrap();
        let version: i64 = sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(version, 93);
        let extras:i64=sqlx::query_scalar("SELECT count(*) FROM sqlite_schema WHERE name IN ('catabolic_paired_restore_intents','catabolic_paired_restore_acks','catabolic_native_bootstrap_intents','catabolic_native_bootstrap_commands','catabolic_native_bootstrap_acks','catabolic_source_relocation_admissions','catabolic_source_relocation_commands','catabolic_source_relocation_acks','preview_sheet_artifacts')").fetch_one(&pool).await.unwrap();
        assert_eq!(extras, 0, "no94-97 DDL or ACK is committed");
        pool.close().await;
        held.stage
            .as_mut()
            .unwrap()
            .read_verified(|_| Ok(()))
            .unwrap();
        assert!(!target.path.join("playscale.sqlite3").exists());
        assert!(!target.path.join("catabolic.sqlite3").exists());
        // Fixture-only cleanup after observing exact quarantined custody and
        // unchanged native bytes. Join actual close, not asynchronous Drop timing.
        // Production exposes no removal/retry shortcut.
        let target = held.target.held.take().unwrap();
        let PreparedPair {
            _writer,
            native,
            motion,
            parent,
            _stopped,
            ..
        } = target.input.current;
        drop(_writer);
        drop(native);
        drop(motion);
        drop(parent);
        _stopped.close().await.unwrap();
        drop(held);
    }
}

#[cfg(test)]
impl SourceConversionActor {
    /// Genuine converted-target policy mutation, not historical/live-source
    /// asymmetry: both paired credentials existed before the immutable capture.
    pub(in crate::native_service::pair) async fn exercise_actual_target_policy_denial(
        mut self,
        requester: Caller,
        reserve_admin: Caller,
        source_id: String,
        candidate: PathBuf,
    ) {
        async fn verify_absent_pair(
            held: &mut SourceActorCustody,
            caller: &Caller,
            expected: &motion_catalog_backend::source_lifecycle::LifecycleProof,
            candidate: &Path,
        ) {
            let mut writer = held.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
            let principal = caller
                .reauthorize(&mut writer, Some(Permission::SourcesManage))
                .await
                .unwrap();
            let requester = motion_catalog_backend::SourceRelocationRequester::from_current_host(
                principal.id.clone(),
                caller.credential().map(|(hash, _)| hash.to_owned()),
                caller.ingress_device_id().map(str::to_owned),
                principal,
            )
            .unwrap();
            // Fixed descriptive lookup repeats the real backend pair proof and
            // rolls back this writer. No intent or native dispatch is permitted.
            assert!(
                held.session
                    .recorded_command(
                        writer,
                        "target-policy-pair-proof",
                        expected,
                        &requester,
                        candidate
                    )
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        let evidence;
        let expected;
        {
            let held = self.held.as_mut().unwrap();
            held.stage.verify_phase_custody().unwrap();
            evidence = held
                .stage
                .held
                .as_ref()
                .unwrap()
                .record
                .as_ref()
                .unwrap()
                .evidence()
                .clone();
            let mut writer = held.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
            expected = motion_catalog_backend::source_lifecycle::require_active_in(
                &mut writer,
                &source_id,
            )
            .await
            .unwrap();
            writer.rollback().await.unwrap();
            verify_absent_pair(held, &reserve_admin, &expected, &candidate).await;
            let mut writer = held.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
            crate::v2::auth::retained::deny_target_source_management_in(
                &mut writer,
                &reserve_admin,
                &requester,
            )
            .await
            .unwrap();
            writer.commit().await.unwrap();
            verify_absent_pair(held, &reserve_admin, &expected, &candidate).await;
            // Initial hashes stay immutable input evidence. A legitimate policy
            // owner changes current target bytes without replacing its inode.
            held.stage.verify_phase_custody().unwrap();
            let reader = held
                .stage
                .source_writer()
                .unwrap()
                .legacy_authorization_reader()
                .await
                .unwrap();
            let mut current = CurrentSourceAuthentication::prepare_legacy(
                reader,
                &requester,
                Permission::SourcesManage,
            )
            .await
            .unwrap();
            current.revalidate().await.unwrap();
            drop(current);
        }
        let error = self
            .relocate(
                requester.clone(),
                "actual-target-policy-denied".into(),
                expected.clone(),
                candidate,
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .err()
            .expect("current source permission must not substitute for target policy");
        assert!(
            super::source_actor::is_forbidden_authorization(&error),
            "actual target authorization must return typed403: {error:#}"
        );
        let mut held = {
            let mut recovery = ACTOR_RECOVERY.get().unwrap().lock().unwrap();
            let index = recovery
                .iter()
                .position(|h| {
                    h.stage.held.as_ref().is_some_and(|s| {
                        s.record.as_ref().is_some_and(|r| r.evidence() == &evidence)
                    })
                })
                .expect("denied actor retains exact complete custody");
            recovery.remove(index)
        };
        held.stage.verify_phase_custody().unwrap();
        let mut writer = held.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        for table in [
            "catabolic_source_relocation_admissions",
            "catabolic_source_relocation_commands",
            "catabolic_source_relocation_acks",
        ] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
                .fetch_one(&mut *writer)
                .await
                .unwrap();
            assert_eq!(count, 0, "target denial must precede every94 effect");
        }
        let actual =
            motion_catalog_backend::source_lifecycle::require_active_in(&mut writer, &source_id)
                .await
                .unwrap();
        assert_eq!(actual, expected);
        writer.rollback().await.unwrap();
        let reader = held
            .stage
            .source_writer()
            .unwrap()
            .legacy_authorization_reader()
            .await
            .unwrap();
        CurrentSourceAuthentication::prepare_legacy(reader, &requester, Permission::SourcesManage)
            .await
            .unwrap()
            .revalidate()
            .await
            .unwrap();
        // Test-only exact quarantine settlement, never a production retry API.
        SourceConversionActor { held: Some(held) }
            .exercise_converted_fixture()
            .await;
    }
}
