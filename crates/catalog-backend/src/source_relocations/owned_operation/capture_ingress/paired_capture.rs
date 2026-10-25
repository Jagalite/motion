//! Same-owner paired completion. No image extraction, reservation or95 effect.
use super::*;

pub(super) fn storage(path_capacity: usize) -> anyhow::Result<u64> {
    fn reader_frame<F, R>(_: F) -> u64
    where F: FnOnce(&'static mut crate::TargetAuthorizationReader<'static>) -> R, R: Future {
        std::mem::size_of::<R>() as u64
    }
    // A new real reader/close tail coexists with the joined native-purpose tail.
    // Its dynamic frame/path graph is separate; the original typed auth cohort
    // is reusable only after the application's actual current-reader retirement.
    add(&[authorization::storage(path_capacity)?,
        borrowed_ingress_size(RetainedCaptureIngress94::prepare_paired_capture),
        borrowed_ingress_size(RetainedCaptureIngress94::await_paired_authorization_close),
        borrowed_facade_size(crate::SourceImageCaptureOperation::prepare_retained_paired_capture),
        borrowed_facade_size(crate::SourceImageCaptureOperation::retained_paired_authorization),
        borrowed_ingress_size(crate::TargetAuthorizationReader::open_retained_paired_capture),
        reader_frame(crate::TargetAuthorizationReader::close_paired_capture_reader),
        std::mem::size_of::<crate::RetainedPairedCaptureCustody>() as u64,
        std::mem::size_of::<Option<crate::RetainedPairedCaptureCustody>>() as u64,
        4 * std::mem::size_of::<anyhow::Result<()>>() as u64])
}
impl RetainedCaptureIngress94 {
    pub(crate) fn claim_paired_capture(&mut self) -> anyhow::Result<()> {
        ensure!(self.phase == Phase::NativeCaptured && self.native_captured()
            && self.native_authorization.close_joined() && self.native_history_retained(),
            "paired capture requires exact retained Native predecessor");
        alive(self.deadline(), &self.cancel())?;
        self.phase = Phase::PairedClaimed;
        Ok(())
    }
    pub(crate) fn prepare_paired_capture(&mut self) -> impl Future<Output = anyhow::Result<()>> + '_ {
        let allowed = self.phase == Phase::PairedClaimed;
        if allowed { self.phase = Phase::PairedPreflight; }
        let cancel = self.cancel();
        let Self { phase, native_capture, .. } = self;
        // The Stage borrower claims synchronously too; no unpolled future can
        // leave the inner producer Ready or destroy the sole Native output.
        let work = if allowed { Some(native_capture.as_mut().unwrap().prepare_retained_pair(cancel)) } else { None };
        async move {
            let Some(work) = work else { anyhow::bail!("paired capture preflight cannot retry"); };
            let result = work.await;
            *phase = if result.is_ok() { Phase::PairedReady } else { Phase::Failed };
            result
        }
    }
    pub(crate) fn claim_paired_authorization_reader(&mut self) -> anyhow::Result<()> {
        ensure!(self.phase == Phase::PairedReady, "paired capture authentication is one-shot");
        self.phase = Phase::PairedAuthorizing;
        self.verify_paired_authorization_owner()?;
        self.paired_authorization.claim()
    }
    pub(crate) fn verify_paired_authorization_owner(&self) -> anyhow::Result<()> {
        ensure!(self.phase == Phase::PairedAuthorizing && self.native_history_retained(),
            "paired capture original94 custody changed");
        alive(self.deadline(), &self.cancel())?;
        self.native_capture.as_ref().context("paired capture owner missing")?.verify_retained_paired_owner()
    }
    pub(crate) fn paired_authorization_path(&self) -> anyhow::Result<std::path::PathBuf> {
        Ok(self.native_capture.as_ref().context("paired capture owner missing")?.retained_paired_path()?.clone())
    }
    pub(crate) fn paired_authorization_reader_slot(&self) -> anyhow::Result<&RestrictedReaderOwner> {
        Ok(&self.paired_authorization)
    }
    pub(crate) fn paired_authorization_reader_slot_mut(&mut self) -> anyhow::Result<&mut RestrictedReaderOwner> {
        Ok(&mut self.paired_authorization)
    }
    pub(crate) fn begin_paired_authorization_close(&mut self) -> anyhow::Result<()> {
        ensure!(self.phase == Phase::PairedAuthorizing, "paired capture reader close purpose differs");
        self.paired_authorization.reader_mut()?;
        self.phase = Phase::PairedReaderClosing;
        self.paired_authorization.begin_close()
    }
    pub(crate) async fn await_paired_authorization_close(&mut self) -> anyhow::Result<()> {
        ensure!(self.phase == Phase::PairedReaderClosing, "paired capture reader close not claimed");
        self.paired_authorization.join_close().await?;
        ensure!(self.paired_authorization.close_joined(), "paired capture needs actual target-close ACK");
        self.phase = Phase::PairedReaderJoined;
        Ok(())
    }
    pub(crate) fn complete_paired_capture(&mut self) -> anyhow::Result<()> {
        ensure!(self.phase == Phase::PairedReaderJoined && self.paired_authorization.close_joined(),
            "paired capture requires distinct joined purpose token");
        self.phase = Phase::Failed;
        let cancel = self.cancel();
        self.native_capture.as_mut().unwrap().complete_retained_pair(&cancel)?;
        self.paired_completed = true;
        self.phase = Phase::PairedCaptured;
        Ok(())
    }
    pub(crate) fn paired_captured(&self) -> bool {
        self.phase == Phase::PairedCaptured && self.native_capture.as_ref().is_some_and(|c| c.paired_captured())
    }
    pub(crate) fn paired_custody(&self) -> crate::RetainedPairedCaptureCustody {
        let native = self.native_custody();
        crate::RetainedPairedCaptureCustody {
            prepared: self.native_capture.as_ref().is_some_and(|c| c.paired_closure_retained()),
            paired_captured: self.paired_completed,
            target_connect: self.paired_authorization.retains_acquisition(),
            target_reader: self.paired_authorization.retains_reader(),
            target_close: self.paired_authorization.retains_close(),
            target_close_ack: self.paired_authorization.close_joined(),
            native_retained: native.native_retained, writer_held: native.writer_held,
            histories_retained: self.native_history_retained(),
        }
    }
}
