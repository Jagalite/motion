//! Finite actual-service staging only. No consuming target/conversion wrapper,
//! copied current grant, process-global recovery sink, native apply or install.
use super::*;
mod control97;
use crate::native_service::pair::restore_input::target::conversion::{ConversionRecord, RECORD};

const RECORD_LIMIT: usize = 16 * 1024;
const LEAF_BYTES: usize = 200 + 9 + 1;
const STAGE_LEAF_SHARE: u64 = 2 * 200 + 2 * LEAF_BYTES as u64;
// ConversionRecord owns the following bounded clones before serialization:
// reservation UUID36/pair64/reservation64/transition51; Motion/native original
// Artifact digests64 each; binding/inventory digests64 each; native identity
// catalog/incarnation/profile from the verified bounded pair; and two Evidence
// names<=200 plus Artifact digests64. Include simultaneous formatting and
// metadata construction growth rather than accounting only the struct layout.
const RECORD_METADATA_SHARE: u64 = 64 * 1024;
// Source export revalidation streams to a deadline-checking sink. It neither
// reopens native SQL nor creates another source/stage image owner.
const SOURCE_SCAN_SHARE: u64 = 128 * 1024;
// One replaceable, non-authoritative edge/root observation plus a stopped
// source-parent pair. Include overlap while obtaining the next pair. Original
// ancestry/source-owner FDs remain retained separately and are never replaced.
const OBSERVATION_FD_SHARE: u64 = 6 * std::mem::size_of::<File>() as u64;
pub(super) const fn storage_reservation() -> u64 {
    3 * publication_file::Preparation::storage_reservation()
        + 2 * std::mem::size_of::<ServiceStaging>() as u64
        + 2 * RECORD_LIMIT as u64
        + RECORD_METADATA_SHARE
        + SOURCE_SCAN_SHARE
        + OBSERVATION_FD_SHARE
        + STAGE_LEAF_SHARE
        + TASK_STORAGE
        + control97::storage_reservation()
}

struct StageSlot {
    preparation: publication_file::Preparation,
    outcome: Option<publication_file::Outcome>,
}
pub(super) struct ServiceStaging {
    slots: [StageSlot; 3],
    image_names: [String; 2],
    // Installed before serialization. The bounded encoded buffer is also kept
    // here, including on partial serialization/preparation/publication failure.
    record: Option<ConversionRecord>,
    encoded: Vec<u8>,
    observation: Option<File>,
    source_observation: Option<(File, File)>,
    control: Option<Box<control97::ServiceControl>>,
}
impl ServiceStaging {
    fn new(path: &Path, install_id: &str) -> anyhow::Result<Self> {
        ensure!(
            install_id.len() == 36,
            "unexpected reservation install identity"
        );
        let names = [
            format!("paired-conversion-{install_id}.motion.sqlite3"),
            format!("paired-conversion-{install_id}.native.sqlite3"),
        ];
        ensure!(
            names.iter().all(|name| name.capacity() <= 200),
            "stage names exceed preadmitted capacity"
        );
        Ok(Self {
            slots: [
                StageSlot {
                    preparation: publication_file::Preparation::new(path, &names[0])?,
                    outcome: None,
                },
                StageSlot {
                    preparation: publication_file::Preparation::new(path, &names[1])?,
                    outcome: None,
                },
                StageSlot {
                    preparation: publication_file::Preparation::new(path, RECORD)?,
                    outcome: None,
                },
            ],
            image_names: names,
            record: None,
            encoded: Vec::with_capacity(RECORD_LIMIT),
            observation: None,
            source_observation: None,
            control: None,
        })
    }
}

impl ServiceTarget {
    /// The synchronous service transition precedes construction of any borrower
    /// future or task. Failure after this claim is terminal, never a retry.
    pub(in crate::native_service) fn claim_staging(
        &mut self,
        workspace: &AuthorizationWorkspace,
        stopped: &StoppedService,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Instant> {
        alive(self.deadline, cancel)?;
        ensure!(
            self.phase == TargetPhase::Reserved && self.staging.is_none(),
            "staging requires original Reserved target"
        );
        workspace.verify_service(stopped)?;
        ensure!(
            workspace.reusable(),
            "previous source authorization is not retired"
        );
        ensure!(
            self.retirement_failure.is_none(),
            "reservation retirement failed"
        );
        let record = self
            .record
            .as_ref()
            .context("original reservation record missing")?;
        // Claim once after read-only preconditions but before any fallible
        // stage-owner allocation. Constructor error/unwind is terminal, with
        // the original service owners retained and no future/effect created.
        self.phase = TargetPhase::Staging;
        let stage = ServiceStaging::new(&self.path, &record.install_id)?;
        // All fixed stage paths/buffers/layout/task shares were admitted by the
        // original export. This installation does not observe a target path.
        self.staging = Some(stage);
        Ok(self.deadline)
    }

    pub(in crate::native_service) async fn run_staging(
        &mut self,
        pair: &mut PreparedPair,
        selection: &mut ServiceSelection,
        workspace: &mut AuthorizationWorkspace,
        shared: &Shared,
    ) -> anyhow::Result<()> {
        ensure!(
            self.phase == TargetPhase::Staging && self.staging.is_some(),
            "service must claim staging synchronously"
        );
        ensure!(
            selection.target_deadline(&shared.cancel)? == self.deadline,
            "staging cannot refresh selected deadline"
        );
        workspace.verify_service(&pair._stopped)?;
        let selected = selection.target_input(&shared.cancel)?;
        selected.target_binding(pair, &shared.cancel)?;
        let PreparedPair {
            _writer,
            native,
            motion,
            parent,
            parent_identity,
            manifest,
            _stopped,
        } = pair;
        #[cfg(test)]
        staging_boundary(shared, 1);
        for index in 0..3 {
            self.prepare_stage(index, selected, shared)?;
            // Long scans, filesystem observations, sidecar checks, serialization
            // and hashing all precede this publication's genuinely fresh current
            // authentication. No token is carried across stages or long scans.
            self.observe_staging(
                selected,
                motion,
                native,
                parent,
                *parent_identity,
                manifest,
                _stopped,
                shared,
            )?;
            #[cfg(test)]
            staging_boundary(shared, index as u8 + 2);
            let slot = &mut self.staging.as_mut().unwrap().slots[index];
            let outcome = slot.outcome.as_mut().expect("installed stage outcome");
            #[cfg(test)]
            if shared.test.staging_sync_failure.load(Ordering::Acquire) == index as u8 + 1 {
                outcome.inject_parent_sync_failure();
            }
            let link = outcome.prepare_link_until(&shared.cancel, self.deadline)?;
            // Claim the SAME admitted workspace before a source borrower future
            // exists. Prior publication retirement must have supplied real ACK.
            workspace.begin()?;
            let mut reader = _writer.authorization_reader().await?;
            // This observation is resource preflight before policy queries, not
            // a claim to measure their later allocation peak. The same named
            // authorization reserve continues to cover bounded query growth.
            let measured = reader
                .retained_allocations()
                .await?
                .checked_add(native.retained_allocations())
                .and_then(|n| n.checked_add(OVERHEAD))
                .context("staging current allocation overflow")?
                .checked_add(_stopped.retained_restore_storage()?)
                .context("staging service allocation overflow")?;
            ensure!(
                measured <= 256 * 1024 * 1024,
                "staging exceeds original retained budget"
            );
            let current =
                CurrentSourceAuthentication::prepare(reader, &self.caller, Permission::SystemAdmin)
                    .await
                    .map_err(auth_error)?;
            // Current::prepare freshly reauthorizes this original caller and
            // verifies its exact owner at completion. From here to the fixed
            // create-only link there is only a pure temporal/deadline fence.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                current.revalidate_temporal().map_err(auth_error)?;
                alive(self.deadline, &shared.cancel)?;
                link.publish()?;
                Ok::<(), anyhow::Error>(())
            }));
            if matches!(&result, Ok(Ok(()))) {
                ensure!(
                    outcome.phase() == PublicationPhase::Durable,
                    "stage publication unresolved; retained without retry or unlink"
                );
            }
            if index == 2 && matches!(&result, Ok(Ok(()))) {
                self.phase = TargetPhase::StagedUnretired;
                #[cfg(test)]
                staging_boundary(shared, 5);
            }
            // Even a durable link does not retire source authorization. Failed,
            // expired or cancelled close retains this exact workspace/writer.
            if let Err(error) = workspace
                .retire_current(current, self.deadline, &shared.cancel)
                .await
            {
                self.retirement_failure = Some(error);
            }
            match result {
                Err(panic) => std::panic::resume_unwind(panic),
                Ok(Err(error)) => return Err(error),
                Ok(Ok(())) => ensure!(
                    self.retirement_failure.is_none() && workspace.reusable(),
                    "staged source authorization retirement incomplete"
                ),
            }
        }
        self.observe_path()?;
        verify_target_lock(
            self.parent.as_ref().unwrap(),
            self.lock.held_lock().unwrap(),
        )?;
        _stopped.verify_lock()?;
        alive(self.deadline, &shared.cancel)?;
        self.phase = TargetPhase::Staged;
        Ok(())
    }

    fn prepare_stage(
        &mut self,
        index: usize,
        selected: &mut SelectedPairInput,
        shared: &Shared,
    ) -> anyhow::Result<()> {
        alive(self.deadline, &shared.cancel)?;
        let parent = self
            .parent
            .as_ref()
            .context("original target parent missing")?;
        let reservation = self
            .record
            .as_ref()
            .context("original reservation missing")?;
        let parent_id = (reservation.parent_device, reservation.parent_inode);
        ensure!(
            identity(parent)? == parent_id,
            "original reserved target parent changed"
        );
        let stage = self.staging.as_mut().expect("claimed service stage");
        if index == 2 {
            let manifest = selected.service_manifest();
            let reservation = self
                .record
                .as_ref()
                .context("original reservation missing")?;
            // Exact legacy leaf/schema semantics. No mutation of either image,
            // conversion to97/96, native session or installation is performed.
            stage.record = Some(ConversionRecord {
                contract: 1,
                install_id: reservation.install_id.clone(),
                pair_sha256: selected.target_archive().sha256().to_owned(),
                reservation_sha256: self
                    .reservation
                    .as_ref()
                    .unwrap()
                    .evidence()
                    .artifact
                    .sha256
                    .clone(),
                source_motion_schema_version: manifest.motion_schema_version,
                source_motion_inventory_sha256: manifest.motion_control_journals_sha256.clone(),
                source_motion_binding_sha256: manifest.motion_binding_sha256.clone(),
                source_motion_image: manifest.motion_database.clone(),
                motion_stage: stage.slots[0].outcome.as_ref().unwrap().evidence().clone(),
                source_native_identity: manifest.native_identity.clone(),
                source_native_inventory_sha256: manifest.native_control_journals_sha256.clone(),
                source_native_image: manifest.native_database.clone(),
                native_stage: stage.slots[1].outcome.as_ref().unwrap().evidence().clone(),
                intended_transition_id: format!("paired-upgrade-{}", reservation.install_id),
            });
            serde_json::to_writer(
                BoundedRecord(&mut stage.encoded),
                stage.record.as_ref().unwrap(),
            )?;
        }
        let slot = &mut stage.slots[index];
        ensure!(
            slot.outcome.is_none(),
            "stage preparation cannot be retried"
        );
        #[cfg(test)]
        if shared.test.staging_prepare_failure.load(Ordering::Acquire) == index as u8 + 1 {
            slot.preparation.inject(
                if index == 2 {
                    publication_file::PreparationPoint::BeforeSync
                } else {
                    publication_file::PreparationPoint::CopyChunk
                },
                publication_file::PreparationFault::Error,
            );
        }
        match index {
            0 => selected.prepare_service_motion(
                &mut slot.preparation,
                parent,
                parent_id,
                &shared.cancel,
                self.deadline,
            )?,
            1 => selected.prepare_service_native(
                &mut slot.preparation,
                parent,
                parent_id,
                &shared.cancel,
                self.deadline,
            )?,
            2 => slot.preparation.prepare_record(
                parent,
                parent_id,
                &stage.encoded,
                &shared.cancel,
                self.deadline,
            )?,
            _ => unreachable!(),
        }
        slot.outcome = Some(
            slot.preparation
                .finish()
                .context("stage preparation is not Ready; exact partial owner retained")?,
        );
        #[cfg(test)]
        shared.test.staging_fds[index].store(
            slot.outcome.as_ref().unwrap().original_record_fd(),
            Ordering::Release,
        );
        Ok(())
    }

    fn observe_staging(
        &mut self,
        selected: &mut SelectedPairInput,
        motion: &mut File,
        native: &mut catabolic_store::catalog_backup::VerifiedExport,
        current_parent: &File,
        current_parent_identity: (u64, u64),
        current_manifest: &PairManifest,
        stopped: &StoppedService,
        shared: &Shared,
    ) -> anyhow::Result<()> {
        alive(self.deadline, &shared.cancel)?;
        stopped.verify_lock()?;
        ensure!(
            identity(current_parent)? == current_parent_identity
                && native.parent_identity()? == current_parent_identity,
            "original current pair parent changed"
        );
        verify_file(motion, &current_manifest.motion_database, &shared.cancel)?;
        native.copy_verified_to(
            &mut DeadlineSink {
                deadline: self.deadline,
                cancel: &shared.cancel,
            },
            &shared.cancel,
        )?;
        selected.target_revalidate(&shared.cancel)?;
        ensure!(
            selected.service_manifest().native_identity.schema_version == 40
                && selected.service_manifest().motion_schema_version == 93,
            "staging requires exact original93/native40 pair"
        );
        // Preserve the original reservation's complete path/containment facts,
        // not just a stale root inventory. Fresh observations use fixed scratch
        // slots and never replace original ancestry or stopped-parent custody.
        self.observe_staging_ancestry(selected, stopped, &shared.cancel)?;
        self.observe_path()?;
        let parent = self.parent.as_ref().unwrap();
        verify_target_lock(
            parent,
            self.lock
                .held_lock()
                .context("retained target lock missing")?,
        )?;
        require_uninstalled_leaves(parent)?;
        let reservation = self
            .reservation
            .as_mut()
            .context("retained reservation missing")?;
        ensure!(
            reservation.phase() == PublicationPhase::Durable,
            "staging requires durable original reservation"
        );
        reservation.read_verified(|_| Ok(()))?;
        // Freshly observe BOTH image leaf/sidecar sets, including the native
        // image not prepared yet, using one preadmitted fixed stack buffer.
        // Earlier copy-time checks are not the final publication preflight.
        let stage = self.staging.as_mut().unwrap();
        let mut leaf = [0u8; LEAF_BYTES];
        for name in &stage.image_names {
            for suffix in ["-wal", "-shm", "-journal"] {
                require_stage_leaf_absent(parent, name, suffix, &mut leaf)?;
            }
        }
        // Exact durable Outcomes are the only allowed existing stage names.
        // Every current/future unpublished leaf must still be absent; no name
        // or file is adopted, overwritten or unlinked on a refusal.
        for (index, slot) in stage.slots.iter_mut().enumerate() {
            if let Some(outcome) = &mut slot.outcome {
                if outcome.phase() == PublicationPhase::Durable {
                    if index == 0 && stage.control.as_ref().is_some_and(|c| c.has_operation()) {
                        // Backend preparation has claimed mutation custody. The
                        // immutable93 artifact verifier is permanently revoked.
                        outcome.verify_mutable_stage_identity()?;
                    } else {
                        outcome.read_verified(|_| Ok(()))?;
                    }
                } else {
                    require_stage_leaf_absent(parent, &outcome.evidence().name, "", &mut leaf)?;
                }
            } else {
                let name = if index < 2 {
                    stage.image_names[index].as_str()
                } else {
                    RECORD
                };
                require_stage_leaf_absent(parent, name, "", &mut leaf)?;
            }
        }
        stopped.verify_lock()?;
        alive(self.deadline, &shared.cancel)
    }

    fn observe_staging_ancestry(
        &mut self,
        selected: &SelectedPairInput,
        stopped: &StoppedService,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        let stage = self
            .staging
            .as_mut()
            .expect("claimed staging observation slots");
        spend_observation(&mut self.ancestry_remaining, self.deadline, cancel)?;
        // The existing helper may drop a temporary first observation if its
        // second open fails before returning. It never owned the original
        // stopped-parent FDs, which remain here unchanged on that refusal.
        stage.source_observation = Some(stopped.source_parents()?);
        let current = stage.source_observation.as_ref().unwrap();
        let original = self
            .source_parents
            .as_ref()
            .context("original stopped source parents missing")?;
        ensure!(
            identity(&current.0)? == identity(&original.0)?
                && identity(&current.1)? == identity(&original.1)?,
            "stopped source parent path changed after reservation"
        );
        for root in selected.target_roots() {
            spend_observation(&mut self.ancestry_remaining, self.deadline, cancel)?;
            let expected = root
                .identity()
                .context("original binding-root identity missing")?;
            stage.observation = Some(crate::native_service::pair::open_parent(Path::new(
                root.path(),
            ))?);
            ensure!(
                identity(stage.observation.as_ref().unwrap())? == expected,
                "selected/current binding-root path changed after reservation"
            );
        }
        verify_retained_edges(
            &self.target_ancestors,
            &mut stage.observation,
            &mut self.ancestry_remaining,
            self.deadline,
            cancel,
        )?;
        verify_retained_edges(
            &self.source_ancestors,
            &mut stage.observation,
            &mut self.ancestry_remaining,
            self.deadline,
            cancel,
        )?;
        Ok(())
    }

    #[cfg(test)]
    pub(in crate::native_service) fn staging_ancestry_evidence(
        &self,
    ) -> (usize, usize, usize, usize, usize) {
        (
            self.ancestry_remaining,
            self.target_ancestors.len(),
            self.target_ancestors.capacity(),
            self.source_ancestors.len(),
            self.source_ancestors.capacity(),
        )
    }
    #[cfg(test)]
    pub(in crate::native_service) fn staging_preparation(
        &self,
        index: usize,
    ) -> Option<&publication_file::Preparation> {
        self.staging
            .as_ref()
            .map(|stage| &stage.slots[index].preparation)
    }
    #[cfg(test)]
    pub(in crate::native_service) fn staging_record(&self) -> Option<&ConversionRecord> {
        self.staging
            .as_ref()
            .and_then(|stage| stage.record.as_ref())
    }
    #[cfg(test)]
    pub(in crate::native_service) fn staging_evidence(
        &self,
    ) -> [Option<&publication_file::Outcome>; 3] {
        std::array::from_fn(|i| {
            self.staging
                .as_ref()
                .and_then(|stage| stage.slots[i].outcome.as_ref())
        })
    }
}

struct BoundedRecord<'a>(&'a mut Vec<u8>);
impl Write for BoundedRecord<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self
            .0
            .len()
            .checked_add(data.len())
            .is_none_or(|n| n > RECORD_LIMIT || n > self.0.capacity())
        {
            return Err(std::io::Error::other(
                "conversion record exceeds preadmitted fixed buffer",
            ));
        }
        self.0.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
struct DeadlineSink<'a> {
    deadline: Instant,
    cancel: &'a AtomicBool,
}
impl Write for DeadlineSink<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.cancel.load(Ordering::Acquire) || Instant::now() >= self.deadline {
            return Err(std::io::Error::other(
                "original staging deadline/cancellation exhausted",
            ));
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// Reservation and already durable conversion stages are expected here. Never
// call require_fresh_leaves, which correctly rejects an existing reservation.
fn require_uninstalled_leaves(parent: &File) -> anyhow::Result<()> {
    for name in [
        "playscale.sqlite3",
        "catabolic.sqlite3",
        "playscale.sqlite3-wal",
        "playscale.sqlite3-shm",
        "playscale.sqlite3-journal",
        "catabolic.sqlite3-wal",
        "catabolic.sqlite3-shm",
        "catabolic.sqlite3-journal",
        "paired-install.intent.json",
        "paired-install.motion.json",
        "paired-install.native.json",
        "paired-install.complete.json",
    ] {
        let name = CString::new(name)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let rc = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        ensure!(
            rc < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT),
            "service stage target contains installation effects or is unobservable"
        );
    }
    Ok(())
}

fn spend_observation(
    remaining: &mut usize,
    deadline: Instant,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    alive(deadline, cancel)?;
    ensure!(
        *remaining > 0,
        "original ancestry observation budget exhausted"
    );
    *remaining -= 1;
    Ok(())
}

// chain() retained each complete chain through a duplicated root self-edge.
// Compare every original edge, then skip that duplicate to the next chain.
// This preserves the original disjointness proof without growing either Vec or
// accepting a new topology. A changed edge is refusal, never path adoption.
fn verify_retained_edges(
    chain: &[File],
    observation: &mut Option<File>,
    remaining: &mut usize,
    deadline: Instant,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    let mut index = 0;
    while index < chain.len() {
        spend_observation(remaining, deadline, cancel)?;
        ensure!(
            index + 1 < chain.len(),
            "retained ancestry chain is incomplete"
        );
        let expected = identity(&chain[index + 1])?;
        let fd = unsafe {
            libc::openat(
                chain[index].as_raw_fd(),
                c"..".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        ensure!(
            fd >= 0,
            "retained ancestry edge unavailable: {}",
            std::io::Error::last_os_error()
        );
        *observation = Some(unsafe { File::from_raw_fd(fd) });
        ensure!(
            identity(observation.as_ref().unwrap())? == expected,
            "reserved target/source ancestry changed before staging"
        );
        index += if identity(&chain[index])? == expected {
            2
        } else {
            1
        };
    }
    Ok(())
}

fn require_stage_leaf_absent(
    parent: &File,
    stage_name: &str,
    suffix: &str,
    buffer: &mut [u8; LEAF_BYTES],
) -> anyhow::Result<()> {
    ensure!(
        stage_name.len() <= 200 && suffix.len() <= 9,
        "stage leaf exceeds preadmitted bound"
    );
    let end = stage_name.len() + suffix.len();
    ensure!(end < buffer.len(), "stage leaf scratch exhausted");
    buffer[..stage_name.len()].copy_from_slice(stage_name.as_bytes());
    buffer[stage_name.len()..end].copy_from_slice(suffix.as_bytes());
    buffer[end] = 0;
    let name = std::ffi::CStr::from_bytes_with_nul(&buffer[..=end])?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let rc = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    ensure!(
        rc < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT),
        "conversion stage or sidecar appeared before fresh authorization"
    );
    Ok(())
}
