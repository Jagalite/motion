//! Fixed reservation publication only. The backend seal authenticates these
//! bytes; this filesystem adapter never constructs restore or rotation rights.
//! Publication decisions stay in the existing core Publication state machine.
use crate::native_service::publication_file::{Outcome, RestorePublicationKind, Sealed};
use anyhow::{Context, ensure};
use playscale_core::paired_backup::{
    Artifact,
    publication::{Evidence, Phase, Publication},
};
use sha2::Sha256;
use std::{
    ffi::CString,
    fs::{File, Metadata},
    mem::size_of,
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

const NAME: &str = "paired-install.restore-reservation.json";
const MAX_RECORD: usize = 16 * 1024;

fn add(parts: &[u64]) -> anyhow::Result<u64> {
    parts.iter().try_fold(0u64, |total, next| {
        total
            .checked_add(*next)
            .context("restore publication allocation overflow")
    })
}

fn bytes(n: usize) -> anyhow::Result<u64> {
    u64::try_from(n).context("restore publication allocation width overflow")
}

/// Additional host storage admitted BEFORE seal preparation, not borrowed from
/// the authorization convention. Borrow the caller's already-retained absolute
/// PathBuf without normalizing or cloning it before admission.
/// Sequential verification calls share one64KiB transient-buffer charge; this
/// is logical live-storage accounting, not a stack layout or process-RSS claim.
///
/// On success the record is streamed from the seal without another byte Vec.
/// The existing publisher's unbounded error-chain formatting remains a separate
/// failure-path limitation: no guessed diagnostic cap is charged or claimed.
pub(super) fn allocation_reservation(path: &PathBuf) -> anyhow::Result<u64> {
    ensure!(
        path.is_absolute(),
        "publication accounting needs absolute path"
    );
    let path_len = bytes(path.as_os_str().as_bytes().len())?;
    let component = path
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name.as_bytes().len()),
            _ => None,
        })
        .max()
        .unwrap_or(0)
        .max(b"proc".len());
    let name = bytes(NAME.len())?;
    let hash = bytes(size_of::<[u8; 32]>() * 2)?;
    // The union includes Sealed/Outcome and core Publication/Evidence layouts.
    // Heap capacities are additional: caller path, cloned Sealed path, fixed
    // Evidence name and sha256.to_owned(). Returned FDs remain host-retained on
    // refusal. No core reducer clone occurs: Publication::apply changes Phase
    // in place, and prepared() moves Evidence into its retained owner.
    let retained = add(&[
        bytes(size_of::<ReservationPublication>())?,
        bytes(size_of::<PathBuf>())?,
        bytes(path.capacity())?,
        path_len,
        name,
        hash,
        bytes(size_of::<Option<(File, File)>>())?,
    ])?;
    // Account separate handoff/construction layouts without assuming moves
    // erase old stack storage. The initial Artifact hash becomes the retained
    // Evidence hash; only the verifier's hexadecimal result is another String.
    let owners = add(&[
        bytes(size_of::<Sealed>())?,
        bytes(size_of::<Outcome>())?,
        bytes(size_of::<Publication>())?,
        bytes(size_of::<Evidence>())?,
        bytes(size_of::<Artifact>())?,
    ])?;
    let verification = add(&[
        bytes(size_of::<[u8; 64 * 1024]>())?,
        bytes(size_of::<Sha256>())?,
        bytes(size_of::<[u8; 32]>())?,
        hash,
        bytes(size_of::<Metadata>() * 4)?,
        bytes(size_of::<libc::statfs>())?,
        bytes(size_of::<libc::stat>())?,
    ])?;
    // open_parent needs one absolute-path scratch and one component CString.
    // Include /proc even for a shorter target. Twofold byte terms conservatively
    // cover Vec/OsString growth while appending a component or NUL; no pathname
    // maximum or second independent admission cap is introduced.
    let absolute_scratch = path_len.max(bytes(b"/proc".len())?);
    let component_cstr = add(&[bytes(component)?, 1])?;
    let leaf_cstr = add(&[name, 1])?;
    let path_scratch = add(&[
        absolute_scratch,
        absolute_scratch,
        component_cstr,
        component_cstr,
        // publish's fixed target CString overlaps verify_linked's copy.
        leaf_cstr,
        leaf_cstr,
        leaf_cstr,
        leaf_cstr,
        bytes(size_of::<PathBuf>())?,
        bytes(size_of::<CString>() * 3)?,
    ])?;
    // PID and source FD are formatted only after opening /proc. Their actual
    // values are unavailable at admission, so use their integer types' decimal
    // widths, not an arbitrary identifier limit. Include both String and CStr
    // construction/growth plus proc/process/fd-directory/observed descriptors.
    let pid_digits = u64::from(u32::MAX.ilog10() + 1);
    let fd_digits = u64::from(libc::c_int::MAX.ilog10() + 1);
    let proc_formatting = add(&[
        pid_digits,
        pid_digits,
        2,
        fd_digits,
        fd_digits,
        2,
        bytes(size_of::<String>() * 2)?,
        bytes(size_of::<CString>() * 2)?,
        bytes(size_of::<File>() * 4)?,
    ])?;
    // Retain this complete high-water amount after staging; removing transient
    // publication work must not lower the backend's already-forwarded budget.
    add(&[
        retained,
        owners,
        verification,
        path_scratch,
        proc_formatting,
    ])
}

pub(super) fn alive(cancel: &AtomicBool, deadline: Instant) -> anyhow::Result<()> {
    ensure!(
        !cancel.load(Ordering::Acquire) && Instant::now() < deadline,
        "restore reservation cancelled or original seal deadline exhausted"
    );
    Ok(())
}

enum State {
    Prepared(Sealed),
    Published(Outcome),
}

/// Retained inside the same source/target custody, including on error. No retry,
/// overwrite, unlink, existing-record adoption or backend authority is exposed.
pub(super) struct ReservationPublication {
    state: Option<State>,
    deadline: Instant,
}

impl ReservationPublication {
    pub(super) fn prepare(
        path: &Path,
        parent_identity: (u64, u64),
        canonical: &[u8],
        sha256: &str,
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> anyhow::Result<Self> {
        alive(cancel, deadline)?;
        ensure!(
            !canonical.is_empty() && canonical.len() <= MAX_RECORD,
            "restore reservation record exceeds fixed envelope"
        );
        // create_checked writes/verifies/fsyncs only an anonymous O_TMPFILE.
        // On preparation failure there is no named partial output to lose.
        // Once it returns, retain Sealed before the caller may await again.
        let sealed = Sealed::create_checked(
            path,
            NAME,
            cancel,
            Some(parent_identity),
            |output| {
                output.write_all(canonical)?;
                Ok(Artifact {
                    bytes: canonical.len() as u64,
                    sha256: sha256.to_owned(),
                })
            },
            |_| Ok(()),
        )?;
        Ok(Self {
            state: Some(State::Prepared(sealed)),
            deadline,
        })
    }

    pub(super) fn publish(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        self.publish_after_link(cancel, || Ok(()))
    }

    fn publish_after_link(
        &mut self,
        cancel: &AtomicBool,
        after_link: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        alive(cancel, self.deadline)?;
        ensure!(
            matches!(&self.state, Some(State::Prepared(_))),
            "restore reservation publication is one-shot"
        );
        let Some(State::Prepared(sealed)) = self.state.take() else {
            unreachable!()
        };
        // publish catches an unwind while owning the actual inode and tracks
        // uncertain link outcomes. There is no await before retaining Outcome.
        // After link, do not let cancellation skip required directory fsync.
        self.state = Some(State::Published(sealed.publish_until(
            cancel,
            self.deadline,
            after_link,
        )));
        let Some(State::Published(outcome)) = &self.state else {
            unreachable!()
        };
        ensure!(
            outcome.phase() == Phase::Durable && outcome.error().is_none(),
            "restore reservation publication unresolved; exact Outcome retained: {}",
            outcome.error().unwrap_or("not durable")
        );
        // A cancelled/expired caller still retains the now-durable Outcome.
        // Never restart the original seal deadline or infer absent output.
        alive(cancel, self.deadline)
    }

    #[cfg(test)]
    pub(super) fn original_publication(&self) -> (&Evidence, Phase, std::os::fd::RawFd) {
        let Some(State::Published(outcome)) = &self.state else {
            panic!("actual published Outcome missing")
        };
        (
            outcome.evidence(),
            outcome.phase(),
            outcome.original_record_fd(),
        )
    }

    pub(super) fn descriptors(&mut self) -> anyhow::Result<(File, File)> {
        let State::Published(outcome) = self
            .state
            .as_mut()
            .context("restore reservation publication state missing")?
        else {
            anyhow::bail!("restore reservation is not published")
        };
        outcome.restore_descriptors(RestorePublicationKind::Reservation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, os::unix::fs::MetadataExt, time::Duration};

    // These exercise actual host descriptor/publication boundaries, not a fake
    // seal authority or a claim of end-to-end source/target authorization.
    const RECORD: &[u8] = br#"{"contract_version":1,"operation_id":"publisher-fixture"}"#;

    fn prepare(dir: &Path, deadline: Instant) -> ReservationPublication {
        let parent = File::open(dir).unwrap();
        let metadata = parent.metadata().unwrap();
        ReservationPublication::prepare(
            dir,
            (metadata.dev(), metadata.ino()),
            RECORD,
            &catabolic_core::sha256(RECORD),
            &AtomicBool::new(false),
            deadline,
        )
        .unwrap()
    }

    fn outcome(publication: &ReservationPublication) -> &Outcome {
        let Some(State::Published(outcome)) = &publication.state else {
            panic!("original publication Outcome missing")
        };
        outcome
    }

    #[test]
    fn named_publication_storage_tracks_actual_paths_without_a_second_cap() {
        let short = PathBuf::from("/short");
        let long = PathBuf::from(format!("/{}", "x".repeat(8192)));
        let short_charge = allocation_reservation(&short).unwrap();
        let long_charge = allocation_reservation(&long).unwrap();
        let delta = bytes(long.as_os_str().len() - short.as_os_str().len()).unwrap();
        let capacity_delta = bytes(long.capacity() - short.capacity()).unwrap();
        // One retained path clone, two absolute-scratch bytes, two component
        // CString bytes, and the actual caller PathBuf capacity. Fixed state,
        // leaf/hash strings and the single verifier buffer do not multiply.
        assert_eq!(long_charge - short_charge, delta * 5 + capacity_delta);
        assert!(short_charge > 64 * 1024);
        assert!(add(&[u64::MAX, 1]).is_err());
        assert!(allocation_reservation(&PathBuf::from("relative")).is_err());
    }

    #[test]
    fn fixed_create_only_record_retains_outcome_and_read_only_descriptors() {
        let dir = tempfile::tempdir().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut publication = prepare(dir.path(), deadline);
        assert!(!dir.path().join(NAME).exists());
        publication.publish(&AtomicBool::new(false)).unwrap();
        let original = outcome(&publication).evidence().clone();
        let (parent, mut record) = publication.descriptors().unwrap();
        assert_eq!(record.metadata().unwrap().ino(), original.file_inode);
        assert_eq!(parent.metadata().unwrap().ino(), original.parent_inode);
        assert!(record.write_all(b"overwrite").is_err());
        drop((parent, record));
        assert_eq!(outcome(&publication).evidence(), &original);
        assert!(publication.publish(&AtomicBool::new(false)).is_err());
        assert_eq!(outcome(&publication).evidence(), &original);
        assert_eq!(std::fs::read(dir.path().join(NAME)).unwrap(), RECORD);
    }

    #[test]
    fn fixed_name_collision_never_adopts_overwrites_or_deletes_existing_record() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(NAME), b"unrelated existing reservation").unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut publication = prepare(dir.path(), deadline);
        assert!(publication.publish(&AtomicBool::new(false)).is_err());
        assert_eq!(outcome(&publication).phase(), Phase::Collision);
        assert!(publication.descriptors().is_err());
        drop(publication);
        assert_eq!(
            std::fs::read(dir.path().join(NAME)).unwrap(),
            b"unrelated existing reservation"
        );
    }

    #[test]
    fn postlink_cancel_finishes_durability_and_retains_original_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut publication = prepare(dir.path(), deadline);
        let cancel = AtomicBool::new(false);
        assert!(
            publication
                .publish_after_link(&cancel, || {
                    cancel.store(true, Ordering::Release);
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(outcome(&publication).phase(), Phase::Durable);
        assert_eq!(std::fs::read(dir.path().join(NAME)).unwrap(), RECORD);
        assert!(publication.publish(&AtomicBool::new(false)).is_err());
    }

    #[test]
    fn postlink_error_or_unwind_retains_exact_visible_record_without_retry() {
        for unwind in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let deadline = Instant::now() + Duration::from_secs(60);
            let mut publication = prepare(dir.path(), deadline);
            assert!(
                publication
                    .publish_after_link(&AtomicBool::new(false), || {
                        if unwind {
                            panic!("injected after-link unwind")
                        }
                        anyhow::bail!("injected after-link failure")
                    })
                    .is_err()
            );
            assert_eq!(outcome(&publication).phase(), Phase::LinkedUnconfirmed);
            let original = outcome(&publication).evidence().clone();
            assert_eq!(
                std::fs::metadata(dir.path().join(NAME)).unwrap().ino(),
                original.file_inode
            );
            assert!(publication.descriptors().is_err());
            assert!(publication.publish(&AtomicBool::new(false)).is_err());
            drop(publication);
            assert_eq!(std::fs::read(dir.path().join(NAME)).unwrap(), RECORD);
        }
    }

    #[test]
    fn cancelled_or_expired_after_staging_retains_prepared_owner_without_link() {
        for cancelled in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let deadline = Instant::now() + Duration::from_secs(60);
            let mut publication = prepare(dir.path(), deadline);
            if !cancelled {
                // Test-only shortening, never a production deadline reset.
                publication.deadline = Instant::now();
            }
            assert!(publication.publish(&AtomicBool::new(cancelled)).is_err());
            assert!(matches!(&publication.state, Some(State::Prepared(_))));
            assert!(!dir.path().join(NAME).exists());
            assert!(publication.descriptors().is_err());
        }
    }

    #[test]
    fn substituted_durable_name_never_transfers_replacement_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut publication = prepare(dir.path(), deadline);
        publication.publish(&AtomicBool::new(false)).unwrap();
        let original = outcome(&publication).evidence().clone();
        std::fs::rename(dir.path().join(NAME), dir.path().join("held-original")).unwrap();
        std::fs::write(dir.path().join(NAME), b"unrelated replacement").unwrap();
        assert!(publication.descriptors().is_err());
        assert_eq!(outcome(&publication).evidence(), &original);
        drop(publication);
        assert_eq!(
            std::fs::read(dir.path().join(NAME)).unwrap(),
            b"unrelated replacement"
        );
        assert_eq!(
            std::fs::read(dir.path().join("held-original")).unwrap(),
            RECORD
        );
    }
}
