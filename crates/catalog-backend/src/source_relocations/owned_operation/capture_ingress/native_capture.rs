//! New fixed native-auth purpose. The acquisition reader remains Joined and
//! untouched; this separately admitted owner keeps every new open/close tail.
use super::*;
use crate::offline_control_upgrade::StageImageCaptureOperation;

pub(super) fn storage(path_capacity: usize) -> anyhow::Result<u64> {
    fn reader_frame<F, R>(_: F) -> u64
    where F: FnOnce(&'static mut crate::TargetAuthorizationReader<'static>) -> R, R: Future {
        std::mem::size_of::<R>() as u64
    }
    add(&[authorization::storage(path_capacity)?, StageImageCaptureOperation::retained_storage()?,
        borrowed_ingress_size(RetainedCaptureIngress94::prepare_native_capture),
        borrowed_ingress_size(RetainedCaptureIngress94::await_native_authorization_close),
        borrowed_facade_size(crate::SourceImageCaptureOperation::prepare_retained_native_capture),
        borrowed_facade_size(crate::SourceImageCaptureOperation::retained_native_authorization),
        borrowed_ingress_size(crate::TargetAuthorizationReader::open_retained_native_capture),
        reader_frame(crate::TargetAuthorizationReader::close_native_capture_reader),
        // Four bridge return destinations coexist with the installed Native
        // result; the payload guard is separate, never an H subtraction.
        4 * std::mem::size_of::<Result<catabolic_store::migration::offline_bootstrap::OfflineRetainedResult<()>,
            catabolic_store::migration::offline_bootstrap::OfflineWorkspaceAdmissionRefused>>() as u64,
        std::mem::size_of::<crate::RetainedNativeCaptureCustody>() as u64,
        std::mem::size_of::<Option<crate::RetainedNativeCaptureCustody>>() as u64,
        4 * std::mem::size_of::<anyhow::Result<()>>() as u64])
}
impl RetainedCaptureIngress94 {
    pub(super) fn native_history_retained(&self) -> bool {
        // The whole94 metadata remains here, but its emptied source slot is
        // not positive custody by itself: exactly one real source destination
        // must exist (old source or the installed Stage Inputs/Parts owner).
        (self.owner.source.is_some() != self.native_capture.is_some())
            && self.owner.active.is_none() && self.owner.completed && self.owner.capture_history_complete()
            && self.owner.close.is_none()
            && self.owner.target.as_ref().is_some_and(|target| target.settled() && target.closed())
    }
    pub(crate) fn claim_native_capture(&mut self) -> anyhow::Result<()> {
        ensure!(self.phase == Phase::Prepared && self.native_capture.is_none()
            && self.owner.capture_input_ready() && self.writer.is_some() && self.identity.is_some()
            && self.owner.source.as_ref().is_some_and(|source| source.owned94.is_none())
            && self.authorization.close_joined(), "native capture requires exact prepared ingress");
        alive(self.deadline(), &self.cancel())?;
        let deadline = self.deadline();
        let identity = self.identity.unwrap();
        self.phase = Phase::NativeClaimed;
        // All relevant destination layouts were admitted at original ingress.
        // No check/callback/allocation occurs between these total field moves.
        let source = self.owner.source.take().unwrap();
        let writer = self.writer.take().unwrap();
        self.native_capture = Some(StageImageCaptureOperation::retain_inputs(source.inner, writer, identity, deadline));
        Ok(())
    }
    pub(crate) fn prepare_native_capture(&mut self) -> impl Future<Output = anyhow::Result<()>> + '_ {
        let allowed = self.phase == Phase::NativeClaimed;
        if allowed { self.phase = Phase::NativePreflight; }
        let cancel = self.cancel();
        let Self { phase, native_capture, .. } = self;
        // Construct the real Stage admission borrower NOW. Its synchronous
        // marker is protected by installed Inputs even if never first-polled.
        let work = if allowed {
            Some(native_capture.as_mut().unwrap().prepare_retained_native(cancel))
        } else { None };
        async move {
            let Some(work) = work else { anyhow::bail!("native capture preflight cannot retry"); };
            let result = work.await;
            *phase = if result.is_ok() { Phase::NativeReady } else { Phase::Failed };
            result
        }
    }
    pub(crate) fn claim_native_authorization_reader(&mut self) -> anyhow::Result<()> {
        ensure!(self.phase == Phase::NativeReady, "native capture authentication is one-shot");
        self.phase = Phase::NativeAuthorizing;
        self.verify_native_authorization_owner()?;
        self.native_authorization.claim()
    }
    pub(crate) fn verify_native_authorization_owner(&self) -> anyhow::Result<()> {
        ensure!(self.phase == Phase::NativeAuthorizing && self.owner.source.is_none()
            && self.native_history_retained(), "native capture original94 custody changed");
        alive(self.deadline(), &self.cancel())?;
        self.native_capture.as_ref().context("native capture owner missing")?.verify_retained_native_owner()
    }
    pub(crate) fn native_authorization_path(&self) -> anyhow::Result<std::path::PathBuf> {
        Ok(self.native_capture.as_ref().context("native capture owner missing")?.retained_native_path()?.clone())
    }
    pub(crate) fn native_authorization_reader_slot(&self) -> anyhow::Result<&RestrictedReaderOwner> {
        Ok(&self.native_authorization)
    }
    pub(crate) fn native_authorization_reader_slot_mut(&mut self) -> anyhow::Result<&mut RestrictedReaderOwner> {
        Ok(&mut self.native_authorization)
    }
    pub(crate) fn begin_native_authorization_close(&mut self) -> anyhow::Result<()> {
        ensure!(self.phase == Phase::NativeAuthorizing, "native capture reader close purpose differs");
        self.native_authorization.reader_mut()?;
        self.phase = Phase::NativeReaderClosing;
        self.native_authorization.begin_close()
    }
    pub(crate) async fn await_native_authorization_close(&mut self) -> anyhow::Result<()> {
        ensure!(self.phase == Phase::NativeReaderClosing, "native capture reader close not claimed");
        self.native_authorization.join_close().await?;
        ensure!(self.native_authorization.close_joined(), "native capture needs actual new target-close ACK");
        self.phase = Phase::NativeReaderJoined;
        Ok(())
    }
    pub(crate) fn capture_native(&mut self) -> anyhow::Result<()> {
        // Fixed pure gates only after application auth fence. Never call the
        // filesystem/pair/history/P verifiers here, or construct a reservation.
        ensure!(self.phase == Phase::NativeReaderJoined && self.native_authorization.close_joined(),
            "native capture requires its own joined purpose token");
        self.phase = Phase::NativeAttempted;
        let cancel = self.cancel();
        let result = self.native_capture.as_mut().unwrap().capture_retained_native(&cancel);
        self.phase = if result.is_ok() { Phase::NativeCaptured } else { Phase::Failed };
        result
    }
    pub(crate) fn native_captured(&self) -> bool {
        self.phase == Phase::NativeCaptured && self.native_capture.as_ref().is_some_and(StageImageCaptureOperation::native_captured)
    }
    pub(crate) fn native_custody(&self) -> crate::RetainedNativeCaptureCustody {
        let (inputs, parts, native_attempted, native_captured, writer_held, writer_handed_back,
            release_attempted, released) = self.native_capture.as_ref()
            .map(StageImageCaptureOperation::native_custody).unwrap_or((false, false, false, false, false, false, false, false));
        crate::RetainedNativeCaptureCustody {
            native_deadline: self.native_capture.as_ref().and_then(StageImageCaptureOperation::native_deadline),
            native_retained: self.native_capture.as_ref().is_some_and(StageImageCaptureOperation::native_retained),
            inputs, parts, target_connect: self.native_authorization.retains_acquisition(),
            target_reader: self.native_authorization.retains_reader(), target_close: self.native_authorization.retains_close(),
            target_close_ack: self.native_authorization.close_joined(), native_attempted, native_captured,
            writer_held, writer_handed_back, exact_close_ack: matches!(self.close_result, Some(Ok(()))),
            release_attempted, released, histories_retained: self.native_history_retained(),
        }
    }
}
