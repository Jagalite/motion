//! Caller-retained, pre-admitted reservation operation. Borrowed futures never
//! become the sole native/session/WRITE/descriptor owner. No recovery registry,
//! destructor allocation, effect, ACK, installation or reopened grant exists.
use super::*;
use crate::paired_restore::durable::{self, DurableReservation, PendingReservation};
use seal::{PreparedSeal, SealAccounting, SealPreparation};
use std::{
    fs::File,
    time::{Duration, Instant},
};

pub(super) struct RestoreAdmission {
    deadline: Instant,
    external_host: u64,
    selection_storage: u64,
    ready: bool,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Ready,
    Preparing,
    Sealed,
    Staged,
    Reserving,
    Reserved,
    Failed,
    Closing,
    CloseFailed,
    Closed,
}
enum Files {
    Pending(PendingReservation),
    Verified(DurableReservation),
}
/// This slot must be retained by the caller across future cancellation/unwind.
/// Dropping the operation itself is an explicit abandonment of that caller's
/// ownership, not a request-cancellation recovery mechanism.
pub(crate) struct StageRestoreOperation {
    image: Option<StageCurrentImage>,
    admission: RestoreAdmission,
    input: Option<SealPreparation>,
    seal: Option<PreparedSeal>,
    files: Option<Files>,
    phase: Phase,
}
pub(crate) struct StageRestoreDescriptorFailure {
    parent: File,
    record: File,
    error: anyhow::Error,
}
impl StageRestoreDescriptorFailure {
    pub(crate) fn into_parts(self) -> (File, File, anyhow::Error) {
        (self.parent, self.record, self.error)
    }
}
pub(super) fn storage() -> anyhow::Result<u64> {
    // Actual named fixed layouts: stable slot, move-only input/output handoff,
    // selected-input and prepared-output slots, and durable verifier workspace.
    [
        std::mem::size_of::<StageRestoreOperation>(),
        std::mem::size_of::<StageCurrentImage>(),
        std::mem::size_of::<Result<StageRestoreOperation, StageCurrentImage>>(),
        std::mem::size_of::<
            Result<
                crate::source_relocations::SourceRestoreOperation,
                crate::source_relocations::SourceCurrentImage,
            >,
        >(),
        std::mem::size_of::<Result<(), StageRestoreDescriptorFailure>>(),
        std::mem::size_of::<Result<(), crate::source_relocations::RestoreDescriptorFailure>>(),
        std::mem::size_of::<SealPreparation>(),
        std::mem::size_of::<PreparedSeal>(),
    ]
    .into_iter()
    .try_fold(durable::reservation_verification_allocations(), |n, x| {
        n.checked_add(x as u64)
            .context("restore operation storage overflow")
    })
}
impl StageCurrentImage {
    pub(crate) fn admit_restore_operation<'a>(
        &'a mut self,
        external_host: u64,
        selection_storage: u64,
        cancel: &'a AtomicBool,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + 'a {
        // Establish the original deadline before the first poll. A failed or
        // abandoned admission cannot refresh it or permit an owned handoff.
        let admission = self
            .restore_admission
            .get_or_insert_with(|| RestoreAdmission {
                deadline: Instant::now() + Duration::from_secs(60),
                external_host,
                selection_storage,
                ready: false,
            });
        admission.ready = false;
        let unchanged = admission.external_host == external_host
            && admission.selection_storage == selection_storage;
        let deadline = admission.deadline;
        async move {
            ensure!(unchanged, "restore operation admission facts changed");
            seal::alive(cancel, deadline)?;
            let owned = storage()?
                .checked_add(selection_storage)
                .context("restore operation selection storage overflow")?;
            SealAccounting {
                external: external_host,
            }
            .preflight_owned(self, owned, deadline, cancel)
            .await?;
            seal::alive(cancel, deadline)?;
            self.restore_admission
                .as_mut()
                .expect("fixed admission")
                .ready = true;
            Ok(())
        }
    }
    pub(crate) fn into_restore_operation(mut self) -> Result<StageRestoreOperation, Self> {
        if !self.restore_admission.as_ref().is_some_and(|a| a.ready) {
            return Err(self);
        }
        let admission = self.restore_admission.take().expect("admitted operation");
        Ok(StageRestoreOperation {
            image: Some(self),
            admission,
            input: None,
            seal: None,
            files: None,
            phase: Phase::Ready,
        })
    }
}
impl StageRestoreOperation {
    pub(crate) fn deadline(&self) -> Instant {
        self.admission.deadline
    }
    pub(crate) fn canonical_record(&self) -> anyhow::Result<&[u8]> {
        ensure!(
            matches!(self.phase, Phase::Sealed | Phase::Staged | Phase::Reserved),
            "restore operation has no publishable seal"
        );
        Ok(&self
            .seal
            .as_ref()
            .context("restore seal missing")?
            .canonical)
    }
    pub(crate) fn sha256(&self) -> anyhow::Result<&str> {
        let _ = self.canonical_record()?;
        Ok(&self.seal.as_ref().expect("checked seal").sha256)
    }
    pub(crate) fn prepare<'a>(
        &'a mut self,
        selection: RestoreSealSelection,
        cancel: &'a AtomicBool,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + 'a {
        // Input and terminal-in-flight state are installed before constructing
        // the future. Dropping even an unpolled future cannot restore Ready.
        let allowed = self.phase == Phase::Ready;
        if allowed {
            self.phase = Phase::Preparing;
        }
        let admission = selection.retained_allocations().and_then(|n| {
            ensure!(
                n <= self.admission.selection_storage,
                "restore selection exceeds pre-admitted storage"
            );
            Ok(())
        });
        if allowed && admission.is_ok() {
            self.input = Some(SealPreparation {
                selection,
                deadline: self.admission.deadline,
                accounting: SealAccounting {
                    external: self.admission.external_host,
                },
            });
        }
        async move {
            ensure!(allowed, "restore operation preparation is not retryable");
            let result = async {
                admission?;
                let image = self
                    .image
                    .as_mut()
                    .context("restore original owner missing")?;
                let input = self
                    .input
                    .as_mut()
                    .context("restore preparation input missing")?;
                let sealed = image.build_prepared_seal(input, cancel).await?;
                self.seal = Some(sealed);
                Ok(())
            }
            .await;
            self.phase = if result.is_ok() {
                Phase::Sealed
            } else {
                Phase::Failed
            };
            result
        }
    }
    pub(crate) fn stage_reservation(
        &mut self,
        parent: File,
        record: File,
    ) -> Result<(), StageRestoreDescriptorFailure> {
        if self.phase != Phase::Sealed || self.files.is_some() {
            return Err(StageRestoreDescriptorFailure {
                parent,
                record,
                error: anyhow::anyhow!("restore reservation cannot replace existing custody"),
            });
        }
        self.files = Some(Files::Pending(PendingReservation::new(parent, record)));
        self.phase = Phase::Staged;
        Ok(())
    }
    pub(crate) fn reserve<'a>(
        &'a mut self,
        cancel: &'a AtomicBool,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + 'a {
        self.reserve_inner(cancel, 0)
    }
    fn reserve_inner<'a>(
        &'a mut self,
        cancel: &'a AtomicBool,
        fault: u8,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + 'a {
        let allowed = self.phase == Phase::Staged;
        if allowed {
            self.phase = Phase::Reserving;
        }
        async move {
            ensure!(allowed, "restore reservation is not retryable");
            let result = async {
                #[cfg(test)]
                if fault == 3 {
                    tokio::task::yield_now().await;
                }
                self.verify_seal(cancel).await?;
                let sealed = self.seal.as_ref().expect("verified seal");
                let deadline = self.admission.deadline;
                let Some(Files::Pending(files)) = self.files.as_mut() else {
                    anyhow::bail!("restore reservation descriptors missing");
                };
                files.verify(sealed.parent_identity, &sealed.canonical, cancel, deadline)?;
                // No poll/callback/allocation between these descriptor moves.
                let Some(Files::Pending(files)) = self.files.take() else {
                    unreachable!()
                };
                match files.into_verified() {
                    Ok(files) => self.files = Some(Files::Verified(files)),
                    Err((files, error)) => {
                        self.files = Some(Files::Pending(files));
                        return Err(error);
                    }
                }
                #[cfg(test)]
                match fault {
                    1 => cancel.store(true, Ordering::Release),
                    2 => panic!("injected post-durability reservation unwind"),
                    _ => (),
                }
                #[cfg(not(test))]
                let _ = fault;
                self.verify_seal(cancel).await?;
                let Some(Files::Verified(files)) = self.files.as_ref() else {
                    unreachable!()
                };
                files.verify_until(cancel, self.deadline())?;
                seal::alive(cancel, self.deadline())
            }
            .await;
            self.phase = if result.is_ok() {
                Phase::Reserved
            } else {
                Phase::Failed
            };
            result
        }
    }
    async fn verify_seal(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        let image = self
            .image
            .as_mut()
            .context("restore original owner missing")?;
        let input = self.input.as_mut().context("restore preparation missing")?;
        let sealed = self.seal.as_ref().context("restore seal missing")?;
        sealed.verify_sealed(image, input, storage()?, cancel).await
    }
    pub(crate) async fn verify(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        ensure!(
            matches!(self.phase, Phase::Sealed | Phase::Staged | Phase::Reserved),
            "incomplete restore operation is opaque"
        );
        self.verify_seal(cancel).await?;
        if let Some(Files::Verified(files)) = &self.files {
            files.verify_until(cancel, self.deadline())?;
        }
        seal::alive(cancel, self.deadline())
    }
    pub(crate) async fn writer_in(&mut self) -> anyhow::Result<&mut SqliteConnection> {
        self.verify(&AtomicBool::new(false)).await?;
        Ok(&mut self.image.as_mut().expect("verified owner").writer)
    }
    /// Terminal settlement, never conversion back into a source/image grant.
    /// The exact SQLx connection stays in this slot across rollback cancellation.
    /// Descriptors remain here after native close; named evidence is not deleted.
    pub(crate) async fn close(&mut self) -> anyhow::Result<()> {
        if self.phase == Phase::Closed {
            return Ok(());
        }
        self.phase = Phase::Closing;
        let image = self.image.as_mut().context("restore close owner missing")?;
        image.witness.verify(false)?;
        crate::motion_snapshot::close_retained_writer(&mut image.writer).await?;
        image.witness.verify(false)?;
        let (session, writer) = self
            .image
            .take()
            .expect("retained close owner")
            .discard_copy();
        drop(writer);
        let result = session.close();
        self.phase = if result.is_ok() {
            Phase::Closed
        } else {
            Phase::CloseFailed
        };
        result
    }
}
#[cfg(test)]
#[path = "reservation/tests.rs"]
mod tests;
