//! Finite retained94 Native capture. Exact input ownership is installed before
//! admission; after failure it can only hand its writer to the original ingress
//! settlement graph. Legacy capture and legacy close are never used here.
use super::*;
use std::sync::Arc;

impl StageImageCaptureOperation {
    pub(crate) fn retained_storage() -> anyhow::Result<u64> {
        fn frame<F, R>(_: F) -> u64
        where F: FnOnce(&'static mut StageImageCaptureOperation, Arc<AtomicBool>) -> R,
            R: std::future::Future,
        { std::mem::size_of::<R>() as u64 }
        storage()?.checked_add(frame(Self::prepare_retained_native))
            .and_then(|n| n.checked_add(frame(Self::prepare_retained_pair)))
            .and_then(|n| n.checked_add(2 * std::mem::size_of::<CaptureCustody>() as u64))
            .and_then(|n| n.checked_add(std::mem::size_of::<Option<Transaction<'static, Sqlite>>>() as u64))
            .context("retained capture layouts overflow")
    }
    /// Caller preadmits this exact destination before either input is taken.
    /// No check, allocation, callback, or asynchronous work occurs in this move.
    pub(crate) fn retain_inputs(source: StageSourceSession,
        writer: Transaction<'static, Sqlite>, identity: usize, deadline: Instant,
    ) -> Self {
        Self { custody: Some(CaptureCustody::Inputs { source, writer, identity, deadline, claimed: false }) }
    }
    pub(crate) fn prepare_retained_native(&mut self, cancel: Arc<AtomicBool>)
        -> impl std::future::Future<Output = anyhow::Result<()>> + '_
    {
        let start = match self.custody.as_mut() {
            Some(CaptureCustody::Inputs { source, identity, deadline, claimed, .. }) if !*claimed => {
                *claimed = true;
                // Inputs is already in the service-owned ingress. Even this
                // never-polled admission has a terminal writer/Native path.
                *deadline = (*deadline).min(Instant::now() + Duration::from_secs(60));
                source.capture_admission = Some(CaptureAdmission {
                    deadline: *deadline, ready: false, writer: Some(*identity), storage: 0,
                });
                true
            }
            _ => false,
        };
        async move {
            ensure!(start, "retained native admission is one-shot");
            self.preflight_retained_inputs(&cancel).await?;
            self.split_retained_inputs();
            let parts = self.retained_parts_mut()?;
            parts.phase = Phase::NativePreflight;
            // Moves-only Native admission handoff, with immediate installation.
            let Some(Native::Session(native)) = parts.native.take() else { unreachable!() };
            match native.begin_current_image_capture() {
                Ok(operation) => parts.native = Some(Native::Capturing(operation)),
                Err(failure) => {
                    parts.native = Some(Native::Session(failure.into_session()));
                    anyhow::bail!("retained native capture storage admission missing");
                }
            }
            let Some(Native::Capturing(operation)) = parts.native.as_mut() else { unreachable!() };
            // All output/error slots are admitted BEFORE fresh host auth. The
            // final lexical loan only consumes them; it creates no reservation.
            parts.native_deadline = Some(operation.deadline());
            operation.admit_control_workspace_result()?;
            parts.proof.preflight_source_capture_workspace94(&parts.reference, &parts.witness)?;
            seal::alive(&cancel, parts.admission.deadline)?;
            parts.phase = Phase::NativeReady;
            Ok(())
        }
    }
    async fn preflight_retained_inputs(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        let Some(CaptureCustody::Inputs { source, writer, deadline, .. }) = self.custody.as_mut() else {
            anyhow::bail!("retained capture inputs missing");
        };
        seal::alive(cancel, *deadline)?;
        let identity = writer_identity(writer).await?;
        ensure!(source.capture_admission.as_ref().unwrap().writer == Some(identity),
            "retained capture original writer identity changed");
        // Existing typed source94 route keeps full H/K and independently paid
        // DBSTATUS. New Stage/reader/future layouts were frozen in ingress H.
        seal::preflight_capture_ingress94(&mut source.native, writer, &mut source.proof,
            &source.witness, &source.reference, *deadline, cancel).await?;
        source.verify_pair_in(writer).await?;
        preflight_motion_image(writer, cancel).await?;
        source.native.admit_current_image_operation(*deadline)?;
        seal::alive(cancel, *deadline)?;
        source.capture_admission.as_mut().unwrap().ready = true;
        Ok(())
    }
    /// Also valid after unpolled/failed admission. Never restores operational
    /// source eligibility or clears a failed admission marker. Total moves only.
    fn split_retained_inputs(&mut self) {
        if !matches!(self.custody, Some(CaptureCustody::Inputs { .. })) { return; }
        let Some(CaptureCustody::Inputs { source, writer, identity, deadline, .. }) = self.custody.take() else { unreachable!() };
        let StageSourceSession { native, proof, witness, reference, capture_admission } = source;
        self.custody = Some(CaptureCustody::Parts(StageCaptureParts {
            native: Some(Native::Session(native)), writer: Some(writer), retained: true, native_started: false, native_deadline: None,
            writer_custody: RetainedWriterCustody::Held, native_result: None,
            proof, witness, reference,
            admission: capture_admission.unwrap_or(CaptureAdmission {
                deadline, ready: false, writer: Some(identity), storage: 0,
            }),
            closure: None, phase: Phase::Failed,
            #[cfg(test)] test_pause: false,
            #[cfg(test)] test_panic: false,
        }));
    }
    fn retained_parts_mut(&mut self) -> anyhow::Result<&mut StageCaptureParts> {
        match self.custody.as_mut() {
            Some(CaptureCustody::Parts(parts)) if parts.retained => Ok(parts),
            _ => anyhow::bail!("retained capture parts missing"),
        }
    }
    pub(crate) fn verify_retained_native_owner(&self) -> anyhow::Result<()> {
        let Some(CaptureCustody::Parts(parts)) = self.custody.as_ref() else {
            anyhow::bail!("retained capture preflight incomplete");
        };
        ensure!(parts.retained && parts.phase == Phase::NativeReady
            && parts.writer_custody == RetainedWriterCustody::Held
            && parts.writer.is_some() && matches!(parts.native, Some(Native::Capturing(_))),
            "retained capture fixed purpose unavailable");
        parts.witness.verify(false)
    }
    pub(crate) fn retained_native_path(&self) -> anyhow::Result<&std::path::PathBuf> {
        self.verify_retained_native_owner()?;
        let Some(CaptureCustody::Parts(parts)) = self.custody.as_ref() else { unreachable!() };
        Ok(&parts.witness.path)
    }
    /// Fixed call directly after the app's pure current+target temporal fence.
    /// No scan, await, P walk, refresh, or reservation construction at this seam.
    pub(crate) fn capture_retained_native(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        let parts = self.retained_parts_mut()?;
        ensure!(parts.phase == Phase::NativeReady && parts.writer_custody == RetainedWriterCustody::Held,
            "retained native capture cannot repeat");
        parts.phase = Phase::NativeAttempted;
        let Some(Native::Capturing(operation)) = parts.native.as_mut() else { unreachable!() };
        match parts.proof.capture_source_workspace94(operation, cancel) {
            Ok(result) => { parts.native_result = Some(result); parts.native_started = true; },
            Err(refusal) => return Err(refusal.into()),
        }
        // The guarded actual result is in custody before inspection. Successful
        // Native output is immediately moved into the correctly ordered Image
        // alternative, before diagnostics, callbacks, awaits or fallible work.
        if parts.native_result.as_ref().unwrap().result().is_ok() {
            let Some(Native::Capturing(operation)) = parts.native.take() else { unreachable!() };
            match operation.finish() {
                Ok(image) => parts.native = Some(Native::Image(image)),
                Err(failure) => {
                    parts.native = Some(Native::Session(failure.into_session()));
                    anyhow::bail!("retained native capture did not complete");
                }
            }
            parts.phase = Phase::NativeCaptured;
            Ok(())
        } else {
            // Inspect the bounded guarded error by borrow only. A fixed
            // category is enough for qualification without cloning, extracting
            // or formatting arbitrary diagnostic contents outside their guard.
            let error = parts.native_result.as_ref().unwrap().result().as_ref().err().unwrap();
            let category = if error.0.contains("allocation budget")
                || error.0.contains("retained output exceeded") {
                "native capture aggregate-budget refusal; guarded result and source retained"
            } else if error.0.contains("deadline") || error.0.contains("cancelled") {
                "native capture deadline-or-cancellation refusal; guarded result and source retained"
            } else {
                "native capture other failure; guarded result and source retained"
            };
            anyhow::bail!(category)
        }
    }
    pub(crate) fn native_captured(&self) -> bool {
        matches!(self.custody.as_ref(), Some(CaptureCustody::Parts(parts))
            if parts.retained && parts.phase == Phase::NativeCaptured
                && matches!(parts.native, Some(Native::Image(_))))
    }
    /// Internal transfer to the SAME ingress settlement graph, never an external
    /// writer extractor. No future construction or check while custody is empty.
    pub(crate) fn terminal_writer(&mut self) -> Option<Transaction<'static, Sqlite>> {
        self.split_retained_inputs();
        let Some(CaptureCustody::Parts(parts)) = self.custody.as_mut() else { return None; };
        if !parts.retained || parts.writer_custody != RetainedWriterCustody::Held { return None; }
        if let Some(Native::Capturing(operation)) = &parts.native {
            parts.native_started |= operation.capture_attempted();
        }
        parts.proof.invalidate_source_workspace94();
        parts.phase = Phase::Closing;
        parts.writer_custody = RetainedWriterCustody::Transferred;
        parts.writer.take()
    }
    pub(crate) fn release_after_exact_close(&mut self) -> anyhow::Result<()> {
        let parts = self.retained_parts_mut()?;
        if parts.writer_custody == RetainedWriterCustody::ReleaseVerified { return Ok(()); }
        ensure!(parts.writer_custody == RetainedWriterCustody::Transferred && parts.writer.is_none(),
            "retained Native release is uncertain or already attempted");
        parts.writer_custody = RetainedWriterCustody::ExactCloseAcknowledged;
        parts.witness.verify(false)?;
        parts.writer_custody = RetainedWriterCustody::ReleaseAttempted;
        drop(parts.native.take());
        parts.writer_custody = RetainedWriterCustody::Released;
        parts.witness.verify(false)?;
        parts.writer_custody = RetainedWriterCustody::ReleaseVerified;
        parts.phase = Phase::Closed;
        Ok(())
    }
    pub(crate) fn native_retained(&self) -> bool {
        match self.custody.as_ref() {
            Some(CaptureCustody::Inputs { .. }) => true,
            Some(CaptureCustody::Parts(parts)) => parts.native.is_some(),
            None => false,
        }
    }
    pub(crate) fn native_deadline(&self) -> Option<Instant> {
        match self.custody.as_ref() {
            Some(CaptureCustody::Parts(parts)) => parts.native_deadline,
            _ => None,
        }
    }
    pub(crate) fn native_custody(&self) -> (bool, bool, bool, bool, bool, bool, bool, bool) {
        match self.custody.as_ref() {
            Some(CaptureCustody::Inputs { .. }) => (true, false, false, false, true, false, false, false),
            Some(CaptureCustody::Parts(parts)) => (
                false, true,
                parts.native_started || matches!(&parts.native, Some(Native::Capturing(operation)) if operation.capture_attempted()),
                matches!(parts.native, Some(Native::Image(_))),
                parts.writer_custody == RetainedWriterCustody::Held && parts.writer.is_some(),
                parts.writer_custody != RetainedWriterCustody::Held,
                matches!(parts.writer_custody, RetainedWriterCustody::ReleaseAttempted | RetainedWriterCustody::Released | RetainedWriterCustody::ReleaseVerified),
                matches!(parts.writer_custody, RetainedWriterCustody::Released | RetainedWriterCustody::ReleaseVerified),
            ),
            None => unreachable!("no observation during total custody moves"),
        }
    }
}

// The same retained capture owner continues through the existing paired-image
// verification algorithm. No ordinary finish/extraction loses its result guard,
// original admission, SQLx close marker or whole94 predecessor.
impl StageImageCaptureOperation {
    pub(crate) fn prepare_retained_pair(&mut self, cancel: Arc<AtomicBool>)
        -> impl std::future::Future<Output = anyhow::Result<()>> + '_
    {
        let start = (|| {
            let parts = self.retained_parts_mut()?;
            ensure!(parts.phase == Phase::NativeCaptured
                && parts.writer_custody == RetainedWriterCustody::Held
                && parts.writer.is_some()
                && matches!(parts.native, Some(Native::Image(_)))
                && parts.native_result.as_ref().is_some_and(|r| r.result().is_ok()),
                "paired capture requires exact retained Native output");
            parts.phase = Phase::PairedRunning;
            Ok(())
        })();
        async move {
            start?;
            let parts = self.retained_parts_mut()?;
            let result = async {
                parts.check_captured(&cancel).await?;
                // Closure is already owned before this fallible postcheck. Its
                // two bounded digest strings are preadmitted by storage().
                // Resample actual writer growth and the unchanged full host
                // graph before the application's NEW authorization purpose.
                let Some(Native::Image(native)) = parts.native.as_mut() else { unreachable!() };
                seal::preflight_retained_captured_extra(native,
                    parts.writer.as_mut().expect("paired original writer"), &mut parts.proof,
                    &parts.witness, &parts.reference, parts.admission.deadline,
                    &cancel, parts.admission.storage).await
            }.await;
            parts.phase = if result.is_ok() { Phase::PairedReady } else { Phase::Failed };
            result
        }
    }
    pub(crate) fn verify_retained_paired_owner(&self) -> anyhow::Result<()> {
        let Some(CaptureCustody::Parts(parts)) = self.custody.as_ref() else {
            anyhow::bail!("paired capture parts missing");
        };
        ensure!(parts.retained && parts.phase == Phase::PairedReady
            && parts.writer_custody == RetainedWriterCustody::Held
            && parts.writer.is_some() && parts.closure.is_some()
            && matches!(parts.native, Some(Native::Image(_)))
            && parts.native_result.as_ref().is_some_and(|r| r.result().is_ok()),
            "paired capture fixed purpose unavailable");
        parts.witness.verify(false)
    }
    pub(crate) fn retained_paired_path(&self) -> anyhow::Result<&std::path::PathBuf> {
        self.verify_retained_paired_owner()?;
        let Some(CaptureCustody::Parts(parts)) = self.custody.as_ref() else { unreachable!() };
        Ok(&parts.witness.path)
    }
    pub(crate) fn complete_retained_pair(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        let parts = self.retained_parts_mut()?;
        // Fixed gates only after genuine app temporal checks. No filesystem,
        // inventory, authorization or accounting scan can follow that fence.
        ensure!(parts.phase == Phase::PairedReady && parts.closure.is_some()
            && parts.writer_custody == RetainedWriterCustody::Held
            && parts.writer.is_some() && matches!(parts.native, Some(Native::Image(_)))
            && parts.native_result.as_ref().is_some_and(|r| r.result().is_ok()),
            "paired capture requires exact verified retained output");
        seal::alive(cancel, parts.admission.deadline)?;
        parts.phase = Phase::PairedCaptured;
        Ok(())
    }
    /// Physical closure custody is descriptive even after a later refusal.
    pub(crate) fn paired_closure_retained(&self) -> bool {
        matches!(self.custody.as_ref(), Some(CaptureCustody::Parts(parts))
            if parts.retained && parts.closure.is_some())
    }
    pub(crate) fn paired_captured(&self) -> bool {
        matches!(self.custody.as_ref(), Some(CaptureCustody::Parts(parts))
            if parts.retained && parts.phase == Phase::PairedCaptured
                && parts.closure.is_some() && matches!(parts.native, Some(Native::Image(_))))
    }
}
