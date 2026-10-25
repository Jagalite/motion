//! Actual service-owned stage→recorded97 lifecycle. Status waiters never own
//! the target, selected files, original writer, workspace or backend operation.
use super::*;

impl RestoreProof {
    /// StoppedOwners is minted only after actual application-task joins, native
    /// terminal admission and both application coordinators close while idle.
    /// Verify that same retained proof, native capability, I/O permit and lock.
    /// No request-supplied shutdown booleans can reach this entry.
    pub(super) fn control_shutdown_facts(
        &self,
    ) -> anyhow::Result<motion_catalog_backend::OfflineControlShutdownFacts> {
        let shared = self.shared.upgrade().context("stopped service released")?;
        ensure!(shared.status.load(Ordering::Acquire) != CLOSED, "service closed");
        let stopped = shared.stopped.lock().unwrap_or_else(|p| p.into_inner());
        let stopped = stopped.as_ref().context("original stopped owners missing")?;
        stopped.app.processing.execution.close_admission_if_idle()?;
        stopped.app.processing.hardware.close_admission_if_idle()?;
        let facts = motion_catalog_backend::OfflineControlShutdownFacts {
            service_admission_stopped: stopped.app.health.shutting_down.load(Ordering::Acquire),
            native_workers_joined: stopped.native.is_some(),
            // The unforgeable StoppedOwners constructor has already joined all
            // registered tasks; this original permit keeps Motion I/O excluded.
            application_owners_drained: stopped._io.num_permits() > 0,
            exclusive_operational_ownership_retained: stopped.lock.verify().is_ok(),
        };
        ensure!(facts.service_admission_stopped && facts.native_workers_joined
            && facts.application_owners_drained && facts.exclusive_operational_ownership_retained,
            "actual stopped service custody no longer complete");
        Ok(facts)
    }
}

impl RestoreSupervisor {
    /// Borrow the exact completed staging worker and timer until both joins are
    /// positively known. Dropping this future cannot start control97 or lose them.
    pub(in crate::native_service) async fn settle_staging_for_control97(
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
            "completed service staging required before control97 retirement"
        );
        {
            let phase = shared.phase.lock().unwrap_or_else(|p| p.into_inner());
            ensure!(
                phase.failure.is_none()
                    && phase.image.is_none()
                    && phase.pair.is_some()
                    && phase.target.as_ref().is_some_and(
                        |target| target.phase == target_reservation::TargetPhase::Staged
                    )
                    && phase
                        .workspace
                        .as_ref()
                        .is_some_and(AuthorizationWorkspace::reusable),
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
                anyhow::bail!("staging worker retirement failed; exact service phase retained");
            }
        }
        if let Some(timer) = self.selection_timer.as_mut() {
            let result = timer.await;
            self.selection_timer = None;
            if let Err(error) = result {
                self.failure = Some(Failure::Error(error.into()));
                shared.status.store(FAILED, Ordering::Release);
                shared.changed.notify_waiters();
                anyhow::bail!("staging timer retirement failed; exact service phase retained");
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


    /// Private finite control97 start. The service owns every phase/resource and
    /// the original caller/deadline; the request may only wait for status.
    pub(in crate::native_service) fn begin_control97(&mut self) -> anyhow::Result<()> {
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
            "control97 requires completed, explicitly retired staging worker and timer"
        );
        let mut phase = shared.phase.lock().unwrap_or_else(|p| p.into_inner());
        ensure!(
            phase.failure.is_none() && phase.image.is_none(),
            "service control97 is one-shot"
        );
        let deadline = phase
            .selection
            .as_ref()
            .context("retained selection missing")?
            .target_deadline(&shared.cancel)?;
        {
            let PhaseOwner {
                target,
                workspace,
                pair,
                ..
            } = &mut *phase;
            let pair = pair.as_ref().context("original source pair missing")?;
            let target = target.as_mut().context("original target missing")?;
            let workspace = workspace
                .as_ref()
                .context("same admitted workspace missing")?;
            ensure!(
                target.claim_control97(workspace, &pair._stopped, &shared.cancel)? == deadline,
                "control97 cannot refresh original deadline"
            );
        }
        // Control97 is claimed before any borrower future/closure is constructed.
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
            let mut phase = retained.phase.lock().unwrap_or_else(|p| p.into_inner());
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let PhaseOwner {
                    target,
                    pair,
                    selection,
                    workspace,
                    ..
                } = &mut *phase;
                let work = target
                    .as_mut()
                    .expect("claimed service target")
                    .run_control97(
                        pair.as_mut().expect("original current pair"),
                        selection.as_mut().expect("bound selected owner"),
                        workspace.as_mut().expect("same admitted workspace"),
                        &retained,
                    );
                let frame = u64::try_from(std::mem::size_of_val(&work))?;
                ensure!(
                    frame <= target_reservation::ASYNC_FRAME_LIMIT,
                    "control97 future exceeds admitted frame share"
                );
                #[cfg(test)]
                retained
                    .test
                    .control97_async_frame
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
            anyhow::bail!("control97 task preflight refused; claimed phase retained without retry");
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
                "control97 spawn outcome uncertain; exact phase retained without retry or close"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
pub(super) fn control97_boundary(shared: &Shared, stage: u8) {
    if shared.test.control97_pause.load(Ordering::Acquire) == stage {
        shared.test.control97_reached.store(stage, Ordering::Release);
        shared.changed.notify_waiters();
        let mut released = shared.test.control97_released.lock().unwrap_or_else(|p| p.into_inner());
        while !*released {
            released = shared.test.control97_release.wait(released).unwrap_or_else(|p| p.into_inner());
        }
        assert!(!shared.test.control97_panic.load(Ordering::Acquire),
            "injected control97 unwind retains exact service custody");
    }
}
