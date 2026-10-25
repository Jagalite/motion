//! Caller-owned current-image producer. Futures borrow the already admitted
//! slot; neither SQLx nor native original custody is an await-local owner.
use super::*;
use catabolic_store::migration::offline_bootstrap::CurrentImageCaptureOperation;
use std::time::{Duration, Instant};

pub(crate) struct CaptureAdmission {
    deadline: Instant,
    ready: bool,
    writer: Option<usize>,
    storage: u64,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Ready,
    Running,
    Captured,
    Complete,
    Failed,
    Closing,
    CloseFailed,
    Closed,
    NativePreflight,
    NativeReady,
    NativeAttempted,
    NativeCaptured,
    PairedRunning,
    PairedReady,
    PairedCaptured,
}
enum Native {
    Session(CompletedCatalogSession),
    Capturing(CurrentImageCaptureOperation),
    Image(CurrentCatalogImage),
}
struct StageCaptureParts {
    // Guarded outputs are destroyed before the Native/source owner and locks.
    native_result: Option<catabolic_store::migration::offline_bootstrap::OfflineRetainedResult<()>>,
    native: Option<Native>,
    writer: Option<Transaction<'static, Sqlite>>,
    retained: bool,
    native_started: bool,
    native_deadline: Option<Instant>,
    writer_custody: RetainedWriterCustody,
    proof: crate::native_bootstrap::SourcePairProof,
    witness: StageWitness,
    reference: UpgradeReference,
    admission: CaptureAdmission,
    closure: Option<CurrentImageClosure>,
    phase: Phase,
    #[cfg(test)]
    test_pause: bool,
    #[cfg(test)]
    test_panic: bool,
}
// This is the existing capture owner, extended to own exact input custody
// BEFORE either admission marker can invalidate the legacy source close path.
// None exists only during adjacent total moves, never across a fallible call.
pub(crate) struct StageImageCaptureOperation {
    custody: Option<CaptureCustody>,
}
enum CaptureCustody {
    Inputs {
        source: StageSourceSession,
        writer: Transaction<'static, Sqlite>,
        identity: usize,
        deadline: Instant,
        claimed: bool,
    },
    Parts(StageCaptureParts),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RetainedWriterCustody {
    Held,
    Transferred,
    ExactCloseAcknowledged,
    ReleaseAttempted,
    Released,
    ReleaseVerified,
}
#[path = "capture/retained.rs"]
mod retained;

fn storage() -> anyhow::Result<u64> {
    // Actual simultaneously live inner/public operation and handoff layouts,
    // separately from native's pre-admitted capture slot and existing owners.
    let layouts = [
        std::mem::size_of::<StageImageCaptureOperation>(),
        std::mem::size_of::<StageCaptureParts>(),
        std::mem::size_of::<Option<CaptureCustody>>(),
        std::mem::size_of::<Result<StageCurrentImage, StageCaptureParts>>(),
        std::mem::size_of::<
            Result<StageImageCaptureOperation, (StageSourceSession, Transaction<'static, Sqlite>)>,
        >(),
        std::mem::size_of::<Result<StageCurrentImage, StageImageCaptureOperation>>(),
        std::mem::size_of::<
            Result<crate::SourceImageCaptureOperation, crate::SourceImageCapturePreparationFailure>,
        >(),
        std::mem::size_of::<Result<crate::SourceCurrentImage, crate::SourceImageCaptureOperation>>(
        ),
    ];
    // Both the retained closure and its replacement can coexist during verify.
    // Each contains one fixed lowercase SHA-256 string (64 bytes); headers are
    // already present in the layouts above. This is logical retained storage,
    // not an allocator/RSS bound or coverage for the closure parser/SQL scratch.
    let closure_digests = 2 * 64u64;
    fn cancelled_frame<F, R>(_: F) -> u64
    where F: FnOnce(&'static mut StageImageCaptureOperation, &'static AtomicBool) -> R,
        R: std::future::Future,
    { std::mem::size_of::<R>() as u64 }
    fn frame<F, R>(_: F) -> u64
    where F: FnOnce(&'static mut StageImageCaptureOperation) -> R,
        R: std::future::Future,
    { std::mem::size_of::<R>() as u64 }
    let fixed = closure_digests
        .checked_add(cancelled_frame(StageImageCaptureOperation::capture))
        .and_then(|n| n.checked_add(cancelled_frame(StageImageCaptureOperation::verify)))
        .and_then(|n| n.checked_add(frame(StageImageCaptureOperation::writer_in)))
        .and_then(|n| n.checked_add(frame(StageImageCaptureOperation::close)))
        .context("current-image capture frame overflow")?;
    layouts.into_iter().try_fold(fixed, |sum, size| {
        sum.checked_add(u64::try_from(size)?)
            .context("current-image capture slot overflow")
    })
}
async fn writer_identity(writer: &mut SqliteConnection) -> anyhow::Result<usize> {
    let mut locked = writer.lock_handle().await?;
    Ok(locked.as_raw_handle().as_ptr() as usize)
}
impl StageSourceSession {
    /// Ingress-only numeric/full-pair admission. All new host slots and the
    /// complete retained94 dynamic share were frozen once in H before opening.
    /// Do not add them again as extra_retained and do not install either native
    /// or Motion capture_admission here. The original retained86 close remains
    /// valid while the fresh authorization/native-call seam is unavailable.
    pub(crate) async fn admit_capture_ingress_writer94(
        &mut self, writer: &mut Transaction<'static, Sqlite>,
        deadline: Instant, cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        seal::alive(cancel, deadline)?;
        seal::preflight_capture_ingress94(&mut self.native, writer, &mut self.proof,
            &self.witness, &self.reference, deadline, cancel).await?;
        self.verify_pair_in(writer).await?;
        preflight_motion_image(writer, cancel).await?;
        seal::alive(cancel, deadline)
    }
    pub(crate) fn admit_current_image_operation<'a>(
        &'a mut self,
        writer: &'a mut Transaction<'static, Sqlite>,
        deadline: Instant,
        cancel: &'a AtomicBool,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + 'a {
        let deadline = deadline.min(Instant::now() + Duration::from_secs(60));
        let admission = self.capture_admission.get_or_insert(CaptureAdmission {
            deadline,
            ready: false,
            writer: None,
            storage: 0,
        });
        admission.deadline = admission.deadline.min(deadline);
        admission.ready = false;
        async move {
            let deadline = self
                .capture_admission
                .as_ref()
                .expect("capture admission")
                .deadline;
            seal::alive(cancel, deadline)?;
            let identity = writer_identity(writer).await?;
            let admission = self.capture_admission.as_mut().expect("capture admission");
            if let Some(previous) = admission.writer {
                ensure!(previous == identity, "capture admission writer changed");
            }
            admission.writer = Some(identity);
            let storage = storage()?;
            seal::preflight_capture_parts(
                &mut self.native,
                writer,
                &self.proof,
                &self.witness,
                &self.reference,
                deadline,
                cancel,
                storage,
            )
            .await?;
            self.verify_pair_in(writer).await?;
            preflight_motion_image(writer, cancel).await?;
            self.native.admit_current_image_operation(deadline)?;
            seal::alive(cancel, deadline)?;
            let admission = self.capture_admission.as_mut().expect("capture admission");
            admission.storage = storage;
            admission.ready = true;
            Ok(())
        }
    }
    /// No allocation, validation, callback or await occurs during this handoff.
    pub(crate) fn begin_current_image_capture(
        self,
        writer: Transaction<'static, Sqlite>,
    ) -> Result<StageImageCaptureOperation, (Self, Transaction<'static, Sqlite>)> {
        if !self.capture_admission.as_ref().is_some_and(|a| a.ready) {
            return Err((self, writer));
        }
        let Self {
            native,
            proof,
            witness,
            reference,
            capture_admission,
        } = self;
        Ok(StageImageCaptureOperation { custody: Some(CaptureCustody::Parts(StageCaptureParts {
            native: Some(Native::Session(native)),
            writer: Some(writer),
            retained: false, native_started: false, native_deadline: None,
            writer_custody: RetainedWriterCustody::Held,
            native_result: None,
            proof,
            witness,
            reference,
            admission: capture_admission.expect("admitted capture"),
            closure: None,
            phase: Phase::Ready,
            #[cfg(test)]
            test_pause: false,
            #[cfg(test)]
            test_panic: false,
        })) })
    }
}
impl StageCaptureParts {
    pub(crate) fn deadline(&self) -> Instant {
        self.admission.deadline
    }
    pub(crate) fn capture<'a>(
        &'a mut self,
        cancel: &'a AtomicBool,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + 'a {
        let ready = self.phase == Phase::Ready;
        if ready {
            self.phase = Phase::Running;
        }
        async move {
            ensure!(ready, "current-image capture is not retryable");
            let result = self.capture_inner(cancel).await;
            self.phase = if result.is_ok() {
                Phase::Complete
            } else {
                Phase::Failed
            };
            result
        }
    }
    async fn capture_inner(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        let deadline = self.admission.deadline;
        seal::alive(cancel, deadline)?;
        ensure!(
            self.admission.writer == Some(writer_identity(self.writer.as_mut().expect("capture held writer")).await?),
            "current-image capture writer changed"
        );
        let Some(Native::Session(native)) = self.native.as_mut() else {
            anyhow::bail!("current-image capture source owner missing");
        };
        seal::preflight_capture_parts(
            native,
            self.writer.as_mut().expect("capture held writer"),
            &self.proof,
            &self.witness,
            &self.reference,
            deadline,
            cancel,
            self.admission.storage,
        )
        .await?;
        verify_pair_parts(
            self.writer.as_mut().expect("capture held writer"),
            &self.proof,
            &self.witness,
            &self.reference,
        )
        .await?;
        preflight_motion_image(self.writer.as_mut().expect("capture held writer"), cancel).await?;
        seal::alive(cancel, deadline)?;
        // Both native transitions are fixed moves only. Store their outcomes
        // before the next callback, error formatting or asynchronous operation.
        let Some(Native::Session(native)) = self.native.take() else {
            unreachable!()
        };
        match native.begin_current_image_capture() {
            Ok(operation) => self.native = Some(Native::Capturing(operation)),
            Err(failure) => {
                self.native = Some(Native::Session(failure.into_session()));
                anyhow::bail!("native current-image capture admission missing");
            }
        }
        let Some(Native::Capturing(operation)) = self.native.as_mut() else {
            unreachable!()
        };
        operation.capture(cancel)?;
        let Some(Native::Capturing(operation)) = self.native.take() else {
            unreachable!()
        };
        match operation.finish() {
            Ok(image) => self.native = Some(Native::Image(image)),
            Err(failure) => {
                self.native = Some(Native::Session(failure.into_session()));
                anyhow::bail!("native current-image capture did not complete");
            }
        }
        #[cfg(test)]
        tests::after_native(self.test_pause, self.test_panic).await;
        self.check_captured(cancel).await
    }
    async fn check_captured(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        let deadline = self.admission.deadline;
        seal::alive(cancel, deadline)?;
        ensure!(
            self.admission.writer == Some(writer_identity(self.writer.as_mut().expect("capture held writer")).await?),
            "current-image capture writer changed"
        );
        let Some(Native::Image(native)) = self.native.as_mut() else {
            anyhow::bail!("captured native image missing");
        };
        if self.retained {
            seal::preflight_retained_captured_extra(native,
                self.writer.as_mut().expect("capture held writer"), &mut self.proof,
                &self.witness, &self.reference, deadline, cancel, self.admission.storage).await?;
        } else {
            seal::preflight_captured_extra(native,
                self.writer.as_mut().expect("capture held writer"), &self.proof,
                &self.witness, &self.reference, deadline, cancel, self.admission.storage).await?;
        }
        verify_pair_parts(
            self.writer.as_mut().expect("capture held writer"),
            &self.proof,
            &self.witness,
            &self.reference,
        )
        .await?;
        preflight_motion_image(self.writer.as_mut().expect("capture held writer"), cancel).await?;
        let closure = image_closure::verify_in(self.writer.as_mut().expect("capture held writer"), native, cancel).await?;
        if let Some(previous) = &self.closure {
            ensure!(
                previous.sha256() == closure.sha256(),
                "captured source closure changed"
            );
        }
        verify_pair_parts(
            self.writer.as_mut().expect("capture held writer"),
            &self.proof,
            &self.witness,
            &self.reference,
        )
        .await?;
        seal::alive(cancel, deadline)?;
        self.closure = Some(closure);
        Ok(())
    }
    /// Fresh host authorization borrows only the original writer. Any use after
    /// capture invalidates completion until the fixed final verification runs.
    pub(crate) fn writer_in(
        &mut self,
    ) -> impl std::future::Future<Output = anyhow::Result<&mut SqliteConnection>> + '_ {
        let allowed = matches!(self.phase, Phase::Ready | Phase::Captured | Phase::Complete);
        if self.phase == Phase::Complete {
            self.phase = Phase::Captured;
        }
        async move {
            ensure!(allowed, "capture authorization owner unavailable");
            ensure!(
                Instant::now() < self.admission.deadline,
                "capture authorization deadline exhausted"
            );
            ensure!(
                self.admission.writer == Some(writer_identity(self.writer.as_mut().expect("capture held writer")).await?),
                "capture authorization writer changed"
            );
            verify_pair_parts(
                self.writer.as_mut().expect("capture held writer"),
                &self.proof,
                &self.witness,
                &self.reference,
            )
            .await?;
            ensure!(
                Instant::now() < self.admission.deadline,
                "capture authorization deadline exhausted"
            );
            Ok(&mut **self.writer.as_mut().expect("capture held writer"))
        }
    }
    pub(crate) fn verify<'a>(
        &'a mut self,
        cancel: &'a AtomicBool,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + 'a {
        let allowed = matches!(self.phase, Phase::Captured | Phase::Complete);
        if allowed {
            self.phase = Phase::Running;
        }
        async move {
            ensure!(allowed, "capture final verification unavailable");
            let result = self.check_captured(cancel).await;
            self.phase = if result.is_ok() {
                Phase::Complete
            } else {
                Phase::Failed
            };
            result
        }
    }
    /// Pure move handoff; every proof and final deadline check happened while
    /// this operation was still borrowed and independently retained.
    pub(crate) fn finish(self) -> Result<StageCurrentImage, Self> {
        if self.phase != Phase::Complete {
            return Err(self);
        }
        let Self {
            native,
            writer,
            proof,
            witness,
            reference,
            closure,
            ..
        } = self;
        let Some(Native::Image(native)) = native else {
            unreachable!("complete capture image")
        };
        Ok(StageCurrentImage {
            native,
            writer: writer.expect("complete capture writer"),
            proof,
            witness,
            reference,
            closure: closure.expect("complete capture closure"),
            restore_admission: None,
        })
    }
    pub(crate) async fn close(&mut self) -> anyhow::Result<()> {
        if self.phase == Phase::Closed {
            return Ok(());
        }
        ensure!(
            self.phase != Phase::CloseFailed,
            "capture close previously failed"
        );
        self.phase = Phase::Closing;
        ensure!(
            self.admission.writer == Some(writer_identity(self.writer.as_mut().expect("capture held writer")).await?),
            "capture close writer changed; original custody remains required"
        );
        self.witness.verify(false)?;
        crate::motion_snapshot::close_retained_writer(self.writer.as_mut().expect("capture held writer")).await?;
        self.witness.verify(false)?;
        // Original writer is proven settled before the explicit native release.
        // No native outcome is presented as operational authority here.
        drop(self.native.take());
        let result = self.witness.verify(false);
        self.phase = if result.is_ok() {
            Phase::Closed
        } else {
            Phase::CloseFailed
        };
        result
    }
}

impl StageImageCaptureOperation {
    pub(crate) fn deadline(&self) -> Instant {
        match self.custody.as_ref().expect("capture custody") {
            CaptureCustody::Inputs { deadline, .. } => *deadline,
            CaptureCustody::Parts(parts) => parts.deadline(),
        }
    }
    // Retained94 cannot reach legacy capture/verify/close, even internally.
    fn legacy(&mut self) -> Option<&mut StageCaptureParts> {
        match self.custody.as_mut()? {
            CaptureCustody::Parts(parts) if !parts.retained => Some(parts),
            _ => None,
        }
    }
    pub(crate) fn capture<'a>(&'a mut self, cancel: &'a AtomicBool)
        -> impl std::future::Future<Output = anyhow::Result<()>> + 'a
    {
        let work = self.legacy().map(|parts| parts.capture(cancel));
        async move { match work { Some(work) => work.await, None => anyhow::bail!("retained capture needs fixed fresh-auth seam") } }
    }
    pub(crate) fn writer_in(&mut self)
        -> impl std::future::Future<Output = anyhow::Result<&mut SqliteConnection>> + '_
    {
        let work = self.legacy().map(StageCaptureParts::writer_in);
        async move { match work { Some(work) => work.await, None => anyhow::bail!("retained capture writer is opaque") } }
    }
    pub(crate) fn verify<'a>(&'a mut self, cancel: &'a AtomicBool)
        -> impl std::future::Future<Output = anyhow::Result<()>> + 'a
    {
        let work = self.legacy().map(|parts| parts.verify(cancel));
        async move { match work { Some(work) => work.await, None => anyhow::bail!("NativeCaptured is not Motion closure verification") } }
    }
    pub(crate) fn finish(self) -> Result<StageCurrentImage, Self> {
        match self.custody {
            Some(CaptureCustody::Parts(parts)) if !parts.retained => match parts.finish() {
                Ok(image) => Ok(image),
                Err(parts) => Err(Self { custody: Some(CaptureCustody::Parts(parts)) }),
            },
            custody => Err(Self { custody }),
        }
    }
    pub(crate) fn close(&mut self)
        -> impl std::future::Future<Output = anyhow::Result<()>> + '_
    {
        let work = self.legacy().map(StageCaptureParts::close);
        async move { match work { Some(work) => work.await, None => anyhow::bail!("retained capture requires exact ingress close ACK") } }
    }
    #[cfg(test)]
    fn parts_for_test(&mut self) -> &mut StageCaptureParts {
        match self.custody.as_mut().unwrap() { CaptureCustody::Parts(parts) => parts, _ => panic!("capture still retains inputs") }
    }
}

#[cfg(test)]
#[path = "capture/tests.rs"]
mod tests;
