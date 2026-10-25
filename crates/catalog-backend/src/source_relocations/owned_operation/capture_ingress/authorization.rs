//! One fixed authorization-only reader and separately fenced writable acquire
//! and BEGIN. This adds no native capture or writer extraction capability.
use super::*;
use crate::native_bootstrap::TargetAuthorizationReader;
use std::{path::PathBuf, task::{Context, Poll}};

fn reader_frame<F, R>(_: F) -> u64
where F: FnOnce(&'static mut TargetAuthorizationReader<'static>) -> R, R: Future {
    std::mem::size_of::<R>() as u64
}
pub(super) fn storage(path_capacity: usize) -> anyhow::Result<u64> {
    // The inline reader is already inside RetainedCaptureIngress94's size.
    // Retain its two real bounded boxes, the five additional path payloads,
    // fixed options/worker logical nodes and concrete borrowed frame/results.
    // The original64MiB authorization cohort covers the two bounded live readers
    // and auth facts. It is reused, not reminted. Closed historical readers have
    // no live SQLite handle; their retained boxes/path tails stay in original H.
    add(&[
        RestrictedReaderOwner::storage_reservation()
            - std::mem::size_of::<RestrictedReaderOwner>() as u64,
        (path_capacity as u64).checked_mul(5).and_then(|n| n.checked_add(1))
            .context("capture authorization path storage overflow")?,
        // Existing RestrictedReaderOwner uses SQLx0.8.6 defaults:50 command
        // and50 row cells, unlike capture's explicitly single-cell channels.
        // query_only and cache_size are two additional fixed PRAGMA entries.
        // Capture selects the existing owner's fixed uncached path, so no
        // statement-cache entries or SQL-text keys accumulate alongside it.
        NODE * (OPTIONS_NODES + 2 + WORKER_NODES + 2 * 50), WORKER_NAME_BYTES,
        2 * std::mem::size_of::<TargetAuthorizationReader<'static>>() as u64,
        2 * std::mem::size_of::<Result<TargetAuthorizationReader<'static>, anyhow::Error>>() as u64,
        4 * std::mem::size_of::<anyhow::Result<()>>() as u64,
        std::mem::size_of::<crate::source_relocations::RetainedCaptureInputCustody>() as u64,
        std::mem::size_of::<Option<crate::source_relocations::RetainedCaptureInputCustody>>() as u64,
        borrowed_ingress_size(RetainedCaptureIngress94::prepare_authorized_begin),
        borrowed_ingress_size(RetainedCaptureIngress94::finish_authorized_input),
        reader_frame(TargetAuthorizationReader::prepare_capture_begin),
        reader_frame(TargetAuthorizationReader::finish_capture_input),
        reader_frame(TargetAuthorizationReader::close_capture_reader),
        borrowed_ingress_size(TargetAuthorizationReader::open_retained_capture),
        borrowed_facade_size(crate::SourceImageCaptureOperation::retained_input_authorization),
    ])
}
impl RetainedCaptureIngress94 {
    pub(crate) fn custody(&self) -> crate::source_relocations::RetainedCaptureInputCustody {
        crate::source_relocations::RetainedCaptureInputCustody {
            target_connect: self.authorization.retains_acquisition(),
            target_reader: self.authorization.retains_reader(),
            target_close: self.authorization.retains_close(),
            target_close_ack: self.authorization.close_joined(),
            acquire_frame: self.acquire.is_some(),
            acquire_started: self.acquire.as_ref().is_some_and(|a| a.started),
            acquire_failed: self.acquire.as_ref().is_some_and(|a| a.panic.is_some())
                || matches!(self.acquire_result, Some(Err(_))),
            begin_frame: self.begin.is_some(),
            begin_started: self.begin.as_ref().is_some_and(|a| a.started),
            begin_failed: self.begin.as_ref().is_some_and(|a| a.panic.is_some())
                || matches!(self.begin_result, Some(Err(_))),
            writer: self.writer.is_some(),
            prepared: self.prepared(),
        }
    }

    pub(crate) fn claim_authorization_reader(&mut self) -> anyhow::Result<()> {
        ensure!(self.phase == Phase::Ready, "capture authorization is one-shot");
        self.phase = Phase::Authorizing;
        self.verify_authorization_owner()?;
        self.authorization.claim()
    }
    pub(crate) fn verify_authorization_owner(&self) -> anyhow::Result<()> {
        ensure!(matches!(self.phase, Phase::Authorizing | Phase::AcquisitionPreflight | Phase::AcquisitionReady
            | Phase::Acquired | Phase::PreparingBegin | Phase::BeginReady),
            "capture authorization owner is not in its fixed open-reader phase");
        alive(self.deadline(), &self.cancel())?;
        ensure!(self.owner.capture_input_ready(), "capture authorization lost positive94 predecessor");
        self.owner.source.as_ref().context("capture original source missing")?
            .inner.capture_ingress_path94()?;
        Ok(())
    }
    pub(crate) fn authorization_path(&self) -> anyhow::Result<PathBuf> {
        Ok(self.owner.source.as_ref().context("capture original source missing")?
            .inner.capture_ingress_path94()?.clone())
    }
    pub(crate) fn authorization_reader_slot(&self) -> anyhow::Result<&RestrictedReaderOwner> {
        Ok(&self.authorization)
    }
    pub(crate) fn authorization_reader_slot_mut(&mut self) -> anyhow::Result<&mut RestrictedReaderOwner> {
        Ok(&mut self.authorization)
    }
    pub(crate) fn preflight_authorized_acquire(&mut self) -> anyhow::Result<()> {
        ensure!(self.phase == Phase::Authorizing, "capture acquire preflight cannot retry");
        self.phase = Phase::AcquisitionPreflight;
        self.verify_authorization_owner()?;
        self.authorization.reader_mut()?;
        let cancel = self.cancel();
        let host = self.owner.admission.capture_ingress.as_ref().unwrap().host;
        let deadline = self.deadline();
        self.owner.source.as_mut().unwrap().inner
            .retain_capture_ingress94(host, deadline, &cancel)?;
        // Real acquire box is installed but unpolled BEFORE fresh SQL auth.
        self.start_acquire()?;
        self.phase = Phase::AcquisitionReady;
        Ok(())
    }
    pub(crate) fn poll_authorized_acquire(&mut self, cx: &mut Context<'_>) -> Poll<anyhow::Result<()>> {
        // No filesystem/hash/SQL scans before this exact effect-capable poll.
        if self.phase != Phase::AcquisitionReady {
            return Poll::Ready(Err(anyhow::anyhow!("capture acquire needs its exact claimed frame")));
        }
        if let Err(error) = alive(self.deadline(), &self.cancel()) {
            self.phase = Phase::Failed;
            return Poll::Ready(Err(error));
        }
        match self.acquire.as_mut().expect("preflight retained acquire").poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => { self.phase = Phase::Failed; Poll::Ready(Err(error)) }
            Poll::Ready(Ok(())) => {
                let attempt = self.acquire.as_mut().unwrap();
                if let Some(result) = attempt.output.take() {
                    match result {
                        Ok(connection) => { self.connection = Some(connection); self.acquire_result = Some(Ok(())); }
                        Err(error) => self.acquire_result = Some(Err(error)),
                    }
                }
                if matches!(self.acquire_result, Some(Ok(()))) {
                    self.phase = Phase::Acquired;
                    Poll::Ready(Ok(()))
                } else {
                    self.phase = Phase::Failed;
                    Poll::Ready(Err(anyhow::anyhow!("capture authorized acquire failed; original custody retained")))
                }
            }
        }
    }
    pub(crate) fn prepare_authorized_begin(&mut self) -> impl Future<Output = anyhow::Result<()>> + '_ {
        let allowed = self.phase == Phase::Acquired;
        if allowed { self.phase = Phase::PreparingBegin; }
        async move {
            ensure!(allowed, "capture BEGIN preflight is one-shot");
            let cancel = self.cancel();
            alive(self.deadline(), &cancel)?;
            self.identity = Some(self.owner.source.as_ref().unwrap().inner
                .verify_capture_connection94(self.connection.as_mut().unwrap()).await?);
            alive(self.deadline(), &cancel)?;
            self.configure = Some(Attempt::new(configure_frame(self.connection.take().unwrap())));
            self.finish_configure().await?;
            alive(self.deadline(), &cancel)?;
            self.begin = Some(Attempt::boxed(Transaction::begin(self.connection.take().unwrap(),
                Some(std::borrow::Cow::Borrowed("BEGIN IMMEDIATE")))));
            ensure!(std::mem::size_of_val(self.begin.as_ref().unwrap().frame.as_ref().get_ref()) as u64
                <= BEGIN_FRAME_STORAGE, "capture BEGIN frame oversized; unpolled exact owner retained");
            self.phase = Phase::BeginReady;
            Ok(())
        }
    }
    pub(crate) fn begin_authorization_close(&mut self) -> anyhow::Result<()> {
        ensure!(self.phase == Phase::BeginReady, "capture reader close requires same preflighted BEGIN");
        self.authorization.reader_mut()?;
        self.authorization.begin_close()
    }
    pub(crate) async fn await_authorization_close(&mut self) -> anyhow::Result<()> {
        self.authorization.join_close().await?;
        ensure!(self.authorization.close_joined(), "capture BEGIN requires actual reader close ACK");
        Ok(())
    }
    pub(crate) fn poll_authorized_begin(&mut self, cx: &mut Context<'_>) -> Poll<anyhow::Result<()>> {
        if !matches!(self.phase, Phase::BeginReady | Phase::Beginning) || !self.authorization.close_joined() {
            return Poll::Ready(Err(anyhow::anyhow!("capture BEGIN needs its exact reader close ACK")));
        }
        if let Err(error) = alive(self.deadline(), &self.cancel()) {
            self.phase = Phase::Failed;
            return Poll::Ready(Err(error));
        }
        self.phase = Phase::Beginning;
        match self.begin.as_mut().expect("retained exact BEGIN").poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => { self.phase = Phase::Failed; Poll::Ready(Err(error)) }
            Poll::Ready(Ok(())) => {
                if let Some(result) = self.begin.as_mut().unwrap().output.take() {
                    match result {
                        Ok(writer) => { self.writer = Some(writer); self.begin_result = Some(Ok(())); }
                        Err(error) => self.begin_result = Some(Err(error)),
                    }
                }
                if matches!(self.begin_result, Some(Ok(()))) {
                    self.phase = Phase::Begun;
                    Poll::Ready(Ok(()))
                } else {
                    self.phase = Phase::Failed;
                    Poll::Ready(Err(anyhow::anyhow!("capture BEGIN failed; exact library custody remains uncertain")))
                }
            }
        }
    }
    pub(crate) fn finish_authorized_input(&mut self) -> impl Future<Output = anyhow::Result<()>> + '_ {
        let allowed = self.phase == Phase::Begun;
        if allowed { self.phase = Phase::Verifying; }
        async move {
            ensure!(allowed, "capture verification cannot retry or grant native capture");
            let cancel = self.cancel();
            let deadline = self.deadline();
            alive(deadline, &cancel)?;
            let result = self.owner.source.as_mut().unwrap().inner.admit_capture_ingress_writer94(
                self.writer.as_mut().unwrap(), deadline, &cancel).await;
            let result = result.and_then(|()| alive(deadline, &cancel));
            self.phase = if result.is_ok() { Phase::Prepared } else { Phase::Failed };
            result
        }
    }
}
