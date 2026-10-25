//! Fixed typed96 transition corridor. These owners retain the stage witness and
//! native custody, never expose Connection/Store or generic dispatch callbacks.
use super::facade::UpgradedControlStore;
use super::*;
use catabolic_store::{
    catalog_commands::ReceiptPolicy,
    migration::{NativeUpgradeQuiescence, offline_bootstrap::HostSourceEvidence},
};
impl UpgradedControlStore {
    pub fn prepare_native(
        self,
        native: &Path,
        quiescence: NativeUpgradeQuiescence,
        transition: &str,
    ) -> anyhow::Result<PreparedControlNativeUpgrade> {
        self.prepare_native_reserved(native, quiescence, transition, 0)
    }
    /// Descriptive retained host allocations, not identity or authorization.
    /// The fixed owner independently adds its target database and phase state.
    pub fn prepare_native_reserved(
        self,
        native: &Path,
        quiescence: NativeUpgradeQuiescence,
        transition: &str,
        external_retained_bytes: u64,
    ) -> anyhow::Result<PreparedControlNativeUpgrade> {
        Ok(PreparedControlNativeUpgrade {
            inner: self.owner.prepare_native_reserved(
                native,
                quiescence,
                transition,
                external_retained_bytes,
            )?,
        })
    }
}
pub struct PreparedControlNativeUpgrade {
    inner: StagePreparedTransition,
}
impl PreparedControlNativeUpgrade {
    /// Refresh measured host retention before the final authorization fence.
    /// This can only increase the retained native reservation.
    pub fn refresh_external_reservation(
        &mut self,
        external_retained_bytes: u64,
    ) -> anyhow::Result<()> {
        self.inner
            .refresh_external_reservation(external_retained_bytes)
    }
    /// Fresh target-policy reads retain this phase's actual writer through the
    /// next fixed effect. Current live-source authorization remains separate.
    pub async fn authorization_reader(
        &mut self,
    ) -> anyhow::Result<crate::native_bootstrap::TargetAuthorizationReader<'_>> {
        self.inner.authorization_reader().await
    }
    pub fn apply(
        self,
        backup_parent: Option<&Path>,
    ) -> anyhow::Result<PendingControlNativeBootstrap> {
        Ok(PendingControlNativeBootstrap {
            inner: self.inner.apply(backup_parent)?,
        })
    }
}
pub struct PendingControlNativeBootstrap {
    inner: StagePendingTransition,
}
impl PendingControlNativeBootstrap {
    /// Refresh measured host retention before the final authorization fence.
    /// This can only increase the retained native reservation.
    pub fn refresh_external_reservation(
        &mut self,
        external_retained_bytes: u64,
    ) -> anyhow::Result<()> {
        self.inner
            .refresh_external_reservation(external_retained_bytes)
    }
    /// Fresh target-policy reads retain this phase's actual writer through the
    /// next fixed effect. Current live-source authorization remains separate.
    pub async fn authorization_reader(
        &mut self,
    ) -> anyhow::Result<crate::native_bootstrap::TargetAuthorizationReader<'_>> {
        self.inner.authorization_reader().await
    }
    pub fn verified_backup_path(&self) -> anyhow::Result<&Path> {
        self.inner.verified_backup_path()
    }
    pub fn save_command(
        self,
        operation: &str,
        evidence: &[HostSourceEvidence<'_>],
    ) -> anyhow::Result<Self> {
        Ok(Self {
            inner: self.inner.save_command(operation, evidence)?,
        })
    }
    pub fn execute_unacknowledged(
        self,
        evidence: &[HostSourceEvidence<'_>],
        policy: ReceiptPolicy,
    ) -> anyhow::Result<ExecutedControlNativeBootstrap> {
        Ok(ExecutedControlNativeBootstrap {
            inner: self.inner.execute_unacknowledged(evidence, policy)?,
        })
    }
}
/// Native receipt exists, but Motion ACK has not been written. The trusted
/// actor must reauthorize current live source and target before consuming ACK.
pub struct ExecutedControlNativeBootstrap {
    inner: StageExecutedTransition,
}
impl ExecutedControlNativeBootstrap {
    /// Refresh measured host retention before the final authorization fence.
    /// This can only increase the retained native reservation.
    pub fn refresh_external_reservation(
        &mut self,
        external_retained_bytes: u64,
    ) -> anyhow::Result<()> {
        self.inner
            .refresh_external_reservation(external_retained_bytes)
    }
    /// Restricted target-policy reads after native effects, under the same
    /// retained writer. Current live-source authorization is independently
    /// required; historical target permissions never grant that authority.
    pub async fn authorization_reader(
        &mut self,
    ) -> anyhow::Result<crate::native_bootstrap::TargetAuthorizationReader<'_>> {
        self.inner.authorization_reader().await
    }
    pub fn acknowledge(self) -> anyhow::Result<AcknowledgedControlNativeBootstrap> {
        Ok(AcknowledgedControlNativeBootstrap {
            inner: self.inner.acknowledge()?,
        })
    }
}
pub struct AcknowledgedControlNativeBootstrap {
    inner: StageAcknowledged,
}
impl AcknowledgedControlNativeBootstrap {
    /// Refresh measured host retention before the final authorization fence.
    /// This can only increase the retained native reservation.
    pub fn refresh_external_reservation(
        &mut self,
        external_retained_bytes: u64,
    ) -> anyhow::Result<()> {
        self.inner
            .refresh_external_reservation(external_retained_bytes)
    }
    pub fn identity(&self) -> &playscale_core::catalog_backend::Identity {
        self.inner.identity()
    }
    pub fn transition_id(&self) -> &str {
        self.inner.transition_id()
    }
    pub fn acknowledgment_digest(&self) -> &str {
        self.inner.acknowledgment_digest()
    }
    pub async fn into_source_session(self) -> anyhow::Result<crate::SourceRelocationSession> {
        Ok(crate::SourceRelocationSession::from_stage(self.inner.into_source_session().await?))
    }
}
