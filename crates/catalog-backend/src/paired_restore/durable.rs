//! Descriptor-only verification for the private consumed restore phases.
//! Expected bytes come ONLY from the prepared/rotated owner's canonical producer;
//! they are not a public DTO grant. Callers retain publication Outcomes on error.
//! This module never creates, adopts, deletes or retries a filesystem artifact.
use anyhow::{Context, ensure};
use sha2::{Digest, Sha256};
use std::{
    ffi::CStr,
    fs::{File, Metadata},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{FileExt, MetadataExt},
    },
    sync::atomic::{AtomicBool, Ordering},
};
pub(crate) const RESERVATION_NAME: &CStr = c"paired-install.restore-reservation.json";
pub(crate) const ARTIFACT_NAME: &CStr = c"paired-install.restore-native.sqlite3";
pub(crate) const ARTIFACT_RECORD_NAME: &CStr = c"paired-install.restore-native.json";
pub(crate) const MAX_RESERVATION: usize = 64 * 1024;
pub(crate) const MAX_ARTIFACT_RECORD: usize = 256 * 1024;
const BUFFER: usize = 64 * 1024;
const MAX_ARTIFACT: u64 = 56 * 1024 * 1024;
type Identity = (u64, u64);
fn check(cancel: &AtomicBool) -> anyhow::Result<()> {
    ensure!(
        !cancel.load(Ordering::Acquire),
        "restore descriptor verification cancelled"
    );
    Ok(())
}
fn id(meta: &Metadata) -> Identity {
    (meta.dev(), meta.ino())
}
fn stable(meta: &Metadata) -> (Identity, u64, i64, i64, i64, i64) {
    (
        id(meta),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}
fn named(parent: &File, name: &CStr) -> anyhow::Result<File> {
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    ensure!(
        fd >= 0,
        "restore fixed artifact unavailable: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: openat returned a new owned descriptor; it never escapes raw.
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn parent_matches(parent: &File, expected: Identity) -> anyhow::Result<()> {
    let meta = parent.metadata()?;
    ensure!(
        meta.is_dir() && id(&meta) == expected,
        "restore parent differs from recorded97 directory"
    );
    Ok(())
}
fn digest_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
/// Device/inode are actual publication evidence; bytes/hash come from the
/// independently verified opaque rotated native image. The phase joins them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArtifactCommitment {
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) len: u64,
    pub(crate) sha256: String,
}
fn file_matches(
    parent: &File,
    file: &File,
    name: &CStr,
    expected: &ArtifactCommitment,
    max: u64,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    check(cancel)?;
    ensure!(
        expected.len > 0 && expected.len <= max && digest_valid(&expected.sha256),
        "restore artifact commitment exceeds fixed cohort"
    );
    let held = file.metadata()?;
    let linked = named(parent, name)?;
    let link = linked.metadata()?;
    ensure!(
        held.is_file()
            && link.is_file()
            && held.nlink() == 1
            && link.nlink() == 1
            && id(&held) == (expected.device, expected.inode)
            && id(&link) == id(&held)
            && held.len() == expected.len
            && link.len() == expected.len,
        "restore held/named file identity or size differs"
    );
    // Bounded streaming: no full-image allocation, no shared cursor mutation.
    let mut hash = Sha256::new();
    let mut buffer = [0u8; BUFFER];
    let mut offset = 0u64;
    while offset < expected.len {
        check(cancel)?;
        let want = (expected.len - offset).min(BUFFER as u64) as usize;
        let n = file.read_at(&mut buffer[..want], offset)?;
        ensure!(n > 0, "restore artifact truncated while reading");
        hash.update(&buffer[..n]);
        offset = offset
            .checked_add(n as u64)
            .context("restore offset overflow")?;
    }
    ensure!(
        format!("{:x}", hash.finalize()) == expected.sha256,
        "restore artifact digest differs"
    );
    let final_named = named(parent, name)?;
    let final_meta = final_named.metadata()?;
    ensure!(
        stable(&held) == stable(&file.metadata()?)
            && stable(&held) == stable(&final_meta)
            && final_meta.is_file()
            && final_meta.nlink() == 1,
        "restore artifact changed during verification"
    );
    check(cancel)
}
fn record_commitment(
    file: &File,
    expected: &[u8],
    max: usize,
) -> anyhow::Result<ArtifactCommitment> {
    ensure!(
        !expected.is_empty() && expected.len() <= max,
        "restore record exceeds fixed envelope"
    );
    let meta = file.metadata()?;
    Ok(ArtifactCommitment {
        device: meta.dev(),
        inode: meta.ino(),
        len: expected.len() as u64,
        sha256: catabolic_core::sha256(expected),
    })
}
/// Descriptor custody before verification. This type grants no durability or
/// reservation authority; its owner must survive every fallible operation.
pub(crate) struct PendingReservation {
    parent: File,
    record: File,
    checked: Option<(Identity, ArtifactCommitment)>,
}
impl PendingReservation {
    #[cfg(test)]
    pub(crate) fn descriptors_for_test(&self) -> (&File, &File) {
        (&self.parent, &self.record)
    }

    pub(crate) fn new(parent: File, record: File) -> Self {
        Self {
            parent,
            record,
            checked: None,
        }
    }
    pub(crate) fn verify(
        &mut self,
        expected_parent: Identity,
        expected: &[u8],
        cancel: &AtomicBool,
        deadline: std::time::Instant,
    ) -> anyhow::Result<()> {
        self.checked = None;
        reservation_alive(cancel, deadline)?;
        let commitment = record_commitment(&self.record, expected, MAX_RESERVATION)?;
        let verify = || -> anyhow::Result<()> {
            reservation_alive(cancel, deadline)?;
            parent_matches(&self.parent, expected_parent)?;
            file_matches(
                &self.parent,
                &self.record,
                RESERVATION_NAME,
                &commitment,
                MAX_RESERVATION as u64,
                cancel,
            )?;
            parent_matches(&self.parent, expected_parent)?;
            reservation_alive(cancel, deadline)
        };
        verify()?;
        self.record.sync_all()?;
        reservation_alive(cancel, deadline)?;
        self.parent.sync_all()?;
        reservation_alive(cancel, deadline)?;
        verify()?;
        self.checked = Some((expected_parent, commitment));
        Ok(())
    }
    pub(crate) fn into_descriptors(self) -> (File, File) {
        (self.parent, self.record)
    }
    pub(crate) fn into_verified(mut self) -> Result<DurableReservation, (Self, anyhow::Error)> {
        let Some((parent_id, commitment)) = self.checked.take() else {
            return Err((
                self,
                anyhow::anyhow!("reservation descriptors were not verified"),
            ));
        };
        Ok(DurableReservation {
            parent: self.parent,
            record: self.record,
            parent_id,
            commitment,
        })
    }
}
pub(crate) struct DurableReservation {
    parent: File,
    record: File,
    parent_id: Identity,
    commitment: ArtifactCommitment,
}
impl DurableReservation {
    #[cfg(test)]
    pub(crate) fn descriptors_for_test(&self) -> (&File, &File) {
        (&self.parent, &self.record)
    }

    pub(crate) fn into_descriptors(self) -> (File, File) {
        (self.parent, self.record)
    }
    pub(crate) fn verify_until(
        &self,
        cancel: &AtomicBool,
        deadline: std::time::Instant,
    ) -> anyhow::Result<()> {
        reservation_alive(cancel, deadline)?;
        self.verify(cancel)?;
        reservation_alive(cancel, deadline)
    }
    pub(crate) fn verify(&self, cancel: &AtomicBool) -> anyhow::Result<()> {
        parent_matches(&self.parent, self.parent_id)?;
        file_matches(
            &self.parent,
            &self.record,
            RESERVATION_NAME,
            &self.commitment,
            MAX_RESERVATION as u64,
            cancel,
        )?;
        parent_matches(&self.parent, self.parent_id)
    }
    pub(crate) fn sha256(&self) -> &str {
        &self.commitment.sha256
    }
}
// Explicit additional live verifier storage, independent of the seal's own
// canonical bytes and previous high-water. No whole-file buffer is allocated.
pub(crate) fn reservation_verification_allocations() -> u64 {
    (BUFFER
        + std::mem::size_of::<Sha256>()
        + std::mem::size_of::<DurableReservation>()
        + 4 * std::mem::size_of::<Metadata>()
        + 4 * std::mem::size_of::<File>()
        + 4 * 64) as u64
}
fn reservation_alive(cancel: &AtomicBool, deadline: std::time::Instant) -> anyhow::Result<()> {
    check(cancel)?;
    ensure!(
        std::time::Instant::now() < deadline,
        "restore reservation deadline exhausted"
    );
    Ok(())
}
pub(crate) struct ReservationVerificationFailure {
    parent: File,
    record: File,
    error: anyhow::Error,
}
impl ReservationVerificationFailure {
    pub(crate) fn into_parts(self) -> (File, File, anyhow::Error) {
        (self.parent, self.record, self.error)
    }
}
/// Retain the caller's exact descriptors even if durability or the post-fsync
/// checks fail. Never delete/adopt a file, reset a deadline or claim syscall
/// preemption. The application separately retains its publication Outcome.
pub(crate) fn verify_reservation_retained(
    parent: File,
    record: File,
    expected_parent: Identity,
    expected: &[u8],
    cancel: &AtomicBool,
    deadline: std::time::Instant,
) -> Result<DurableReservation, ReservationVerificationFailure> {
    let mut pending = PendingReservation::new(parent, record);
    if let Err(error) = pending.verify(expected_parent, expected, cancel, deadline) {
        let (parent, record) = pending.into_descriptors();
        return Err(ReservationVerificationFailure {
            parent,
            record,
            error,
        });
    }
    pending.into_verified().map_err(|(pending, error)| {
        let (parent, record) = pending.into_descriptors();
        ReservationVerificationFailure {
            parent,
            record,
            error,
        }
    })
}

pub(crate) fn verify_reservation(
    parent: File,
    record: File,
    expected_parent: Identity,
    expected_canonical: &[u8],
    cancel: &AtomicBool,
) -> anyhow::Result<DurableReservation> {
    let commitment = record_commitment(&record, expected_canonical, MAX_RESERVATION)?;
    let owner = DurableReservation {
        parent,
        record,
        parent_id: expected_parent,
        commitment,
    };
    owner.verify(cancel)?;
    owner.record.sync_all()?;
    owner.parent.sync_all()?;
    owner.verify(cancel)?;
    Ok(owner)
}
pub(crate) struct DurableArtifact {
    reservation: DurableReservation,
    artifact: File,
    record: File,
    artifact_commitment: ArtifactCommitment,
    record_commitment: ArtifactCommitment,
}
impl DurableArtifact {
    pub(crate) fn verify(&self, cancel: &AtomicBool) -> anyhow::Result<()> {
        self.reservation.verify(cancel)?;
        file_matches(
            &self.reservation.parent,
            &self.artifact,
            ARTIFACT_NAME,
            &self.artifact_commitment,
            MAX_ARTIFACT,
            cancel,
        )?;
        file_matches(
            &self.reservation.parent,
            &self.record,
            ARTIFACT_RECORD_NAME,
            &self.record_commitment,
            MAX_ARTIFACT_RECORD as u64,
            cancel,
        )?;
        self.reservation.verify(cancel)
    }
    pub(crate) fn commitment(&self) -> &ArtifactCommitment {
        &self.artifact_commitment
    }
    pub(crate) fn reservation_sha256(&self) -> &str {
        self.reservation.sha256()
    }
    pub(crate) fn record_sha256(&self) -> &str {
        &self.record_commitment.sha256
    }
}
/// Exact record bytes are minted from the rotated phase's bounded canonical
/// origin plus these artifact/parent identities, never arbitrary caller JSON.
pub(crate) fn verify_artifact(
    reservation: DurableReservation,
    artifact: File,
    record: File,
    expected_canonical: &[u8],
    expected: ArtifactCommitment,
    cancel: &AtomicBool,
) -> anyhow::Result<DurableArtifact> {
    let record_commitment = record_commitment(&record, expected_canonical, MAX_ARTIFACT_RECORD)?;
    let owner = DurableArtifact {
        reservation,
        artifact,
        record,
        artifact_commitment: expected,
        record_commitment,
    };
    owner.verify(cancel)?;
    owner.artifact.sync_all()?;
    owner.record.sync_all()?;
    owner.reservation.parent.sync_all()?;
    owner.verify(cancel)?;
    Ok(owner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, os::unix::fs::symlink};
    const RESERVATION: &[u8] = br#"{"contract":1,"operation":"actual-fixture"}"#;
    const RECORD: &[u8] = br#"{"contract":1,"artifact":"fixed-fixture"}"#;
    struct Fixture {
        dir: tempfile::TempDir,
        parent: File,
        reservation: File,
        artifact: File,
        record: File,
    }
    fn write(dir: &std::path::Path, name: &CStr, bytes: &[u8]) -> File {
        let mut file = File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(dir.join(name.to_str().unwrap()))
            .unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
        file
    }
    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let parent = File::open(dir.path()).unwrap();
        let reservation = write(dir.path(), RESERVATION_NAME, RESERVATION);
        let artifact = write(
            dir.path(),
            ARTIFACT_NAME,
            b"actual opaque-image fixture bytes",
        );
        let record = write(dir.path(), ARTIFACT_RECORD_NAME, RECORD);
        Fixture {
            dir,
            parent,
            reservation,
            artifact,
            record,
        }
    }
    #[test]
    fn retained_reservation_refusals_return_exact_parent_and_file() {
        for case in 0..5 {
            let f = fixture();
            let other = fixture();
            let expected_parent = id(&f.parent.metadata().unwrap());
            let parent = if case == 0 {
                other.parent.try_clone().unwrap()
            } else {
                f.parent.try_clone().unwrap()
            };
            let record = f.reservation.try_clone().unwrap();
            let expected = (
                id(&parent.metadata().unwrap()),
                id(&record.metadata().unwrap()),
            );
            if case == 1 {
                std::fs::rename(
                    f.dir.path().join(RESERVATION_NAME.to_str().unwrap()),
                    f.dir.path().join("original-reservation.json"),
                )
                .unwrap();
                write(f.dir.path(), RESERVATION_NAME, RESERVATION);
            }
            let bytes: &[u8] = if case == 2 {
                b"wrong canonical bytes"
            } else {
                RESERVATION
            };
            let cancel = AtomicBool::new(case == 3);
            let now = std::time::Instant::now();
            let deadline = if case == 4 {
                now - std::time::Duration::from_secs(1)
            } else {
                now + std::time::Duration::from_secs(30)
            };
            let failure = match verify_reservation_retained(
                parent,
                record,
                expected_parent,
                bytes,
                &cancel,
                deadline,
            ) {
                Ok(_) => panic!("invalid reservation case{case} accepted"),
                Err(failure) => failure,
            };
            let (parent, record, _error) = failure.into_parts();
            assert_eq!(
                (
                    id(&parent.metadata().unwrap()),
                    id(&record.metadata().unwrap())
                ),
                expected
            );
            assert_eq!(record.metadata().unwrap().len(), RESERVATION.len() as u64);
        }
    }
    fn reserve(f: &Fixture, flag: &AtomicBool) -> anyhow::Result<DurableReservation> {
        verify_reservation(
            f.parent.try_clone()?,
            f.reservation.try_clone()?,
            id(&f.parent.metadata()?),
            RESERVATION,
            flag,
        )
    }
    fn artifact(f: &Fixture) -> anyhow::Result<DurableArtifact> {
        let expected = record_commitment(&f.artifact, b"actual opaque-image fixture bytes", 1024)?;
        verify_artifact(
            reserve(f, &AtomicBool::new(false))?,
            f.artifact.try_clone()?,
            f.record.try_clone()?,
            RECORD,
            expected,
            &AtomicBool::new(false),
        )
    }
    #[test]
    fn actual_fixed_descriptor_roundtrip_revalidates_without_shared_seek() {
        let f = fixture();
        let owner = artifact(&f).unwrap();
        owner.verify(&AtomicBool::new(false)).unwrap();
        assert_eq!(
            owner.reservation_sha256(),
            catabolic_core::sha256(RESERVATION)
        );
        assert_eq!(owner.record_sha256(), catabolic_core::sha256(RECORD));
        assert_eq!(owner.commitment().len, f.artifact.metadata().unwrap().len());
    }
    #[test]
    fn replacement_truncation_and_in_place_corruption_refuse() {
        let f = fixture();
        let owner = artifact(&f).unwrap();
        f.artifact.write_at(b"wrong", 0).unwrap();
        assert!(owner.verify(&AtomicBool::new(false)).is_err());
        f.artifact.set_len(1).unwrap();
        assert!(owner.verify(&AtomicBool::new(false)).is_err());
        std::fs::rename(
            f.dir.path().join(ARTIFACT_NAME.to_str().unwrap()),
            f.dir.path().join("old"),
        )
        .unwrap();
        write(
            f.dir.path(),
            ARTIFACT_NAME,
            b"actual opaque-image fixture bytes",
        );
        assert!(owner.verify(&AtomicBool::new(false)).is_err());
    }
    #[test]
    fn foreign_parent_symlink_and_extra_link_refuse_without_cleanup() {
        let f = fixture();
        let other = fixture();
        assert!(
            verify_reservation(
                f.parent.try_clone().unwrap(),
                f.reservation.try_clone().unwrap(),
                id(&other.parent.metadata().unwrap()),
                RESERVATION,
                &AtomicBool::new(false)
            )
            .is_err()
        );
        std::fs::hard_link(
            f.dir.path().join(RESERVATION_NAME.to_str().unwrap()),
            f.dir.path().join("alias"),
        )
        .unwrap();
        assert!(reserve(&f, &AtomicBool::new(false)).is_err());
        assert!(f.dir.path().join("alias").exists());
        std::fs::rename(
            other.dir.path().join(RESERVATION_NAME.to_str().unwrap()),
            other.dir.path().join("held"),
        )
        .unwrap();
        symlink(
            "held",
            other.dir.path().join(RESERVATION_NAME.to_str().unwrap()),
        )
        .unwrap();
        assert!(reserve(&other, &AtomicBool::new(false)).is_err());
        assert!(other.dir.path().join("held").exists());
    }
    #[test]
    fn wrong_record_artifact_commitment_and_duplicate_json_refuse() {
        let f = fixture();
        assert!(
            verify_reservation(
                f.parent.try_clone().unwrap(),
                f.reservation.try_clone().unwrap(),
                id(&f.parent.metadata().unwrap()),
                br#"{"contract":1,"contract":1}"#,
                &AtomicBool::new(false)
            )
            .is_err()
        );
        let mut wrong =
            record_commitment(&f.artifact, b"actual opaque-image fixture bytes", 1024).unwrap();
        wrong.inode ^= 1;
        assert!(
            verify_artifact(
                reserve(&f, &AtomicBool::new(false)).unwrap(),
                f.artifact.try_clone().unwrap(),
                f.record.try_clone().unwrap(),
                RECORD,
                wrong,
                &AtomicBool::new(false)
            )
            .is_err()
        );
        f.record.write_at(b"!", 0).unwrap();
        assert!(artifact(&f).is_err());
    }
    #[test]
    fn cancellation_and_envelope_limits_refuse_before_grant() {
        let f = fixture();
        assert!(reserve(&f, &AtomicBool::new(true)).is_err());
        assert!(
            verify_reservation(
                f.parent.try_clone().unwrap(),
                f.reservation.try_clone().unwrap(),
                id(&f.parent.metadata().unwrap()),
                &vec![0; MAX_RESERVATION + 1],
                &AtomicBool::new(false)
            )
            .is_err()
        );
        let mut wrong =
            record_commitment(&f.artifact, b"actual opaque-image fixture bytes", 1024).unwrap();
        wrong.len = MAX_ARTIFACT + 1;
        assert!(
            verify_artifact(
                reserve(&f, &AtomicBool::new(false)).unwrap(),
                f.artifact.try_clone().unwrap(),
                f.record.try_clone().unwrap(),
                RECORD,
                wrong,
                &AtomicBool::new(false)
            )
            .is_err()
        );
        assert_eq!(
            f.artifact.metadata().unwrap().len(),
            b"actual opaque-image fixture bytes".len() as u64
        );
    }
}
