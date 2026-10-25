//! One actual NativeService-owned restore slot. Requests only wait for status.
//! Stop and initial export custody stay inline until the native export admits
//! this slot's fixed storage. No request cancellation path allocates in Drop or
//! transfers custody to a process-global sink. This remains an unactivated seam:
//! dropping the service owner instead of explicitly closing it is not settlement.
use super::*;
use crate::native_catalog::terminal::PairExportCompletion;
use crate::native_service::pair::restore_input::selected::{
    SERVICE_INPUT_PATH_STORAGE, ServiceSelection,
};
use crate::native_service::{Owners, StoppedOwners, StoppedService, TerminalOwner};
use std::sync::{
    Mutex, Weak,
    atomic::{AtomicU8, AtomicU64},
};
use std::time::Instant;
mod control97;
#[cfg(test)]
mod failure_diagnostics;
mod native96;
pub(in crate::native_service::pair) mod source94;
mod source96;
#[cfg(test)]
pub(in crate::native_service::pair) use native96::native96_boundary;
#[cfg(test)]
mod selection_tests;
mod target_reservation;
use target_reservation::{AuthorizationWorkspace, ServiceTarget};
use tokio::{sync::Notify, task::JoinHandle};

/// Opaque actual service handoff, constructed only after the original Ready97
/// phase and real workspace close ACK are checked together under Shared.phase.
pub(in crate::native_service::pair) struct ServiceRecorded97 {
    ready: motion_catalog_backend::RecordedAdmittedControl,
    workspace: AuthorizationWorkspace,
}
impl ServiceRecorded97 {
    pub(in crate::native_service::pair) fn into_parts(
        self,
    ) -> (
        motion_catalog_backend::RecordedAdmittedControl,
        AuthorizationWorkspace,
    ) {
        (self.ready, self.workspace)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::native_service) enum Source94Goal {
    Complete,
    ReceiptOnly,
}

const IDLE: u8 = 0;
const RUNNING: u8 = 1;
const COMPLETE: u8 = 2;
const FAILED: u8 = 3;
const CLOSED: u8 = 4;

// Named logical task-storage cohort, admitted with the actual service layout
// BEFORE pair acquisition. Check the real unpolled future/closure frames before
// spawn. Two copies cover construction/handoff layout overlap without assuming
// a move erases stack storage. Opaque Tokio task bookkeeping has a separate
// bounded logical share: this is NOT its measured allocation or an RSS proof.
const SELECTION_DEADLINE_FRAME_LIMIT: u64 = 2 * 1024;
const SELECTION_WORKER_FRAME_LIMIT: u64 = 256;
const SELECTION_TOKIO_TASK_SHARE: u64 = 8 * 1024;
const SELECTION_TASK_STORAGE: u64 = 2 * SELECTION_DEADLINE_FRAME_LIMIT
    + 2 * SELECTION_WORKER_FRAME_LIMIT
    + SELECTION_TOKIO_TASK_SHARE;

fn selection_task_frames<T, U>(deadline: &T, worker: &U) -> anyhow::Result<(u64, u64)> {
    let deadline = u64::try_from(std::mem::size_of_val(deadline))?;
    let worker = u64::try_from(std::mem::size_of_val(worker))?;
    ensure!(
        deadline <= SELECTION_DEADLINE_FRAME_LIMIT,
        "selected deadline future exceeds its preadmitted frame share"
    );
    ensure!(
        worker <= SELECTION_WORKER_FRAME_LIMIT,
        "selected blocking closure exceeds its preadmitted frame share"
    );
    Ok((deadline, worker))
}

enum Failure {
    Error(anyhow::Error),
    Panic(Box<dyn std::any::Any + Send>),
}
struct PhaseOwner {
    target: Option<ServiceTarget>,
    workspace: Option<AuthorizationWorkspace>,
    selection: Option<ServiceSelection>,
    pair: Option<PreparedPair>,
    image: Option<Custody>,
    failure: Option<Failure>,
    close_failure: Option<Failure>,
}
impl Drop for PhaseOwner {
    fn drop(&mut self) {
        // Service-owner destruction is NOT successful asynchronous settlement.
        // Avoid the legacy PostRelocationPairImage/NativeConversionStage global
        // recovery sinks even at this explicitly unsupported activation boundary.
        if let Some(image) = &mut self.image {
            dismantle_stage(&mut image.stage);
        }
    }
}
pub(in crate::native_service::pair) struct Shared {
    // Writer/image resources must release before original stopped source owners.
    phase: Mutex<PhaseOwner>,
    stopped: Mutex<Option<StoppedOwners>>,
    cancel: Arc<AtomicBool>,
    status: AtomicU8,
    changed: Notify,
    storage: u64,
    // Additional selected metadata/path high water. The fixed caller-buffer
    // share is already in storage and must never be included here again.
    selection_storage: AtomicU64,
    // A separate named cohort admitted once after selected completion, before
    // any authentication reader. Never subtracted or charged per later phase.
    authorization_storage: AtomicU64,
    #[cfg(test)]
    test: TestBoundary,
}
#[cfg(test)]
#[derive(Default)]
struct TestBoundary {
    failure_diagnostics: Mutex<Option<failure_diagnostics::Capture>>,
    pause: AtomicBool,
    published: AtomicBool,
    release: Notify,
    publication_fd: std::sync::atomic::AtomicI32,
    selection_pause: AtomicU8,
    selection_reached: AtomicU8,
    selection_release: Notify,
    selection_panic: AtomicBool,
    selection_fd: std::sync::atomic::AtomicI32,
    selection_close_failure: AtomicBool,
    selection_deadline_frame: AtomicU64,
    selection_worker_frame: AtomicU64,
    target_pause: AtomicU8,
    target_reached: AtomicU8,
    target_release: std::sync::Condvar,
    target_released: Mutex<bool>,
    target_panic: AtomicBool,
    target_reservation_sync_failure: AtomicBool,
    target_async_frame: AtomicU64,
    target_join_pause: AtomicBool,
    target_join_reached: AtomicBool,
    target_join_release: std::sync::Condvar,
    target_join_released: Mutex<bool>,
    staging_async_frame: AtomicU64,
    staging_pause: AtomicU8,
    staging_reached: AtomicU8,
    staging_release: std::sync::Condvar,
    staging_released: Mutex<bool>,
    staging_panic: AtomicBool,
    staging_sync_failure: AtomicU8,
    staging_prepare_failure: AtomicU8,
    staging_fds: [std::sync::atomic::AtomicI32; 3],
    control97_async_frame: AtomicU64,
    control97_pause: AtomicU8,
    control97_reached: AtomicU8,
    control97_release: std::sync::Condvar,
    control97_released: Mutex<bool>,
    control97_panic: AtomicBool,
    native96_pause: AtomicU8,
    native96_reached: AtomicU8,
    native96_release: std::sync::Condvar,
    native96_released: Mutex<bool>,
    native96_panic: AtomicBool,
    native96_async_frame: AtomicU64,
    source96_pause: AtomicU8,
    source96_reached: AtomicU8,
    source96_release: std::sync::Condvar,
    source96_released: Mutex<bool>,
    source96_panic: AtomicBool,
    source96_inner_panic: AtomicU8,
    source96_unpolled: AtomicU8,
    source96_async_frame: AtomicU64,
    source94_pause: AtomicU8,
    source94_reached: AtomicU8,
    source94_release: std::sync::Condvar,
    source94_released: Mutex<bool>,
    source94_panic: AtomicBool,
    source94_unpolled: AtomicU8,
    source94_unprepared_saved: AtomicU8,
    source94_async_frame: AtomicU64,
    source94_native_deadline: Mutex<Option<Instant>>,
    source94_paired_auth_pause: AtomicU8,
    source94_paired_auth_entered: Arc<AtomicBool>,
    source94_paired_auth_gate: Arc<Notify>,
    selection_join_pause: AtomicBool,
    selection_join_reached: AtomicBool,
    selection_join_release: std::sync::Condvar,
    selection_join_released: Mutex<bool>,
}
impl Shared {
    #[cfg(test)]
    pub(in crate::native_service::pair::restore_input::target::conversion) fn unpolled_capture_target_prepare94(&self) -> bool {
        self.test.source94_unpolled.load(Ordering::Acquire) == 7
    }
    #[cfg(test)]
    pub(in crate::native_service::pair::restore_input::target::conversion) fn unpolled_capture_target_close94(&self) -> bool {
        self.test.source94_unpolled.load(Ordering::Acquire) == 6
    }
    #[cfg(test)]
    pub(in crate::native_service::pair::restore_input::target::conversion) fn record_native_capture_deadline94(&self, deadline: Option<Instant>) {
        *self.test.source94_native_deadline.lock().unwrap_or_else(|p| p.into_inner()) = deadline;
    }
    #[cfg(test)]
    pub(in crate::native_service::pair::restore_input::target::conversion) fn unpolled_native_capture94(&self, stage: u8) -> bool {
        self.test.source94_unpolled.load(Ordering::Acquire) == stage
    }
    #[cfg(test)]
    pub(in crate::native_service::pair::restore_input::target::conversion) fn unpolled_paired_capture94(&self, stage: u8) -> bool {
        self.test.source94_unpolled.load(Ordering::Acquire) == stage
    }
    #[cfg(test)]
    pub(in crate::native_service::pair::restore_input::target::conversion) fn paired_capture_auth_observation94(
        &self, stage: u8,
    ) -> Option<crate::v2::auth::retained::PreparationObservation> {
        (self.test.source94_paired_auth_pause.load(Ordering::Acquire) == stage).then(|| {
            crate::v2::auth::retained::PreparationObservation {
                entered: Arc::clone(&self.test.source94_paired_auth_entered),
                gate: Some(Arc::clone(&self.test.source94_paired_auth_gate)),
                fail: false, panic: false,
            }
        })
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn unprepared_saved94(
        &self, purpose: motion_catalog_backend::SourceRelocation94Purpose,
    ) -> bool {
        use motion_catalog_backend::SourceRelocation94Purpose as Purpose;
        self.test.source94_unprepared_saved.load(Ordering::Acquire)
            == match purpose { Purpose::Intent => 1, Purpose::Dispatch => 2, Purpose::Ack => 3, Purpose::Reconcile => 4 }
    }
    pub(super) async fn after_publication(
        &self,
        _publication: &reservation::ReservationPublication,
    ) -> anyhow::Result<()> {
        #[cfg(test)]
        if self.test.pause.load(Ordering::Acquire) {
            let (_, _, fd) = _publication.original_publication();
            self.test.publication_fd.store(fd, Ordering::Release);
            self.test.published.store(true, Ordering::Release);
            self.changed.notify_waiters();
            self.test.release.notified().await;
        }
        Ok(())
    }
}
/// A weak reference to the actual source-owner slot, not a descriptive identity.
pub(in crate::native_service) struct RestoreProof {
    shared: Weak<Shared>,
}
impl RestoreProof {
    pub(in crate::native_service) fn verify_lock(&self) -> anyhow::Result<()> {
        let shared = self
            .shared
            .upgrade()
            .context("restore service owner released")?;
        ensure!(
            shared.status.load(Ordering::Acquire) != CLOSED,
            "restore service closed"
        );
        let stopped = shared.stopped.lock().unwrap_or_else(|p| p.into_inner());
        stopped
            .as_ref()
            .context("actual stopped source owner missing")?
            .lock
            .verify()
    }
    pub(in crate::native_service) fn source_parents(&self) -> anyhow::Result<(File, File)> {
        let shared = self
            .shared
            .upgrade()
            .context("restore service owner released")?;
        let stopped = shared.stopped.lock().unwrap_or_else(|p| p.into_inner());
        crate::native_service::stopped_source_parents(
            stopped.as_ref().context("actual stopped owner missing")?,
        )
    }
    pub(in crate::native_service) fn source_db(&self) -> anyhow::Result<SqlitePool> {
        let shared = self
            .shared
            .upgrade()
            .context("restore service owner released")?;
        let stopped = shared.stopped.lock().unwrap_or_else(|p| p.into_inner());
        Ok(stopped
            .as_ref()
            .context("actual stopped source owner missing")?
            .app
            .db
            .clone())
    }
    pub(in crate::native_service) fn retained_storage(&self) -> anyhow::Result<u64> {
        let shared = self
            .shared
            .upgrade()
            .context("restore service owner released")?;
        shared
            .storage
            .checked_add(shared.selection_storage.load(Ordering::Acquire))
            .and_then(|n| n.checked_add(shared.authorization_storage.load(Ordering::Acquire)))
            .context("service selected/authorization storage overflow")
    }
}
/// Opaque named admission provenance. Only admit_target_authorization below
/// constructs it after measuring this same pair and installing its named cohort.
/// No scalar/DTO/Clone constructor.
/// The finite control97 bridge borrows this same workspace with its original
/// target and stopped owner. It cannot extract an independently reusable grant.
pub(in crate::native_service::pair) struct ServiceAuthorizationAdmission {
    shared: Weak<Shared>,
}
impl ServiceAuthorizationAdmission {
    pub(in crate::native_service::pair) fn verify(
        &self,
        stopped: &StoppedService,
    ) -> anyhow::Result<()> {
        let proof = stopped
            .supervised
            .as_ref()
            .context("actual service stopped proof required")?;
        ensure!(
            Weak::ptr_eq(&self.shared, &proof.shared),
            "authorization workspace belongs to another service"
        );
        let shared = self
            .shared
            .upgrade()
            .context("authorization service owner released")?;
        ensure!(
            shared.authorization_storage.load(Ordering::Acquire)
                == AuthorizationWorkspace::service_storage_reservation()
                && shared.status.load(Ordering::Acquire) != CLOSED,
            "named service authorization admission is unavailable"
        );
        Ok(())
    }
}

/// Runs only in the retained blocking coordinator, with the original phase
/// exclusively borrowed. The finished export/selection scratch no longer needs
/// the future auth cohort. This exact measured admission precedes reader open;
/// it preserves all previous counters and never obtains budget from generic H.
fn admit_target_authorization(
    shared: &Arc<Shared>,
    pair: &mut PreparedPair,
    selection: &ServiceSelection,
) -> anyhow::Result<AuthorizationWorkspace> {
    selection.target_deadline(&shared.cancel)?;
    let proof = pair
        ._stopped
        .supervised
        .as_ref()
        .context("actual service proof missing")?;
    ensure!(
        Weak::ptr_eq(&proof.shared, &Arc::downgrade(shared)),
        "authorization admission belongs to another pair"
    );
    ensure!(
        shared.authorization_storage.load(Ordering::Acquire) == 0,
        "named authorization cohort is one-shot"
    );
    {
        let stopped = shared.stopped.lock().unwrap_or_else(|p| p.into_inner());
        let stopped = stopped
            .as_ref()
            .context("original stopped owners missing")?;
        ensure!(
            stopped.native.is_some(),
            "original idle native terminal owner missing"
        );
        stopped.lock.verify()?;
    }
    let additional = AuthorizationWorkspace::service_storage_reservation();
    // VerifiedExport.retained_allocations measures the original Store's live
    // cache/schema/statements AFTER export temporaries drop, not export length.
    // The same TerminalOwner stays closed, idle and retained in Shared.stopped:
    // selection and target use files, never original native host/SQL operations.
    // PreparedPair adds that frozen cache observation plus existing service
    // counters. Export/audit metadata keeps the existing OVERHEAD convention;
    // full exported images are on anonymous FDs, not resident Vec copies here.
    // The writer's lexical status-measurement scratch was preadmitted in the
    // fixed target host share before export. This is not an allocator/RSS bound.
    let measured = pair
        ._writer
        .retained_allocations_blocking()?
        .checked_add(pair.retained_native_allocations()?)
        .and_then(|n| n.checked_add(OVERHEAD))
        .and_then(|n| n.checked_add(additional))
        .context("target authorization admission overflow")?;
    ensure!(
        measured <= 256 * 1024 * 1024,
        "target authorization cohort exceeds retained budget"
    );
    selection.target_deadline(&shared.cancel)?;
    // Both operations below are pure ownership installation. No reader, path
    // observation, future poll or fallible operation can intervene.
    shared
        .authorization_storage
        .store(additional, Ordering::Release);
    Ok(AuthorizationWorkspace::for_service(
        ServiceAuthorizationAdmission {
            shared: Arc::downgrade(shared),
        },
    ))
}

/// Constructed inline by real NativeService::start, not by a restore request.
#[derive(Default)]
pub(in crate::native_service) struct RestoreSupervisor {
    stopping: bool,
    stopped: Option<StoppedOwners>,
    terminal_task: Option<JoinHandle<anyhow::Result<TerminalOwner>>>,
    writer: Option<FreshWriter>,
    pair_task: Option<JoinHandle<PairExportCompletion<ImageParts>>>,
    pair_completion: Option<PairExportCompletion<ImageParts>>,
    export_storage: Option<u64>,
    shared: Option<Arc<Shared>>,
    worker: Option<JoinHandle<()>>,
    selection_timer: Option<JoinHandle<()>>,
    // A spawn unwind need not prove that the runtime accepted no task. Without
    // its returned handle, do not close or reconstruct the original phase.
    selection_spawn_failed: bool,
    failure: Option<Failure>,
    close_started: bool,
    close_failure: Option<Failure>,
    terminal_close_task: Option<JoinHandle<anyhow::Result<()>>>,
    terminal_close_started: bool,
    terminal_closed: bool,
    inline_closed: bool,
}
impl RestoreSupervisor {
    pub(in crate::native_service) fn active(&self) -> bool {
        self.stopping
    }

    pub(in crate::native_service) async fn stop(
        &mut self,
        owner_slot: &mut Option<Owners>,
    ) -> anyhow::Result<()> {
        ensure!(self.failure.is_none(), "supervised stop previously failed");
        if self.stopped.is_some() || self.shared.is_some() {
            return Ok(());
        }
        let owners = owner_slot
            .as_mut()
            .context("actual service owner missing")?;
        if !self.stopping {
            self.stopping = true;
            owners.stop_intent.request();
            owners.app.processing.execution.begin_drain();
            owners.app.processing.hardware.begin_drain();
            owners
                .app
                .native_catalog
                .as_ref()
                .context("native owner missing")?
                .begin_drain();
            owners
                .app
                .health
                .shutting_down
                .store(true, Ordering::Relaxed);
            owners.stop.cancel();
        }
        // Every join is borrowed from the actual service. Dropping this future
        // leaves both original Owners and every unfinished handle in that service.
        let (worker, http, metrics, ingress, additional) = tokio::join!(
            owners.worker.deadline(120),
            owners.http.deadline(5),
            owners.metrics.deadline(2),
            async {
                match owners.ingress.as_mut() {
                    Some(t) => t.deadline(5).await,
                    None => Ok(()),
                }
            },
            async {
                let mut result = Ok(());
                for task in &mut owners.additional {
                    if let Err(e) = task.deadline(120).await {
                        result = Err(e);
                    }
                }
                result
            }
        );
        if let Err(error) = worker.and(http).and(metrics).and(ingress).and(additional) {
            self.failure = Some(Failure::Error(anyhow::anyhow!(error)));
            anyhow::bail!("supervised stop failed; actual owners remain in service");
        }
        if owners.terminal.is_none() {
            if self.terminal_task.is_none() {
                self.terminal_task = Some(
                    owners
                        .app
                        .native_catalog
                        .as_ref()
                        .context("native owner missing")?
                        .begin_close_admission_for_stopped_service(),
                );
            }
            let result = self
                .terminal_task
                .as_mut()
                .expect("stored terminal task")
                .await;
            self.terminal_task = None;
            match result {
                Ok(Ok(terminal)) => owners.terminal = Some(terminal),
                Ok(Err(error)) => {
                    self.failure = Some(Failure::Error(error));
                    anyhow::bail!("terminal stop failed; original owners retained");
                }
                Err(error) => {
                    self.failure = Some(Failure::Error(error.into()));
                    anyhow::bail!("terminal stop worker failed; original owners retained");
                }
            }
        }
        owners.lock.verify()?;
        if owners.io.is_none() {
            owners.io = Some(
                owners
                    .app
                    .processing
                    .io
                    .clone()
                    .try_acquire_owned()
                    .context("Motion I/O ownership remains active")?,
            );
        }
        owners.app.processing.execution.close_admission_if_idle()?;
        owners.app.processing.hardware.close_admission_if_idle()?;
        // Pure moves, with no await or allocation between the two owned phases.
        let mut owners = owner_slot.take().expect("actual joined owners");
        self.stopped = Some(StoppedOwners {
            app: owners.app,
            native: owners.terminal.take(),
            _io: owners.io.take().expect("retained I/O admission"),
            lock: owners.lock,
        });
        Ok(())
    }

    pub(in crate::native_service) async fn prepare_pair(
        &mut self,
        parent: PathBuf,
        flag: Arc<AtomicBool>,
    ) -> anyhow::Result<()> {
        self.prepare_pair_inner(
            parent,
            flag,
            #[cfg(test)]
            None,
        )
        .await
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) async fn prepare_pair_for_abandonment_fixture(
        &mut self,
        parent: PathBuf,
        flag: Arc<AtomicBool>,
        started: &std::cell::Cell<bool>,
    ) -> anyhow::Result<()> {
        self.prepare_pair_inner(parent, flag, Some(started)).await
    }
    async fn prepare_pair_inner(
        &mut self,
        parent: PathBuf,
        flag: Arc<AtomicBool>,
        #[cfg(test)] started: Option<&std::cell::Cell<bool>>,
    ) -> anyhow::Result<()> {
        ensure!(
            self.shared.is_none()
                && self.failure.is_none()
                && !self.close_started
                && self.close_failure.is_none(),
            "pair preparation is one-shot and cannot resume after close"
        );
        let stopped = self
            .stopped
            .as_mut()
            .context("actual stopped service missing")?;
        stopped.lock.verify()?;
        let mut cancellation = PairCancellation(flag.clone(), true);
        if self.pair_task.is_none() && self.pair_completion.is_none() {
            if self.writer.is_none() {
                self.writer = Some(FreshWriter::begin(&stopped.app.db, &flag).await?);
            }
            let storage = allocation_reservation()?;
            self.export_storage = Some(storage);
            self.pair_task = Some(
                stopped
                    .native
                    .as_mut()
                    .context("terminal owner missing")?
                    .begin_pair_export(
                        parent,
                        flag.clone(),
                        self.writer.take().expect("retained writer"),
                        storage,
                        move |native, writer, profile, flag, path| {
                            compose_borrowed(
                                native,
                                writer,
                                profile,
                                path,
                                &flag,
                                || Ok(()),
                                storage,
                            )
                        },
                    ),
            );
        }
        #[cfg(test)]
        if let Some(started) = started {
            // Stack-only test observation after the actual handle is retained.
            // This yield is absent in production and creates no custody owner.
            started.set(true);
            tokio::task::yield_now().await;
        }
        if self.pair_completion.is_none() {
            let result = self
                .pair_task
                .as_mut()
                .context("pair worker missing")?
                .await;
            self.pair_task = None;
            match result {
                Ok(completion) => self.pair_completion = Some(completion),
                Err(error) => {
                    self.failure = Some(Failure::Error(error.into()));
                    anyhow::bail!("pair worker failed; stopped owner retained");
                }
            }
        }
        stopped.lock.verify()?;
        // Resolve the already-admitted scalar while every original output is
        // still in the service-owned completion. No fallible work after taking.
        let storage = self.export_storage.context("pair export storage missing")?;
        let PairExportCompletion { writer, outcome } =
            self.pair_completion.take().expect("retained completion");
        let parts = match outcome {
            Ok(Ok(parts)) => parts,
            Ok(Err(error)) => {
                self.writer = Some(writer);
                self.failure = Some(Failure::Error(error));
                anyhow::bail!("pair export refused; original writer and stopped owner retained");
            }
            Err(panic) => {
                self.writer = Some(writer);
                self.failure = Some(Failure::Panic(panic));
                anyhow::bail!("pair export panicked; original writer and stopped owner retained");
            }
        };
        let images = parts.with_writer(writer);
        let stopped = self
            .stopped
            .take()
            .expect("retained original stopped owner");
        // First new shared-slot allocation, AFTER native export admitted storage.
        self.shared = Some(Arc::new_cyclic(|weak| Shared {
            phase: Mutex::new(PhaseOwner {
                target: None,
                workspace: None,
                selection: None,
                pair: Some(PreparedPair {
                    _writer: images.writer,
                    native: images.native,
                    motion: images.motion,
                    parent: images.parent,
                    parent_identity: images.parent_identity,
                    manifest: images.manifest,
                    _stopped: StoppedService {
                        owners: None,
                        supervised: Some(RestoreProof {
                            shared: weak.clone(),
                        }),
                    },
                }),
                image: None,
                failure: None,
                close_failure: None,
            }),
            stopped: Mutex::new(Some(stopped)),
            cancel: flag,
            status: AtomicU8::new(IDLE),
            changed: Notify::new(),
            storage,
            selection_storage: AtomicU64::new(0),
            authorization_storage: AtomicU64::new(0),
            #[cfg(test)]
            test: Default::default(),
        }));
        cancellation.1 = false;
        Ok(())
    }

    /// Synchronous one-shot transition. A request owns only its status waiter;
    /// even an unpolled waiter cannot undo this service-owned Selecting phase.
    /// Caller supplies the original absolute bound: no earlier selection/service
    /// deadline exists, and backend-local SQL bounds are not inherited coverage.
    pub(in crate::native_service) fn begin_selection(
        &mut self,
        path: PathBuf,
        staging: PathBuf,
        deadline: Instant,
    ) -> anyhow::Result<()> {
        let runtime = tokio::runtime::Handle::try_current()?;
        let shared = self
            .shared
            .as_ref()
            .context("actual service slot missing")?;
        ensure!(
            self.worker.is_none()
                && self.selection_timer.is_none()
                && !self.close_started
                && self.close_failure.is_none()
                && shared.status.load(Ordering::Acquire) == IDLE,
            "service selection is one-shot and cannot resume after close"
        );
        let mut phase = shared.phase.lock().unwrap_or_else(|p| p.into_inner());
        ensure!(
            phase.pair.is_some()
                && phase.image.is_none()
                && phase.selection.is_none()
                && phase.failure.is_none(),
            "service ReadyPair phase required"
        );
        // The caller buffers were admitted before pair acquisition. No clone
        // occurs here, and no new owner is installed until task preflight passes.
        let selection = ServiceSelection::new(path, staging, deadline)?;
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
                        // Cooperative cancellation: neither this timer nor
                        // waiter loss proves preemptive SQLite/I/O settlement.
                        retained.cancel.store(true, Ordering::Release);
                        retained.changed.notify_waiters();
                        break;
                    }
                }
            }
        };
        let retained = shared.clone();
        #[cfg(test)]
        let worker_runtime = runtime.clone();
        let blocking_work = move || {
            let mut phase = retained.phase.lock().unwrap_or_else(|p| p.into_inner());
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                #[cfg(test)]
                selection_boundary(&retained, &worker_runtime, 1);
                let PhaseOwner {
                    pair, selection, ..
                } = &mut *phase;
                let pair = pair.as_mut().context("retained current pair missing")?;
                let selection = selection.as_mut().context("retained selection missing")?;
                let additional = selection.admit(pair, &retained.cancel)?;
                // The measured charge is installed before any path clone,
                // selected read or new metadata allocation. Never decreased.
                retained
                    .selection_storage
                    .fetch_max(additional, Ordering::AcqRel);
                selection.inspect(pair, &retained.cancel)?;
                #[cfg(test)]
                {
                    retained.test.selection_fd.store(
                        selection
                            .evidence_for_fixture()
                            .0
                            .expect("installed selected evidence")
                            .0
                            .as_raw_fd(),
                        Ordering::Release,
                    );
                    selection_boundary(&retained, &worker_runtime, 2);
                }
                selection.bind(pair, &retained.cancel)?;
                Ok::<(), anyhow::Error>(())
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
            // A spawn failure may already have quarantined this phase.
            // A possibly accepted worker cannot overwrite that refusal.
            let _ = retained.status.compare_exchange(
                RUNNING,
                state,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            retained.changed.notify_waiters();
            drop(phase);
            #[cfg(test)]
            if retained.test.selection_join_pause.load(Ordering::Acquire) {
                retained
                    .test
                    .selection_join_reached
                    .store(true, Ordering::Release);
                retained.changed.notify_waiters();
                let mut released = retained.test.selection_join_released.lock().unwrap();
                while !*released {
                    released = retained.test.selection_join_release.wait(released).unwrap();
                }
            }
        };
        // These are the actual unpolled values subsequently handed to Tokio,
        // not JoinHandle sizes or an inferred free share of generic OVERHEAD.
        // Refusal here leaves ReadyPair/IDLE and no started or unjoinable task.
        let _frame_sizes = selection_task_frames(&deadline_future, &blocking_work)?;
        #[cfg(test)]
        {
            shared
                .test
                .selection_deadline_frame
                .store(_frame_sizes.0, Ordering::Release);
            shared
                .test
                .selection_worker_frame
                .store(_frame_sizes.1, Ordering::Release);
        }
        phase.selection = Some(selection);
        shared.status.store(RUNNING, Ordering::Release);
        drop(phase);
        // Spawn itself may unwind. Selecting and exact original custody are
        // already installed, so a failed spawn is terminal, never retryable.
        let spawned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.selection_timer = Some(runtime.spawn(deadline_future));
            self.worker = Some(runtime.spawn_blocking(blocking_work));
        }));
        if let Err(panic) = spawned {
            // Do not lock phase from this async-runtime caller: an uncertain
            // accepted worker could hold it while awaiting the retained writer.
            self.failure = Some(Failure::Panic(panic));
            self.selection_spawn_failed = true;
            shared.cancel.store(true, Ordering::Release);
            shared.status.store(FAILED, Ordering::Release);
            shared.changed.notify_waiters();
            anyhow::bail!(
                "service selection spawn outcome uncertain; original phase retained without retry or close"
            );
        }
        Ok(())
    }

    /// This is retirement of an already COMPLETE selection, not target begin.
    /// An unpolled/dropped future changes no target status and starts no effect.
    /// Sole handles remain borrowed from this service until verified completion;
    /// a later call may safely finish remaining retirement before begin_target.
    pub(in crate::native_service) async fn settle_selection_for_target(
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
            "completed service selection required before retirement"
        );
        {
            let phase = shared.phase.lock().unwrap_or_else(|p| p.into_inner());
            ensure!(
                phase.target.is_none()
                    && phase.workspace.is_none()
                    && phase.pair.is_some()
                    && phase.image.is_none()
                    && phase.failure.is_none(),
                "only the original selected pair may retire for target"
            );
            phase
                .selection
                .as_ref()
                .context("selected phase missing")?
                .target_deadline(&shared.cancel)?;
        }
        if let Some(worker) = self.worker.as_mut() {
            let result = worker.await;
            self.worker = None;
            if let Err(error) = result {
                self.failure = Some(Failure::Error(error.into()));
                shared.status.store(FAILED, Ordering::Release);
                shared.changed.notify_waiters();
                anyhow::bail!("selected worker retirement failed; exact pair retained");
            }
        }
        if let Some(timer) = self.selection_timer.as_mut() {
            let result = timer.await;
            self.selection_timer = None;
            if let Err(error) = result {
                self.failure = Some(Failure::Error(error.into()));
                shared.status.store(FAILED, Ordering::Release);
                shared.changed.notify_waiters();
                anyhow::bail!("selected timer retirement failed; exact pair retained");
            }
        }
        let phase = shared.phase.lock().unwrap_or_else(|p| p.into_inner());
        phase
            .selection
            .as_ref()
            .expect("unchanged selected phase")
            .target_deadline(&shared.cancel)?;
        Ok(())
    }

    /// Synchronous one-shot start after explicit selection retirement. Requests
    /// own only subsequent status waiters. No parent/lock/publication can escape
    /// the original Shared.phase or be dropped when a request is cancelled.
    pub(in crate::native_service) fn begin_target(
        &mut self,
        path: PathBuf,
        caller: Caller,
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
            "target requires completed, explicitly retired selection"
        );
        let mut phase = shared.phase.lock().unwrap_or_else(|p| p.into_inner());
        ensure!(
            phase.target.is_none()
                && phase.workspace.is_none()
                && phase.pair.is_some()
                && phase.image.is_none()
                && phase.failure.is_none(),
            "target phase is one-shot"
        );
        let deadline = phase
            .selection
            .as_ref()
            .context("retained selection missing")?
            .target_deadline(&shared.cancel)?;
        let proof = phase
            .pair
            .as_ref()
            .unwrap()
            ._stopped
            .supervised
            .as_ref()
            .context("actual service stopped proof required")?;
        ensure!(
            Weak::ptr_eq(&proof.shared, &Arc::downgrade(shared)),
            "selected pair belongs to another service"
        );
        // Native export admitted fixed target host storage before Shared existed.
        // The named auth cohort is admitted later by the same retained worker,
        // after completed selection, not during earlier export scratch.
        ensure!(
            shared.storage >= target_reservation::storage_reservation(),
            "named target service share was not admitted"
        );
        let target = ServiceTarget::new(path, caller, deadline, &shared.cancel)?;
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
                let pair = pair.as_mut().expect("original current pair");
                let selection = selection.as_mut().expect("bound selected phase");
                ensure!(
                    workspace.is_none(),
                    "target authorization workspace is one-shot"
                );
                *workspace = Some(admit_target_authorization(&retained, pair, selection)?);
                let work = target.as_mut().expect("installed target").run(
                    pair,
                    selection,
                    workspace.as_mut().expect("named admitted auth workspace"),
                    &retained,
                );
                let frame = u64::try_from(std::mem::size_of_val(&work))?;
                ensure!(
                    frame <= target_reservation::ASYNC_FRAME_LIMIT,
                    "target future exceeds preadmitted frame share"
                );
                #[cfg(test)]
                retained
                    .test
                    .target_async_frame
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
            drop(phase);
            #[cfg(test)]
            if retained.test.target_join_pause.load(Ordering::Acquire) {
                retained
                    .test
                    .target_join_reached
                    .store(true, Ordering::Release);
                retained.changed.notify_waiters();
                let mut released = retained.test.target_join_released.lock().unwrap();
                while !*released {
                    released = retained.test.target_join_release.wait(released).unwrap();
                }
            }
        };
        selection_task_frames(&deadline_future, &blocking_work)?;
        // From here on every original owner is in its stable service slot,
        // including failures/panics during spawn and uncertain accepted work.
        phase.target = Some(target);
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
                "target spawn outcome uncertain; exact phase retained without retry or close"
            );
        }
        Ok(())
    }

    /// Borrow the exact completed target worker and timer until both joins are
    /// positively known. Dropping this future cannot start staging or lose them.
    pub(in crate::native_service) async fn settle_target_for_staging(
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
            "completed service target required before staging retirement"
        );
        {
            let phase = shared.phase.lock().unwrap_or_else(|p| p.into_inner());
            ensure!(
                phase.failure.is_none()
                    && phase.image.is_none()
                    && phase.pair.is_some()
                    && phase.target.as_ref().is_some_and(
                        |target| target.phase == target_reservation::TargetPhase::Reserved
                    )
                    && phase
                        .workspace
                        .as_ref()
                        .is_some_and(AuthorizationWorkspace::reusable),
                "original Reserved target and retired workspace required"
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
                anyhow::bail!("target worker retirement failed; exact service phase retained");
            }
        }
        if let Some(timer) = self.selection_timer.as_mut() {
            let result = timer.await;
            self.selection_timer = None;
            if let Err(error) = result {
                self.failure = Some(Failure::Error(error.into()));
                shared.status.store(FAILED, Ordering::Release);
                shared.changed.notify_waiters();
                anyhow::bail!("target timer retirement failed; exact service phase retained");
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

    /// Private finite staging start. The service owns every phase/resource and
    /// the original caller/deadline; the request may only wait for status.
    pub(in crate::native_service) fn begin_staging(&mut self) -> anyhow::Result<()> {
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
            "staging requires completed, explicitly retired target worker and timer"
        );
        let mut phase = shared.phase.lock().unwrap_or_else(|p| p.into_inner());
        ensure!(
            phase.failure.is_none() && phase.image.is_none(),
            "service staging is one-shot"
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
                target.claim_staging(workspace, &pair._stopped, &shared.cancel)? == deadline,
                "staging cannot refresh original deadline"
            );
        }
        // Staging is claimed before any borrower future/closure is constructed.
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
                    .run_staging(
                        pair.as_mut().expect("original current pair"),
                        selection.as_mut().expect("bound selected owner"),
                        workspace.as_mut().expect("same admitted workspace"),
                        &retained,
                    );
                let frame = u64::try_from(std::mem::size_of_val(&work))?;
                ensure!(
                    frame <= target_reservation::ASYNC_FRAME_LIMIT,
                    "staging future exceeds admitted frame share"
                );
                #[cfg(test)]
                retained
                    .test
                    .staging_async_frame
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
            anyhow::bail!("staging task preflight refused; claimed phase retained without retry");
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
                "staging spawn outcome uncertain; exact phase retained without retry or close"
            );
        }
        Ok(())
    }

    pub(in crate::native_service) fn begin_reservation(
        &mut self,
        caller: Caller,
    ) -> anyhow::Result<()> {
        let shared = self
            .shared
            .as_ref()
            .context("actual service slot missing")?;
        ensure!(
            self.worker.is_none() && shared.status.load(Ordering::Acquire) == IDLE,
            "restore worker is one-shot"
        );
        {
            let phase = shared.phase.lock().unwrap_or_else(|p| p.into_inner());
            ensure!(
                phase.pair.is_none() && phase.image.is_some(),
                "service copied-image phase missing"
            );
            let held = phase.image.as_ref().expect("retained image");
            let proof = held
                .stage
                .held
                .as_ref()
                .context("conversion custody missing")?
                .target
                .held
                .as_ref()
                .context("target custody missing")?
                .input
                .current
                ._stopped
                .supervised
                .as_ref()
                .context("service-owned stopped proof required")?;
            ensure!(
                Weak::ptr_eq(&proof.shared, &Arc::downgrade(shared)),
                "copied image belongs to another service"
            );
        }
        shared.status.store(RUNNING, Ordering::Release);
        let retained = shared.clone();
        let runtime = tokio::runtime::Handle::current();
        self.worker = Some(tokio::task::spawn_blocking(move || {
            let mut phase = retained.phase.lock().unwrap_or_else(|p| p.into_inner());
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                runtime.block_on(reserve_restore(
                    phase.image.as_mut().expect("stored image"),
                    &caller,
                    &retained.cancel,
                    &retained,
                ))
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
            retained.status.store(state, Ordering::Release);
            retained.changed.notify_waiters();
        }));
        Ok(())
    }

    pub(in crate::native_service) async fn wait(&self) -> anyhow::Result<()> {
        let shared = self
            .shared
            .as_ref()
            .context("actual service slot missing")?;
        let mut cancellation = PairCancellation(shared.cancel.clone(), true);
        loop {
            let changed = shared.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            match shared.status.load(Ordering::Acquire) {
                COMPLETE => {
                    cancellation.1 = false;
                    return Ok(());
                }
                FAILED => {
                    cancellation.1 = false;
                    anyhow::bail!("restore operation refused; original custody remains in service");
                }
                RUNNING => changed.await,
                _ => anyhow::bail!("restore worker not running"),
            }
        }
    }

    /// A close waiter never owns the cleanup future. The actual service keeps
    /// its worker handle and both original source and target custody throughout.
    pub(in crate::native_service) async fn close(&mut self) -> anyhow::Result<()> {
        ensure!(
            !self.selection_spawn_failed,
            "selection spawn outcome uncertain; no joined worker proof, exact phase retained"
        );
        ensure!(
            self.close_failure.is_none(),
            "restore close previously failed; available custody retained"
        );
        if self.inline_closed {
            return Ok(());
        }
        if self.shared.is_none() {
            return self.close_inline().await;
        }
        if !self.close_started {
            // Closing an outstanding selection also requests cancellation. Its
            // worker/timer remain owned here across every borrowed join.
            if self.selection_timer.is_some() {
                self.shared
                    .as_ref()
                    .expect("retained shared slot")
                    .cancel
                    .store(true, Ordering::Release);
            }
            if let Some(worker) = self.worker.as_mut() {
                let result = worker.await;
                self.worker = None;
                if let Err(error) = result {
                    self.close_failure = Some(Failure::Error(error.into()));
                    anyhow::bail!(
                        "restore worker join failed; service retains custody and will not repoll completed handle"
                    );
                }
            }
            if let Some(timer) = self.selection_timer.as_mut() {
                let result = timer.await;
                self.selection_timer = None;
                if let Err(error) = result {
                    self.close_failure = Some(Failure::Error(error.into()));
                    anyhow::bail!("selection deadline worker failed; exact phase retained");
                }
            }
            let shared = self
                .shared
                .as_ref()
                .context("shared restore slot not prepared")?;
            let retained = shared.clone();
            let runtime = tokio::runtime::Handle::current();
            self.close_started = true;
            self.worker = Some(tokio::task::spawn_blocking(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    runtime.block_on(close_shared(&retained))
                }));
                let mut phase = retained.phase.lock().unwrap_or_else(|p| p.into_inner());
                let status = match result {
                    Ok(Ok(())) => CLOSED,
                    Ok(Err(error)) => {
                        phase.close_failure = Some(Failure::Error(error));
                        FAILED
                    }
                    Err(panic) => {
                        phase.close_failure = Some(Failure::Panic(panic));
                        FAILED
                    }
                };
                retained.status.store(status, Ordering::Release);
                retained.changed.notify_waiters();
            }));
        }
        if let Some(worker) = self.worker.as_mut() {
            let result = worker.await;
            self.worker = None;
            if let Err(error) = result {
                self.close_failure = Some(Failure::Error(error.into()));
                anyhow::bail!(
                    "restore close worker failed; service retains available custody and terminal join failure"
                );
            }
        }
        ensure!(
            self.shared
                .as_ref()
                .context("restore service slot missing")?
                .status
                .load(Ordering::Acquire)
                == CLOSED,
            "restore close failed; available custody and failure remain service-owned"
        );
        Ok(())
    }

    /// Before export admission, no shared allocation is necessary for cleanup.
    /// Original writer/completion and stopped owners remain inline throughout
    /// borrowed rollback, terminal join, and pool-close waits.
    async fn close_inline(&mut self) -> anyhow::Result<()> {
        self.close_started = true;
        ensure!(
            self.stopped.is_some(),
            "stop is incomplete; actual service still retains original Owners and handles"
        );
        if let Some(task) = self.pair_task.as_mut() {
            let result = task.await;
            self.pair_task = None;
            match result {
                Ok(completion) => self.pair_completion = Some(completion),
                Err(error) => {
                    self.close_failure = Some(Failure::Error(error.into()));
                    anyhow::bail!(
                        "pair worker join failed; source remains held, terminal failure is not retryable"
                    );
                }
            }
        }
        if let Some(completion) = self.pair_completion.as_mut() {
            if let Err(error) = completion.writer.close().await {
                self.close_failure = Some(Failure::Error(error));
                anyhow::bail!("pair writer close failed; exact completion retained");
            }
            // Anonymous exported images have no installed durable names. Drop
            // only after their original writer has positively settled.
            self.pair_completion = None;
        }
        if let Some(writer) = self.writer.as_mut() {
            if let Err(error) = writer.close().await {
                self.close_failure = Some(Failure::Error(error));
                anyhow::bail!("pair writer close failed; exact writer retained");
            }
            self.writer = None;
        }
        if !self.terminal_close_started {
            let terminal = self
                .stopped
                .as_mut()
                .expect("retained stopped owner")
                .native
                .take()
                .context("terminal owner missing")?;
            self.terminal_close_started = true;
            // Legacy consuming terminal close is a named activation blocker on
            // failure. Never claim the native capability remains here afterward.
            self.terminal_close_task = Some(terminal.begin_close());
        }
        if let Some(task) = self.terminal_close_task.as_mut() {
            let result = task.await;
            self.terminal_close_task = None;
            match result {
                Ok(Ok(())) => self.terminal_closed = true,
                Ok(Err(error)) => {
                    self.close_failure = Some(Failure::Error(error));
                    anyhow::bail!(
                        "legacy terminal close failed; remaining App, IO, lock and failure retained, not Closed"
                    );
                }
                Err(error) => {
                    self.close_failure = Some(Failure::Error(error.into()));
                    anyhow::bail!(
                        "terminal close worker failed; remaining source custody retained, not Closed"
                    );
                }
            }
        }
        ensure!(self.terminal_closed, "terminal close outcome incomplete");
        self.stopped
            .as_ref()
            .expect("retained stopped owner")
            .app
            .db
            .close()
            .await;
        self.stopped = None;
        self.inline_closed = true;
        Ok(())
    }

    #[cfg(test)]
    pub(in crate::native_service::pair) fn assert_inline_export_for_fixture(&self, fd: i32) {
        assert!(self.pair_task.is_some());
        assert!(self.shared.is_none());
        assert_eq!(self.stopped.as_ref().unwrap().lock.file.as_raw_fd(), fd);
    }

    #[cfg(test)]
    pub(in crate::native_service::pair) async fn exercise_abandoned_waiter(
        &mut self,
        caller: Caller,
        target: &Path,
        source_lock: &File,
        original_source_fd: i32,
    ) {
        use std::future::Future;
        use std::task::Poll;
        let shared = self
            .shared
            .as_ref()
            .expect("actual startup service slot")
            .clone();
        assert_eq!(
            shared
                .stopped
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .lock
                .file
                .as_raw_fd(),
            original_source_fd
        );
        shared.test.pause.store(true, Ordering::Release);
        self.begin_reservation(caller).unwrap();
        {
            // A real polled waiter is abandoned, not an unpolled async body.
            let mut waiter = std::pin::pin!(self.wait());
            std::future::poll_fn(|cx| {
                assert!(matches!(waiter.as_mut().poll(cx), Poll::Pending));
                Poll::Ready(())
            })
            .await;
            loop {
                let changed = shared.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if shared.test.published.load(Ordering::Acquire) {
                    break;
                }
                assert_eq!(
                    shared.status.load(Ordering::Acquire),
                    RUNNING,
                    "genuine reservation failed before publication"
                );
                changed.await;
            }
        } // Drop signals cancellation only; NativeService still owns slot/handle.
        assert!(shared.cancel.load(Ordering::Acquire));
        assert!(self.worker.is_some());
        assert!(fs2::FileExt::try_lock_exclusive(source_lock).is_err());
        let record_path = target.join("paired-install.restore-reservation.json");
        let named = std::fs::metadata(&record_path).unwrap();
        let original_fd = shared.test.publication_fd.load(Ordering::Acquire);
        let mut original = std::mem::MaybeUninit::<libc::stat>::uninit();
        assert_eq!(
            unsafe { libc::fstat(original_fd, original.as_mut_ptr()) },
            0
        );
        let original = unsafe { original.assume_init() };
        assert_eq!(
            (original.st_dev, original.st_ino),
            (named.dev(), named.ino())
        );
        shared.test.release.notify_one();
        assert!(self.wait().await.is_err());
        {
            let mut phase = shared.phase.lock().unwrap();
            let held = phase.image.as_mut().expect("service retained exact image");
            let (evidence, publication_phase, retained_fd) =
                held.reservation.as_ref().unwrap().original_publication();
            assert_eq!(retained_fd, original_fd);
            assert_eq!(
                publication_phase,
                playscale_core::paired_backup::publication::Phase::Durable
            );
            assert_eq!(
                (evidence.file_device, evidence.file_inode),
                (named.dev(), named.ino())
            );
            assert!(matches!(held.phase, Phase::Operation(_)));
            let Phase::Operation(operation) = &mut held.phase else {
                unreachable!()
            };
            let db = operation.writer_in().await.unwrap();
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM catabolic_paired_restore_intents"
                )
                .fetch_one(&mut *db)
                .await
                .unwrap(),
                0
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM catabolic_paired_restore_acks")
                    .fetch_one(db)
                    .await
                    .unwrap(),
                0
            );
        }
        assert!(!target.join("playscale.sqlite3").exists());
        assert!(!target.join("catabolic.sqlite3").exists());
        self.close().await.unwrap();
        assert_eq!(shared.status.load(Ordering::Acquire), CLOSED);
        assert_eq!(std::fs::metadata(&record_path).unwrap().ino(), named.ino());
    }

    #[cfg(test)]
    pub(in crate::native_service::pair) fn stage_image_for_fixture(
        &mut self,
        mut image: PostRelocationPairImage,
    ) {
        let shared = self.shared.as_ref().expect("actual service slot");
        let mut phase = shared.phase.lock().unwrap_or_else(|p| p.into_inner());
        assert!(phase.image.is_none() && phase.pair.is_none());
        phase.image = image.held.take();
    }

    #[cfg(test)]
    pub(in crate::native_service::pair) fn take_pair_for_fixture(&mut self) -> PreparedPair {
        self.shared
            .as_ref()
            .expect("actual shared service")
            .phase
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pair
            .take()
            .expect("actual prepared pair")
    }
}

/// Fixed live layout, two Arc refcounts, and named bounded caller-path and
/// selection-task shares. Native export admits all under the existing cap.
/// Existing failure-chain formatting remains a separate unbounded error-path
/// limitation. Inline service/handle layouts are forwarded too, not hidden in
/// the old pair/authentication conventions. There is only one shared slot.
fn allocation_reservation() -> anyhow::Result<u64> {
    [
        std::mem::size_of::<RestoreSupervisor>(),
        std::mem::size_of::<Shared>(),
        2 * std::mem::size_of::<std::sync::atomic::AtomicUsize>(),
        std::mem::size_of::<PairExportCompletion<ImageParts>>(),
    ]
    .into_iter()
    .try_fold(
        SERVICE_INPUT_PATH_STORAGE
            .checked_add(SELECTION_TASK_STORAGE)
            .and_then(|n| n.checked_add(target_reservation::storage_reservation()))
            .context("service selected/target named admission overflow")?,
        |sum, n| {
            sum.checked_add(u64::try_from(n)?)
                .context("service restore allocation overflow")
        },
    )
}

fn dismantle_stage(stage: &mut NativeConversionStage) {
    if let Some(mut held) = stage.held.take() {
        // Leave every durable name intact. Taking these wrappers prevents their
        // legacy global Drop sinks; this does not claim rollback/close success.
        drop(held.target.held.take());
    }
}

async fn close_shared(shared: &Shared) -> anyhow::Result<()> {
    {
        let mut phase = shared.phase.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(held) = phase.image.as_mut() {
            close_image(held).await?;
            if let Some(stage) = held.stage.held.as_mut() {
                stage
                    .target
                    .held
                    .as_mut()
                    .context("target custody missing during close")?
                    .input
                    .current
                    ._writer
                    .close()
                    .await?;
            }
            held.pool.close().await;
            dismantle_stage(&mut held.stage);
        }
        {
            let PhaseOwner { target, pair, .. } = &mut *phase;
            if let Some(target) = target.as_mut() {
                #[cfg(test)]
                source94::source94_boundary(shared, 21);
                target
                    .settle_source94(
                        &mut pair
                            .as_mut()
                            .context("source94 lost original pair")?
                            ._writer,
                    )
                    .await?;
                target
                    .settle_source96(
                        &mut pair
                            .as_mut()
                            .context("source96 lost original pair")?
                            ._writer,
                    )
                    .await?;
                target
                    .settle_native96(
                        &mut pair
                            .as_mut()
                            .context("native96 lost original pair")?
                            ._writer,
                    )
                    .await?;
                target.settle_control97().await?;
            }
        }
        {
            let PhaseOwner {
                workspace, pair, ..
            } = &mut *phase;
            if let Some(workspace) = workspace.as_mut() {
                let writer = &mut pair
                    .as_mut()
                    .context("service authorization lost its exact original pair")?
                    ._writer;
                workspace.settle_service_close(writer).await?;
            }
        }
        if let Some(pair) = phase.pair.as_mut() {
            #[cfg(test)]
            ensure!(
                !shared.test.selection_close_failure.load(Ordering::Acquire),
                "injected selected close refusal before exact writer settlement"
            );
            pair._writer.close().await?;
        }
        // Selected FDs/path/hash/facts remain owned until exact writer close
        // succeeds. No target/conversion wrapper or legacy Drop sink is used.
        drop(phase.selection.take());
        drop(phase.pair.take());
    }
    let mut stopped = shared.stopped.lock().unwrap_or_else(|p| p.into_inner());
    let owners = stopped
        .as_mut()
        .context("actual stopped source owner missing during close")?;
    // Explicit legacy ownership boundary. Failure/unwind does not mean Closed,
    // and does not claim the consumed terminal capability remains in this slot.
    // It remains an activation blocker, distinct from request cancellation.
    owners
        .native
        .take()
        .context("terminal close already incomplete")?
        .close()
        .await?;
    owners.app.db.close().await;
    drop(stopped.take());
    // Named target files are never unlinked. Even a terminal-close failure
    // above keeps the exact target lock/publications and workspace in service.
    let mut phase = shared.phase.lock().unwrap_or_else(|p| p.into_inner());
    drop(phase.target.take());
    drop(phase.workspace.take());
    Ok(())
}

#[cfg(test)]
fn target_boundary(shared: &Shared, stage: u8) {
    if shared.test.target_pause.load(Ordering::Acquire) == stage {
        shared.test.target_reached.store(stage, Ordering::Release);
        shared.changed.notify_waiters();
        let mut released = shared
            .test
            .target_released
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        while !*released {
            released = shared
                .test
                .target_release
                .wait(released)
                .unwrap_or_else(|p| p.into_inner());
        }
        *released = false;
        assert!(
            !shared.test.target_panic.load(Ordering::Acquire),
            "injected target worker panic with original service custody retained"
        );
    }
}

#[cfg(test)]
fn selection_boundary(shared: &Shared, runtime: &tokio::runtime::Handle, stage: u8) {
    if shared.test.selection_pause.load(Ordering::Acquire) == stage {
        shared
            .test
            .selection_reached
            .store(stage, Ordering::Release);
        shared.changed.notify_waiters();
        runtime.block_on(shared.test.selection_release.notified());
        assert!(
            !shared.test.selection_panic.load(Ordering::Acquire),
            "injected selected worker panic with original service phase retained"
        );
    }
}

#[cfg(test)]
fn staging_boundary(shared: &Shared, stage: u8) {
    if shared.test.staging_pause.load(Ordering::Acquire) == stage {
        shared.test.staging_reached.store(stage, Ordering::Release);
        shared.changed.notify_waiters();
        let mut released = shared
            .test
            .staging_released
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        while !*released {
            released = shared
                .test
                .staging_release
                .wait(released)
                .unwrap_or_else(|p| p.into_inner());
        }
        *released = false;
        assert!(
            !shared.test.staging_panic.load(Ordering::Acquire),
            "injected service staging panic with original custody retained"
        );
    }
}
