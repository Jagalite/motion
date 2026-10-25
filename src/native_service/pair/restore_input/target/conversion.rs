//! Reservation-bound durable Motion and native conversion stages. Not a final deployment DB.
//! Both stages are create-only; exact96 may later mutate only their recorded
//! inodes. The retained target server lock and durable reservation exclude other
//! cooperating service/install owners before they open a database. Parent and
//! held/named file checks detect stale or replaced paths. This is not a claim of
//! arbitrary same-UID/privileged ABA resistance during SQLite pathname opens.
//! No pathname SQLite acquisition is performed by this finite staging owner.
mod source_actor;
mod source_conversion;
mod source_image;
use super::*;
use crate::native_service::publication_file::{Outcome, Sealed};
use playscale_core::paired_backup::{Artifact, publication::Evidence};
use serde::{Deserialize, Serialize};
use source_conversion::{SourceActorCustody, SourceConversionActor};
pub(in crate::native_service) use source_image::{RestoreProof, RestoreSupervisor};
const RECORD: &str = "paired-install.conversion.json";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversionRecord {
    contract: u32,
    install_id: String,
    pair_sha256: String,
    reservation_sha256: String,
    source_motion_schema_version: u32,
    source_motion_inventory_sha256: String,
    source_motion_binding_sha256: String,
    source_motion_image: Artifact,
    motion_stage: Evidence,
    source_native_identity: NativeIdentity,
    source_native_inventory_sha256: String,
    source_native_image: Artifact,
    native_stage: Evidence,
    intended_transition_id: String,
}
struct Retained {
    target: FreshRestoreTarget,
    stage: Option<Outcome>,
    motion_stage: Option<Outcome>,
    record: Option<Outcome>,
}
/// No path-only constructor/retry. A future recovery owner must independently
/// authenticate the durable record and same held/named stage identity.
pub(in crate::native_service::pair) struct NativeConversionStage {
    held: Option<Retained>,
}
static RECOVERY: std::sync::OnceLock<std::sync::Mutex<Vec<Retained>>> = std::sync::OnceLock::new();
impl Drop for NativeConversionStage {
    fn drop(&mut self) {
        let Some(held) = self.held.take() else { return };
        // Even durable staging is unfinished installation. Preserve custody on
        // abandoned/interrupted conversion; never adopt or unlink a name later.
        if [&held.stage, &held.motion_stage].into_iter().any(|stage| {
            stage.as_ref().is_some_and(|o| {
                matches!(
                    o.phase(),
                    Phase::LinkAttempted
                        | Phase::LinkedUnconfirmed
                        | Phase::Durable
                        | Phase::OutcomeUnknown
                )
            })
        }) {
            tracing::error!(motion_stage=?held.motion_stage.as_ref().map(|o|o.evidence()), stage=?held.stage.as_ref().map(|o|o.evidence()), record=?held.record.as_ref().map(|o|o.evidence()), "native conversion stage retained for explicit recovery");
            RECOVERY
                .get_or_init(Default::default)
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(held);
        }
    }
}
impl FreshRestoreTarget {
    pub(in crate::native_service::pair) fn stage_native_conversion(
        self,
        cancel: &AtomicBool,
    ) -> anyhow::Result<NativeConversionStage> {
        let mut owner = NativeConversionStage {
            held: Some(Retained {
                target: self,
                stage: None,
                motion_stage: None,
                record: None,
            }),
        };
        let retained = owner.held.as_mut().context("conversion custody missing")?;
        let target = retained
            .target
            .held
            .as_mut()
            .context("target custody missing")?;
        cancelled(cancel)?;
        target.input.verify_archive(cancel)?;
        verify_target_lock(&target.parent, &target.lock)?;
        let parent_id = identity(&target.parent)?;
        ensure!(
            identity(&super::super::super::open_parent(&target.path)?)? == parent_id,
            "conversion parent replaced"
        );
        ensure!(
            target.reservation.phase() == Phase::Durable,
            "conversion requires durable target reservation"
        );
        ensure!(
            target.input.manifest.native_identity.schema_version == 40,
            "conversion stage requires genuine original40 pair"
        );
        let native_artifact = target.input.manifest.native_database.clone();
        let motion_artifact = target.input.manifest.motion_database.clone();
        // Both immutable extracted images become durable before any 96 effect.
        // In particular, Motion's saved plan/envelope/ACK must commit to this
        // real stage, never solely to an anonymous in-memory connection.
        for (kind, source, artifact, slot) in [
            (
                "motion",
                &mut target.input.motion,
                &motion_artifact,
                &mut retained.motion_stage,
            ),
            (
                "native",
                &mut target.input.native,
                &native_artifact,
                &mut retained.stage,
            ),
        ] {
            let name = format!("paired-conversion-{}.{kind}.sqlite3", target.install_id);
            let sealed = stage_image(
                &target.path,
                &target.parent,
                parent_id,
                &name,
                source,
                artifact,
                cancel,
            )?;
            *slot = Some(sealed.publish(cancel, || Ok(())));
            ensure!(
                slot.as_ref().is_some_and(|o| o.phase() == Phase::Durable),
                "conversion image publication uncertain; explicit recovery required"
            );
        }
        let stage = retained
            .stage
            .as_ref()
            .context("native conversion stage missing")?;
        let motion_stage = retained
            .motion_stage
            .as_ref()
            .context("Motion conversion stage missing")?;
        let record = ConversionRecord {
            contract: 1,
            install_id: target.install_id.clone(),
            pair_sha256: target.input.pair_sha256().to_owned(),
            reservation_sha256: target.reservation.evidence().artifact.sha256.clone(),
            source_motion_schema_version: target.input.manifest.motion_schema_version,
            source_motion_inventory_sha256: target
                .input
                .manifest
                .motion_control_journals_sha256
                .clone(),
            source_motion_binding_sha256: target.input.manifest.motion_binding_sha256.clone(),
            source_motion_image: motion_artifact,
            motion_stage: motion_stage.evidence().clone(),
            source_native_identity: target.input.manifest.native_identity.clone(),
            source_native_inventory_sha256: target
                .input
                .manifest
                .native_control_journals_sha256
                .clone(),
            source_native_image: native_artifact,
            native_stage: stage.evidence().clone(),
            intended_transition_id: format!("paired-upgrade-{}", target.install_id),
        };
        let bytes = serde_json::to_vec(&record)?;
        ensure!(bytes.len() <= 16 * 1024, "conversion record exceeds bound");
        let sealed = Sealed::create_checked(
            &target.path,
            RECORD,
            cancel,
            Some(parent_id),
            |output| {
                output.write_all(&bytes)?;
                Ok(Artifact {
                    bytes: bytes.len() as u64,
                    sha256: catabolic_core::sha256(&bytes),
                })
            },
            |_| Ok(()),
        )?;
        retained.record = Some(sealed.publish(cancel, || Ok(())));
        ensure!(
            retained
                .record
                .as_ref()
                .is_some_and(|o| o.phase() == Phase::Durable),
            "conversion record durability uncertain; explicit recovery required"
        );
        verify_target_lock(&target.parent, &target.lock)?;
        cancelled(cancel)?;
        Ok(owner)
    }
}

// Filesystem effects only: publication phase decisions remain in the existing
// core publication owner. A partial pair is retained, not treated as atomic.
fn stage_image(
    path: &std::path::Path,
    parent: &File,
    parent_id: (u64, u64),
    name: &str,
    source: &mut File,
    artifact: &Artifact,
    cancel: &AtomicBool,
) -> anyhow::Result<Sealed> {
    verify_file(source, artifact, cancel)?;
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let leaf = CString::new(format!("{name}{suffix}"))?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let rc = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                leaf.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        ensure!(
            rc < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT),
            "conversion stage or SQLite sidecar already exists"
        );
    }
    Sealed::create_checked(
        path,
        name,
        cancel,
        Some(parent_id),
        |output| {
            source.seek(SeekFrom::Start(0))?;
            let mut buffer = [0u8; 64 * 1024];
            let mut total = 0u64;
            loop {
                cancelled(cancel)?;
                let n = source.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                total = total
                    .checked_add(n as u64)
                    .context("conversion length overflow")?;
                ensure!(total <= artifact.bytes, "conversion source grew");
                output.write_all(&buffer[..n])?;
            }
            ensure!(total == artifact.bytes, "conversion source truncated");
            Ok(artifact.clone())
        },
        |file| verify_file(file, artifact, cancel),
    )
}

#[cfg(test)]
impl NativeConversionStage {
    pub(in crate::native_service::pair) fn exercise_record_and_replacement(mut self) {
        let retained = self.held.as_mut().unwrap();
        let target = retained.target.held.as_ref().unwrap();
        let stage = retained.stage.as_mut().unwrap();
        let motion_stage = retained.motion_stage.as_mut().unwrap();
        assert_eq!(motion_stage.phase(), Phase::Durable);
        let record = retained.record.as_mut().unwrap();
        assert_eq!(stage.phase(), Phase::Durable);
        assert_eq!(record.phase(), Phase::Durable);
        let decoded: ConversionRecord = record
            .read_verified(|file| {
                let mut raw = Vec::new();
                file.read_to_end(&mut raw)?;
                let value: ConversionRecord = serde_json::from_slice(&raw)?;
                ensure!(
                    serde_json::to_vec(&value)? == raw,
                    "noncanonical conversion record"
                );
                Ok(value)
            })
            .unwrap();
        assert_eq!(decoded.native_stage, *stage.evidence());
        assert_eq!(decoded.motion_stage, *motion_stage.evidence());
        assert_eq!(
            decoded.source_motion_image,
            target.input.manifest.motion_database
        );
        assert_eq!(
            decoded.source_motion_binding_sha256,
            target.input.manifest.motion_binding_sha256
        );
        motion_stage.read_verified(|_| Ok(())).unwrap();
        assert_eq!(decoded.install_id, target.install_id);
        assert_eq!(
            decoded.source_native_image,
            target.input.manifest.native_database
        );
        assert!(!target.path.join("catabolic.sqlite3").exists());
        assert!(!target.path.join("playscale.sqlite3").exists());
        assert!(
            crate::native_service::require_complete_pair_install(
                &target.path,
                &target.path.join("catabolic.sqlite3"),
                &target.lock
            )
            .is_err()
        );
        let path = target.path.join(&stage.evidence().name);
        let held_path = target.path.join("fixture-original-stage");
        std::fs::rename(&path, &held_path).unwrap();
        std::fs::write(&path, b"unrelated replacement").unwrap();
        assert!(stage.read_verified(|_| Ok(())).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"unrelated replacement");
        assert!(target.path.join(RECORD).is_file());
        // Explicit fixture cleanup releases the retained owners, never deletes
        // the replacement/record as production recovery or retries publication.
        let retained = self.held.take().unwrap();
        let target_path = retained.target.held.as_ref().unwrap().path.clone();
        drop(retained);
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(target_path.join("server.lock"))
            .unwrap();
        fs2::FileExt::try_lock_exclusive(&lock).unwrap();
        assert!(
            crate::native_service::require_complete_pair_install(
                &target_path,
                &target_path.join("catabolic.sqlite3"),
                &lock
            )
            .is_err()
        );
        assert_eq!(std::fs::read(path).unwrap(), b"unrelated replacement");
    }
}

#[cfg(test)]
mod stage_tests {
    use super::*;

    // These exercise the actual descriptor publisher, not SQLite conversion or
    // pair authority. The genuine pair fixture above supplies that separate input.
    fn source() -> (File, Artifact) {
        let bytes = b"bounded descriptor fixture";
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        (
            file,
            Artifact {
                bytes: bytes.len() as u64,
                sha256: catabolic_core::sha256(bytes),
            },
        )
    }

    #[test]
    fn conversion_stage_refuses_all_existing_sqlite_sidecars_without_changes() {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let dir = tempfile::tempdir().unwrap();
            let parent = super::super::super::super::open_parent(dir.path()).unwrap();
            let collision = dir.path().join(format!("stage.sqlite3{suffix}"));
            std::fs::write(&collision, b"unrelated").unwrap();
            let (mut source, artifact) = source();
            assert!(
                stage_image(
                    dir.path(),
                    &parent,
                    identity(&parent).unwrap(),
                    "stage.sqlite3",
                    &mut source,
                    &artifact,
                    &AtomicBool::new(false)
                )
                .is_err()
            );
            assert_eq!(std::fs::read(collision).unwrap(), b"unrelated");
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn conversion_cancel_before_link_has_no_visible_stage() {
        let dir = tempfile::tempdir().unwrap();
        let parent = super::super::super::super::open_parent(dir.path()).unwrap();
        let (mut source, artifact) = source();
        let flag = AtomicBool::new(false);
        let sealed = stage_image(
            dir.path(),
            &parent,
            identity(&parent).unwrap(),
            "stage.sqlite3",
            &mut source,
            &artifact,
            &flag,
        )
        .unwrap();
        flag.store(true, std::sync::atomic::Ordering::Release);
        let outcome = sealed.publish(&flag, || Ok(()));
        assert_ne!(outcome.phase(), Phase::Durable);
        assert!(!dir.path().join("stage.sqlite3").exists());
    }

    #[test]
    fn conversion_second_stage_collision_preserves_first_exact_stage() {
        let dir = tempfile::tempdir().unwrap();
        let parent = super::super::super::super::open_parent(dir.path()).unwrap();
        let (mut source, artifact) = source();
        let flag = AtomicBool::new(false);
        let first = stage_image(
            dir.path(),
            &parent,
            identity(&parent).unwrap(),
            "motion.sqlite3",
            &mut source,
            &artifact,
            &flag,
        )
        .unwrap();
        let mut first = first.publish(&flag, || Ok(()));
        assert_eq!(first.phase(), Phase::Durable);
        std::fs::write(
            dir.path().join("native.sqlite3-journal"),
            b"unrelated journal",
        )
        .unwrap();
        assert!(
            stage_image(
                dir.path(),
                &parent,
                identity(&parent).unwrap(),
                "native.sqlite3",
                &mut source,
                &artifact,
                &flag
            )
            .is_err()
        );
        first.read_verified(|_| Ok(())).unwrap();
        assert!(!dir.path().join("native.sqlite3").exists());
        assert_eq!(
            std::fs::read(dir.path().join("native.sqlite3-journal")).unwrap(),
            b"unrelated journal"
        );
    }
}
