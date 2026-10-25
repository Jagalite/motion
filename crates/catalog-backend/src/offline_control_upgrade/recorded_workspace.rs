//! Borrow-only proof of the persistent half of the original control64 cohort.
//! This is no source validation, external-budget credit or detached certificate.
use super::*;
use crate::native_bootstrap::prepare_operation::{NativeIntentAdmission, apply_operation::{
    NativeApplyAdmission, NativeIntentApplyProgress,
    bootstrap_operation::{BootstrapPurpose, BootstrapStep, BootstrapWork},
}};

fn add(total: &mut u64, bytes: u64) -> anyhow::Result<()> {
    *total = total.checked_add(bytes).context("control workspace capacity overflow")?;
    Ok(())
}
fn retained(operation: &RecordedControlOperation) -> anyhow::Result<u64> {
    let held = operation.prepared.as_ref().context("control workspace owner missing")?;
    // Headers are conservative even where original inline storage already pays
    // for them. The separately named SQLx reader, SQLite DBSTATUS, external
    // service cohort and publication charge are NEVER made available here.
    let mut bytes = std::mem::size_of::<RecordedControlOperation>() as u64;
    add(&mut bytes, std::mem::size_of::<PreparedRecordedControlUpgrade>() as u64)?;
    add(&mut bytes, std::mem::size_of::<QualifiedControlWorkspace<'_>>() as u64)?;
    add(&mut bytes, held.reference.0.retained_allocations()?)?;
    add(&mut bytes, held.original.retained_allocations()?)?;
    add(&mut bytes, held.prepared.before.retained_allocations()?)?;
    add(&mut bytes, operation.progress.additions.retained_allocations()?)?;
    // Include these small separately backed fields conservatively rather than
    // using their charge to enlarge the workspace's available half.
    add(&mut bytes, held.prepared.binding.capacity() as u64)?;
    add(&mut bytes, held.prepared.witness.path.capacity() as u64)?;
    add(&mut bytes, held.prepared.witness.initial_sha.capacity() as u64)?;
    for text in [&held.anchor.operation_id, &held.anchor.selected_pair_sha256,
        &held.anchor.original93_image_sha256, &held.anchor.original93_inventory_sha256,
        &held.anchor.frozen_binding_sha256] {
        add(&mut bytes, text.capacity() as u64)?;
    }
    // Ready97 has transferred all publication payloads into this witness. Its
    // actual capacities (and even its two unused verifier buffers) are counted
    // conservatively in addition to their unchanged independent publication H.
    let publication = operation.progress.publication.as_ref()
        .context("control workspace publication missing")?;
    ensure!(publication.phase() == "durable evidence transferred",
        "control workspace publication is not transferred");
    let record = held.prepared.witness.record.as_ref()
        .context("control workspace durable record missing")?;
    add(&mut bytes, record.retained_and_verification_bytes()?)?;
    Ok(bytes)
}

/// Cannot be constructed, cloned, moved to another owner, or turned into a
/// byte allowance by callers. It is stored only on the measured recorded owner.
/// That owner's retained cohort has no mutation transition in Ready97; native
/// work only mutates its independently accounted DB/admission state. Settlement
/// leaves Ready97; tail extraction occurs after the one native apply attempt.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkspacePurpose {
    Apply,
    BootstrapPreflight(BootstrapPurpose),
    BootstrapExecute,
}
pub(super) struct ControlWorkspaceQualification {
    eligible: bool,
    purpose: WorkspacePurpose,
}
impl ControlWorkspaceQualification {
    pub(super) fn measure(operation: &RecordedControlOperation) -> anyhow::Result<Self> {
        Self::measure_for(operation, WorkspacePurpose::Apply)
    }
    pub(super) fn measure_for(
        operation: &RecordedControlOperation,
        purpose: WorkspacePurpose,
    ) -> anyhow::Result<Self> {
        ensure!(operation.phase == RecordedControlPhase::Ready97,
            "control workspace qualification requires Ready97");
        ensure!(operation.workspace_qualification.is_none(),
            "control workspace qualification cannot retry");
        ensure!(operation.control_workspace.is_some(), "control workspace backing missing");
        Ok(Self { eligible: retained(operation)? <= OfflineControlWorkspaceBacking::RETAINED_LIMIT, purpose })
    }
    #[cfg(test)]
    pub(super) fn eligible(&self) -> bool { self.eligible }
}

pub(super) enum ControlWorkspaceUse<'a> {
    Qualified(QualifiedControlWorkspace<'a>),
    FullCharge(&'a mut RecordedControlOperation),
}
/// Private constructor consumes this same owner's preflight proof and keeps
/// its exclusive borrow until the fixed native call returns. No public occupancy
/// number, transferred certificate, source query or capacity walk is accepted.
/// Native only enforces the counted backing/context; this producer proves host
/// occupancy and suspends every semantic control scan while native borrows it.
pub(super) struct QualifiedControlWorkspace<'a> {
    operation: &'a mut RecordedControlOperation,
}
impl<'a> QualifiedControlWorkspace<'a> {
    pub(super) fn borrow(operation: &'a mut RecordedControlOperation) -> anyhow::Result<ControlWorkspaceUse<'a>> {
        Self::borrow_for(operation, WorkspacePurpose::Apply)
    }
    pub(super) fn borrow_for(
        operation: &'a mut RecordedControlOperation,
        expected: WorkspacePurpose,
    ) -> anyhow::Result<ControlWorkspaceUse<'a>> {
        ensure!(operation.phase == RecordedControlPhase::Ready97,
            "control workspace requires original Ready97");
        ensure!(operation.control_workspace.is_some(), "control workspace backing missing");
        let qualification = operation.workspace_qualification.take()
            .context("control workspace preflight proof missing")?;
        ensure!(qualification.purpose == expected, "control workspace purpose differs");
        if !qualification.eligible {
            return Ok(ControlWorkspaceUse::FullCharge(operation));
        }
        Ok(ControlWorkspaceUse::Qualified(Self { operation }))
    }
    pub(super) fn apply(
        self,
        admitted: &mut NativeIntentAdmission,
        apply: &mut NativeApplyAdmission,
        progress: &mut NativeIntentApplyProgress,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        let held = self.operation.prepared.as_ref().context("qualified control owner missing")?;
        let backing = self.operation.control_workspace.as_mut()
            .context("qualified control backing missing")?;
        admitted.apply_in(&held.prepared.db, apply, progress, cancel, Some(backing))
    }
}

impl ControlWorkspaceUse<'_> {
    /// The entire recorded owner stays exclusively borrowed, but Native acquires
    /// a loan only INSIDE each fixed producer call. Host SQL/accounting between
    /// producers never runs under a scratch loan. No public callback escapes.
    pub(super) fn bootstrap_step(
        self,
        admitted: &mut NativeIntentAdmission,
        work: &mut BootstrapWork,
        step: BootstrapStep,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        let continue_execute = matches!(&step, BootstrapStep::Preflight(BootstrapPurpose::Execute));
        let (operation, eligible) = match self {
            Self::Qualified(workspace) => (workspace.operation, true),
            Self::FullCharge(operation) => (operation, false),
        };
        let held = operation.prepared.as_ref().context("bootstrap control owner missing")?;
        let backing = if eligible {
            Some(operation.control_workspace.as_mut().context("bootstrap control backing missing")?)
        } else {
            None
        };
        admitted.bootstrap_step_in(&held.prepared.db, work, step, cancel, backing)?;
        // Publish only after ALL Execute preflight checks and scans succeeded.
        // The measured persistent cohort cannot mutate in Ready97. The later
        // effect consumes this purpose-bound proof without walking capacities
        // after the app's genuine current+target temporal fence.
        if continue_execute {
            operation.workspace_qualification = Some(ControlWorkspaceQualification {
                eligible,
                purpose: WorkspacePurpose::BootstrapExecute,
            });
        }
        Ok(())
    }
}
