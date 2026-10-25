use super::*;
impl RestoreSupervisor {
    /// Borrow the exact completed intent96 worker and timer until both joins are
    /// positively known. Dropping this future cannot start source96 or lose them.
    pub(in crate::native_service) async fn settle_native96_for_source96(
        &mut self,
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
            "completed service intent96 required before source96 retirement"
        );
        {
            let phase = shared.phase.lock().unwrap_or_else(|p| p.into_inner());
            ensure!(
                phase.failure.is_none()
                    && phase.image.is_none()
                    && phase.pair.is_some()
                    && phase.target.as_ref().is_some_and(
                        |target| target.phase == target_reservation::TargetPhase::ReadyIntent96
                    ),
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
                anyhow::bail!("intent96 worker retirement failed; exact service phase retained");
            }
        }
        if let Some(timer) = self.selection_timer.as_mut() {
            let result = timer.await;
            self.selection_timer = None;
            if let Err(error) = result {
                self.failure = Some(Failure::Error(error.into()));
                shared.status.store(FAILED, Ordering::Release);
                shared.changed.notify_waiters();
                anyhow::bail!("intent96 timer retirement failed; exact service phase retained");
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

    /// Private finite source96 start. The service owns every phase/resource and
    /// the original caller/deadline; the request may only wait for status.
    pub(in crate::native_service) fn begin_source96(&mut self) -> anyhow::Result<()> {
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
            "source96 requires completed, explicitly retired intent96 worker and timer"
        );
        let mut phase = shared.phase.lock().unwrap_or_else(|p| p.into_inner());
        ensure!(
            phase.failure.is_none() && phase.image.is_none(),
            "service source96 is one-shot"
        );
        let deadline = phase
            .selection
            .as_ref()
            .context("retained selection missing")?
            .target_deadline(&shared.cancel)?;
        {
            let PhaseOwner { target, pair, .. } = &mut *phase;
            let pair = pair.as_ref().context("original source pair missing")?;
            let target = target.as_mut().context("original target missing")?;
            ensure!(
                target.claim_source96(&pair._stopped, &shared.cancel)? == deadline,
                "source96 cannot refresh original deadline"
            );
        }
        // Native96 is claimed before any borrower future/closure is constructed.
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
                failure_diagnostics::WorkerKind::Source96,
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
                    .run_source96(
                        pair.as_mut().expect("original current pair"),
                        selection.as_mut().expect("bound selected owner"),
                        &retained,
                    );
                let frame = u64::try_from(std::mem::size_of_val(&work))?;
                ensure!(
                    frame <= target_reservation::ASYNC_FRAME_LIMIT,
                    "source96 future exceeds admitted frame share"
                );
                #[cfg(test)]
                retained
                    .test
                    .source96_async_frame
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
            anyhow::bail!("source96 task preflight refused; claimed phase retained without retry");
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
                "source96 spawn outcome uncertain; exact phase retained without retry or close"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
pub(in crate::native_service::pair) fn source96_boundary(shared: &Shared, stage: u8) {
    if shared.test.source96_pause.load(Ordering::Acquire) == stage {
        shared.test.source96_reached.store(stage, Ordering::Release);
        shared.changed.notify_waiters();
        let mut released = shared
            .test
            .source96_released
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        while !*released {
            released = shared
                .test
                .source96_release
                .wait(released)
                .unwrap_or_else(|p| p.into_inner());
        }
        assert!(
            !shared.test.source96_panic.load(Ordering::Acquire),
            "injected service96 unwind retains original phase"
        );
    }
}
