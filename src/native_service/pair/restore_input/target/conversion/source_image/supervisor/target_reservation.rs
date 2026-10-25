//! Service-owned, one-shot selected target reservation and finite image staging.
//! No consuming legacy target wrapper, global recovery sink, or native41/95 grant.
mod staging;
use super::*;
use crate::native_service::pair::restore_input::{
    selected::SelectedPairInput,
    target::{require_fresh, require_fresh_leaves, verify_target_lock},
};
use crate::native_service::{pair_install, publication_file};
use crate::v2::auth::retained::CurrentSourceAuthentication;
use playscale_core::paired_backup::publication::Phase as PublicationPhase;
use std::{future::Future, task::Poll};

pub(super) use super::super::super::source_conversion::retained_native_intent::AuthorizationWorkspace;
const PATH_SHARE: u64 = libc::PATH_MAX as u64;
const ANCESTRY_WORK: usize = 16384;
const ANCESTRY_DEPTH: usize = 128;
// The original reservation consumes three of eight slots. Staging admits four
// additional observations explicitly; no later phase may grow this capacity.
const RESERVATION_PATH_OBSERVATIONS: usize = 8;
const STAGING_PATH_OBSERVATIONS: usize = 8; // staging plus finite control97 preflights
const PATH_OBSERVATIONS: usize = RESERVATION_PATH_OBSERVATIONS + STAGING_PATH_OBSERVATIONS;
// Fixed create-only marker/reservation records each have a 16KiB encoded bound.
// Cover simultaneous serde encode/hash/recheck buffers and transient growth,
// retained digest/name/path buffers, metadata and procfs syscall arguments.
// Sealed's hashing buffer is 64KiB. These are logical retained/transient shares,
// not a claim about allocator overhead, kernel FD memory, or process RSS.
const PUBLISHER_SHARE: u64 = 1024 * 1024;
// Selected binding roots admit up to 256KiB total path bytes; include their
// absolute/component scratch, not merely the caller target PATH_MAX buffer.
const PATH_CLONE_SHARE: u64 = 32 * 256 * 1024;
// Lexical SQLx handle lease/rusqlite borrowed wrapper and scalar status probe.
// Admitted before measuring the later cohort, not taken from its auth reserve.
const MEASUREMENT_SHARE: u64 = 64 * 1024;
pub(super) const ASYNC_FRAME_LIMIT: u64 = 128 * 1024;
pub(super) const TASK_STORAGE: u64 = 2 * ASYNC_FRAME_LIMIT
    + 2 * SELECTION_DEADLINE_FRAME_LIMIT
    + 2 * SELECTION_WORKER_FRAME_LIMIT
    + SELECTION_TOKIO_TASK_SHARE;

pub(super) const fn storage_reservation() -> u64 {
    PATH_SHARE
        + Caller::RETAINED_STORAGE_LIMIT
        + PATH_CLONE_SHARE
        + PUBLISHER_SHARE
        + MEASUREMENT_SHARE
        + 2 * (2 * ANCESTRY_WORK + ANCESTRY_DEPTH + 16) as u64 * std::mem::size_of::<File>() as u64
        + 2 * std::mem::size_of::<ServiceTarget>() as u64
        + TASK_STORAGE
        + staging::storage_reservation()
        + STAGING_PATH_OBSERVATIONS as u64 * std::mem::size_of::<File>() as u64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TargetPhase {
    Inputs,
    Authenticating,
    Preflight,
    LockPublication,
    ReservationPublication,
    ReservedUnretired,
    Reserved,
    Staging,
    StagedUnretired,
    Staged,
    Control97,
    Ready97Unretired,
    Ready97,
    Native96,
    ReadyIntent96,
    Source96,
    SourceReady,
    Source94,
    ReceiptPending,
    Source94Ready,
    Source94ClosingTarget,
    Source94CaptureInputReady,
    Source94CaptureIngress,
    Source94CaptureIngressRetained,
    Source94CaptureAcquiring,
    Source94CapturePrepared,
    Source94NativeCapturing,
    Source94NativeCaptured,
    Source94PairedCapturing,
    Source94PairedCaptured,
}
/// All returned target FDs and publication owners are installed here before
/// subsequent fallible work. Anonymous preparation's internal pre-return failure
/// remains the publisher's explicit boundary, not named-effect custody loss.
pub(super) struct ServiceTarget {
    pub(super) phase: TargetPhase,
    path: PathBuf,
    caller: Caller,
    deadline: Instant,
    parent: Option<File>,
    target_ancestors: Vec<File>,
    source_ancestors: Vec<File>,
    path_observations: Vec<File>,
    source_parents: Option<(File, File)>,
    ancestry_remaining: usize,
    record: Option<pair_install::Reservation>,
    lock: pair_install::TargetLock,
    reservation: Option<publication_file::Outcome>,
    retirement_failure: Option<anyhow::Error>,
    staging: Option<staging::ServiceStaging>,
}
impl ServiceTarget {
    pub(super) fn new(
        path: PathBuf,
        caller: Caller,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Self> {
        alive(deadline, cancel)?;
        ensure!(
            path.is_absolute(),
            "service target requires an absolute path"
        );
        ensure!(
            u64::try_from(path.capacity())? <= PATH_SHARE,
            "target caller path exceeds preadmitted share"
        );
        caller.retained_storage(cancel, deadline)?;
        // The exact service's native export already admitted these capacities.
        // This constructor makes no target filesystem observation or effect.
        Ok(Self {
            phase: TargetPhase::Inputs,
            path,
            caller,
            deadline,
            parent: None,
            target_ancestors: Vec::with_capacity(ANCESTRY_DEPTH + 1),
            source_ancestors: Vec::with_capacity(2 * ANCESTRY_WORK + 1),
            path_observations: Vec::with_capacity(PATH_OBSERVATIONS),
            source_parents: None,
            ancestry_remaining: ANCESTRY_WORK,
            record: None,
            lock: pair_install::TargetLock::new(),
            reservation: None,
            retirement_failure: None,
            staging: None,
        })
    }
    /// The original writer is exclusively borrowed by genuine current auth for
    /// the complete preflight/publication. Both it and the complete bound pair
    /// stay in Shared.phase, not in a request or detached consuming wrapper.
    pub(super) async fn run(
        &mut self,
        pair: &mut PreparedPair,
        selection: &mut ServiceSelection,
        workspace: &mut AuthorizationWorkspace,
        shared: &Shared,
    ) -> anyhow::Result<()> {
        ensure!(
            self.phase == TargetPhase::Inputs,
            "target attempt cannot be retried"
        );
        self.phase = TargetPhase::Authenticating;
        ensure!(
            selection.target_deadline(&shared.cancel)? == self.deadline,
            "target cannot replace original selection deadline"
        );
        workspace.verify_service(&pair._stopped)?;
        let selected = selection.target_input(&shared.cancel)?;
        selected.target_binding(pair, &shared.cancel)?;
        workspace.begin()?;
        let PreparedPair {
            _writer,
            native,
            parent,
            parent_identity,
            _stopped,
            ..
        } = pair;
        // Reader acquisition and prepare borrow only the source-owned slot.
        // On failure the exact pair still retains its acquisition/reader; later
        // service cleanup joins that owner without a synthetic auth/close ACK.
        let reader = _writer.authorization_reader().await?;
        let mut current =
            CurrentSourceAuthentication::prepare(reader, &self.caller, Permission::SystemAdmin)
                .await
                .map_err(auth_error)?;
        self.phase = TargetPhase::Preflight;
        // Catch after the token is installed in this worker frame, so even
        // asynchronous revalidation panic can retire that same actual reader.
        let result = catch_async(async {
            alive(self.deadline, &shared.cancel)?;
            current.revalidate().await.map_err(auth_error)?;
            let measured = current
                .retained_allocations()
                .await
                .map_err(auth_error)?
                .checked_add(native.retained_allocations())
                .and_then(|n| n.checked_add(OVERHEAD))
                .context("target current allocation overflow")?
                .checked_add(_stopped.retained_restore_storage()?)
                .context("target service allocation overflow")?;
            ensure!(
                measured <= 256 * 1024 * 1024,
                "target current/authentication allocation cohort exceeded"
            );
            self.preflight_and_publish(
                selected,
                parent,
                *parent_identity,
                _stopped,
                &current,
                shared,
            )
        })
        .await;
        // Installing this exact close future is synchronous inside retire_current
        // before any close poll. Cancellation changes result/reusability, not
        // ownership of the SQLx close frame in the service workspace.
        let retired = workspace
            .retire_current(current, self.deadline, &shared.cancel)
            .await;
        if let Err(error) = retired {
            self.retirement_failure = Some(error);
        }
        match result {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(Err(error)) => Err(error),
            Ok(Ok(())) => {
                ensure!(
                    self.retirement_failure.is_none(),
                    "target authorization retirement incomplete; exact target/workspace retained"
                );
                self.phase = TargetPhase::Reserved;
                Ok(())
            }
        }
    }
    fn preflight_and_publish(
        &mut self,
        selected: &mut SelectedPairInput,
        current_parent: &File,
        current_parent_identity: (u64, u64),
        stopped: &StoppedService,
        current: &CurrentSourceAuthentication<'_>,
        shared: &Shared,
    ) -> anyhow::Result<()> {
        let cancel = &shared.cancel;
        alive(self.deadline, cancel)?;
        stopped.verify_lock()?;
        ensure!(
            identity(current_parent)? == current_parent_identity,
            "current pair parent changed before target preflight"
        );
        self.parent = Some(crate::native_service::pair::open_parent(&self.path)?);
        let start = self
            .parent
            .as_ref()
            .expect("installed target parent")
            .try_clone()?;
        self.chain(start, true, cancel)?;
        self.disjoint(current_parent.try_clone()?, cancel)?;
        for parent in selected.target_archive().parents() {
            self.disjoint(parent.try_clone()?, cancel)?;
        }
        self.source_parents = Some(stopped.source_parents()?);
        for index in 0..2 {
            let parents = self
                .source_parents
                .as_ref()
                .expect("installed source parents");
            let parent = if index == 0 { &parents.0 } else { &parents.1 };
            self.disjoint(parent.try_clone()?, cancel)?;
        }
        for root in selected.target_roots() {
            alive(self.deadline, cancel)?;
            let expected = root
                .identity()
                .context("restore root identity proof missing")?;
            let source = crate::native_service::pair::open_parent(Path::new(root.path()))?;
            // chain installs the returned root FD before any identity check.
            let start = self.chain(source, false, cancel)?;
            ensure!(
                identity(&self.source_ancestors[start])? == expected,
                "restore source/output root identity changed"
            );
            self.check_disjoint(start)?;
        }
        require_fresh(self.parent.as_ref().expect("installed target parent"))?;
        self.observe_path()?;
        selected.target_revalidate(cancel)?;
        stopped.verify_lock()?;
        alive(self.deadline, cancel)?;
        let (parent_device, parent_inode) = identity(self.parent.as_ref().unwrap())?;
        self.record = Some(pair_install::Reservation {
            contract: 1,
            install_id: uuid::Uuid::new_v4().to_string(),
            pair_sha256: selected.target_archive().sha256().to_owned(),
            parent_device,
            parent_inode,
        });
        self.lock
            .prepare(&self.path, self.record.as_ref().unwrap(), cancel)?;
        self.reservation = Some(pair_install::prepare_target_reservation(
            &self.path,
            self.record.as_ref().unwrap(),
            cancel,
        )?);
        #[cfg(test)]
        target_boundary(shared, 1);
        // Every fallible/allocating publisher observation precedes the final
        // pure temporal+original cancellation/deadline fence. No callback can
        // insert work between that fence and the fixed create-only link.
        self.phase = TargetPhase::LockPublication;
        {
            let link = self.lock.prepare_link_until(cancel, self.deadline)?;
            current.revalidate_temporal().map_err(auth_error)?;
            alive(self.deadline, cancel)?;
            link.publish()?;
        }
        ensure!(
            self.lock.phase() == Some(PublicationPhase::Durable),
            "target lock publication unresolved; no retry or unlink"
        );
        verify_target_lock(
            self.parent.as_ref().unwrap(),
            self.lock.held_lock().unwrap(),
        )?;
        self.observe_path()?;
        require_fresh_leaves(self.parent.as_ref().unwrap(), true)?;
        selected.target_revalidate(cancel)?;
        stopped.verify_lock()?;
        #[cfg(test)]
        {
            target_boundary(shared, 2);
            if shared
                .test
                .target_reservation_sync_failure
                .load(Ordering::Acquire)
            {
                self.reservation
                    .as_mut()
                    .unwrap()
                    .inject_parent_sync_failure();
            }
        }
        self.phase = TargetPhase::ReservationPublication;
        {
            let link = self
                .reservation
                .as_mut()
                .unwrap()
                .prepare_link_until(cancel, self.deadline)?;
            current.revalidate_temporal().map_err(auth_error)?;
            alive(self.deadline, cancel)?;
            link.publish()?;
        }
        ensure!(
            self.reservation.as_ref().unwrap().phase() == PublicationPhase::Durable,
            "target reservation durability unresolved; exact publication retained"
        );
        verify_target_lock(
            self.parent.as_ref().unwrap(),
            self.lock.held_lock().unwrap(),
        )?;
        self.observe_path()?;
        self.phase = TargetPhase::ReservedUnretired;
        #[cfg(test)]
        target_boundary(shared, 3);
        Ok(())
    }
    fn observe_path(&mut self) -> anyhow::Result<()> {
        ensure!(
            self.path_observations.len() < PATH_OBSERVATIONS,
            "target path observation share exhausted"
        );
        self.path_observations
            .push(crate::native_service::pair::open_parent(&self.path)?);
        ensure!(
            identity(self.path_observations.last().unwrap())?
                == identity(self.parent.as_ref().unwrap())?,
            "target directory changed"
        );
        Ok(())
    }
    fn chain(&mut self, first: File, target: bool, cancel: &AtomicBool) -> anyhow::Result<usize> {
        let chain = if target {
            &mut self.target_ancestors
        } else {
            &mut self.source_ancestors
        };
        let start = chain.len();
        // Capacity was preadmitted; this move precedes all subsequent failures.
        chain.push(first);
        loop {
            alive(self.deadline, cancel)?;
            ensure!(
                self.ancestry_remaining > 0 && chain.len() - start <= ANCESTRY_DEPTH,
                "restore target ancestry budget exceeded"
            );
            self.ancestry_remaining -= 1;
            let previous = chain.len() - 1;
            let fd = unsafe {
                libc::openat(
                    chain[previous].as_raw_fd(),
                    c"..".as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            ensure!(
                fd >= 0,
                "restore ancestry unavailable: {}",
                std::io::Error::last_os_error()
            );
            chain.push(unsafe { File::from_raw_fd(fd) });
            let next = identity(chain.last().unwrap())?;
            if next == identity(&chain[previous])? {
                return Ok(start);
            }
            for ancestor in &chain[start..=previous] {
                ensure!(identity(ancestor)? != next, "restore ancestry cycle");
            }
        }
    }
    fn disjoint(&mut self, source: File, cancel: &AtomicBool) -> anyhow::Result<()> {
        let start = self.chain(source, false, cancel)?;
        self.check_disjoint(start)
    }
    fn check_disjoint(&self, start: usize) -> anyhow::Result<()> {
        let source = identity(&self.source_ancestors[start])?;
        let target = identity(self.parent.as_ref().unwrap())?;
        for ancestor in &self.target_ancestors {
            ensure!(
                identity(ancestor)? != source,
                "restore target lies in source/output ancestry"
            );
        }
        for ancestor in &self.source_ancestors[start..] {
            ensure!(
                identity(ancestor)? != target,
                "restore target contains source/output ancestry"
            );
        }
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn evidence(&self) -> (Option<&File>, Option<&publication_file::Outcome>) {
        (self.lock.held_lock(), self.reservation.as_ref())
    }
}
fn alive(deadline: Instant, cancel: &AtomicBool) -> anyhow::Result<()> {
    cancelled(cancel)?;
    ensure!(
        Instant::now() < deadline,
        "original selected caller deadline exhausted"
    );
    Ok(())
}
#[derive(Debug)]
struct AuthenticationError(crate::v2::Problem);
impl std::fmt::Display for AuthenticationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "service target current authorization: {:?}", self.0)
    }
}
impl std::error::Error for AuthenticationError {}
fn auth_error(error: crate::v2::Problem) -> anyhow::Error {
    AuthenticationError(error).into()
}
async fn catch_async<F: Future>(future: F) -> Result<F::Output, Box<dyn std::any::Any + Send>> {
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|cx| {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
            Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
            Ok(Poll::Pending) => Poll::Pending,
            Err(panic) => Poll::Ready(Err(panic)),
        }
    })
    .await
}
