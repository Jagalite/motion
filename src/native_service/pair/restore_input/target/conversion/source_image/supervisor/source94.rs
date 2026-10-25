use super::*;

enum Source94Work {
    Relocate(motion_catalog_backend::SourceRelocation94Request, Source94Goal),
    Reconcile,
    CloseTargetForCapture,
    AcquireCaptureInput,
    CaptureNative,
    CapturePaired,
}
impl RestoreSupervisor {
    /// Borrow exact completed predecessors until worker and timer joins are
    /// positively known. A dropped waiter cannot advance or discard either.
    pub(in crate::native_service) async fn settle_source94_predecessor(
        &mut self,
        expected: target_reservation::TargetPhase,
    ) -> anyhow::Result<()> {
        let shared = self
            .shared
            .as_ref()
            .context("actual service slot missing")?;
        ensure!(
            !self.selection_spawn_failed
                && self.failure.is_none()
                && !self.close_started
                && self.close_failure.is_none()
                && shared.status.load(Ordering::Acquire) == COMPLETE,
            "completed exact source96, ReceiptPending or Ready94 required before source94 retirement"
        );
        {
            let phase = shared.phase.lock().unwrap_or_else(|p| p.into_inner());
            ensure!(
                phase.failure.is_none()
                    && phase.image.is_none()
                    && phase.pair.is_some()
                    && phase
                        .target
                        .as_ref()
                        .is_some_and(|target| target.phase == expected),
                "original Staged target and retired workspace required"
            );
            phase
                .selection
                .as_ref()
                .context("retained selection missing")?
                .target_deadline(&shared.cancel)?;
        }
        if let Some(worker) = self.worker.as_mut() {
            let result = worker.await;
            self.worker = None;
            if let Err(error) = result {
                self.failure = Some(Failure::Error(error.into()));
                shared.status.store(FAILED, Ordering::Release);
                shared.changed.notify_waiters();
                anyhow::bail!(
                    "source94 predecessor worker retirement failed; exact service phase retained"
                );
            }
        }
        if let Some(timer) = self.selection_timer.as_mut() {
            let result = timer.await;
            self.selection_timer = None;
            if let Err(error) = result {
                self.failure = Some(Failure::Error(error.into()));
                shared.status.store(FAILED, Ordering::Release);
                shared.changed.notify_waiters();
                anyhow::bail!(
                    "source94 predecessor timer retirement failed; exact service phase retained"
                );
            }
        }
        shared
            .phase
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .selection
            .as_ref()
            .unwrap()
            .target_deadline(&shared.cancel)?;
        Ok(())
    }

    /// Private finite source94 start. The service owns every phase/resource and
    /// the original caller/deadline; the request may only wait for status.
    pub(in crate::native_service) async fn settle_source96_for_relocation94(
        &mut self,
    ) -> anyhow::Result<()> {
        self.settle_source94_predecessor(target_reservation::TargetPhase::SourceReady)
            .await
    }
    pub(in crate::native_service) async fn settle_relocation94_for_reconcile(
        &mut self,
    ) -> anyhow::Result<()> {
        self.settle_source94_predecessor(target_reservation::TargetPhase::ReceiptPending)
            .await
    }
    pub(in crate::native_service) async fn settle_relocation94_for_capture_input(
        &mut self,
    ) -> anyhow::Result<()> {
        self.settle_source94_predecessor(target_reservation::TargetPhase::Source94Ready)
            .await
    }
    pub(in crate::native_service) fn settle_capture_input_for_ingress(
        &mut self,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        self.settle_source94_predecessor(target_reservation::TargetPhase::Source94CaptureInputReady)
    }
    /// No new worker or timer: only a synchronous, already-admitted move after
    /// both exact predecessor handles have actually been joined.
    pub(in crate::native_service) fn retain_source94_capture_ingress(&mut self) -> anyhow::Result<()> {
        let shared = self.shared.as_ref().context("actual service slot missing")?;
        ensure!(self.worker.is_none() && self.selection_timer.is_none()
            && !self.selection_spawn_failed && self.failure.is_none()
            && !self.close_started && self.close_failure.is_none()
            && shared.status.load(Ordering::Acquire) == COMPLETE,
            "service ingress requires positively joined exact predecessor worker and timer");
        let mut phase = shared.phase.lock().unwrap_or_else(|p| p.into_inner());
        ensure!(phase.failure.is_none() && phase.close_failure.is_none() && phase.image.is_none(),
            "service ingress cannot revive failed or image custody");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let PhaseOwner { target, pair, selection, .. } = &mut *phase;
            target.as_mut().context("service ingress original target missing")?
                .retain_source94_capture_ingress(
                    pair.as_mut().context("service ingress original pair missing")?,
                    selection.as_mut().context("service ingress original selection missing")?, shared)
        }));
        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                phase.failure = Some(Failure::Error(error));
                shared.status.store(FAILED, Ordering::Release);
                shared.changed.notify_waiters();
                anyhow::bail!("service ingress refused; exact available custody retained")
            }
            Err(panic) => {
                phase.failure = Some(Failure::Panic(panic));
                shared.status.store(FAILED, Ordering::Release);
                shared.changed.notify_waiters();
                anyhow::bail!("service ingress panicked; exact available custody retained")
            }
        }
    }
    pub(in crate::native_service) fn begin_source94_capture_acquisition(&mut self) -> anyhow::Result<()> {
        self.begin_source94_work(Source94Work::AcquireCaptureInput)
    }
    pub(in crate::native_service) fn settle_capture_acquisition(&mut self) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        self.settle_source94_predecessor(target_reservation::TargetPhase::Source94CapturePrepared)
    }
    pub(in crate::native_service) fn begin_source94_native_capture(&mut self) -> anyhow::Result<()> {
        self.begin_source94_work(Source94Work::CaptureNative)
    }
    pub(in crate::native_service) fn settle_native_capture(&mut self) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        self.settle_source94_predecessor(target_reservation::TargetPhase::Source94NativeCaptured)
    }
    pub(in crate::native_service) fn begin_source94_paired_capture(&mut self) -> anyhow::Result<()> {
        self.begin_source94_work(Source94Work::CapturePaired)
    }
    pub(in crate::native_service) fn settle_paired_capture(&mut self) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        self.settle_source94_predecessor(target_reservation::TargetPhase::Source94PairedCaptured)
    }
    pub(in crate::native_service) fn begin_source94_capture_input(&mut self) -> anyhow::Result<()> {
        self.begin_source94_work(Source94Work::CloseTargetForCapture)
    }
    pub(in crate::native_service) fn begin_relocation94(
        &mut self,
        request: motion_catalog_backend::SourceRelocation94Request,
    ) -> anyhow::Result<()> {
        self.begin_source94_work(Source94Work::Relocate(request, Source94Goal::Complete))
    }
    pub(in crate::native_service) fn begin_relocation94_to_receipt(
        &mut self,
        request: motion_catalog_backend::SourceRelocation94Request,
    ) -> anyhow::Result<()> {
        self.begin_source94_work(Source94Work::Relocate(request, Source94Goal::ReceiptOnly))
    }
    pub(in crate::native_service) fn begin_reconcile94(&mut self) -> anyhow::Result<()> {
        self.begin_source94_work(Source94Work::Reconcile)
    }
    fn begin_source94_work(
        &mut self,
        work: Source94Work,
    ) -> anyhow::Result<()> {
        let runtime = tokio::runtime::Handle::try_current()?;
        let shared = self
            .shared
            .as_ref()
            .context("actual service slot missing")?;
        ensure!(
            self.worker.is_none()
                && self.selection_timer.is_none()
                && !self.selection_spawn_failed
                && self.failure.is_none()
                && !self.close_started
                && self.close_failure.is_none()
                && shared.status.load(Ordering::Acquire) == COMPLETE,
            "source94 requires completed, explicitly joined exact predecessor worker and timer"
        );
        let mut phase = shared.phase.lock().unwrap_or_else(|p| p.into_inner());
        ensure!(
            phase.failure.is_none() && phase.image.is_none(),
            "service source94 is one-shot"
        );
        let deadline = phase
            .selection
            .as_ref()
            .context("retained selection missing")?
            .target_deadline(&shared.cancel)?;
        {
            let PhaseOwner {
                target,
                pair,
                selection,
                ..
            } = &mut *phase;
            let pair = pair.as_mut().context("original source pair missing")?;
            let selected = selection
                .as_mut()
                .context("original selected owner missing")?;
            let target = target.as_mut().context("original target missing")?;
            let claimed_deadline = match work {
                Source94Work::Relocate(request, goal) =>
                    target.claim_source94(pair, selected, Some(request), goal, shared)?,
                Source94Work::Reconcile =>
                    target.claim_source94(pair, selected, None, Source94Goal::Complete, shared)?,
                Source94Work::CloseTargetForCapture =>
                    target.claim_source94_capture_input(pair, selected, shared)?,
                Source94Work::AcquireCaptureInput =>
                    target.claim_source94_capture_acquisition(pair, selected, shared)?,
                Source94Work::CaptureNative =>
                    target.claim_source94_native_capture(pair, selected, shared)?,
                Source94Work::CapturePaired =>
                    target.claim_source94_paired_capture(pair, selected, shared)?,
            };
            ensure!(claimed_deadline == deadline, "source94 cannot refresh original deadline");
        }
        // The exact source94 owner is installed and claimed before any borrower
        // future/closure is constructed. Reuse the already admitted service task
        // slots only after BOTH previous handles joined, without re-adding H.
        // A preflight failure below is terminal and leaves exact owners retained.
        let retained = shared.clone();
        let deadline_future = async move {
            loop {
                let changed = retained.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if retained.status.load(Ordering::Acquire) != RUNNING {
                    break;
                }
                tokio::select! {
                    _ = changed => {},
                    _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                        retained.cancel.store(true, Ordering::Release);
                        retained.changed.notify_waiters();
                        break;
                    }
                }
            }
        };
        let retained = shared.clone();
        let worker_runtime = runtime.clone();
        let blocking_work = move || {
            // A test-thread subscriber does not follow spawn_blocking. Install
            // only this fixture's bounded capture on the actual worker thread.
            #[cfg(test)]
            let diagnostic_dispatch = failure_diagnostics::dispatch(
                &retained,
                failure_diagnostics::WorkerKind::Source94,
            );
            #[cfg(test)]
            let _diagnostic_guard = diagnostic_dispatch
                .as_ref()
                .map(tracing::dispatcher::set_default);
            let mut phase = retained.phase.lock().unwrap_or_else(|p| p.into_inner());
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let PhaseOwner {
                    target,
                    pair,
                    selection,
                    ..
                } = &mut *phase;
                let work = target
                    .as_mut()
                    .expect("claimed service target")
                    .run_source94(
                        pair.as_mut().expect("original current pair"),
                        selection.as_mut().expect("bound selected owner"),
                        &retained,
                    );
                let frame = u64::try_from(std::mem::size_of_val(&work))?;
                ensure!(
                    frame <= target_reservation::ASYNC_FRAME_LIMIT,
                    "source94 future exceeds admitted frame share"
                );
                #[cfg(test)]
                retained
                    .test
                    .source94_async_frame
                    .store(frame, Ordering::Release);
                worker_runtime.block_on(work)
            }));
            let state = match result {
                Ok(Ok(())) => COMPLETE,
                Ok(Err(error)) => {
                    phase.failure = Some(Failure::Error(error));
                    FAILED
                }
                Err(panic) => {
                    phase.failure = Some(Failure::Panic(panic));
                    FAILED
                }
            };
            let _ = retained.status.compare_exchange(
                RUNNING,
                state,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            retained.changed.notify_waiters();
        };
        if let Err(error) = selection_task_frames(&deadline_future, &blocking_work) {
            phase.failure = Some(Failure::Error(error));
            shared.status.store(FAILED, Ordering::Release);
            shared.changed.notify_waiters();
            anyhow::bail!("source94 task preflight refused; claimed phase retained without retry");
        }
        shared.status.store(RUNNING, Ordering::Release);
        drop(phase);
        let spawned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.selection_timer = Some(runtime.spawn(deadline_future));
            self.worker = Some(runtime.spawn_blocking(blocking_work));
        }));
        if let Err(panic) = spawned {
            self.failure = Some(Failure::Panic(panic));
            self.selection_spawn_failed = true;
            shared.cancel.store(true, Ordering::Release);
            shared.status.store(FAILED, Ordering::Release);
            shared.changed.notify_waiters();
            anyhow::bail!(
                "source94 spawn outcome uncertain; exact phase retained without retry or close"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
pub(in crate::native_service::pair) fn source94_boundary(shared: &Shared, stage: u8) {
    if shared.test.source94_pause.load(Ordering::Acquire) == stage {
        shared.test.source94_reached.store(stage, Ordering::Release);
        shared.changed.notify_waiters();
        let mut released = shared
            .test
            .source94_released
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        while !*released {
            released = shared
                .test
                .source94_release
                .wait(released)
                .unwrap_or_else(|p| p.into_inner());
        }
        assert!(
            !shared.test.source94_panic.load(Ordering::Acquire),
            "injected service94 unwind retains original phase"
        );
    }
}
