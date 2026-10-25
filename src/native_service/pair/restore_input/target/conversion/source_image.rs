//! Fixed post94 copy handoff. No95 mutation, rotation, install or reopen grant.
//! The native/current-image owner and SAME target writer stay ahead of the pool
//! and original stage custody. Pool close must never await a checked-out writer.
//! Aggregate94/copied-image reservation refresh is still a qualification gap;
//! component bounds are not a claim of an end-to-end256MiB/RSS bound.
mod reservation;
pub(in crate::native_service::pair) mod supervisor;
use super::*;
use crate::v2::auth::{
    Caller,
    retained::{AuthenticationSnapshot, CurrentSourceAuthentication},
};
use motion_catalog_backend::{
    RestoreSealSelection, SourceCurrentImage, SourceRelocationSession, SourceRestoreOperation,
};
use playscale_core::access::Permission;
use sqlx::{Sqlite, SqlitePool, Transaction};
pub(in crate::native_service) use supervisor::{RestoreProof, RestoreSupervisor};

enum Phase {
    Session {
        session: SourceRelocationSession,
        writer: Option<Transaction<'static, Sqlite>>,
    },
    Image(SourceCurrentImage),
    Operation(SourceRestoreOperation),
    Closed,
    CloseFailed,
    // Unexpected unwind during a consuming backend call cannot mint a retry or
    // reconstructed session. Remaining namespace/source custody is quarantined.
    Transitioning,
}
struct Custody {
    phase: Phase,
    // The original publication owner never transfers to the backend. Even a
    // prepared anonymous record and an uncertain linked Outcome are retained.
    reservation: Option<reservation::ReservationPublication>,
    // Exact incoming descriptors returned by a refused consuming backend call.
    // They are not reconstructed or used to adopt an existing reservation.
    reservation_descriptors: Option<(File, File)>,
    pool: SqlitePool,
    stage: NativeConversionStage,
}
pub(in crate::native_service::pair) struct PostRelocationPairImage {
    held: Option<Custody>,
}
static RECOVERY: std::sync::OnceLock<std::sync::Mutex<Vec<Custody>>> = std::sync::OnceLock::new();
impl Drop for PostRelocationPairImage {
    fn drop(&mut self) {
        if let Some(held) = self.held.take() {
            tracing::error!(
                unknown_transfer = matches!(held.phase, Phase::Transitioning),
                "post94 image handoff retains exact available phase/stage custody for recovery"
            );
            RECOVERY
                .get_or_init(Default::default)
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(held);
        }
    }
}
#[derive(Debug)]
struct AuthorizationError(crate::v2::Problem);
impl std::fmt::Display for AuthorizationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "post94 copied-image authorization: {:?}", self.0)
    }
}
impl std::error::Error for AuthorizationError {}
fn authorization(error: crate::v2::Problem) -> anyhow::Error {
    AuthorizationError(error).into()
}

impl SourceConversionActor {
    pub(in crate::native_service::pair) async fn into_post_relocation_image(
        self,
        caller: Caller,
        cancel: Arc<AtomicBool>,
    ) -> anyhow::Result<PostRelocationPairImage> {
        let runtime = tokio::runtime::Handle::current();
        let mut cancellation = PairCancellation(cancel.clone(), true);
        let result = tokio::task::spawn_blocking(move || {
            runtime.block_on(capture(self, caller, cancel, false))
        })
        .await
        .context("post94 copy owner panicked; stage requires explicit recovery")?;
        cancellation.1 = false;
        result
    }
}
async fn authorize(
    db: &mut sqlx::SqliteConnection,
    caller: &Caller,
    current: &mut CurrentSourceAuthentication<'_>,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    let principal = caller
        .reauthorize(db, Some(Permission::SystemAdmin))
        .await
        .map_err(authorization)?;
    let temporal =
        AuthenticationSnapshot::prepare_in(db, caller, Permission::SystemAdmin, &principal)
            .await
            .map_err(authorization)?;
    current.revalidate().await.map_err(authorization)?;
    cancelled(cancel)?;
    temporal.revalidate().map_err(authorization)?;
    current.revalidate_temporal().map_err(authorization)
}
async fn capture(
    mut actor: SourceConversionActor,
    caller: Caller,
    cancel: Arc<AtomicBool>,
    exercise_cancelled: bool,
) -> anyhow::Result<PostRelocationPairImage> {
    let SourceActorCustody {
        session,
        pool,
        stage,
    } = actor.held.take().context("source actor missing")?;
    let mut owner = PostRelocationPairImage {
        held: Some(Custody {
            phase: Phase::Session {
                session,
                writer: None,
            },
            reservation: None,
            reservation_descriptors: None,
            pool,
            stage,
        }),
    };
    let held = owner.held.as_mut().context("post94 copy custody missing")?;
    held.stage.verify_phase_custody()?;
    cancelled(&cancel)?;
    let reader = held
        .stage
        .source_writer()?
        .legacy_authorization_reader()
        .await?;
    let mut current =
        CurrentSourceAuthentication::prepare_legacy(reader, &caller, Permission::SystemAdmin)
            .await
            .map_err(authorization)?;
    let writer = held.pool.begin_with("BEGIN IMMEDIATE").await?;
    let Phase::Session { writer: slot, .. } = &mut held.phase else {
        unreachable!()
    };
    *slot = Some(writer);
    authorize(
        slot.as_mut().context("post94 target writer missing")?,
        &caller,
        &mut current,
        &cancel,
    )
    .await?;
    let Phase::Session {
        session,
        writer: Some(writer),
    } = std::mem::replace(&mut held.phase, Phase::Transitioning)
    else {
        unreachable!()
    };
    // The genuine fixture checks refusal before the first real capture, so it
    // need not extract an operational image from a completed restore operation.
    let (session, writer) = if exercise_cancelled {
        match session
            .into_current_image(writer, &AtomicBool::new(true))
            .await
        {
            Err(failure) => {
                let (session, writer, _) = failure.into_parts();
                (session, writer)
            }
            Ok(image) => {
                held.phase = Phase::Image(image);
                anyhow::bail!("cancelled image handoff succeeded");
            }
        }
    } else {
        (session, writer)
    };
    match session.into_current_image(writer, &cancel).await {
        Ok(image) => held.phase = Phase::Image(image),
        Err(failure) => {
            let (session, writer, error) = failure.into_parts();
            held.phase = Phase::Session {
                session,
                writer: Some(writer),
            };
            return Err(error.context("post94 copy refused with original session/writer retained"));
        }
    }
    let Phase::Image(image) = &mut held.phase else {
        unreachable!()
    };
    image.verify_current(&cancel).await?;
    authorize(image.writer_in().await?, &caller, &mut current, &cancel).await?;
    drop(current);
    Ok(owner)
}

fn verify_reservation_target(path: &Path, parent: &File, lock: &File) -> anyhow::Result<()> {
    verify_target_lock(parent, lock)?;
    ensure!(
        identity(&crate::native_service::pair::open_parent(path)?)? == identity(parent)?,
        "restore reservation target parent changed"
    );
    Ok(())
}

const SELECTION_PREFIX: &str = "restore-seal-";
fn selection_storage(install_id: &str, selected: &InspectedRestoreInput) -> anyhow::Result<u64> {
    let mut bytes = u64::try_from(SELECTION_PREFIX.len())?;
    for value in [
        install_id,
        install_id,
        selected.pair_sha256(),
        &selected.manifest.motion_database.sha256,
        &selected.manifest.motion_control_journals_sha256,
        &selected.manifest.motion_binding_sha256,
        &selected.manifest.native_control_journals_sha256,
    ] {
        bytes = bytes
            .checked_add(u64::try_from(value.len())?)
            .context("restore selection allocation overflow")?;
    }
    Ok(bytes)
}
fn copy_selection(
    install_id: &str,
    archive: &ArchiveCustody,
    manifest: &PairManifest,
) -> RestoreSealSelection {
    // Every new capacity was derived from these borrowed lengths at admission.
    // No format! growth, path normalization, hash or owned selection precedes it.
    let selected_pair_sha256 = match archive {
        ArchiveCustody::Published(outcome) => &outcome.evidence().artifact.sha256,
        ArchiveCustody::Selected(artifact) => artifact.sha256(),
    };
    let mut operation_id = String::with_capacity(SELECTION_PREFIX.len() + install_id.len());
    operation_id.push_str(SELECTION_PREFIX);
    operation_id.push_str(install_id);
    RestoreSealSelection {
        operation_id,
        install_id: install_id.to_owned(),
        selected_pair_sha256: selected_pair_sha256.to_owned(),
        source_motion_image_sha256: manifest.motion_database.sha256.to_owned(),
        source_motion_inventory_sha256: manifest.motion_control_journals_sha256.to_owned(),
        source_motion_binding_sha256: manifest.motion_binding_sha256.to_owned(),
        source_native_inventory_sha256: manifest.native_control_journals_sha256.to_owned(),
    }
}

/// Borrowed by the actual service worker. The held owner outlives this future,
/// including an unpolled/dropped future or a panic caught by that worker.
async fn reserve_restore(
    held: &mut Custody,
    caller: &Caller,
    cancel: &AtomicBool,
    service: &supervisor::Shared,
) -> anyhow::Result<()> {
    prepare_operation(held, caller, cancel, true).await?;
    let target = held
        .stage
        .held
        .as_mut()
        .context("conversion custody missing")?
        .target
        .held
        .as_mut()
        .context("target custody missing")?;
    let reader = target.input.current._writer.authorization_reader().await?;
    let mut current = CurrentSourceAuthentication::prepare(reader, caller, Permission::SystemAdmin)
        .await
        .map_err(authorization)?;
    let Phase::Operation(operation) = &mut held.phase else {
        unreachable!()
    };
    let deadline = operation.deadline();
    reservation::alive(cancel, deadline)?;
    authorize(operation.writer_in().await?, caller, &mut current, cancel).await?;
    reservation::alive(cancel, deadline)?;
    verify_reservation_target(&target.path, &target.parent, &target.lock)?;
    held.reservation = Some(reservation::ReservationPublication::prepare(
        &target.path,
        identity(&target.parent)?,
        operation.canonical_record()?,
        operation.sha256()?,
        cancel,
        deadline,
    )?);
    // Keep actual anonymous Sealed custody across authorization after staging's
    // fsync, and retain original Outcome before any post-publication await.
    authorize(operation.writer_in().await?, caller, &mut current, cancel).await?;
    reservation::alive(cancel, deadline)?;
    verify_reservation_target(&target.path, &target.parent, &target.lock)?;
    held.reservation
        .as_mut()
        .context("prepared reservation missing")?
        .publish(cancel)?;
    service
        .after_publication(
            held.reservation
                .as_ref()
                .expect("retained original Outcome"),
        )
        .await?;
    authorize(operation.writer_in().await?, caller, &mut current, cancel).await?;
    reservation::alive(cancel, deadline)?;
    verify_reservation_target(&target.path, &target.parent, &target.lock)?;
    held.reservation_descriptors = Some(
        held.reservation
            .as_mut()
            .context("reservation publication missing")?
            .descriptors()?,
    );
    reservation::alive(cancel, deadline)?;
    let (parent, record) = held
        .reservation_descriptors
        .take()
        .expect("stored original FDs");
    if let Err(failure) = operation.stage_reservation(parent, record) {
        let (parent, record, error) = failure.into_parts();
        held.reservation_descriptors = Some((parent, record));
        return Err(error.context("restore reservation refused with original FDs retained"));
    }
    operation.reserve(cancel).await?;
    operation.verify(cancel).await?;
    authorize(operation.writer_in().await?, caller, &mut current, cancel).await?;
    reservation::alive(cancel, deadline)?;
    verify_reservation_target(&target.path, &target.parent, &target.lock)?;
    current.retire_inline().await.map_err(authorization)?;
    held.stage.verify_phase_custody()?;
    reservation::alive(cancel, deadline)
}

async fn prepare_operation(
    held: &mut Custody,
    caller: &Caller,
    cancel: &AtomicBool,
    publication: bool,
) -> anyhow::Result<()> {
    ensure!(
        matches!(held.phase, Phase::Image(_))
            && held.reservation.is_none()
            && held.reservation_descriptors.is_none(),
        "restore operation is one-shot"
    );
    held.stage.verify_phase_custody()?;
    cancelled(cancel)?;
    let target = held
        .stage
        .held
        .as_mut()
        .context("conversion custody missing")?
        .target
        .held
        .as_mut()
        .context("target custody missing")?;
    let selected_bytes = selection_storage(&target.install_id, &target.input)?;
    let pinned = target
        .input
        .current
        .retained_native_allocations()?
        .checked_add(target.input.archive_reservation())
        .context("restore pinned allocation overflow")?;
    let publication_bytes = if publication {
        reservation::allocation_reservation(&target.path)?
    } else {
        0
    };
    let reader = target.input.current._writer.authorization_reader().await?;
    let mut current = CurrentSourceAuthentication::prepare(reader, caller, Permission::SystemAdmin)
        .await
        .map_err(authorization)?;
    let external = current
        .retained_allocations()
        .await
        .map_err(authorization)?
        .checked_add(pinned)
        .and_then(|n| n.checked_add(OVERHEAD))
        .and_then(|n| n.checked_add(64 * 1024 * 1024))
        .and_then(|n| n.checked_add(motion_catalog_backend::motion_snapshot::FreshWriter::authorization_storage_reservation()))
        .and_then(|n| n.checked_add(publication_bytes))
        .context("restore reservation allocation overflow")?;
    let Phase::Image(image) = &mut held.phase else {
        unreachable!()
    };
    authorize(image.writer_in().await?, caller, &mut current, cancel).await?;
    image
        .admit_restore_operation(external, selected_bytes, cancel)
        .await?;
    // Borrow only disjoint immutable selected fields. Current authentication
    // continues to hold the original source writer through post-prepare checks.
    let selected = copy_selection(
        &target.install_id,
        &target.input.archive,
        &target.input.manifest,
    );
    let Phase::Image(image) = std::mem::replace(&mut held.phase, Phase::Transitioning) else {
        unreachable!()
    };
    held.phase = match image.into_restore_operation() {
        Ok(operation) => Phase::Operation(operation),
        Err(image) => {
            held.phase = Phase::Image(image);
            anyhow::bail!("restore admission missing; original image retained");
        }
    };
    let Phase::Operation(operation) = &mut held.phase else {
        unreachable!()
    };
    operation.prepare(selected, cancel).await?;
    authorize(operation.writer_in().await?, caller, &mut current, cancel).await?;
    let deadline = operation.deadline();
    current.retire_inline().await.map_err(authorization)?;
    reservation::alive(cancel, deadline)
}

/// Terminal-only cleanup. Image extraction here cannot return an operational
/// source to a caller: the exact writer remains in the slot until proved rolled
/// back, then the native source is deliberately closed without any later grant.
async fn close_image(held: &mut Custody) -> anyhow::Result<()> {
    if let Phase::Operation(operation) = &mut held.phase {
        return operation.close().await;
    }
    if matches!(held.phase, Phase::Closed) {
        return Ok(());
    }
    if matches!(held.phase, Phase::Image(_)) {
        let Phase::Image(image) = std::mem::replace(&mut held.phase, Phase::Transitioning) else {
            unreachable!()
        };
        let (session, writer) = image.discard_copy();
        held.phase = Phase::Session {
            session,
            writer: Some(writer),
        };
    }
    let Phase::Session { writer, .. } = &mut held.phase else {
        anyhow::bail!("image close retains incomplete phase without a settlement claim");
    };
    if let Some(writer) = writer.as_mut() {
        motion_catalog_backend::motion_snapshot::close_retained_writer(writer).await?;
    }
    let Phase::Session { session, writer } = std::mem::replace(&mut held.phase, Phase::CloseFailed)
    else {
        unreachable!()
    };
    drop(writer);
    session.close()?;
    held.phase = Phase::Closed;
    Ok(())
}

#[cfg(test)]
pub(in crate::native_service::pair) struct ClosedImageFixture {
    owner: PostRelocationPairImage,
}
#[cfg(test)]
impl SourceConversionActor {
    /// Same genuine mode7 qualification, now without reconstructing an old grant.
    pub(in crate::native_service::pair) async fn exercise_current_image_fixture(
        self,
        caller: Caller,
    ) -> ClosedImageFixture {
        assert_eq!(
            self.held
                .as_ref()
                .unwrap()
                .session
                .identity()
                .schema_version,
            41
        );
        let runtime = tokio::runtime::Handle::current();
        let mut owner = tokio::task::spawn_blocking(move || {
            runtime
                .block_on(capture(
                    self,
                    caller.clone(),
                    Arc::new(AtomicBool::new(false)),
                    true,
                ))
                .map(|owner| (owner, caller))
        })
        .await
        .unwrap()
        .unwrap();
        let held = owner.0.held.as_mut().unwrap();
        let Phase::Image(image) = &mut held.phase else {
            panic!("actual copied image missing")
        };
        assert!(image.generation() > 0);
        assert!(
            image.effect_count() > 0,
            "genuine94 effect/ACK closure required"
        );
        assert_eq!(image.native_inventory_sha256().len(), 64);
        assert_eq!(image.closure_sha256().len(), 64);
        image.verify_current(&AtomicBool::new(false)).await.unwrap();
        prepare_operation(held, &owner.1, &AtomicBool::new(false), false)
            .await
            .unwrap();
        let Phase::Operation(operation) = &mut held.phase else {
            unreachable!()
        };
        assert_eq!(operation.sha256().unwrap().len(), 64);
        assert!(!operation.canonical_record().unwrap().is_empty());
        assert!(operation.canonical_record().unwrap().len() <= 16 * 1024);
        operation.close().await.unwrap();
        ClosedImageFixture { owner: owner.0 }
    }
}
#[cfg(test)]
impl ClosedImageFixture {
    pub(in crate::native_service::pair) async fn exercise_converted_fixture(mut self) {
        let held = self.owner.held.as_mut().unwrap();
        held.stage.verify_phase_custody().unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT max(version) FROM _sqlx_migrations")
                .fetch_one(&held.pool)
                .await
                .unwrap(),
            97
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM catabolic_native_bootstrap_acks")
                .fetch_one(&held.pool)
                .await
                .unwrap(),
            1
        );
        let target = held
            .stage
            .held
            .as_mut()
            .unwrap()
            .target
            .held
            .as_mut()
            .unwrap();
        assert_eq!(target.input.manifest.native_identity.schema_version, 40);
        assert_eq!(target.input.manifest.motion_schema_version, 93);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT max(version) FROM _sqlx_migrations")
                .fetch_one(&target.input.current._stopped.source_db().unwrap())
                .await
                .unwrap(),
            93
        );
        assert!(!target.path.join("playscale.sqlite3").exists());
        assert!(!target.path.join("catabolic.sqlite3").exists());
        target.input.current._writer.close().await.unwrap();
        held.pool.close().await;
        let mut held = self.owner.held.take().unwrap();
        // Existing mode7 final cleanup, no production source/old-image extraction.
        drop(held.stage.held.take());
    }
}
