//! Same-owner current native image plus complete Motion94 ACK closure.
//! This phase is not a rotated image,95 journal effect, install or reopen grant.
#[path = "current_image/capture.rs"]
mod capture;
pub(crate) use capture::{CaptureAdmission, StageImageCaptureOperation};
#[path = "current_image/reservation.rs"]
mod reservation;
#[path = "current_image/seal.rs"]
mod seal;
use super::*;
use crate::source_relocations::image_closure::{self, CurrentImageClosure};
use catabolic_store::migration::offline_bootstrap::CurrentCatalogImage;
pub(crate) use reservation::{StageRestoreDescriptorFailure, StageRestoreOperation};
pub use seal::RestoreSealSelection;
use sqlx::{Sqlite, Transaction};

pub(crate) struct StageCurrentImage {
    native: CurrentCatalogImage,
    writer: Transaction<'static, Sqlite>,
    proof: crate::native_bootstrap::SourcePairProof,
    witness: StageWitness,
    reference: UpgradeReference,
    closure: CurrentImageClosure,
    restore_admission: Option<reservation::RestoreAdmission>,
}
/// Returning an error never silently discards the only admitted post94 owner.
/// The caller receives back its exact original writer, session and error.
pub(crate) struct StageCurrentImageFailure {
    session: StageSourceSession,
    writer: Transaction<'static, Sqlite>,
    error: anyhow::Error,
}
impl StageCurrentImageFailure {
    pub(crate) fn error(&self) -> &anyhow::Error {
        &self.error
    }
    pub(crate) fn into_parts(
        self,
    ) -> (
        StageSourceSession,
        Transaction<'static, Sqlite>,
        anyhow::Error,
    ) {
        (self.session, self.writer, self.error)
    }
}

// Reuse the existing56MiB Motion inspected-image cohort, not a claim that this
// scalar covers all simultaneous native/host allocations. Logical page count
// includes the current writer state, unlike a stale file length under WAL.
async fn preflight_motion_image(
    db: &mut SqliteConnection,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    ensure!(
        !cancel.load(Ordering::Acquire),
        "current image capture cancelled"
    );
    let pages: i64 = sqlx::query_scalar("PRAGMA main.page_count")
        .fetch_one(&mut *db)
        .await?;
    let page_size: i64 = sqlx::query_scalar("PRAGMA main.page_size")
        .fetch_one(&mut *db)
        .await?;
    let bytes = u64::try_from(pages)?
        .checked_mul(u64::try_from(page_size)?)
        .context("current Motion image size overflow")?;
    ensure!(
        bytes > 0 && bytes <= crate::motion_snapshot::MAX_IMAGE,
        "current Motion image exceeds inspected-image cohort"
    );
    Ok(())
}

impl StageSourceSession {
    pub(crate) async fn into_current_image(
        mut self,
        mut writer: Transaction<'static, Sqlite>,
        cancel: &AtomicBool,
    ) -> Result<StageCurrentImage, StageCurrentImageFailure> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let before = async {
            seal::preflight_capture(&mut self, &mut writer, deadline, cancel).await?;
            self.verify_pair_in(&mut writer).await?;
            preflight_motion_image(&mut writer, cancel).await
        }
        .await;
        if let Err(error) = before {
            return Err(StageCurrentImageFailure {
                session: self,
                writer,
                error,
            });
        }
        let StageSourceSession {
            capture_admission: _,
            native,
            proof,
            witness,
            reference,
        } = self;
        let mut native = match native.into_current_image(cancel) {
            Ok(image) => image,
            Err(failure) => {
                let error = anyhow::anyhow!("native current image capture: {}", failure.error());
                return Err(StageCurrentImageFailure {
                    session: StageSourceSession {
                        capture_admission: None,
                        native: failure.into_session(),
                        proof,
                        witness,
                        reference,
                    },
                    writer,
                    error,
                });
            }
        };
        let checked = async {
            seal::preflight_captured(
                &mut native,
                &mut writer,
                &proof,
                &witness,
                &reference,
                deadline,
                cancel,
            )
            .await?;
            verify_pair_parts(&mut writer, &proof, &witness, &reference).await?;
            preflight_motion_image(&mut writer, cancel).await?;
            let closure = image_closure::verify_in(&mut writer, &native, cancel).await?;
            verify_pair_parts(&mut writer, &proof, &witness, &reference).await?;
            ensure!(
                !cancel.load(Ordering::Acquire),
                "current image capture cancelled"
            );
            seal::alive(cancel, deadline)?;
            Ok::<_, anyhow::Error>(closure)
        }
        .await;
        match checked {
            Ok(closure) => Ok(StageCurrentImage {
                native,
                writer,
                proof,
                witness,
                reference,
                closure,
                restore_admission: None,
            }),
            Err(error) => Err(StageCurrentImageFailure {
                session: StageSourceSession {
                    capture_admission: None,
                    native: native.discard_copy(),
                    proof,
                    witness,
                    reference,
                },
                writer,
                error,
            }),
        }
    }
}
impl StageCurrentImage {
    #[cfg(test)]
    pub(super) fn allocation_provenance_for_test(&self) -> (u64, u64) {
        self.proof.allocation_provenance()
    }

    pub(crate) fn native_inventory_sha256(&self) -> &str {
        self.native.current_inventory_sha256()
    }
    pub(crate) fn generation(&self) -> i64 {
        self.native.generation()
    }
    pub(crate) fn closure_sha256(&self) -> &str {
        self.closure.sha256()
    }
    pub(crate) fn effect_count(&self) -> usize {
        self.closure.effect_count()
    }
    /// Trusted application authorization access to its SAME previously supplied
    /// writer. No new connection, pool or native Store access is created.
    pub(crate) async fn writer_in(&mut self) -> anyhow::Result<&mut SqliteConnection> {
        verify_pair_parts(
            &mut self.writer,
            &self.proof,
            &self.witness,
            &self.reference,
        )
        .await?;
        Ok(&mut self.writer)
    }
    pub(crate) async fn verify_current(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        verify_pair_parts(
            &mut self.writer,
            &self.proof,
            &self.witness,
            &self.reference,
        )
        .await?;
        preflight_motion_image(&mut self.writer, cancel).await?;
        let current = image_closure::verify_in(&mut self.writer, &self.native, cancel).await?;
        ensure!(
            current.sha256() == self.closure.sha256(),
            "current paired94 closure changed"
        );
        verify_pair_parts(
            &mut self.writer,
            &self.proof,
            &self.witness,
            &self.reference,
        )
        .await
    }
    /// Discard only the uninstalled copy, preserving original post94 custody.
    pub(crate) fn discard_copy(self) -> (StageSourceSession, Transaction<'static, Sqlite>) {
        (
            StageSourceSession {
                capture_admission: None,
                native: self.native.discard_copy(),
                proof: self.proof,
                witness: self.witness,
                reference: self.reference,
            },
            self.writer,
        )
    }
}
