//! Preadmitted terminal settlement, never source authority or a retry corridor.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceSessionClosePhase {
    Ready,
    FailedBeforeRelease,
    /// Native release began, but an unwind may prevent observing its completion.
    ReleaseAttempted,
    CloseFailedAfterRelease,
    Closed,
}
struct CloseTail {
    _proof: crate::native_bootstrap::SourcePairProof,
    witness: StageWitness,
    _reference: UpgradeReference,
}
pub(crate) struct StageSourceCloseOperation {
    source: Option<StageSourceSession>,
    native: Option<CompletedCatalogSession>,
    tail: Option<CloseTail>,
    phase: SourceSessionClosePhase,
}
/// Actual fixed destinations and verifier locals. The two exact record buffers
/// are admitted separately from the genuine retained record's length. Existing
/// verifier uses vec![0; expected_len], read_exact_at, and metadata, not a growing
/// read_to_end, JSON parser, SHA encoder, query or path clone. This is logical
/// allocation admission, not an allocator/RSS or OS syscall scratch guarantee.
/// Both Vecs and the temporary named File/metadata drop before verify returns;
/// the second witness verification reuses this lexical scratch after release,
/// without refreshing or subtracting any cumulative reservation.
pub(crate) const fn source_close_storage() -> usize {
    std::mem::size_of::<StageSourceCloseOperation>()
        + std::mem::size_of::<Option<StageSourceCloseOperation>>()
        + std::mem::size_of::<Result<StageSourceCloseOperation, StageSourceSession>>()
        + std::mem::size_of::<StageSourceSession>()
        + std::mem::size_of::<CloseTail>()
        + std::mem::size_of::<CompletedCatalogSession>()
        + std::mem::size_of::<crate::SourceSessionCloseOperation>()
        + std::mem::size_of::<Option<crate::SourceSessionCloseOperation>>()
        + std::mem::size_of::<
            Result<crate::SourceSessionCloseOperation, crate::SourceRelocationSession>,
        >()
        + std::mem::size_of::<Result<(), crate::SourceSessionCloseOperation>>()
        + 2 * std::mem::size_of::<Vec<u8>>()
        + 6 * std::mem::size_of::<std::fs::Metadata>()
        + 2 * std::mem::size_of::<File>()
        + std::mem::size_of::<anyhow::Result<Option<File>>>()
        + std::mem::size_of::<anyhow::Result<Vec<u8>>>()
        + 3 * std::mem::size_of::<anyhow::Result<()>>()
}
impl StageSourceSession {
    pub(crate) fn begin_retained_close(mut self) -> Result<StageSourceCloseOperation, Self> {
        self.proof.invalidate_source_workspace94();
        if self.capture_admission.is_some() || !self.proof.has_retained_close_admission() {
            return Err(self);
        }
        Ok(StageSourceCloseOperation {
            source: Some(self),
            native: None,
            tail: None,
            phase: SourceSessionClosePhase::Ready,
        })
    }
}
impl StageSourceCloseOperation {
    pub(crate) fn phase(&self) -> SourceSessionClosePhase {
        self.phase
    }
    #[cfg(test)]
    pub(crate) fn custody_for_test(&self) -> (bool, bool, bool, i32, Option<usize>) {
        use std::os::fd::AsRawFd;
        let (witness, proof) = if let Some(source) = &self.source {
            (&source.witness, &source.proof)
        } else {
            let tail = self.tail.as_ref().unwrap();
            (&tail.witness, &tail._proof)
        };
        (
            self.source.is_some(),
            self.native.is_some(),
            self.tail.is_some(),
            witness.file.as_raw_fd(),
            proof.retained_bootstrap_slot_for_test().0,
        )
    }
    pub(crate) fn close(&mut self) -> anyhow::Result<()> {
        self.close_inner(0)
    }
    pub(crate) fn close_inner(&mut self, fault: u8) -> anyhow::Result<()> {
        ensure!(
            self.phase == SourceSessionClosePhase::Ready,
            "source close cannot retry"
        );
        self.phase = SourceSessionClosePhase::FailedBeforeRelease;
        checkpoint(fault, 1);
        self.source
            .as_ref()
            .expect("ready close source")
            .witness
            .verify(false)?;
        checkpoint(fault, 2);
        // All following handoffs are moves, with no callback, allocation or I/O
        // between taking the source and installing its complete destinations.
        let StageSourceSession {
            native,
            proof,
            witness,
            reference,
            capture_admission,
        } = self.source.take().expect("verified close source");
        let _ = capture_admission; // Checked before the infallible handoff.
        self.native = Some(native);
        self.tail = Some(CloseTail {
            _proof: proof,
            witness,
            _reference: reference,
        });
        self.phase = SourceSessionClosePhase::ReleaseAttempted;
        checkpoint(fault, 3);
        drop(self.native.take().expect("retained native release"));
        // This state is assigned only after the actual native destructor returns.
        self.phase = SourceSessionClosePhase::CloseFailedAfterRelease;
        checkpoint(fault, 4);
        self.tail
            .as_ref()
            .expect("retained close tail")
            .witness
            .verify(false)?;
        self.phase = SourceSessionClosePhase::Closed;
        Ok(())
    }
}
fn checkpoint(fault: u8, point: u8) {
    #[cfg(test)]
    if fault == point {
        panic!("synthetic retained source-close boundary {point}");
    }
    let _ = (fault, point);
}
