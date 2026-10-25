//! The actual tokens stay borrowed through both distinct effect-capable polls.
//! Fresh SQL runs after each preflight, not on each resumed Pending poll. The
//! fence cannot interrupt SQLx work already queued on its own worker thread.
use super::*;
use std::{sync::atomic::{AtomicBool, Ordering}, time::Instant, task::Poll};

impl CurrentSourceAuthentication<'_> {
    async fn refresh_capture(&mut self, caller: &Caller) -> Result<(), Problem> {
        if self.snapshot.permission != Permission::SystemAdmin {
            return Err(AccessError::Forbidden(Permission::SystemAdmin).into());
        }
        self.reader.verify().await.map_err(Problem::internal)?;
        let family = match &self.snapshot.principal.device_id {
            Some(id) => self.reader.lineage(id).await?,
            None => None,
        };
        let db = self.reader.authorize_connection().await.map_err(Problem::internal)?;
        // Last heavy owner/path checks precede genuine fresh policy/lineage SQL.
        let fresh = AuthenticationSnapshot::capture_authenticated(
            db, caller, Permission::SystemAdmin, self.snapshot.principal(), family).await?;
        self.snapshot = fresh;
        Ok(())
    }
}
impl TargetAuthentication<'_> {
    pub(crate) fn preflight_capture_acquire(&mut self) -> anyhow::Result<()> {
        self.reader.preflight_capture_acquire()
    }
    pub(crate) async fn refresh_capture(
        &mut self, caller: &Caller, current: &mut CurrentSourceAuthentication<'_>,
    ) -> anyhow::Result<u64> {
        ensure_capture_permission(&self.snapshot, &current.snapshot)?;
        anyhow::ensure!(matches!(&current.reader, CurrentReader::Retained(_)), "capture requires original retained current reader");
        self.reader.verify().await?;
        let family = match &self.snapshot.principal.device_id {
            Some(id) => super::super::super::signin::target_authorization_lineage(&mut self.reader, id)
                .await.map_err(capture_target_error)?,
            None => None,
        };
        let db = self.reader.authorize_connection().await?;
        let fresh = AuthenticationSnapshot::capture_authenticated(
            db, caller, Permission::SystemAdmin, self.snapshot.principal(), family)
            .await.map_err(capture_target_error)?;
        self.snapshot = fresh;
        current.refresh_capture(caller).await
            .map_err(|e| anyhow::anyhow!("capture current authorization: {e:?}"))?;
        // Post-read DBSTATUS checks bound cache/schema growth inside the SAME
        // original workspace. They add no new H or authorization grant. Both
        // snapshots remain live until the subsequent immediate pure fence.
        let source = current.retained_allocations().await
            .map_err(|e| anyhow::anyhow!("capture current allocation: {e:?}"))?;
        let target = self.reader.capture_authorization_allocations().await?;
        source.checked_add(target).ok_or_else(|| anyhow::anyhow!("capture authorization measurement overflow"))
    }
    pub(crate) async fn acquire_capture_input(
        &mut self, current: &CurrentSourceAuthentication<'_>, deadline: Instant, cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        std::future::poll_fn(|cx| {
            if let Err(error) = capture_fence(&self.snapshot, current, deadline, cancel) {
                return Poll::Ready(Err(error));
            }
            self.reader.poll_capture_acquire(cx)
        }).await
    }
    pub(crate) fn prepare_capture_begin(&mut self) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        self.reader.prepare_capture_begin()
    }
    pub(crate) fn close_capture_reader(&mut self) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        self.reader.close_capture_reader()
    }
    pub(crate) async fn begin_capture_input(
        &mut self, current: &CurrentSourceAuthentication<'_>, deadline: Instant, cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        // Genuine target snapshot remains live after exact close ACK. Policy-row
        // stability now depends on the supported retained cooperative stage
        // locks, NOT on a closed-reader authorization or a path/digest grant.
        std::future::poll_fn(|cx| {
            if let Err(error) = capture_fence(&self.snapshot, current, deadline, cancel) {
                return Poll::Ready(Err(error));
            }
            self.reader.poll_capture_begin(cx)
        }).await
    }
    pub(crate) fn finish_capture_input(&mut self) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        self.reader.finish_capture_input()
    }
    /// The distinct capture-purpose target reader installs its own close frame
    /// synchronously. The old acquisition reader's Joined state is not reused.
    pub(crate) fn close_native_capture_reader(&mut self) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        self.reader.close_native_capture_reader()
    }
    /// No host scan, await, callback, allocation reservation or P traversal may
    /// be inserted between this pure temporal fence and the fixed Native call.
    /// Both genuine refreshed snapshots remain alive through that call.
    pub(crate) fn capture_native(
        &mut self, current: &CurrentSourceAuthentication<'_>, deadline: Instant, cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        capture_fence(&self.snapshot, current, deadline, cancel)?;
        self.reader.capture_native()
    }

    /// Distinct paired-purpose reader. Installation of its close frame occurs
    /// synchronously, so dropping this borrower cannot manufacture its ACK.
    pub(crate) fn close_paired_capture_reader(&mut self) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        self.reader.close_paired_capture_reader()
    }
    /// Only the fixed opaque paired transition follows this scan-free fence.
    /// Both genuinely refreshed SystemAdmin snapshots remain alive until then.
    pub(crate) fn complete_paired_capture(
        &mut self, current: &CurrentSourceAuthentication<'_>, deadline: Instant, cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        capture_fence(&self.snapshot, current, deadline, cancel)?;
        self.reader.complete_paired_capture()
    }
}
fn capture_target_error(error: Problem) -> anyhow::Error {
    anyhow::anyhow!("capture target authorization: {error:?}")
}
fn ensure_capture_permission(target: &AuthenticationSnapshot, current: &AuthenticationSnapshot) -> anyhow::Result<()> {
    anyhow::ensure!(target.permission == Permission::SystemAdmin && current.permission == Permission::SystemAdmin,
        "capture acquisition requires two genuine SystemAdmin snapshots");
    Ok(())
}
fn capture_fence(target: &AuthenticationSnapshot, current: &CurrentSourceAuthentication<'_>, deadline: Instant, cancel: &AtomicBool) -> anyhow::Result<()> {
    ensure_capture_permission(target, &current.snapshot)?;
    anyhow::ensure!(matches!(&current.reader, CurrentReader::Retained(_)), "capture requires original retained current reader");
    anyhow::ensure!(!cancel.load(Ordering::Acquire) && Instant::now() < deadline,
        "capture acquisition cancelled or original deadline expired");
    target.revalidate().map_err(capture_target_error)?;
    current.revalidate_temporal().map_err(|e| anyhow::anyhow!("capture current authorization: {e:?}"))?;
    Ok(())
}
