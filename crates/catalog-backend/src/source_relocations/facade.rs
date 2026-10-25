//! Fixed trusted-host adapter. Requester descriptions are not authentication.
use super::{coordinator, history::Requester};
use crate::{offline_control_upgrade::StageSourceSession, source_lifecycle::LifecycleProof};
use catabolic_core::catalog_command::CommandEnvelope;
use catabolic_store::catalog_commands::ReceiptPolicy;
use sqlx::{Sqlite, Transaction};
use std::{path::Path, sync::atomic::AtomicBool};
pub struct SourceRelocationRequester {
    pub(super) inner: Requester,
}
impl SourceRelocationRequester {
    /// Trusted host must derive these facts from fresh actual Caller checks on
    /// the retained writer. Never deserialize this from a request body.
    pub fn from_current_host(
        requester_id: String,
        credential_hash: Option<String>,
        ingress_id: Option<String>,
        principal: playscale_core::access::Principal,
    ) -> anyhow::Result<Self> {
        let inner = Requester {
            requester_id,
            credential_hash,
            ingress_id,
            accepted_principal: principal,
        };
        super::history::validate_requester(&inner)?;
        Ok(Self { inner })
    }
}
/// Constructed only by genuine96 ACK. Host retains actual stage, stopped service,
/// locks and drained owners until settlement. No path/pool/native getter.
pub struct SourceRelocationSession {
    pub(crate) inner: StageSourceSession,
    pub(super) owned94: Option<Box<super::owned_operation::Admission>>,
}
impl SourceRelocationSession {
    pub(crate) fn from_stage(inner: StageSourceSession) -> Self { Self { inner, owned94: None } }
    /// Moves only. The genuine retained bootstrap producer already admitted the
    /// close destination; legacy or capture-admitted sessions return unchanged.
    /// This is the service-only retained route. The old consuming `close(self)`
    /// remains a separate legacy API and is not made failure-owning by admission;
    /// connected service callers must install this operation instead. Once this
    /// handoff succeeds there is no extractor back to that legacy source API.
    pub fn begin_retained_close(self) -> Result<SourceSessionCloseOperation, Self> {
        match self.inner.begin_retained_close() {
            Ok(inner) => Ok(SourceSessionCloseOperation { inner, _owned94: self.owned94 }),
            Err(inner) => Err(Self { inner, owned94: self.owned94 }),
        }
    }
    pub fn identity(&self) -> &playscale_core::catalog_backend::Identity {
        self.inner.identity()
    }
    pub async fn recorded_command(
        &mut self,
        mut writer: Transaction<'static, Sqlite>,
        operation: &str,
        expected: &LifecycleProof,
        requester: &SourceRelocationRequester,
        candidate: &Path,
    ) -> anyhow::Result<Option<CommandEnvelope>> {
        anyhow::ensure!(self.owned94.is_none(), "owned94 admitted source rejects legacy operation");
        self.inner.verify_pair_in(&mut writer).await?;
        let command = super::journal::recorded_command_in(
            &mut writer,
            operation,
            expected,
            &requester.inner,
            candidate,
        )
        .await?;
        anyhow::ensure!(self.owned94.is_none(), "owned94 admitted source rejects legacy operation");
        self.inner.verify_pair_in(&mut writer).await?;
        writer.rollback().await?;
        Ok(command)
    }
    pub async fn commit_new_intent(
        &mut self,
        writer: Transaction<'static, Sqlite>,
        operation: &str,
        expected: &LifecycleProof,
        requester: &SourceRelocationRequester,
        candidate: &Path,
        cancel: &AtomicBool,
    ) -> anyhow::Result<CommandEnvelope> {
        anyhow::ensure!(self.owned94.is_none(), "owned94 admitted source rejects legacy operation");
        coordinator::commit_new_intent(
            &mut self.inner,
            writer,
            operation,
            expected,
            &requester.inner,
            candidate,
            cancel,
        )
        .await
    }
    /// Native settlement only. Final live-source temporal and target Caller
    /// authorization occurs through the returned phase before its receipt ACK.
    pub async fn dispatch_recorded<'a>(
        &'a mut self,
        mut writer: Transaction<'static, Sqlite>,
        operation: &str,
        requester: &SourceRelocationRequester,
        policy: ReceiptPolicy,
        cancel: &AtomicBool,
    ) -> anyhow::Result<DispatchedSourceRelocation<'a>> {
        anyhow::ensure!(self.owned94.is_none(), "owned94 admitted source rejects legacy operation");
        self.inner.verify_pair_in(&mut writer).await?;
        coordinator::dispatch_in(
            &mut self.inner,
            &mut writer,
            operation,
            &requester.inner,
            policy,
            cancel,
        )
        .await?;
        self.prepare_existing_receipt(writer, operation, requester)
            .await
    }
    /// Recovery never executes a missing operation.
    pub async fn prepare_existing_receipt<'a>(
        &'a mut self,
        mut writer: Transaction<'static, Sqlite>,
        operation: &str,
        requester: &SourceRelocationRequester,
    ) -> anyhow::Result<DispatchedSourceRelocation<'a>> {
        anyhow::ensure!(self.owned94.is_none(), "owned94 admitted source rejects legacy operation");
        self.inner.verify_pair_in(&mut writer).await?;
        let journal = super::journal::load_in(&mut writer, operation, &requester.inner)
            .await?
            .ok_or_else(|| anyhow::anyhow!("relocation requires committed original intent"))?;
        let receipt = self.inner.receipt_in(journal).await?.ok_or_else(|| {
            anyhow::anyhow!("relocation ACK requires existing permanent native receipt")
        })?;
        anyhow::ensure!(self.owned94.is_none(), "owned94 admitted source rejects legacy operation");
        self.inner.verify_pair_in(&mut writer).await?;
        Ok(DispatchedSourceRelocation {
            session: self,
            writer,
            operation: operation.to_owned(),
            receipt,
        })
    }
    pub fn close(self) -> anyhow::Result<()> {
        self.inner.close()
    }
}
/// Exclusive session borrow plus exact operation/receipt and SAME retained
/// transaction. Dropping the phase leaves native receipt durable and no ACK.
pub struct DispatchedSourceRelocation<'session> {
    session: &'session mut SourceRelocationSession,
    writer: Transaction<'static, Sqlite>,
    operation: String,
    receipt: catabolic_store::migration::offline_bootstrap::SourceRenewalReceipt,
}
impl DispatchedSourceRelocation<'_> {
    /// Trusted fixed app authentication only. Do not end or mutate this writer;
    /// actual transaction, pair and command identity are verified again below.
    pub fn writer_in(&mut self) -> &mut sqlx::SqliteConnection {
        &mut self.writer
    }
    pub async fn commit_existing_receipt_ack(
        mut self,
        requester: &SourceRelocationRequester,
    ) -> anyhow::Result<LifecycleProof> {
        self.session.inner.verify_pair_in(&mut self.writer).await?;
        let journal = super::journal::load_in(&mut self.writer, &self.operation, &requester.inner)
            .await?
            .ok_or_else(|| anyhow::anyhow!("relocation original intent disappeared"))?;
        anyhow::ensure!(
            journal.command().canonical_payload() == self.receipt.command().canonical_payload(),
            "relocation settled command identity differs"
        );
        let receipt = self
            .session
            .inner
            .receipt_in(journal)
            .await?
            .ok_or_else(|| anyhow::anyhow!("relocation permanent receipt disappeared"))?;
        anyhow::ensure!(
            receipt.command().canonical_payload() == self.receipt.command().canonical_payload(),
            "relocation settled receipt differs"
        );
        require_same_receipt_effect(&receipt, self.receipt.effect_description())?;
        let successor = super::journal::acknowledge_in(
            &mut self.writer,
            &self.operation,
            &requester.inner,
            &receipt,
        )
        .await?;
        self.session.inner.verify_pair_in(&mut self.writer).await?;
        self.writer.commit().await?;
        Ok(successor)
    }
}

// Exact authenticated equality, not a shape check or a receipt constructor.
pub(super) fn require_same_receipt_effect(
    actual: &catabolic_store::migration::offline_bootstrap::SourceRenewalReceipt,
    expected: &serde_json::Value,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        actual.effect_description() == expected,
        "relocation settled receipt differs"
    );
    Ok(())
}

/// Opaque terminal close owner. Dropped request futures cannot own its resources:
/// the service must install this value before calling any borrowed close step.
pub struct SourceSessionCloseOperation {
    inner: crate::offline_control_upgrade::StageSourceCloseOperation,
    _owned94: Option<Box<super::owned_operation::Admission>>,
}
impl SourceSessionCloseOperation {
    pub fn phase(&self) -> crate::offline_control_upgrade::SourceSessionClosePhase {
        self.inner.phase()
    }
    #[cfg(test)]
    pub(crate) fn close_with_fault_for_test(&mut self, fault: u8) -> anyhow::Result<()> {
        self.inner.close_inner(fault)
    }
    #[cfg(test)]
    pub(crate) fn custody_for_test(&self) -> (bool, bool, bool, i32, Option<usize>) {
        self.inner.custody_for_test()
    }
    pub fn close(&mut self) -> anyhow::Result<()> {
        self.inner.close()
    }
    /// A failed/uncertain operation remains whole; it never yields source authority.
    pub fn finish(self) -> Result<(), Self> {
        if self.phase() == crate::offline_control_upgrade::SourceSessionClosePhase::Closed {
            Ok(())
        } else {
            Err(self)
        }
    }
}
