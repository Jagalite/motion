//! Fixed post94 copied-image handoff, not95 mutation or install authority.
use super::SourceRelocationSession;
use anyhow::Context as _;
use crate::offline_control_upgrade::{StageCurrentImage, StageCurrentImageFailure};
use sqlx::{Sqlite, SqliteConnection, Transaction};
use std::sync::atomic::AtomicBool;

pub struct SourceCurrentImage {
    inner: StageCurrentImage,
}
pub struct SourceCurrentImageFailure {
    inner: Option<StageCurrentImageFailure>,
    refused: Option<(SourceRelocationSession, Transaction<'static, Sqlite>, anyhow::Error)>,
}
impl SourceRelocationSession {
    /// Consume the same admitted session and caller-owned WRITE. The trusted
    /// host must retain current-source authorization and reauthorize its target
    /// after this potentially long copy before any later consequential effect.
    pub async fn into_current_image(
        self,
        writer: Transaction<'static, Sqlite>,
        cancel: &AtomicBool,
    ) -> Result<SourceCurrentImage, SourceCurrentImageFailure> {
        if self.owned94.is_some() {
            return Err(SourceCurrentImageFailure { inner: None, refused: Some((self, writer, anyhow::anyhow!("owned94 admitted source rejects legacy capture"))) });
        }
        self.inner
            .into_current_image(writer, cancel)
            .await
            .map(|inner| SourceCurrentImage { inner })
            .map_err(|inner| SourceCurrentImageFailure { inner: Some(inner), refused: None })
    }
}
impl SourceCurrentImageFailure {
    pub fn error(&self) -> &anyhow::Error {
        if let Some((_,_,error))=&self.refused {error} else {self.inner.as_ref().unwrap().error()}
    }
    /// Recover exact original custody even when explicit rollback may itself
    /// fail. These are the caller's original writer/session, not new grants.
    pub fn into_parts(
        self,
    ) -> (
        SourceRelocationSession,
        Transaction<'static, Sqlite>,
        anyhow::Error,
    ) {
        if let Some(parts)=self.refused { return parts; }
        let (inner, writer, error) = self.inner.unwrap().into_parts();
        (SourceRelocationSession::from_stage(inner), writer, error)
    }
}
impl SourceCurrentImage {
    pub fn native_inventory_sha256(&self) -> &str {
        self.inner.native_inventory_sha256()
    }
    pub fn generation(&self) -> i64 {
        self.inner.generation()
    }
    pub fn closure_sha256(&self) -> &str {
        self.inner.closure_sha256()
    }
    pub fn effect_count(&self) -> usize {
        self.inner.effect_count()
    }
    /// Trusted application authorization access to its original retained WRITE.
    /// Fixed verification repeats before subsequent phase use. No native Store,
    /// arbitrary pool/path constructor, copy bytes or installation is exposed.
    pub async fn writer_in(&mut self) -> anyhow::Result<&mut SqliteConnection> {
        self.inner.writer_in().await
    }
    pub async fn verify_current(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        self.inner.verify_current(cancel).await
    }
    pub fn discard_copy(self) -> (SourceRelocationSession, Transaction<'static, Sqlite>) {
        let (inner, writer) = self.inner.discard_copy();
        (SourceRelocationSession::from_stage(inner), writer)
    }
}

/// Descriptive selected-pair facts. Genuine retained ownership is required by
/// the consuming method; constructing these facts alone grants nothing.
pub use crate::offline_control_upgrade::RestoreSealSelection;
/// Pre-admitted operation retained by the actual service, not a request future.
/// Every asynchronous method borrows this owner. Incomplete/failed phases have
/// no source/image extraction or fresh-effect grant.
pub struct SourceRestoreOperation {
    inner: crate::offline_control_upgrade::StageRestoreOperation,
}
pub struct RestoreDescriptorFailure {
    inner: crate::offline_control_upgrade::StageRestoreDescriptorFailure,
}
impl RestoreDescriptorFailure {
    pub fn into_parts(self) -> (std::fs::File, std::fs::File, anyhow::Error) {
        self.inner.into_parts()
    }
}
impl SourceCurrentImage {
    /// Admit the fixed slot and actual bounded selection storage BEFORE cloning
    /// selection strings or moving this image into an independently retained slot.
    pub fn admit_restore_operation<'a>(
        &'a mut self,
        external_host: u64,
        selection_storage_bytes: u64,
        cancel: &'a AtomicBool,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + 'a {
        self.inner
            .admit_restore_operation(external_host, selection_storage_bytes, cancel)
    }
    /// Moves only. Missing admission returns the exact unchanged image.
    pub fn into_restore_operation(self) -> Result<SourceRestoreOperation, Self> {
        match self.inner.into_restore_operation() {
            Ok(inner) => Ok(SourceRestoreOperation { inner }),
            Err(inner) => Err(Self { inner }),
        }
    }
}
impl SourceRestoreOperation {
    pub fn deadline(&self) -> std::time::Instant {
        self.inner.deadline()
    }
    pub fn canonical_record(&self) -> anyhow::Result<&[u8]> {
        self.inner.canonical_record()
    }
    pub fn sha256(&self) -> anyhow::Result<&str> {
        self.inner.sha256()
    }
    pub fn prepare<'a>(
        &'a mut self,
        selected: RestoreSealSelection,
        cancel: &'a AtomicBool,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + 'a {
        self.inner.prepare(selected, cancel)
    }
    /// Synchronous FD handoff into the already admitted caller-retained slot.
    pub fn stage_reservation(
        &mut self,
        parent: std::fs::File,
        record: std::fs::File,
    ) -> Result<(), RestoreDescriptorFailure> {
        self.inner
            .stage_reservation(parent, record)
            .map_err(|inner| RestoreDescriptorFailure { inner })
    }
    pub fn reserve<'a>(
        &'a mut self,
        cancel: &'a AtomicBool,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + 'a {
        self.inner.reserve(cancel)
    }
    pub async fn verify(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        self.inner.verify(cancel).await
    }
    pub async fn writer_in(&mut self) -> anyhow::Result<&mut SqliteConnection> {
        self.inner.writer_in().await
    }
    /// Explicit terminal settlement; no names are deleted and no old grant is
    /// reconstructed. A failed close is not successful service settlement.
    pub async fn close(&mut self) -> anyhow::Result<()> {
        self.inner.close().await
    }
}

/// Stable, pre-admitted copied-image producer. Its futures borrow exact source
/// and writer custody; a failed or abandoned capture is close-only, not a new
/// operational source grant. The service must retain this slot independently
/// of request waiters.
enum CaptureOwner {
    Legacy(crate::offline_control_upgrade::StageImageCaptureOperation),
    Retained94(super::owned_operation::RetainedCaptureIngress94),
}
/// Immutable physical custody observations, never auth or capture authority.
#[derive(Clone, Copy, Debug)]
pub struct RetainedCaptureInputCustody {
    pub target_connect: bool,
    pub target_reader: bool,
    pub target_close: bool,
    pub target_close_ack: bool,
    pub acquire_frame: bool,
    pub acquire_started: bool,
    pub acquire_failed: bool,
    pub begin_frame: bool,
    pub begin_started: bool,
    pub begin_failed: bool,
    pub writer: bool,
    pub prepared: bool,
}
/// Opaque physical observations only. No image, writer, source or future grant.
#[derive(Clone, Copy, Debug)]
pub struct RetainedNativeCaptureCustody {
    pub native_deadline: Option<std::time::Instant>,
    pub native_retained: bool,
    pub inputs: bool,
    pub parts: bool,
    pub target_connect: bool,
    pub target_reader: bool,
    pub target_close: bool,
    pub target_close_ack: bool,
    pub native_attempted: bool,
    pub native_captured: bool,
    pub writer_held: bool,
    pub writer_handed_back: bool,
    pub exact_close_ack: bool,
    pub release_attempted: bool,
    pub released: bool,
    pub histories_retained: bool,
}
/// Physical custody observations, not a paired-image/export/restore grant.
#[derive(Clone, Copy, Debug)]
pub struct RetainedPairedCaptureCustody {
    pub prepared: bool,
    pub paired_captured: bool,
    pub target_connect: bool,
    pub target_reader: bool,
    pub target_close: bool,
    pub target_close_ack: bool,
    pub native_retained: bool,
    pub writer_held: bool,
    pub histories_retained: bool,
}
pub struct SourceImageCaptureOperation {
    inner: CaptureOwner,
}
/// Missing pre-handoff admission has performed no capture. Only this failure
/// can return the unchanged source session and caller's original writer.
pub struct SourceImageCapturePreparationFailure {
    session: SourceRelocationSession,
    writer: Transaction<'static, Sqlite>,
}
impl SourceImageCapturePreparationFailure {
    pub fn into_parts(self) -> (SourceRelocationSession, Transaction<'static, Sqlite>) {
        (self.session, self.writer)
    }
}
impl SourceRelocationSession {
    pub fn admit_current_image_operation<'a>(
        &'a mut self,
        writer: &'a mut Transaction<'static, Sqlite>,
        deadline: std::time::Instant,
        cancel: &'a AtomicBool,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + 'a {
        // Preserve the legacy inner method's synchronous invalidation before
        // returning even an unpolled waiter. An owned94 marker never enters it.
        let work = if self.owned94.is_none() {
            Some(self.inner.admit_current_image_operation(writer, deadline, cancel))
        } else { None };
        async move {
            match work {Some(work)=>work.await,None=>Err(anyhow::anyhow!("owned94 admitted source rejects capture admission"))}
        }
    }
    /// Moves only after admission; it does not start a detached consuming task.
    pub fn begin_current_image_capture(
        self,
        writer: Transaction<'static, Sqlite>,
    ) -> Result<SourceImageCaptureOperation, SourceImageCapturePreparationFailure> {
        if self.owned94.is_some() {return Err(SourceImageCapturePreparationFailure {session:self,writer});}
        match self.inner.begin_current_image_capture(writer) {
            Ok(inner) => Ok(SourceImageCaptureOperation { inner: CaptureOwner::Legacy(inner) }),
            Err((inner, writer)) => Err(SourceImageCapturePreparationFailure {
                session: SourceRelocationSession::from_stage(inner),
                writer,
            }),
        }
    }
}
impl SourceImageCaptureOperation {
    pub(crate) fn from_retained94(inner: super::owned_operation::RetainedCaptureIngress94) -> Self {
        Self { inner: CaptureOwner::Retained94(inner) }
    }
    /// Actual fixed-stage acquisition and same-writer ingress admission only.
    /// The original94 source/tails remain whole; this grants no native capture.
    pub fn prepare_retained_input(&mut self) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        let work = match &mut self.inner {
            CaptureOwner::Retained94(inner) => Some(inner.prepare()),
            CaptureOwner::Legacy(_) => None,
        };
        async move {
            match work {
                Some(work) => work.await,
                None => Err(anyhow::anyhow!("legacy capture has no retained94 ingress")),
            }
        }
    }
    /// The same whole94 owns the restricted authentication-only target reader.
    /// This borrowing token never supplies the independently acquired writer.
    pub fn retained_input_authorization(&mut self) -> impl std::future::Future<Output = anyhow::Result<crate::TargetAuthorizationReader<'_>>> + '_ {
        let work = match &mut self.inner {
            CaptureOwner::Retained94(inner) => Some(crate::TargetAuthorizationReader::open_retained_capture(inner)),
            CaptureOwner::Legacy(_) => None,
        };
        async move {
            match work { Some(work) => work.await, None => Err(anyhow::anyhow!("capture authorization requires retained94 input")) }
        }
    }
    pub fn retained_input_custody(&self) -> Option<RetainedCaptureInputCustody> {
        match &self.inner { CaptureOwner::Retained94(inner) => Some(inner.custody()), CaptureOwner::Legacy(_) => None }
    }
    pub fn retained_input_prepared(&self) -> bool {
        matches!(&self.inner, CaptureOwner::Retained94(inner) if inner.prepared())
    }
    pub fn deadline(&self) -> std::time::Instant {
        match &self.inner {
            CaptureOwner::Legacy(inner) => inner.deadline(),
            CaptureOwner::Retained94(inner) => inner.deadline(),
        }
    }
    pub fn capture<'a>(
        &'a mut self,
        cancel: &'a AtomicBool,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + 'a {
        // Preserve legacy synchronous invalidation. Service ingress never calls
        // the old capture algorithm while its immediate fresh-auth seam is absent.
        let work = match &mut self.inner {
            CaptureOwner::Legacy(inner) => Some(inner.capture(cancel)),
            CaptureOwner::Retained94(_) => None,
        };
        async move {
            match work {
                Some(work) => work.await,
                None => Err(anyhow::anyhow!("retained94 capture requires fixed purpose-bound authorization")),
            }
        }
    }
    /// Only the separate legacy owner exposes its writer. Real retained94
    /// ingress is opaque until a fresh purpose-bound host authorization seam.
    pub fn writer_in(
        &mut self,
    ) -> impl std::future::Future<Output = anyhow::Result<&mut SqliteConnection>> + '_ {
        let work = match &mut self.inner {
            CaptureOwner::Legacy(inner) => Some(inner.writer_in()),
            CaptureOwner::Retained94(_) => None,
        };
        async move {
            match work {
                Some(work) => work.await,
                None => Err(anyhow::anyhow!("retained94 capture writer is not an authorization grant")),
            }
        }
    }
    pub fn verify<'a>(
        &'a mut self,
        cancel: &'a AtomicBool,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + 'a {
        let work = match &mut self.inner {
            CaptureOwner::Legacy(inner) => Some(inner.verify(cancel)),
            CaptureOwner::Retained94(_) => None,
        };
        async move {
            match work {
                Some(work) => work.await,
                None => Err(anyhow::anyhow!("retained94 opaque Native image grants no Motion closure verification")),
            }
        }
    }
    pub fn finish(self) -> Result<SourceCurrentImage, Self> {
        match self.inner {
            CaptureOwner::Legacy(inner) => match inner.finish() {
                Ok(inner) => Ok(SourceCurrentImage { inner }),
                Err(inner) => Err(Self { inner: CaptureOwner::Legacy(inner) }),
            },
            CaptureOwner::Retained94(inner) => Err(Self { inner: CaptureOwner::Retained94(inner) }),
        }
    }
    pub fn close(&mut self) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        // Construct the retained close now, so its one-way terminal claim is
        // not deferred into this wrapper's first poll. Legacy behavior remains.
        let work = match &mut self.inner {
            CaptureOwner::Legacy(inner) => Err(inner.close()),
            CaptureOwner::Retained94(inner) => Ok(inner.close()),
        };
        async move {
            match work { Ok(work) => work.await, Err(work) => work.await }
        }
    }
}

impl SourceImageCaptureOperation {
    pub fn claim_retained_native_capture(&mut self) -> anyhow::Result<()> {
        match &mut self.inner {
            CaptureOwner::Retained94(inner) => inner.claim_native_capture(),
            CaptureOwner::Legacy(_) => anyhow::bail!("native purpose requires retained94 ingress"),
        }
    }
    pub fn prepare_retained_native_capture(&mut self) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        let work = match &mut self.inner {
            CaptureOwner::Retained94(inner) => Some(inner.prepare_native_capture()),
            CaptureOwner::Legacy(_) => None,
        };
        async move { match work { Some(work) => work.await, None => anyhow::bail!("native purpose requires retained94 ingress") } }
    }
    pub fn retained_native_authorization(&mut self) -> impl std::future::Future<Output = anyhow::Result<crate::TargetAuthorizationReader<'_>>> + '_ {
        let work = match &mut self.inner {
            CaptureOwner::Retained94(inner) => Some(crate::TargetAuthorizationReader::open_retained_native_capture(inner)),
            CaptureOwner::Legacy(_) => None,
        };
        async move { match work { Some(work) => work.await, None => anyhow::bail!("native authorization requires retained94 ingress") } }
    }
    pub fn retained_native_captured(&self) -> bool {
        matches!(&self.inner, CaptureOwner::Retained94(inner) if inner.native_captured())
    }
    pub fn retained_native_custody(&self) -> Option<RetainedNativeCaptureCustody> {
        match &self.inner { CaptureOwner::Retained94(inner) => Some(inner.native_custody()), CaptureOwner::Legacy(_) => None }
    }
}

impl SourceImageCaptureOperation {
    pub fn claim_retained_paired_capture(&mut self) -> anyhow::Result<()> {
        match &mut self.inner {
            CaptureOwner::Retained94(inner) => inner.claim_paired_capture(),
            _ => anyhow::bail!("paired capture requires retained94 service custody"),
        }
    }
    pub fn prepare_retained_paired_capture(&mut self) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        let work = match &mut self.inner {
            CaptureOwner::Retained94(inner) => Some(inner.prepare_paired_capture()),
            _ => None,
        };
        async move { work.context("paired capture requires retained94 owner")?.await }
    }
    pub fn retained_paired_authorization(&mut self) -> impl std::future::Future<Output = anyhow::Result<crate::TargetAuthorizationReader<'_>>> + '_ {
        let work = match &mut self.inner {
            CaptureOwner::Retained94(inner) => Some(crate::TargetAuthorizationReader::open_retained_paired_capture(inner)),
            _ => None,
        };
        async move { work.context("paired capture requires retained94 authorization")?.await }
    }
    pub fn retained_paired_captured(&self) -> bool {
        matches!(&self.inner, CaptureOwner::Retained94(inner) if inner.paired_captured())
    }
    pub fn retained_paired_custody(&self) -> Option<RetainedPairedCaptureCustody> {
        match &self.inner { CaptureOwner::Retained94(inner) => Some(inner.paired_custody()), _ => None }
    }
}
