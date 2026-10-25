//! Private same-directory fresh-target install journal. Production construction
//! awaits verified95/96 converted images; these records alone grant no restore.
use super::publication_file::{Outcome, PreparedLink, Sealed};
use anyhow::{Context, ensure};
use playscale_core::paired_backup::{
    Artifact,
    install::{self, Input, Phase},
    publication::{Evidence, Phase as PublicationPhase},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Instant,
};
const LOCK: &str = "server.lock";
const LOCK_KIND: &str = "motion-paired-install-lock";
const RESERVATION: &str = "paired-install.reservation.json";
const INTENT: &str = "paired-install.intent.json";
const COMPLETE: &str = "paired-install.complete.json";
const MOTION_ACK: &str = "paired-install.motion.json";
const NATIVE_ACK: &str = "paired-install.native.json";
const LIMIT: u64 = 16384;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reservation {
    pub(super) contract: u32,
    pub(super) install_id: String,
    pub(super) pair_sha256: String,
    pub(super) parent_device: u64,
    pub(super) parent_inode: u64,
}
/// Self-identification, not restore authority. The hash binds exactly the
/// canonical Reservation later required by the existing install journal.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LockMarker {
    contract: u32,
    kind: String,
    install_id: String,
    reservation_sha256: String,
}
pub(super) struct TargetLock {
    publication: Option<Outcome>,
    preparation_started: bool,
    locked: bool,
    preparation_error: Option<String>,
}
impl TargetLock {
    /// Install this empty owner in the service target slot before any IO.
    pub(super) fn new() -> Self {
        Self {
            publication: None,
            preparation_started: false,
            locked: false,
            preparation_error: None,
        }
    }
    /// A returned anonymous Outcome is retained before the fallible flock.
    /// Sealed's internal creation failures still release only an unpublished
    /// anonymous file, never a possibly named or already acquired target lock.
    pub(super) fn prepare(
        &mut self,
        path: &Path,
        record: &Reservation,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ensure!(
            !self.preparation_started,
            "target lock preparation cannot be retried"
        );
        self.preparation_started = true;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let reservation = reservation_bytes(record)?;
            let bytes = serde_json::to_vec(&LockMarker {
                contract: 1,
                kind: LOCK_KIND.into(),
                install_id: record.install_id.clone(),
                reservation_sha256: hash(&reservation),
            })?;
            ensure!(
                bytes.len() <= LIMIT as usize,
                "target lock marker exceeds bound"
            );
            self.publication = Some(
                Sealed::create_checked(
                    path,
                    LOCK,
                    cancel,
                    Some((record.parent_device, record.parent_inode)),
                    |out| {
                        out.write_all(&bytes)?;
                        Ok(Artifact {
                            bytes: bytes.len() as u64,
                            sha256: hash(&bytes),
                        })
                    },
                    |_| Ok(()),
                )?
                .retain(),
            );
            self.publication
                .as_ref()
                .expect("marker just retained")
                .lock_target_marker()?;
            self.locked = true;
            Ok(())
        }))
        .unwrap_or_else(|_| {
            Err(anyhow::anyhow!(
                "target lock preparation panicked; retained returned owner"
            ))
        });
        if let Err(error) = &result {
            self.preparation_error = Some(format!("{error:#}"));
        }
        result
    }
    /// Borrow the already retained marker for one fixed create-only link.
    pub(super) fn prepare_link_until<'a>(
        &'a mut self,
        cancel: &'a AtomicBool,
        deadline: Instant,
    ) -> anyhow::Result<PreparedLink<'a>> {
        ensure!(self.locked, "target marker actual FD is not locked");
        self.publication
            .as_mut()
            .context("target lock marker missing")?
            .prepare_link_until(cancel, deadline)
    }
    /// Retained marker FD, including after a failed flock. Presence is not lock
    /// authority: only successful prepare sets locked and permits preflight.
    /// Borrowed only for fixed identity/startup checks, never an owned escape.
    pub(super) fn held_lock(&self) -> Option<&File> {
        self.publication
            .as_ref()
            .and_then(Outcome::target_marker_lock)
    }
    pub(super) fn phase(&self) -> Option<PublicationPhase> {
        self.publication.as_ref().map(Outcome::phase)
    }
    pub(super) fn evidence(&self) -> Option<&Evidence> {
        self.publication.as_ref().map(Outcome::evidence)
    }
    pub(super) fn error(&self) -> Option<&str> {
        self.preparation_error
            .as_deref()
            .or_else(|| self.publication.as_ref().and_then(Outcome::error))
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    contract: u32,
    install_id: String,
    pair_sha256: String,
    transition_sha256: String,
    reservation_sha256: String,
    motion: Evidence,
    native: Evidence,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    contract: u32,
    install_id: String,
    intent_sha256: String,
    phase: String,
}
fn hash(b: &[u8]) -> String {
    catabolic_core::sha256(b)
}
fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn canonical<T: serde::de::DeserializeOwned + Serialize>(b: &[u8]) -> anyhow::Result<T> {
    ensure!(b.len() <= LIMIT as usize, "install record exceeds bound");
    let v: T = serde_json::from_slice(b)?;
    ensure!(serde_json::to_vec(&v)? == b, "noncanonical install record");
    Ok(v)
}
fn reservation_bytes(record: &Reservation) -> anyhow::Result<Vec<u8>> {
    let bytes = serde_json::to_vec(record)?;
    ensure!(
        record.contract == 1
            && uuid::Uuid::parse_str(&record.install_id).is_ok()
            && digest(&record.pair_sha256)
            && record.parent_inode != 0
            && bytes.len() <= LIMIT as usize,
        "invalid target reservation"
    );
    Ok(bytes)
}
fn identity(f: &File) -> anyhow::Result<(u64, u64)> {
    let m = f.metadata()?;
    Ok((m.dev(), m.ino()))
}
struct PinnedRecord {
    file: File,
    bytes: Vec<u8>,
}
fn read_at(parent: &File, name: &str) -> anyhow::Result<Option<PinnedRecord>> {
    let name = std::ffi::CString::new(name)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ENOENT) {
            return Ok(None);
        }
        return Err(e.into());
    }
    let mut f = unsafe { File::from_raw_fd(fd) };
    let m = f.metadata()?;
    ensure!(
        m.is_file() && m.nlink() == 1 && m.len() <= LIMIT,
        "invalid install record storage"
    );
    let mut b = Vec::new();
    (&mut f).take(LIMIT + 1).read_to_end(&mut b)?;
    ensure!(
        b.len() as u64 == m.len(),
        "install record changed while reading"
    );
    Ok(Some(PinnedRecord { file: f, bytes: b }))
}
fn validate_intent(i: &Intent, parent: &File) -> anyhow::Result<()> {
    ensure!(
        i.contract == 1
            && uuid::Uuid::parse_str(&i.install_id).is_ok()
            && digest(&i.pair_sha256)
            && digest(&i.transition_sha256)
            && digest(&i.reservation_sha256),
        "invalid install commitments"
    );
    let p = identity(parent)?;
    for (e, name) in [
        (&i.motion, "playscale.sqlite3"),
        (&i.native, "catabolic.sqlite3"),
    ] {
        ensure!(
            e.valid() && e.name == name && (e.parent_device, e.parent_inode) == p,
            "install target identity mismatch"
        );
    }
    ensure!(
        (i.motion.file_device, i.motion.file_inode) != (i.native.file_device, i.native.file_inode),
        "install images alias"
    );
    Ok(())
}
fn validate_record(
    bytes: &[u8],
    intent: &Intent,
    intent_hash: &str,
    phase: &str,
) -> anyhow::Result<()> {
    let r: Record = canonical(bytes)?;
    ensure!(
        r.contract == 1
            && r.install_id == intent.install_id
            && r.intent_sha256 == intent_hash
            && r.phase == phase,
        "install phase record mismatch"
    );
    Ok(())
}
fn verify_named_inode(parent: &File, e: &Evidence) -> anyhow::Result<File> {
    let name = std::ffi::CString::new(e.name.as_str())?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    ensure!(
        fd >= 0,
        "installed image unavailable: {}",
        std::io::Error::last_os_error()
    );
    let f = unsafe { File::from_raw_fd(fd) };
    let m = f.metadata()?;
    ensure!(
        m.is_file() && m.nlink() == 1 && (m.dev(), m.ino()) == (e.file_device, e.file_inode),
        "installed image inode changed"
    );
    Ok(f)
}
/// Called after actual server.lock acquisition and before opening either DB.
/// Completed hashes are historical installation evidence, not current DB hashes.
pub(super) fn startup_guard(path: &Path, native: &Path, lock: &File) -> anyhow::Result<()> {
    let parent = super::pair::open_parent(path)?;
    let lock_record = read_at(&parent, LOCK)?.context("server lock path missing")?;
    ensure!(
        identity(&lock_record.file)? == identity(lock)?,
        "server lock path identity changed"
    );
    // Ordinary server locks have always been empty. A historical empty lock-only
    // directory cannot retroactively be distinguished from ordinary startup.
    // New restore locks self-identify before becoming named, closing that gap
    // for new attempts without inventing authority for the marker itself.
    let marker = if lock_record.bytes.is_empty() {
        None
    } else {
        let marker: LockMarker = canonical(&lock_record.bytes)?;
        ensure!(
            marker.contract == 1
                && marker.kind == LOCK_KIND
                && uuid::Uuid::parse_str(&marker.install_id).is_ok()
                && digest(&marker.reservation_sha256),
            "invalid paired target lock marker"
        );
        Some(marker)
    };
    let reservation = read_at(&parent, RESERVATION)?;
    if let Some(marker) = &marker {
        let reservation = reservation.as_ref().context(
            "marked paired restore target has no reservation; explicit recovery required",
        )?;
        let r: Reservation = canonical(&reservation.bytes)?;
        reservation_bytes(&r)?;
        ensure!(
            r.install_id == marker.install_id
                && hash(&reservation.bytes) == marker.reservation_sha256
                && (r.parent_device, r.parent_inode) == identity(&parent)?,
            "paired target lock marker/reservation mismatch"
        );
    }
    let intent = read_at(&parent, INTENT)?;
    let Some(intent_record) = intent else {
        ensure!(
            reservation.is_none(),
            "paired restore target reserved; explicit conversion/install recovery required before database creation"
        );
        for name in [
            MOTION_ACK,
            NATIVE_ACK,
            COMPLETE,
            "paired-install.conversion.json",
        ] {
            ensure!(
                read_at(&parent, name)?.is_none(),
                "orphan paired install record requires recovery"
            );
        }
        let current_lock =
            read_at(&parent, LOCK)?.context("server lock vanished during startup")?;
        ensure!(
            identity(&current_lock.file)? == identity(&lock_record.file)?
                && current_lock.bytes == lock_record.bytes,
            "server lock marker changed during startup"
        );
        ensure!(
            identity(&super::pair::open_parent(path)?)? == identity(&parent)?,
            "startup parent changed"
        );
        return Ok(());
    };
    ensure!(
        native == path.join("catabolic.sqlite3"),
        "paired install external native target requires explicit recovery"
    );
    let reservation = reservation.context("paired installation reservation missing")?;
    let r: Reservation = canonical(&reservation.bytes)?;
    let i: Intent = canonical(&intent_record.bytes)?;
    ensure!(
        r.contract == 1
            && r.install_id == i.install_id
            && r.pair_sha256 == i.pair_sha256
            && (r.parent_device, r.parent_inode) == identity(&parent)?
            && hash(&reservation.bytes) == i.reservation_sha256,
        "paired installation reservation linkage mismatch"
    );
    validate_intent(&i, &parent)?;
    let h = hash(&intent_record.bytes);
    let mut pinned_records = vec![
        (LOCK, lock_record),
        (RESERVATION, reservation),
        (INTENT, intent_record),
    ];
    for (name, phase) in [
        (MOTION_ACK, "motion"),
        (NATIVE_ACK, "native"),
        (COMPLETE, "complete"),
    ] {
        let b = read_at(&parent, name)?.context(
            "paired installation incomplete; explicit recovery required before database admission",
        )?;
        validate_record(&b.bytes, &i, &h, phase)?;
        pinned_records.push((name, b));
    }
    let motion = verify_named_inode(&parent, &i.motion)?;
    let native = verify_named_inode(&parent, &i.native)?;
    // This establishes current durability after an earlier unknown final sync;
    // it never asserts that the previous publisher reported success.
    parent.sync_all()?;
    for (name, pinned) in &pinned_records {
        let current = read_at(&parent, name)?.context("install record vanished during recovery")?;
        ensure!(
            identity(&current.file)? == identity(&pinned.file)? && current.bytes == pinned.bytes,
            "install record changed during recovery"
        );
    }
    ensure!(
        identity(&verify_named_inode(&parent, &i.motion)?)? == identity(&motion)?
            && identity(&verify_named_inode(&parent, &i.native)?)? == identity(&native)?,
        "installed inode changed during recovery"
    );
    ensure!(
        identity(&super::pair::open_parent(path)?)? == identity(&parent)?,
        "install parent changed"
    );
    Ok(())
}

pub(super) fn reserve_target(
    path: &Path,
    record: &Reservation,
    cancel: &AtomicBool,
) -> anyhow::Result<Outcome> {
    Ok(reservation_stage(path, record, cancel)?.publish(cancel, || Ok(())))
}
/// Return only an anonymous owner for immediate retention by the target slot.
/// Its later fixed borrowed preflight/publication cannot transfer ownership.
pub(super) fn prepare_target_reservation(
    path: &Path,
    record: &Reservation,
    cancel: &AtomicBool,
) -> anyhow::Result<Outcome> {
    Ok(reservation_stage(path, record, cancel)?.retain())
}
fn reservation_stage(
    path: &Path,
    record: &Reservation,
    cancel: &AtomicBool,
) -> anyhow::Result<Sealed> {
    let bytes = reservation_bytes(record)?;
    Sealed::create_checked(
        path,
        RESERVATION,
        cancel,
        Some((record.parent_device, record.parent_inode)),
        |out| {
            out.write_all(&bytes)?;
            Ok(Artifact {
                bytes: bytes.len() as u64,
                sha256: hash(&bytes),
            })
        },
        |_| Ok(()),
    )
}

struct Install {
    path: PathBuf,
    parent: File,
    lock: File,
    intent: Intent,
    intent_bytes: Vec<u8>,
    motion: Option<Sealed>,
    native: Option<Sealed>,
    phase: Phase,
    records: Vec<Outcome>, // Own every visible/uncertain descriptor; never unlink.
    #[cfg(test)]
    fail_completion_sync: bool,
}
impl Install {
    fn custody(&self) -> anyhow::Result<()> {
        ensure!(
            identity(&super::pair::open_parent(&self.path)?)? == identity(&self.parent)?,
            "install parent replaced"
        );
        let m = std::fs::symlink_metadata(self.path.join("server.lock"))?;
        ensure!(
            m.is_file() && m.nlink() == 1 && (m.dev(), m.ino()) == identity(&self.lock)?,
            "install target lock replaced"
        );
        validate_intent(&self.intent, &self.parent)
    }
    fn record(&mut self, name: &str, bytes: &[u8], cancel: &AtomicBool) -> anyhow::Result<()> {
        ensure!(
            bytes.len() <= LIMIT as usize,
            "install record exceeds bound"
        );
        self.custody()?;
        let e = &self.intent.motion;
        let stage = Sealed::create_checked(
            &self.path,
            name,
            cancel,
            Some((e.parent_device, e.parent_inode)),
            |out| {
                out.write_all(bytes)?;
                Ok(Artifact {
                    bytes: bytes.len() as u64,
                    sha256: hash(bytes),
                })
            },
            |_| Ok(()),
        )?;
        let outcome = stage.publish(cancel, || {
            #[cfg(test)]
            ensure!(
                !(name == COMPLETE && self.fail_completion_sync),
                "injected final directory-sync boundary failure"
            );
            Ok(())
        });
        let durable = outcome.phase() == PublicationPhase::Durable;
        self.records.push(outcome);
        ensure!(durable, "install record visibility/durability unresolved");
        Ok(())
    }
    fn phase_record(&mut self, name: &str, phase: &str, cancel: &AtomicBool) -> anyhow::Result<()> {
        let b = serde_json::to_vec(&Record {
            contract: 1,
            install_id: self.intent.install_id.clone(),
            intent_sha256: hash(&self.intent_bytes),
            phase: phase.into(),
        })?;
        self.record(name, &b, cancel)
    }
    fn run(
        &mut self,
        cancel: &AtomicBool,
        after: impl Fn(Phase) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        // A failed run is retained as recovery, never restartable by this owner.
        ensure!(
            self.phase == Phase::Prepared,
            "install owner cannot retry partial effects"
        );
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> anyhow::Result<()> {
                self.custody()?;
                for record in &mut self.records {
                    record.read_verified(|_| Ok(()))?;
                }
                let reservation = read_at(&self.parent, RESERVATION)?
                    .context("target reservation disappeared")?;
                let record: Reservation = canonical(&reservation.bytes)?;
                ensure!(
                    hash(&reservation.bytes) == self.intent.reservation_sha256
                        && record.install_id == self.intent.install_id
                        && record.pair_sha256 == self.intent.pair_sha256,
                    "target reservation differs from retained intent"
                );
                for leaf in [
                    INTENT,
                    MOTION_ACK,
                    NATIVE_ACK,
                    COMPLETE,
                    "playscale.sqlite3",
                    "catabolic.sqlite3",
                    "playscale.sqlite3-wal",
                    "playscale.sqlite3-shm",
                    "playscale.sqlite3-journal",
                    "catabolic.sqlite3-wal",
                    "catabolic.sqlite3-shm",
                    "catabolic.sqlite3-journal",
                ] {
                    let leaf = std::ffi::CString::new(leaf)?;
                    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
                    let result = unsafe {
                        libc::fstatat(
                            self.parent.as_raw_fd(),
                            leaf.as_ptr(),
                            stat.as_mut_ptr(),
                            libc::AT_SYMLINK_NOFOLLOW,
                        )
                    };
                    ensure!(
                        result < 0
                            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT),
                        "fresh install target is occupied or unobservable"
                    );
                }
                self.record(INTENT, &self.intent_bytes.clone(), cancel)?;
                self.phase = install::advance(self.phase, Input::IntentSynced)
                    .map_err(anyhow::Error::msg)?;
                after(self.phase)?;
                let out = self
                    .motion
                    .take()
                    .context("motion stage missing")?
                    .publish(cancel, || Ok(()));
                let durable = out.phase() == PublicationPhase::Durable;
                self.records.push(out);
                ensure!(durable, "motion image visibility/durability unresolved");
                self.phase_record(MOTION_ACK, "motion", cancel)?;
                self.phase = install::advance(self.phase, Input::MotionSynced)
                    .map_err(anyhow::Error::msg)?;
                after(self.phase)?;
                let out = self
                    .native
                    .take()
                    .context("native stage missing")?
                    .publish(cancel, || Ok(()));
                let durable = out.phase() == PublicationPhase::Durable;
                self.records.push(out);
                ensure!(durable, "native image visibility/durability unresolved");
                self.phase_record(NATIVE_ACK, "native", cancel)?;
                self.phase = install::advance(self.phase, Input::NativeSynced)
                    .map_err(anyhow::Error::msg)?;
                after(self.phase)?;
                self.custody()?;
                // Recheck all actual image bytes/inodes and immutable phase records
                // immediately before completion, not only at their earlier links.
                for record in &mut self.records {
                    record.read_verified(|_| Ok(()))?;
                }
                self.phase_record(COMPLETE, "complete", cancel)?;
                self.phase = install::advance(self.phase, Input::CompletionSynced)
                    .map_err(anyhow::Error::msg)?;
                Ok(())
            }))
            .unwrap_or_else(|_| {
                Err(anyhow::anyhow!(
                    "install owner unwound; exact partial evidence retained"
                ))
            });
        if result.is_err() {
            self.phase = Phase::RecoveryRequired;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn stage(path: &Path, name: &str, body: &[u8]) -> Sealed {
        Sealed::create_checked(
            path,
            name,
            &AtomicBool::new(false),
            None,
            |w| {
                w.write_all(body)?;
                Ok(Artifact {
                    bytes: body.len() as u64,
                    sha256: hash(body),
                })
            },
            |_| Ok(()),
        )
        .unwrap()
    }
    // These are descriptor/journal protocol fixtures, not validated SQLite or
    // actual95/96 restore authority. No production constructor accepts such bytes.
    fn fixture() -> (tempfile::TempDir, File, Install) {
        fixture_with_images(b"motion-final", b"native-final", false)
    }
    fn fixture_with_images(
        motion_bytes: &[u8],
        native_bytes: &[u8],
        marked: bool,
    ) -> (tempfile::TempDir, File, Install) {
        let dir = tempfile::tempdir().unwrap();
        let parent = super::super::pair::open_parent(dir.path()).unwrap();
        let (parent_device, parent_inode) = identity(&parent).unwrap();
        let install_id = uuid::Uuid::new_v4().to_string();
        let record = Reservation {
            contract: 1,
            install_id: install_id.clone(),
            pair_sha256: "a".repeat(64),
            parent_device,
            parent_inode,
        };
        let cancel = AtomicBool::new(false);
        let mut marker = TargetLock::new();
        let lock = if marked {
            marker.prepare(dir.path(), &record, &cancel).unwrap();
            marker
                .prepare_link_until(&cancel, Instant::now() + std::time::Duration::from_secs(30))
                .unwrap()
                .publish()
                .unwrap();
            marker.held_lock().unwrap().try_clone().unwrap()
        } else {
            let lock = std::fs::OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(dir.path().join(LOCK))
                .unwrap();
            fs2::FileExt::try_lock_exclusive(&lock).unwrap();
            lock
        };
        let motion = stage(dir.path(), "playscale.sqlite3", motion_bytes);
        let native = stage(dir.path(), "catabolic.sqlite3", native_bytes);
        let reservation = reserve_target(dir.path(), &record, &cancel).unwrap();
        assert_eq!(reservation.phase(), PublicationPhase::Durable);
        let reservation_sha256 = reservation.evidence().artifact.sha256.clone();
        let intent = Intent {
            contract: 1,
            install_id,
            pair_sha256: "a".repeat(64),
            transition_sha256: "b".repeat(64),
            reservation_sha256,
            motion: motion.evidence().clone(),
            native: native.evidence().clone(),
        };
        let bytes = serde_json::to_vec(&intent).unwrap();
        let install = Install {
            path: dir.path().into(),
            parent: super::super::pair::open_parent(dir.path()).unwrap(),
            lock: lock.try_clone().unwrap(),
            intent,
            intent_bytes: bytes,
            motion: Some(motion),
            native: Some(native),
            phase: Phase::Prepared,
            records: marker
                .publication
                .into_iter()
                .chain(std::iter::once(reservation))
                .collect(),
            fail_completion_sync: false,
        };
        (dir, lock, install)
    }
    fn fresh_reservation(path: &Path) -> Reservation {
        let parent = super::super::pair::open_parent(path).unwrap();
        let (parent_device, parent_inode) = identity(&parent).unwrap();
        Reservation {
            contract: 1,
            install_id: uuid::Uuid::new_v4().to_string(),
            pair_sha256: "a".repeat(64),
            parent_device,
            parent_inode,
        }
    }
    #[test]
    fn marked_lock_is_fully_written_and_actual_retained_fd_locked_before_create_only_link() {
        let dir = tempfile::tempdir().unwrap();
        let record = fresh_reservation(dir.path());
        let cancel = AtomicBool::new(false);
        let mut owner = TargetLock::new();
        assert!(owner.held_lock().is_none());
        owner.prepare(dir.path(), &record, &cancel).unwrap();
        let evidence = owner.evidence().unwrap().clone();
        let held = owner.held_lock().unwrap();
        assert_eq!(held.metadata().unwrap().nlink(), 0);
        assert!(!dir.path().join(LOCK).exists());
        // Open a separate file description of this exact anonymous inode. A
        // cloned File alone would share the same flock and would not test it.
        let competing = File::options()
            .read(true)
            .write(true)
            .open(format!("/proc/self/fd/{}", held.as_raw_fd()))
            .unwrap();
        assert!(fs2::FileExt::try_lock_exclusive(&competing).is_err());
        let mut bytes = Vec::new();
        (&competing).read_to_end(&mut bytes).unwrap();
        let marker: LockMarker = canonical(&bytes).unwrap();
        assert_eq!(marker.kind, LOCK_KIND);
        assert_eq!(marker.install_id, record.install_id);
        assert_eq!(
            marker.reservation_sha256,
            hash(&reservation_bytes(&record).unwrap())
        );
        assert_eq!(evidence.artifact.sha256, hash(&bytes));
        owner
            .prepare_link_until(&cancel, Instant::now() + std::time::Duration::from_secs(30))
            .unwrap()
            .publish()
            .unwrap();
        assert_eq!(owner.phase(), Some(PublicationPhase::Durable));
        assert_eq!(std::fs::read(dir.path().join(LOCK)).unwrap(), bytes);
        assert_eq!(
            identity(&File::open(dir.path().join(LOCK)).unwrap()).unwrap(),
            (evidence.file_device, evidence.file_inode)
        );
        assert!(
            startup_guard(
                dir.path(),
                &dir.path().join("catabolic.sqlite3"),
                owner.held_lock().unwrap()
            )
            .is_err()
        );
        assert!(owner.prepare(dir.path(), &record, &cancel).is_err());
    }
    #[test]
    fn marked_lock_sync_uncertainty_and_collision_retain_exact_fd_without_unlink_or_retry() {
        for collision in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let record = fresh_reservation(dir.path());
            let cancel = AtomicBool::new(false);
            let deadline = Instant::now() + std::time::Duration::from_secs(30);
            let mut owner = TargetLock::new();
            owner.prepare(dir.path(), &record, &cancel).unwrap();
            let exact = identity(owner.held_lock().unwrap()).unwrap();
            if collision {
                std::fs::write(dir.path().join(LOCK), b"unrelated").unwrap();
            } else {
                owner
                    .publication
                    .as_mut()
                    .unwrap()
                    .inject_parent_sync_failure();
            }
            assert!(
                owner
                    .prepare_link_until(&cancel, deadline)
                    .unwrap()
                    .publish()
                    .is_err()
            );
            assert_eq!(
                owner.phase(),
                Some(if collision {
                    PublicationPhase::Collision
                } else {
                    PublicationPhase::LinkedUnconfirmed
                })
            );
            assert_eq!(identity(owner.held_lock().unwrap()).unwrap(), exact);
            assert!(owner.prepare_link_until(&cancel, deadline).is_err());
            let named = File::options()
                .read(true)
                .write(true)
                .open(dir.path().join(LOCK))
                .unwrap();
            if collision {
                assert_ne!(identity(&named).unwrap(), exact);
                assert_eq!(std::fs::read(dir.path().join(LOCK)).unwrap(), b"unrelated");
            } else {
                assert_eq!(identity(&named).unwrap(), exact);
                assert!(fs2::FileExt::try_lock_exclusive(&named).is_err());
                assert!(
                    startup_guard(
                        dir.path(),
                        &dir.path().join("catabolic.sqlite3"),
                        owner.held_lock().unwrap()
                    )
                    .is_err()
                );
            }
            drop(owner);
            assert!(dir.path().join(LOCK).is_file());
        }
    }
    #[test]
    fn ordinary_empty_lock_only_directory_still_admits_but_nonempty_unknown_marker_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let lock = File::options()
            .create_new(true)
            .read(true)
            .write(true)
            .open(dir.path().join(LOCK))
            .unwrap();
        fs2::FileExt::try_lock_exclusive(&lock).unwrap();
        let native = dir.path().join("catabolic.sqlite3");
        // This necessarily also covers indistinguishable legacy empty restore
        // lock-only directories. New producers no longer create those states.
        startup_guard(dir.path(), &native, &lock).unwrap();
        for bytes in [b"{}".as_slice(), b"not a marker".as_slice()] {
            std::fs::write(dir.path().join(LOCK), bytes).unwrap();
            assert!(startup_guard(dir.path(), &native, &lock).is_err());
        }
        assert!(!native.exists());
    }
    #[test]
    fn marked_lock_requires_matching_canonical_reservation_and_complete_install() {
        for bad in [
            "missing",
            "malformed",
            "wrong-id",
            "wrong-pair",
            "noncanonical",
            "incomplete",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let record = fresh_reservation(dir.path());
            let cancel = AtomicBool::new(false);
            let mut owner = TargetLock::new();
            owner.prepare(dir.path(), &record, &cancel).unwrap();
            owner
                .prepare_link_until(&cancel, Instant::now() + std::time::Duration::from_secs(30))
                .unwrap()
                .publish()
                .unwrap();
            let mut bytes = reservation_bytes(&record).unwrap();
            match bad {
                "missing" => (),
                "malformed" => {
                    bytes = b"{}".to_vec();
                }
                "noncanonical" => bytes.push(b'\n'),
                "wrong-id" | "wrong-pair" => {
                    let mut altered: Reservation = canonical(&bytes).unwrap();
                    if bad == "wrong-id" {
                        altered.install_id = uuid::Uuid::new_v4().to_string();
                    } else {
                        altered.pair_sha256 = "b".repeat(64);
                    }
                    bytes = reservation_bytes(&altered).unwrap();
                }
                _ => (),
            }
            if bad != "missing" {
                std::fs::write(dir.path().join(RESERVATION), bytes).unwrap();
            }
            assert!(
                startup_guard(
                    dir.path(),
                    &dir.path().join("catabolic.sqlite3"),
                    owner.held_lock().unwrap()
                )
                .is_err(),
                "{bad}"
            );
            assert!(!dir.path().join("playscale.sqlite3").exists());
            assert!(!dir.path().join("catabolic.sqlite3").exists());
        }
        for bad in [
            "kind",
            "install-id",
            "reservation",
            "unknown-field",
            "oversized",
        ] {
            let (dir, lock, mut install) =
                fixture_with_images(b"motion-final", b"native-final", true);
            install.run(&AtomicBool::new(false), |_| Ok(())).unwrap();
            let native = dir.path().join("catabolic.sqlite3");
            startup_guard(dir.path(), &native, &lock).unwrap();
            let mut marker: LockMarker =
                canonical(&std::fs::read(dir.path().join(LOCK)).unwrap()).unwrap();
            match bad {
                "kind" => marker.kind = "different-producer".into(),
                "install-id" => marker.install_id = uuid::Uuid::new_v4().to_string(),
                "reservation" => marker.reservation_sha256 = "0".repeat(64),
                _ => (),
            }
            let mut bytes = serde_json::to_vec(&marker).unwrap();
            if bad == "unknown-field" {
                bytes.pop();
                bytes.extend_from_slice(b",\"unexpected\":true}");
            }
            if bad == "oversized" {
                bytes.resize(LIMIT as usize + 1, b' ');
            }
            std::fs::write(dir.path().join(LOCK), bytes).unwrap();
            assert!(startup_guard(dir.path(), &native, &lock).is_err(), "{bad}");
        }
    }
    #[tokio::test]
    async fn completed_marked_install_allows_real_later_sqlite_commits_without_historical_rehash() {
        use sqlx::Connection;
        let seeds = tempfile::tempdir().unwrap();
        let seed = seeds.path().join("seed.sqlite3");
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&seed)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Delete);
        let mut db = sqlx::SqliteConnection::connect_with(&options)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE application_state (id INTEGER PRIMARY KEY, value TEXT NOT NULL)")
            .execute(&mut db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO application_state VALUES (1, 'installed')")
            .execute(&mut db)
            .await
            .unwrap();
        db.close().await.unwrap();
        let image = std::fs::read(&seed).unwrap();
        // Real SQLite storage, still a journal-protocol fixture rather than an
        // invented native-schema or production restore authorization token.
        let (dir, lock, mut install) = fixture_with_images(&image, &image, true);
        install.run(&AtomicBool::new(false), |_| Ok(())).unwrap();
        let native = dir.path().join("catabolic.sqlite3");
        startup_guard(dir.path(), &native, &lock).unwrap();
        for leaf in ["playscale.sqlite3", "catabolic.sqlite3"] {
            let path = dir.path().join(leaf);
            let original_inode = identity(&File::open(&path).unwrap()).unwrap();
            let mut db = sqlx::SqliteConnection::connect_with(
                &sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&path)
                    .journal_mode(sqlx::sqlite::SqliteJournalMode::Delete),
            )
            .await
            .unwrap();
            sqlx::query("UPDATE application_state SET value='legitimate later write' WHERE id=1")
                .execute(&mut db)
                .await
                .unwrap();
            sqlx::query("INSERT INTO application_state VALUES (2, 'later insert')")
                .execute(&mut db)
                .await
                .unwrap();
            db.close().await.unwrap();
            assert_eq!(
                identity(&File::open(&path).unwrap()).unwrap(),
                original_inode
            );
            assert_ne!(std::fs::read(&path).unwrap(), image);
            startup_guard(dir.path(), &native, &lock).unwrap();
        }
    }
    #[test]
    fn durable_journal_precedes_images_and_completed_restart_allows_later_writes() {
        let (dir, lock, mut install) = fixture();
        let native = dir.path().join("catabolic.sqlite3");
        assert!(startup_guard(dir.path(), &native, &lock).is_err());
        install
            .run(&AtomicBool::new(false), |phase| {
                if phase == Phase::IntentDurable {
                    assert!(dir.path().join(INTENT).is_file());
                    assert!(!dir.path().join("playscale.sqlite3").exists());
                    assert!(!native.exists());
                    assert!(startup_guard(dir.path(), &native, &lock).is_err());
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(install.phase, Phase::Complete);
        startup_guard(dir.path(), &native, &lock).unwrap();
        // Hashes commit installation, not mutable operational database contents.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&native)
            .unwrap()
            .write_all(b"later-write")
            .unwrap();
        startup_guard(dir.path(), &native, &lock).unwrap();
    }
    #[test]
    fn crash_points_refuse_startup_and_do_not_blindly_retry_or_unlink() {
        for stop in [
            Phase::IntentDurable,
            Phase::MotionDurable,
            Phase::NativeDurable,
        ] {
            let (dir, lock, mut install) = fixture();
            let native = dir.path().join("catabolic.sqlite3");
            assert!(
                install
                    .run(&AtomicBool::new(false), |p| {
                        ensure!(p != stop, "injected crash boundary");
                        Ok(())
                    })
                    .is_err()
            );
            assert_eq!(install.phase, Phase::RecoveryRequired);
            assert!(startup_guard(dir.path(), &native, &lock).is_err());
            assert!(install.run(&AtomicBool::new(false), |_| Ok(())).is_err());
            drop(install);
            assert!(dir.path().join(INTENT).exists());
            assert_eq!(
                dir.path().join("playscale.sqlite3").exists(),
                stop != Phase::IntentDurable
            );
            assert_eq!(native.exists(), stop == Phase::NativeDurable);
        }
    }
    #[test]
    fn collision_symlink_corrupt_completion_and_inode_substitution_refuse() {
        let (dir, lock, mut install) = fixture();
        let native = dir.path().join("catabolic.sqlite3");
        std::os::unix::fs::symlink("missing", dir.path().join(INTENT)).unwrap();
        assert!(startup_guard(dir.path(), &native, &lock).is_err());
        assert!(install.run(&AtomicBool::new(false), |_| Ok(())).is_err());
        assert!(!native.exists());
        let (dir, lock, mut install) = fixture();
        let native = dir.path().join("catabolic.sqlite3");
        install.run(&AtomicBool::new(false), |_| Ok(())).unwrap();
        std::fs::rename(&native, dir.path().join("retained-original")).unwrap();
        std::fs::write(&native, b"replacement").unwrap();
        assert!(startup_guard(dir.path(), &native, &lock).is_err());
        std::fs::write(dir.path().join(COMPLETE), b"{}").unwrap();
        assert!(startup_guard(dir.path(), &native, &lock).is_err());
    }
    #[test]
    fn cancellation_and_post_link_unwind_retain_partial_journal_and_target_lock() {
        for panic in [false, true] {
            let (dir, lock, mut install) = fixture();
            drop(lock);
            let cancel = AtomicBool::new(false);
            assert!(
                install
                    .run(&cancel, |phase| {
                        if phase == Phase::MotionDurable && panic {
                            panic!("injected post-link owner panic");
                        }
                        if phase == Phase::IntentDurable && !panic {
                            cancel.store(true, std::sync::atomic::Ordering::Release);
                        }
                        Ok(())
                    })
                    .is_err()
            );
            assert_eq!(install.phase, Phase::RecoveryRequired);
            let competing = File::options()
                .read(true)
                .write(true)
                .open(dir.path().join("server.lock"))
                .unwrap();
            assert!(fs2::FileExt::try_lock_exclusive(&competing).is_err());
            assert!(dir.path().join(INTENT).exists());
            assert!(!dir.path().join(COMPLETE).exists());
            assert!(
                startup_guard(
                    dir.path(),
                    &dir.path().join("catabolic.sqlite3"),
                    &install.lock
                )
                .is_err()
            );
        }
    }
    #[test]
    fn final_sync_failure_reopens_exact_completed_records_without_rewriting_images() {
        let (dir, lock, mut install) = fixture();
        install.fail_completion_sync = true;
        assert!(install.run(&AtomicBool::new(false), |_| Ok(())).is_err());
        assert_eq!(install.phase, Phase::RecoveryRequired);
        assert_eq!(
            install.records.last().unwrap().phase(),
            PublicationPhase::LinkedUnconfirmed
        );
        let native = dir.path().join("catabolic.sqlite3");
        let old = std::fs::read(&native).unwrap();
        drop(install);
        drop(lock); // Simulated process-owner release, not a live retry.
        let lock = File::options()
            .read(true)
            .write(true)
            .open(dir.path().join("server.lock"))
            .unwrap();
        fs2::FileExt::try_lock_exclusive(&lock).unwrap();
        startup_guard(dir.path(), &native, &lock).unwrap();
        assert_eq!(std::fs::read(&native).unwrap(), old);
        std::fs::write(dir.path().join(MOTION_ACK), b"{}").unwrap();
        assert!(startup_guard(dir.path(), &native, &lock).is_err());
    }
    #[test]
    fn reservation_only_reopen_and_wrong_intent_linkage_refuse() {
        let (dir, lock, install) = fixture();
        let native = dir.path().join("catabolic.sqlite3");
        drop(install);
        drop(lock);
        let lock = File::options()
            .read(true)
            .write(true)
            .open(dir.path().join("server.lock"))
            .unwrap();
        fs2::FileExt::try_lock_exclusive(&lock).unwrap();
        assert!(startup_guard(dir.path(), &native, &lock).is_err());
        assert!(!native.exists());
        assert!(!dir.path().join("playscale.sqlite3").exists());
        let (dir, lock, mut install) = fixture();
        install.intent.reservation_sha256 = "0".repeat(64);
        install.intent_bytes = serde_json::to_vec(&install.intent).unwrap();
        assert!(install.run(&AtomicBool::new(false), |_| Ok(())).is_err());
        assert!(startup_guard(dir.path(), &dir.path().join("catabolic.sqlite3"), &lock).is_err());
    }
    #[test]
    fn rollback_journal_collision_refuses_before_intent_or_image_link() {
        for leaf in ["playscale.sqlite3-journal", "catabolic.sqlite3-journal"] {
            let (dir, _lock, mut install) = fixture();
            std::fs::write(dir.path().join(leaf), b"unrelated hot journal").unwrap();
            assert!(install.run(&AtomicBool::new(false), |_| Ok(())).is_err());
            assert!(!dir.path().join(INTENT).exists());
            assert!(!dir.path().join("playscale.sqlite3").exists());
            assert!(!dir.path().join("catabolic.sqlite3").exists());
            assert_eq!(
                std::fs::read(dir.path().join(leaf)).unwrap(),
                b"unrelated hot journal"
            );
        }
    }
}
