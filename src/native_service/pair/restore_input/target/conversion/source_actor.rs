//! Fixed staged94 operations. The actor comes only from consumed copied-pair
//! conversion. It remains nonactivatable until the separate install/recovery
//! owner is complete. No request can supply a pool, native session or stage.
use super::*;
use crate::v2::auth::{
    Caller,
    retained::{AuthenticationSnapshot, CurrentSourceAuthentication},
};
use motion_catalog_backend::{SourceRelocationRequester, source_lifecycle::LifecycleProof};
use playscale_core::access::{Permission, Principal};
use std::path::PathBuf;

fn requester(caller: &Caller, principal: Principal) -> anyhow::Result<SourceRelocationRequester> {
    SourceRelocationRequester::from_current_host(
        principal.id.clone(),
        caller.credential().map(|(hash, _)| hash.to_owned()),
        caller.ingress_device_id().map(str::to_owned),
        principal,
    )
}
#[derive(Debug)]
struct AuthorizationError(crate::v2::Problem);
impl std::fmt::Display for AuthorizationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "staged source relocation authorization: {:?}", self.0)
    }
}
impl std::error::Error for AuthorizationError {}
fn auth_error(error: crate::v2::Problem) -> anyhow::Error {
    AuthorizationError(error).into()
}
#[cfg(test)]
pub(super) fn is_forbidden_authorization(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<AuthorizationError>()
        .is_some_and(|error| error.0.status == axum::http::StatusCode::FORBIDDEN)
}
enum Action {
    Relocate {
        expected: LifecycleProof,
        candidate: PathBuf,
    },
    ExistingReceipt,
}
impl SourceConversionActor {
    pub(in crate::native_service::pair) async fn relocate(
        self,
        caller: Caller,
        operation: String,
        expected: LifecycleProof,
        candidate: PathBuf,
        cancel: Arc<AtomicBool>,
    ) -> anyhow::Result<(Self, LifecycleProof)> {
        self.run_owned(
            caller,
            operation,
            Action::Relocate {
                expected,
                candidate,
            },
            cancel,
        )
        .await
    }
    /// Receipt-only recovery cannot execute an absent operation or reconstruct
    /// an intent from current roots. The original immutable intent is required.
    pub(in crate::native_service::pair) async fn reconcile_existing_receipt(
        self,
        caller: Caller,
        operation: String,
        cancel: Arc<AtomicBool>,
    ) -> anyhow::Result<(Self, LifecycleProof)> {
        self.run_owned(caller, operation, Action::ExistingReceipt, cancel)
            .await
    }
    async fn run_owned(
        self,
        caller: Caller,
        operation: String,
        action: Action,
        flag: Arc<AtomicBool>,
    ) -> anyhow::Result<(Self, LifecycleProof)> {
        let mut cancellation = PairCancellation(flag.clone(), true);
        let runtime = tokio::runtime::Handle::current();
        // Dropping the waiter signals cancellation but cannot drop the source
        // writer, stage locks, drained owners or native session mid-effect.
        // Failure or a lost successful reply drops the actor into its existing
        // whole-custody recovery quarantine, never claims an idle/closed owner.
        let result = tokio::task::spawn_blocking(move || {
            runtime.block_on(run(self, caller, operation, action, flag))
        })
        .await
        .context("staged source relocation owner panicked; retained recovery is required")?;
        cancellation.1 = false;
        result
    }
}
async fn run(
    mut actor: SourceConversionActor,
    caller: Caller,
    operation: String,
    action: Action,
    cancel: Arc<AtomicBool>,
) -> anyhow::Result<(SourceConversionActor, LifecycleProof)> {
    cancelled(&cancel)?;
    let held = actor
        .held
        .as_mut()
        .context("source actor custody missing")?;
    let SourceActorCustody {
        session,
        pool,
        stage,
    } = held;
    stage.verify_phase_custody()?;
    let reader = stage.source_writer()?.legacy_authorization_reader().await?;
    let mut current =
        CurrentSourceAuthentication::prepare_legacy(reader, &caller, Permission::SourcesManage)
            .await
            .map_err(auth_error)?;

    if let Action::Relocate {
        expected,
        candidate,
    } = action
    {
        let mut writer = pool.begin_with("BEGIN IMMEDIATE").await?;
        let target = authorize_pair(&mut writer, &caller, &mut current, &cancel).await?;
        // This fixed lookup checks exact original requester/CAS/candidate and
        // rolls back its read writer. A retry never rewrites saved admission.
        let saved = session
            .recorded_command(writer, &operation, &expected, &target, &candidate)
            .await?;
        if saved.is_none() {
            let mut writer = pool.begin_with("BEGIN IMMEDIATE").await?;
            let target = authorize_pair(&mut writer, &caller, &mut current, &cancel).await?;
            session
                .commit_new_intent(writer, &operation, &expected, &target, &candidate, &cancel)
                .await?;
        }
        let mut writer = pool.begin_with("BEGIN IMMEDIATE").await?;
        let target = authorize_pair(&mut writer, &caller, &mut current, &cancel).await?;
        let phase = session
            .dispatch_recorded(
                writer,
                &operation,
                &target,
                catabolic_store::catalog_commands::ReceiptPolicy {
                    retention: std::time::Duration::from_secs(3600),
                    max_receipts: 100,
                    max_hold_records: 100,
                    max_logical_bytes: 16 * 1024 * 1024,
                },
                &cancel,
            )
            .await?;
        let proof = finish(phase, &caller, &mut current, &cancel).await?;
        drop(current);
        stage.verify_phase_custody()?;
        Ok((actor, proof))
    } else {
        let mut writer = pool.begin_with("BEGIN IMMEDIATE").await?;
        let target = authorize_pair(&mut writer, &caller, &mut current, &cancel).await?;
        let phase = session
            .prepare_existing_receipt(writer, &operation, &target)
            .await?;
        let proof = finish(phase, &caller, &mut current, &cancel).await?;
        drop(current);
        stage.verify_phase_custody()?;
        Ok((actor, proof))
    }
}
async fn finish(
    mut phase: motion_catalog_backend::DispatchedSourceRelocation<'_>,
    caller: &Caller,
    current: &mut CurrentSourceAuthentication<'_>,
    cancel: &AtomicBool,
) -> anyhow::Result<LifecycleProof> {
    // SAME target writer and exact command/receipt remain inside this opaque
    // phase. Current live-source and distinct target policy must both still
    // authorize after native work, before the permanent Motion ACK.
    let target = authorize_pair(phase.writer_in(), caller, current, cancel).await?;
    phase.commit_existing_receipt_ack(&target).await
}
async fn authorize_pair(
    writer: &mut sqlx::SqliteConnection,
    caller: &Caller,
    current: &mut CurrentSourceAuthentication<'_>,
    cancel: &AtomicBool,
) -> anyhow::Result<SourceRelocationRequester> {
    let principal = caller
        .reauthorize(writer, Some(Permission::SourcesManage))
        .await
        .map_err(auth_error)?;
    let temporal =
        AuthenticationSnapshot::prepare_in(writer, caller, Permission::SourcesManage, &principal)
            .await
            .map_err(auth_error)?;
    current.revalidate().await.map_err(auth_error)?;
    // Both actual owners have now been checked. Recheck both credentials at
    // current time without another IO wait before the next fixed effect/ACK.
    temporal.revalidate().map_err(auth_error)?;
    current.revalidate_temporal().map_err(auth_error)?;
    cancelled(cancel)?;
    requester(caller, principal)
}

#[cfg(test)]
impl SourceConversionActor {
    /// Called only by the real stopped-service/public-source/copied-pair fixture.
    /// Native dispatch is real; dropping its returned phase deliberately models
    /// a lost Motion ACK while retaining the actual native receipt and owners.
    pub(in crate::native_service::pair) async fn exercise_actual_relocation_fixture(
        mut self,
        caller: Caller,
        source_id: String,
        candidate: PathBuf,
    ) -> Self {
        let operation = "actual-copied-source-relocation".to_owned();
        let cancel = Arc::new(AtomicBool::new(false));
        let expected;
        {
            let SourceActorCustody {
                session,
                pool,
                stage,
            } = self.held.as_mut().unwrap();
            stage.verify_phase_custody().unwrap();
            let reader = stage
                .source_writer()
                .unwrap()
                .legacy_authorization_reader()
                .await
                .unwrap();
            let mut current = CurrentSourceAuthentication::prepare_legacy(
                reader,
                &caller,
                Permission::SourcesManage,
            )
            .await
            .unwrap();
            let mut writer = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
            let target = authorize_pair(&mut writer, &caller, &mut current, &cancel)
                .await
                .unwrap();
            let absent = session
                .prepare_existing_receipt(writer, &operation, &target)
                .await;
            assert!(
                absent
                    .as_ref()
                    .err()
                    .is_some_and(|e| e.to_string().contains("committed original intent")),
                "receipt-only recovery must refuse absent intent without dispatch"
            );
            drop(absent);
            let mut writer = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
            expected = motion_catalog_backend::source_lifecycle::require_active_in(
                &mut writer,
                &source_id,
            )
            .await
            .unwrap();
            let target = authorize_pair(&mut writer, &caller, &mut current, &cancel)
                .await
                .unwrap();
            session
                .commit_new_intent(writer, &operation, &expected, &target, &candidate, &cancel)
                .await
                .unwrap();
            let mut writer = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
            let target = authorize_pair(&mut writer, &caller, &mut current, &cancel)
                .await
                .unwrap();
            let phase = session
                .dispatch_recorded(
                    writer,
                    &operation,
                    &target,
                    catabolic_store::catalog_commands::ReceiptPolicy {
                        retention: std::time::Duration::from_secs(3600),
                        max_receipts: 100,
                        max_hold_records: 100,
                        max_logical_bytes: 16 * 1024 * 1024,
                    },
                    &cancel,
                )
                .await
                .unwrap();
            drop(phase);
            drop(current);
        }
        let pending: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM catabolic_source_relocation_acks WHERE operation_id=?",
        )
        .bind(&operation)
        .fetch_one(&self.held.as_ref().unwrap().pool)
        .await
        .unwrap();
        assert_eq!(
            pending, 0,
            "real native receipt has not been ACKed by Motion"
        );
        let (actor, recovered) = self
            .reconcile_existing_receipt(caller.clone(), operation.clone(), cancel.clone())
            .await
            .unwrap();
        self = actor;
        assert_eq!(recovered.source_id, source_id);
        assert!(recovered.epoch > expected.epoch);
        assert!(recovered.source_revision > expected.source_revision);
        let (actor, replay) = self
            .relocate(
                caller,
                operation.clone(),
                expected,
                candidate.clone(),
                cancel,
            )
            .await
            .unwrap();
        self = actor;
        assert_eq!(
            replay, recovered,
            "exact replay must not renew the source again"
        );
        let pool = &self.held.as_ref().unwrap().pool;
        let mut writer = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let (current, mapping) =
            motion_catalog_backend::source_lifecycle::current_mapping_in(&mut writer, &source_id)
                .await
                .unwrap();
        assert_eq!(current, recovered);
        assert_eq!(mapping.motion_root, candidate.to_str().unwrap());
        let effect_raw: String = sqlx::query_scalar(
            "SELECT native_effect_json FROM catabolic_source_relocation_acks WHERE operation_id=?",
        )
        .bind(&operation)
        .fetch_one(&mut *writer)
        .await
        .unwrap();
        let effect: serde_json::Value = serde_json::from_str(&effect_raw).unwrap();
        assert_eq!(
            effect["policy_json"].as_str().unwrap(),
            catabolic_core::encode(
                &serde_json::json!({"preset":"strict","revision":0,"settings":{"uuid":"check","device":"check","inode":"check"}})
            ),
            "absent enrollment must preserve the exact native check policy through lost-ACK recovery and replay"
        );
        for table in [
            "catabolic_source_relocation_admissions",
            "catabolic_source_relocation_commands",
            "catabolic_source_relocation_acks",
        ] {
            let count: i64 = sqlx::query_scalar(&format!(
                "SELECT count(*) FROM {table} WHERE operation_id=?"
            ))
            .bind(&operation)
            .fetch_one(&mut *writer)
            .await
            .unwrap();
            assert_eq!(count, 1, "retry duplicated {table}");
        }
        writer.rollback().await.unwrap();
        self
    }
}
