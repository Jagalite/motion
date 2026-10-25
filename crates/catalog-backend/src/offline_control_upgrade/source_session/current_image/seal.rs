//! Fixed current-pair seal before any copied-image rotation. The selected facts
//! are descriptive trusted-host claims, checked against the retained97 record;
//! they never construct native authority or authorize filesystem publication.
#[path = "seal/allocation.rs"]
mod allocation;
use super::*;
use serde::Serialize;
use std::time::Instant;

const MAX_SEAL: usize = 16 * 1024;
const FRAMING: u64 = 64 * 1024;
const LIMIT: u64 = 256 * 1024 * 1024;
// The fixed copier independently admits this same inventory/verifier allowance.
const SNAPSHOT_SCRATCH: u64 = 48 * 1024 * 1024;

pub(super) struct SealAccounting {
    pub(super) external: u64,
}
// Native current-image capture does not materialize a Motion snapshot. Admit
// the Motion copier's scratch only when that later fixed phase actually starts;
// never subtract an already-forwarded high-water reservation on a retry.
#[derive(Clone, Copy)]
enum HostPhase {
    CurrentNativeImage,
    MotionSeal,
}
impl HostPhase {
    fn base(self, external: u64, owned: u64, target: u64) -> anyhow::Result<u64> {
        let scratch = match self {
            Self::CurrentNativeImage => 0,
            Self::MotionSeal => SNAPSHOT_SCRATCH,
        };
        sum(&[external, owned, target, scratch])
    }
}
fn sum(parts: &[u64]) -> anyhow::Result<u64> {
    parts.iter().try_fold(0u64, |total, value| {
        total
            .checked_add(*value)
            .filter(|n| *n <= LIMIT)
            .context("restore seal aggregate allocation exceeded")
    })
}
// All inputs are fixed retained owner fields. This is numeric admission only,
// never a replacement for the following exact pair/closure authentication.
async fn preflight_host(
    phase: HostPhase,
    writer: &mut SqliteConnection,
    proof: &crate::native_bootstrap::SourcePairProof,
    witness: &StageWitness,
    reference: &UpgradeReference,
    external: u64,
    extra_retained: u64,
    native: u64,
    previous: u64,
    deadline: Instant,
    cancel: &AtomicBool,
) -> anyhow::Result<u64> {
    alive(cancel, deadline)?;
    let record = witness
        .record
        .as_ref()
        .map(|record| record.retained_and_verification_bytes())
        .transpose()?
        .unwrap_or(0);
    let owned = sum(&[
        proof.retained_allocations()?,
        reference.0.retained_allocations()?,
        record,
        u64::try_from(witness.path.capacity())?,
        u64::try_from(witness.initial_sha.capacity())?,
        extra_retained,
        FRAMING,
    ])?;
    let target = {
        let mut lease = writer.lock_handle().await?;
        let db = unsafe { Connection::from_handle(lease.as_raw_handle().as_ptr()) }?;
        crate::motion_snapshot::connection_allocations(&[&db])?
    };
    let base = phase.base(external, owned, target)?;
    let remaining = LIMIT
        .checked_sub(sum(&[native, base])?)
        .context("restore seal scanner has no allocation remainder")?;
    // Existing record verification buffers and path framing were admitted above.
    // Reject a foreign/ended writer before parsing any of its journal contents.
    witness.verify(false)?;
    {
        let mut lease = writer.lock_handle().await?;
        let db = unsafe { Connection::from_handle(lease.as_raw_handle().as_ptr()) }?;
        verify_writer_identity(&db, witness)?;
    }

    let transient = allocation::preflight_in(writer, remaining, deadline, cancel).await?;
    let target_after = {
        let mut lease = writer.lock_handle().await?;
        let db = unsafe { Connection::from_handle(lease.as_raw_handle().as_ptr()) }?;
        crate::motion_snapshot::connection_allocations(&[&db])?
    };
    // Final retained growth is conservatively counted again. Prior scratch is
    // not an input to fresh arithmetic, so retries cannot compound the allowance.
    let fresh = sum(&[base, transient, target_after.saturating_sub(target)])?;
    let forwarded = previous.max(fresh);
    sum(&[native, forwarded])?;
    alive(cancel, deadline)?;
    Ok(forwarded)
}
pub(super) async fn preflight_capture(
    session: &mut StageSourceSession,
    writer: &mut SqliteConnection,
    deadline: Instant,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    preflight_capture_parts(
        &mut session.native,
        writer,
        &session.proof,
        &session.witness,
        &session.reference,
        deadline,
        cancel,
        0,
    )
    .await
}
pub(super) async fn preflight_capture_parts(
    native: &mut CompletedCatalogSession,
    writer: &mut SqliteConnection,
    proof: &crate::native_bootstrap::SourcePairProof,
    witness: &StageWitness,
    reference: &UpgradeReference,
    deadline: Instant,
    cancel: &AtomicBool,
    extra_retained: u64,
) -> anyhow::Result<()> {
    alive(cancel, deadline)?;
    let previous = native.external_retained_reservation()?;
    let external = proof.capture_external_baseline(previous)?;
    let forwarded = preflight_host(
        HostPhase::CurrentNativeImage,
        writer,
        proof,
        witness,
        reference,
        external,
        extra_retained,
        native.retained_allocations()?,
        previous,
        deadline,
        cancel,
    )
    .await?;
    native.refresh_external_reservation(
        catabolic_store::migration::offline_bootstrap::OfflineAllocationReservation::new(
            forwarded,
        )?,
    )?;
    proof.record_allocation_forward(previous, forwarded)?;
    alive(cancel, deadline)
}
/// Existing89–91 ingress only: account the real writer and all transitive
/// verification first, then freshly qualify the original split control owner.
/// No certificate or exclusive loan crosses an await. This grants no Native
/// capture and leaves the legacy/native-capture preflight above unchanged.
pub(super) async fn preflight_capture_ingress94(
    native: &mut CompletedCatalogSession,
    writer: &mut SqliteConnection,
    proof: &mut crate::native_bootstrap::SourcePairProof,
    witness: &StageWitness,
    reference: &UpgradeReference,
    deadline: Instant,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    alive(cancel, deadline)?;
    let previous = native.external_retained_reservation()?;
    let external = proof.capture_external_baseline(previous)?;
    let forwarded = preflight_host(
        HostPhase::CurrentNativeImage,
        writer,
        proof,
        witness,
        reference,
        external,
        0,
        native.retained_allocations()?,
        previous,
        deadline,
        cancel,
    )
    .await?;
    // Keep the full forwarded high-water, including actual target DBSTATUS,
    // scanner allowance and observed growth. Only the genuine existing backing
    // can qualify; foreign/missing/ineligible backing receives no scalar credit.
    proof.refresh_source_workspace94(native, reference, witness, forwarded)?;
    proof.record_allocation_forward(previous, forwarded)?;
    alive(cancel, deadline)
}
pub(super) async fn preflight_captured(
    native: &mut CurrentCatalogImage,
    writer: &mut SqliteConnection,
    proof: &crate::native_bootstrap::SourcePairProof,
    witness: &StageWitness,
    reference: &UpgradeReference,
    deadline: Instant,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    preflight_captured_extra(
        native, writer, proof, witness, reference, deadline, cancel, 0,
    )
    .await
}
pub(super) async fn preflight_captured_extra(
    native: &mut CurrentCatalogImage,
    writer: &mut SqliteConnection,
    proof: &crate::native_bootstrap::SourcePairProof,
    witness: &StageWitness,
    reference: &UpgradeReference,
    deadline: Instant,
    cancel: &AtomicBool,
    extra_retained: u64,
) -> anyhow::Result<()> {
    alive(cancel, deadline)?;
    let previous = native.external_retained_reservation()?;
    let external = proof.capture_external_baseline(previous)?;
    let forwarded = preflight_host(
        HostPhase::CurrentNativeImage,
        writer,
        proof,
        witness,
        reference,
        external,
        extra_retained,
        native.retained_allocations()?,
        previous,
        deadline,
        cancel,
    )
    .await?;
    native.refresh_external_reservation(
        catabolic_store::migration::offline_bootstrap::OfflineAllocationReservation::new(
            forwarded,
        )?,
    )?;
    proof.record_allocation_forward(previous, forwarded)?;
    alive(cancel, deadline)
}
// Retained source94 alone carries the original affine host backing. The full
// host scan is unchanged; freshly measure its exact persistent cohort before
// synchronously refreshing the same native owner. Ordinary callers stay above.
pub(super) async fn preflight_retained_captured_extra(
    native: &mut CurrentCatalogImage, writer: &mut SqliteConnection,
    proof: &mut crate::native_bootstrap::SourcePairProof, witness: &StageWitness,
    reference: &UpgradeReference, deadline: Instant, cancel: &AtomicBool,
    extra_retained: u64,
) -> anyhow::Result<()> {
    alive(cancel, deadline)?;
    let previous = native.external_retained_reservation()?;
    let external = proof.capture_external_baseline(previous)?;
    let forwarded = preflight_host(HostPhase::CurrentNativeImage, writer, proof,
        witness, reference, external, extra_retained, native.retained_allocations()?,
        previous, deadline, cancel).await?;
    proof.refresh_image_workspace94(native, reference, witness, forwarded)?;
    proof.record_allocation_forward(previous, forwarded)?;
    alive(cancel, deadline)
}
impl SealAccounting {
    async fn preflight(
        &mut self,
        image: &mut StageCurrentImage,
        selection: &RestoreSealSelection,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<u64> {
        self.preflight_with_extra(
            image,
            selection,
            super::reservation::storage()?,
            deadline,
            cancel,
        )
        .await
    }
    async fn preflight_with_extra(
        &mut self,
        image: &mut StageCurrentImage,
        selection: &RestoreSealSelection,
        extra: u64,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<u64> {
        self.preflight_owned(
            image,
            sum(&[selection.retained_allocations()?, extra])?,
            deadline,
            cancel,
        )
        .await
    }
    pub(super) async fn preflight_owned(
        &mut self,
        image: &mut StageCurrentImage,
        owned: u64,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<u64> {
        ensure!(
            image.witness.record.is_some(),
            "restore seal requires durable original97 evidence"
        );
        let previous = image.native.external_retained_reservation()?;
        image.proof.retain_external_claim(self.external)?;
        let external = image.proof.capture_external_baseline(previous)?;
        let native = image.native.retained_allocations()?;
        let forwarded = preflight_host(
            HostPhase::MotionSeal,
            &mut image.writer,
            &image.proof,
            &image.witness,
            &image.reference,
            external,
            owned,
            native,
            previous,
            deadline,
            cancel,
        )
        .await?;
        image.native.refresh_external_reservation(
            catabolic_store::migration::offline_bootstrap::OfflineAllocationReservation::new(
                forwarded,
            )?,
        )?;
        image.proof.record_allocation_forward(previous, forwarded)?;
        alive(cancel, deadline)?;
        // Copier independently admits this same48MiB scratch. Every other
        // retained and transient allowance, including high-water, stays covered.
        sum(&[
            native,
            forwarded
                .checked_sub(SNAPSHOT_SCRATCH)
                .context("restore seal snapshot reservation missing")?,
        ])
    }
}

pub struct RestoreSealSelection {
    pub operation_id: String,
    pub install_id: String,
    pub selected_pair_sha256: String,
    pub source_motion_image_sha256: String,
    pub source_motion_inventory_sha256: String,
    pub source_motion_binding_sha256: String,
    pub source_native_inventory_sha256: String,
}
impl RestoreSealSelection {
    pub(super) fn retained_allocations(&self) -> anyhow::Result<u64> {
        let mut bytes = 0u64;
        for value in [
            &self.operation_id,
            &self.install_id,
            &self.selected_pair_sha256,
            &self.source_motion_image_sha256,
            &self.source_motion_inventory_sha256,
            &self.source_motion_binding_sha256,
            &self.source_native_inventory_sha256,
        ] {
            bytes = sum(&[bytes, u64::try_from(value.capacity())?])?;
        }
        Ok(bytes)
    }
    fn validate(&self) -> anyhow::Result<()> {
        for id in [&self.operation_id, &self.install_id] {
            ensure!(
                !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control),
                "restore seal identifier exceeds supported cohort"
            );
        }
        for hash in [
            &self.selected_pair_sha256,
            &self.source_motion_image_sha256,
            &self.source_motion_inventory_sha256,
            &self.source_motion_binding_sha256,
            &self.source_native_inventory_sha256,
        ] {
            ensure!(
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "restore seal selected commitment is malformed"
            );
        }
        Ok(())
    }
}
#[derive(Serialize)]
struct Seal<'a> {
    contract_version: u32,
    operation_id: &'a str,
    install_id: &'a str,
    selected_pair_sha256: &'a str,
    source_motion_image_sha256: &'a str,
    source_motion_inventory_sha256: &'a str,
    source_motion_binding_sha256: &'a str,
    source_native_inventory_sha256: &'a str,
    stage_device: u64,
    stage_inode: u64,
    control_record_sha256: &'a str,
    bootstrap: &'a crate::native_bootstrap::SourcePairCommitments,
    native_generation: i64,
    native_current_inventory_sha256: &'a str,
    native_transition_sha256: &'a str,
    source_effect_closure_sha256: &'a str,
    source_effect_count: usize,
    motion_current_image_sha256: &'a str,
    motion_current_inventory_sha256: &'a str,
}

pub(super) struct SealPreparation {
    pub(super) selection: RestoreSealSelection,
    pub(super) deadline: Instant,
    pub(super) accounting: SealAccounting,
}
pub(super) struct PreparedSeal {
    pub(super) canonical: Vec<u8>,
    pub(super) sha256: String,
    pub(super) parent_identity: (u64, u64),
    motion_inventory_sha256: String,
}
pub(super) fn alive(cancel: &AtomicBool, deadline: Instant) -> anyhow::Result<()> {
    ensure!(
        !cancel.load(Ordering::Acquire) && Instant::now() < deadline,
        "restore sealing cancelled or deadline exhausted"
    );
    Ok(())
}
struct Count(usize);
impl std::io::Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|n| *n <= MAX_SEAL)
            .ok_or_else(|| std::io::Error::other("restore seal exceeds bounded record"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn canonical(seal: &Seal<'_>) -> anyhow::Result<Vec<u8>> {
    let mut count = Count(0);
    serde_json::to_writer(&mut count, seal)?;
    let mut bytes = Vec::with_capacity(count.0);
    serde_json::to_writer(&mut bytes, seal)?;
    ensure!(bytes.len() == count.0, "restore seal encoding changed");
    Ok(bytes)
}
impl StageCurrentImage {
    pub(super) async fn build_prepared_seal(
        &mut self,
        input: &mut SealPreparation,
        cancel: &AtomicBool,
    ) -> anyhow::Result<PreparedSeal> {
        let (canonical, parent_identity, motion_inventory_sha256) = self
            .build_seal(
                &input.selection,
                &mut input.accounting,
                cancel,
                input.deadline,
            )
            .await?;
        let sha256 = catabolic_core::sha256(&canonical);
        alive(cancel, input.deadline)?;
        Ok(PreparedSeal {
            canonical,
            sha256,
            parent_identity,
            motion_inventory_sha256,
        })
    }
    async fn build_seal(
        &mut self,
        selection: &RestoreSealSelection,
        accounting: &mut SealAccounting,
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> anyhow::Result<(Vec<u8>, (u64, u64), String)> {
        alive(cancel, deadline)?;
        selection.validate()?;
        let retained = accounting
            .preflight(self, selection, deadline, cancel)
            .await?;
        self.verify_current(cancel).await?;
        let record = self
            .witness
            .record
            .as_ref()
            .context("restore seal requires durable original97 evidence")?;
        let parent = record.verified_parent_identity()?;
        let anchor = record.anchor();
        ensure!(
            selection.selected_pair_sha256 == anchor.selected_pair_sha256
                && selection.source_motion_image_sha256 == anchor.original93_image_sha256
                && selection.source_motion_inventory_sha256 == anchor.original93_inventory_sha256
                && (self.witness.device, self.witness.inode)
                    == (anchor.stage_device, anchor.stage_inode),
            "restore seal selected pair or original stage differs"
        );
        let control_record = record.sha256();
        // Native measurement excludes the external host scalar. The copier
        // separately includes four simultaneous images, its48MiB inventory and
        // verifier reserve, and the actual leased target SQLite counters.
        let (bytes, current) = {
            let mut lease = self.writer.lock_handle().await?;
            let db = unsafe { Connection::from_handle(lease.as_raw_handle().as_ptr()) }?;
            let commitments = self.proof.seal_commitments_in(&db)?;
            ensure!(
                commitments.identity.catalog_id == self.native.catalog_id()
                    && commitments.identity.incarnation == self.native.incarnation()
                    && commitments.identity.schema_version == 41,
                "restore seal current native identity differs"
            );
            let snapshot = crate::motion_snapshot::capture_current_source_pair(
                &db,
                &self.proof,
                cancel,
                retained,
            )?;
            ensure!(
                snapshot
                    .binding()
                    .context("restore seal binding missing")?
                    .evidence_sha256()
                    == selection.source_motion_binding_sha256,
                "restore seal original full binding differs"
            );
            let seal = Seal {
                contract_version: 1,
                operation_id: &selection.operation_id,
                install_id: &selection.install_id,
                selected_pair_sha256: &selection.selected_pair_sha256,
                source_motion_image_sha256: &selection.source_motion_image_sha256,
                source_motion_inventory_sha256: &selection.source_motion_inventory_sha256,
                source_motion_binding_sha256: &selection.source_motion_binding_sha256,
                source_native_inventory_sha256: &selection.source_native_inventory_sha256,
                stage_device: self.witness.device,
                stage_inode: self.witness.inode,
                control_record_sha256: &control_record,
                bootstrap: &commitments,
                native_generation: self.native.generation(),
                native_current_inventory_sha256: self.native.current_inventory_sha256(),
                native_transition_sha256: self.native.transition_digest(),
                source_effect_closure_sha256: self.closure.sha256(),
                source_effect_count: self.closure.effect_count(),
                motion_current_image_sha256: snapshot.sha256(),
                motion_current_inventory_sha256: snapshot.inventory_sha256(),
            };
            let bytes = canonical(&seal)?;
            let current = snapshot.inventory_sha256().to_owned();
            (bytes, current)
            // The temporary verified serialized Motion image is dropped here,
            // before any native rotation or durable artifact allocation.
        };
        self.verify_current(cancel).await?;
        {
            let mut lease = self.writer.lock_handle().await?;
            let db = unsafe { Connection::from_handle(lease.as_raw_handle().as_ptr()) }?;
            ensure!(
                crate::motion_snapshot::retained_inventory_sha256(&db, cancel)? == current,
                "restore seal current Motion inventory changed"
            );
        }
        alive(cancel, deadline)?;
        Ok((bytes, parent, current))
    }
}
impl PreparedSeal {
    pub(super) async fn verify_sealed(
        &self,
        image: &mut StageCurrentImage,
        input: &mut SealPreparation,
        extra: u64,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        alive(cancel, input.deadline)?;
        let owned = sum(&[
            extra,
            u64::try_from(self.canonical.capacity())?,
            u64::try_from(self.sha256.capacity())?,
            u64::try_from(self.motion_inventory_sha256.capacity())?,
        ])?;
        input
            .accounting
            .preflight_with_extra(image, &input.selection, owned, input.deadline, cancel)
            .await?;
        image.verify_current(cancel).await?;
        let record = image
            .witness
            .record
            .as_ref()
            .context("restore reservation lost original97 record")?;
        ensure!(
            record.verified_parent_identity()? == self.parent_identity,
            "restore reservation original parent changed"
        );
        {
            let mut lease = image.writer.lock_handle().await?;
            let db = unsafe { Connection::from_handle(lease.as_raw_handle().as_ptr()) }?;
            ensure!(
                crate::motion_snapshot::retained_inventory_sha256(&db, cancel)?
                    == self.motion_inventory_sha256,
                "restore reservation sealed Motion inventory changed"
            );
        }
        alive(cancel, input.deadline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn motion_snapshot_scratch_begins_at_seal_not_native_capture() {
        let (external, owned, target) = (80_501_960, 3_228_735, 2_473_096);
        let capture = HostPhase::CurrentNativeImage
            .base(external, owned, target)
            .unwrap();
        let seal = HostPhase::MotionSeal.base(external, owned, target).unwrap();
        assert_eq!(seal - capture, SNAPSHOT_SCRATCH);
        // Actual measured scanner/growth remain charged, not classified as
        // future copier scratch. Previous admitted high-water remains a floor.
        let transient = 31_946_024;
        let growth = 2_486_152 - target;
        let fresh = sum(&[capture, transient, growth]).unwrap();
        assert_eq!(fresh, 118_162_871);
        assert_eq!(115_458_560u64.max(fresh), fresh);
        assert_eq!(168_494_519u64.max(fresh), 168_494_519);
        assert_eq!(sum(&[seal, transient, growth]).unwrap(), 168_494_519);
        // No change to the cap: the later seal must refuse if its own scratch
        // cannot fit before materializing a Motion snapshot.
        assert!(HostPhase::CurrentNativeImage.base(LIMIT, 0, 0).is_ok());
        assert!(HostPhase::MotionSeal.base(LIMIT, 0, 0).is_err());
        assert!(HostPhase::CurrentNativeImage.base(u64::MAX, 1, 0).is_err());
    }
    #[test]
    fn descriptive_selection_and_canonical_record_remain_bounded() {
        let mut selection = RestoreSealSelection {
            operation_id: "operation".into(),
            install_id: "install".into(),
            selected_pair_sha256: "a".repeat(64),
            source_motion_image_sha256: "b".repeat(64),
            source_motion_inventory_sha256: "c".repeat(64),
            source_motion_binding_sha256: "d".repeat(64),
            source_native_inventory_sha256: "e".repeat(64),
        };
        selection.validate().unwrap();
        selection.operation_id.push('\0');
        assert!(selection.validate().is_err());
        selection.operation_id = "x".repeat(257);
        assert!(selection.validate().is_err());
        selection.operation_id = "valid".into();
        selection.selected_pair_sha256 = "A".repeat(64);
        assert!(selection.validate().is_err());
        let mut count = Count(0);
        assert!(serde_json::to_writer(&mut count, &"x".repeat(MAX_SEAL)).is_err());
    }
}
