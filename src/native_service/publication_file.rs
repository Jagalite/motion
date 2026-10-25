//! Private Linux create-only descriptor publication. Pair composition supplies
//! independently verified bytes and retains stopped-service custody. This owner
//! conveys no restore authority; unlinkable database-export FDs remain unchanged.
use anyhow::ensure;
use playscale_core::paired_backup::{
    Artifact,
    publication::{Evidence, Input, Phase, Publication, valid_name},
};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

mod retained_preparation;
pub(super) use retained_preparation::{Preparation, PreparationPhase, PreparationStorage};
#[cfg(test)]
pub(super) use retained_preparation::{PreparationFault, PreparationPoint};

const MAX_CONTAINER: u64 = 128 * 1024 * 1024;
struct BoundedWriter<'a> {
    file: &'a mut File,
    bytes: u64,
}
impl Write for BoundedWriter<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self
            .bytes
            .checked_add(data.len() as u64)
            .is_none_or(|n| n > MAX_CONTAINER)
        {
            return Err(io::Error::other("pair container cohort exceeded"));
        }
        let n = self.file.write(data)?;
        self.bytes += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
pub(super) struct Sealed {
    parent: File,
    file: File,
    path: PathBuf,
    state: Publication,
}
pub(super) struct Outcome {
    owner: Sealed,
    error: Option<String>,
    preflight_started: bool,
    // Initial immutable evidence is revoked before handing the retained stage
    // to a SQLite mutation owner, including failed preparation/commit panic.
    mutable_stage: bool,
    #[cfg(test)]
    fail_parent_sync: bool,
    #[cfg(test)]
    panic_preflight: bool,
    #[cfg(test)]
    panic_link_attempt: bool,
}
/// A one-shot continuation borrowing the exact, already retained Outcome.
/// All filesystem observations, hashing and syscall argument preparation have
/// finished before this is returned. This is not an authorization grant.
pub(super) struct PreparedLink<'a> {
    outcome: &'a mut Outcome,
    proc: File,
    source: CString,
    target: CString,
    cancel: &'a AtomicBool,
    deadline: Option<Instant>,
}
fn id(file: &File) -> anyhow::Result<(u64, u64)> {
    let s = file.metadata()?;
    Ok((s.dev(), s.ino()))
}
fn open_at(parent: &File, name: &std::ffi::CStr, flags: i32) -> anyhow::Result<File> {
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    ensure!(
        fd >= 0,
        "descriptor open failed: {}",
        io::Error::last_os_error()
    );
    Ok(unsafe { File::from_raw_fd(fd) })
}
/// Pin genuine procfs and then the current process FD directory. The source FD
/// stays private/live through linkat, so its number cannot be recycled here.
/// Linux documents this unprivileged alternative to AT_EMPTY_PATH at
/// https://man7.org/linux/man-pages/man2/linkat.2.html . No capability changes.
fn proc_fds() -> anyhow::Result<File> {
    let proc = super::pair::open_parent(Path::new("/proc"))?;
    require_procfs(&proc)?;
    let pid = CString::new(std::process::id().to_string())?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let process = open_at(&proc, &pid, flags)?;
    require_procfs(&process)?;
    let fds = open_at(&process, c"fd", flags)?;
    require_procfs(&fds)?;
    Ok(fds)
}
fn require_procfs(file: &File) -> anyhow::Result<()> {
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    ensure!(
        unsafe { libc::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) } == 0,
        "procfs observation failed"
    );
    ensure!(
        unsafe { stat.assume_init() }.f_type == libc::PROC_SUPER_MAGIC,
        "FD publication requires genuine procfs"
    );
    Ok(())
}
/// Only the three fixed private95 publication resources may cross into the
/// backend descriptor verifier. This is not a generic descriptor escape.
#[derive(Clone, Copy)]
pub(super) enum RestorePublicationKind {
    Reservation,
    NativeArtifact,
    NativeArtifactRecord,
}
impl RestorePublicationKind {
    fn name(self) -> &'static str {
        match self {
            Self::Reservation => "paired-install.restore-reservation.json",
            Self::NativeArtifact => "paired-install.restore-native.sqlite3",
            Self::NativeArtifactRecord => "paired-install.restore-native.json",
        }
    }
}
impl Outcome {
    /// Prepare while the actual owner is already in its service-owned slot.
    /// The caller performs its genuine authorization check after this returns,
    /// then consumes the borrowed continuation without any intervening work.
    pub(super) fn prepare_link_until<'a>(
        &'a mut self,
        cancel: &'a AtomicBool,
        deadline: Instant,
    ) -> anyhow::Result<PreparedLink<'a>> {
        self.prepare_link(cancel, Some(deadline))
    }
    fn prepare_link<'a>(
        &'a mut self,
        cancel: &'a AtomicBool,
        deadline: Option<Instant>,
    ) -> anyhow::Result<PreparedLink<'a>> {
        ensure!(
            !self.preflight_started && self.phase() == Phase::Ready && self.error.is_none(),
            "publication preflight cannot be retried"
        );
        self.preflight_started = true;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            #[cfg(test)]
            assert!(!self.panic_preflight, "injected preflight panic");
            self.refuse_cancelled_or_expired(cancel, deadline)?;
            self.owner.current_parent()?;
            super::pair::verify_file(
                &mut self.owner.file,
                &self.owner.state.evidence().artifact,
                cancel,
            )?;
            let proc = proc_fds()?;
            let source = CString::new(self.owner.file.as_raw_fd().to_string())?;
            let observed = open_at(
                &proc,
                &source,
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )?;
            ensure!(
                id(&observed)? == id(&self.owner.file)?,
                "procfs FD does not name owned stage"
            );
            drop(observed);
            let target = CString::new(self.owner.state.evidence().name.as_bytes())?;
            // Parent checks also belong before, never after, caller authorization.
            self.owner.current_parent()?;
            Ok((proc, source, target))
        }));
        let (proc, source, target) = self.retain_result(result)?;
        Ok(PreparedLink {
            outcome: self,
            proc,
            source,
            target,
            cancel,
            deadline,
        })
    }
    fn refuse_cancelled_or_expired(
        &mut self,
        cancel: &AtomicBool,
        deadline: Option<Instant>,
    ) -> anyhow::Result<()> {
        if cancel.load(Ordering::Acquire) {
            self.owner.state.apply(Input::Cancel).unwrap();
            anyhow::bail!("publication cancelled before link");
        }
        if deadline.is_some_and(|until| Instant::now() >= until) {
            self.owner.state.apply(Input::Cancel).unwrap();
            anyhow::bail!("publication original deadline exhausted before link");
        }
        Ok(())
    }
    fn retain_result<T>(
        &mut self,
        result: std::thread::Result<anyhow::Result<T>>,
    ) -> anyhow::Result<T> {
        let result = result.unwrap_or_else(|_| {
            if self.phase() == Phase::LinkAttempted {
                // The syscall may already have completed. Never infer absence.
                let _ = self.owner.state.apply(Input::LinkOutcomeUnknown);
            }
            Err(anyhow::anyhow!(
                "publication owner panicked; retained exact visibility evidence"
            ))
        });
        if let Err(error) = &result {
            self.error = Some(format!("{error:#}"));
        }
        result
    }
    /// Fixed lock producer only. No descriptor ownership or write capability is
    /// returned; flock is applied to this actual retained publication inode.
    pub(super) fn lock_target_marker(&self) -> anyhow::Result<()> {
        ensure!(
            self.phase() == Phase::Ready
                && !self.preflight_started
                && self.evidence().name == "server.lock",
            "target lock requires unattempted anonymous marker"
        );
        fs2::FileExt::try_lock_exclusive(&self.owner.file)?;
        Ok(())
    }
    pub(super) fn target_marker_lock(&self) -> Option<&File> {
        (self.evidence().name == "server.lock").then_some(&self.owner.file)
    }
    fn sync_parent(&self) -> anyhow::Result<()> {
        #[cfg(test)]
        ensure!(
            !self.fail_parent_sync,
            "injected parent directory sync failure"
        );
        self.owner.parent.sync_all()?;
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn inject_parent_sync_failure(&mut self) {
        self.fail_parent_sync = true;
    }
    /// Pin a new READ-ONLY file descriptor for a fixed durable restore resource.
    /// The original Outcome stays retained by the server throughout the backend
    /// call, including errors. No private anonymous or mutable stage FD escapes.
    pub(super) fn restore_descriptors(
        &mut self,
        kind: RestorePublicationKind,
    ) -> anyhow::Result<(File, File)> {
        ensure!(
            self.phase() == Phase::Durable && self.evidence().name == kind.name(),
            "restore descriptor transfer requires exact durable resource"
        );
        self.owner.current_parent()?;
        self.owner.verify_linked()?;
        let file = open_at(
            &self.owner.parent,
            &CString::new(kind.name())?,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )?;
        ensure!(
            id(&file)? == (self.evidence().file_device, self.evidence().file_inode),
            "restore descriptor changed during transfer"
        );
        let parent = self.owner.parent.try_clone()?;
        self.owner.verify_linked()?;
        self.owner.current_parent()?;
        Ok((parent, file))
    }
    /// Pin only the already durable, exact service Motion conversion stage.
    /// Every returned clone is installed into the caller's stable service slots
    /// immediately, before another fallible action. Never reopen a named stage
    /// as authority or surrender the original Outcome on partial failure.
    pub(super) fn pin_service_control(
        &mut self,
        expected: &Evidence,
        file: &mut Option<File>,
        parent: &mut Option<File>,
    ) -> anyhow::Result<()> {
        ensure!(
            file.is_none() && parent.is_none(),
            "control pin cannot retry"
        );
        ensure!(
            self.phase() == Phase::Durable
                && !self.mutable_stage
                && self.evidence() == expected
                && expected.name.starts_with("paired-conversion-")
                && expected.name.ends_with(".motion.sqlite3"),
            "control pin requires its exact durable Motion stage"
        );
        self.owner.current_parent()?;
        self.owner.verify_linked()?;
        *file = Some(self.owner.file.try_clone()?);
        *parent = Some(self.owner.parent.try_clone()?);
        self.owner.current_parent()?;
        self.owner.verify_linked()?;
        self.mutable_stage = true;
        Ok(())
    }
    /// Service96 only: caller already owns native lifecycle/writer leases.
    /// Hash and identity are checked before revoking initial-byte authority.
    /// This is cooperative namespace exclusion, not atomic SQLite FD adoption.
    pub(super) fn pin_service_native(
        &mut self,
        expected: &Evidence,
        file: &mut Option<File>,
    ) -> anyhow::Result<()> {
        ensure!(file.is_none(), "native pin cannot retry");
        ensure!(
            self.phase() == Phase::Durable
                && !self.mutable_stage
                && self.evidence() == expected
                && expected.name.starts_with("paired-conversion-")
                && expected.name.ends_with(".native.sqlite3"),
            "native pin requires exact durable stage"
        );
        self.owner.current_parent()?;
        self.owner.verify_linked()?;
        *file = Some(self.owner.file.try_clone()?);
        self.owner.current_parent()?;
        self.owner.verify_linked()?;
        self.mutable_stage = true;
        Ok(())
    }
    /// Identity only after an explicitly owned SQLite mutation phase. Original
    /// artifact hashes remain initial evidence, never current-byte approval.
    pub(super) fn verify_mutable_stage_identity(&self) -> anyhow::Result<()> {
        ensure!(
            self.phase() == Phase::Durable,
            "conversion stage publication unresolved"
        );
        self.owner.current_parent()?;
        let evidence = self.evidence();
        let name = CString::new(evidence.name.as_bytes())?;
        let named = open_at(
            &self.owner.parent,
            &name,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )?;
        let held = self.owner.file.metadata()?;
        let current = named.metadata()?;
        ensure!(
            held.is_file()
                && current.is_file()
                && held.nlink() == 1
                && current.nlink() == 1
                && (held.dev(), held.ino()) == (evidence.file_device, evidence.file_inode)
                && (current.dev(), current.ino()) == (evidence.file_device, evidence.file_inode),
            "conversion stage named/held identity changed"
        );
        Ok(())
    }
    /// No descriptor, Write or seek capability escapes this exclusive borrow.
    /// Actual published bytes are verified both before and after the read.
    pub(super) fn read_verified<T>(
        &mut self,
        read: impl FnOnce(&mut dyn Read) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        ensure!(
            self.phase() == Phase::Durable && !self.mutable_stage,
            "pair publication immutable evidence unavailable after mutation claim"
        );
        self.owner.current_parent()?;
        self.owner.verify_linked()?;
        self.owner.file.seek(SeekFrom::Start(0))?;
        let result = read(&mut self.owner.file)?;
        self.owner.verify_linked()?;
        self.owner.current_parent()?;
        Ok(result)
    }
    pub(super) fn phase(&self) -> Phase {
        self.owner.state.phase()
    }
    pub(super) fn evidence(&self) -> &Evidence {
        self.owner.state.evidence()
    }
    #[cfg(test)]
    pub(super) fn original_record_fd(&self) -> std::os::fd::RawFd {
        self.owner.file.as_raw_fd()
    }
    pub(super) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}
impl Sealed {
    pub(super) fn evidence(&self) -> &Evidence {
        self.state.evidence()
    }
    fn create(
        path: &Path,
        name: &str,
        cancel: &AtomicBool,
        fill: impl FnOnce(&mut dyn Write) -> anyhow::Result<Artifact>,
    ) -> anyhow::Result<Self> {
        Self::create_checked(path, name, cancel, None, fill, |_| Ok(()))
    }
    pub(super) fn create_checked(
        path: &Path,
        name: &str,
        cancel: &AtomicBool,
        expected_parent: Option<(u64, u64)>,
        fill: impl FnOnce(&mut dyn Write) -> anyhow::Result<Artifact>,
        verify: impl FnOnce(&mut File) -> anyhow::Result<()>,
    ) -> anyhow::Result<Self> {
        ensure!(valid_name(name), "invalid pair artifact leaf");
        ensure!(
            !cancel.load(Ordering::Acquire),
            "publication cancelled before staging"
        );
        let parent = super::pair::open_parent(path)?;
        if let Some(expected) = expected_parent {
            ensure!(
                id(&parent)? == expected,
                "publication differs from verified pair parent"
            );
        }
        // This NEW stage is intentionally linkable. Existing native/Motion
        // O_TMPFILE|O_EXCL artifacts are not modified or adopted.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                c".".as_ptr(),
                libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC,
                0o600,
            )
        };
        ensure!(
            fd >= 0,
            "linkable anonymous stage unavailable: {}",
            io::Error::last_os_error()
        );
        let mut file = unsafe { File::from_raw_fd(fd) };
        let mut writer = BoundedWriter {
            file: &mut file,
            bytes: 0,
        };
        let artifact = fill(&mut writer)?;
        ensure!(
            writer.bytes == artifact.bytes,
            "container byte count mismatch"
        );
        drop(writer);
        super::pair::verify_file(&mut file, &artifact, cancel)?;
        file.seek(SeekFrom::Start(0))?;
        verify(&mut file)?;
        super::pair::verify_file(&mut file, &artifact, cancel)?;
        file.sync_all()?;
        let (parent_device, parent_inode) = id(&parent)?;
        let (file_device, file_inode) = id(&file)?;
        let state = Publication::prepared(Evidence {
            parent_device,
            parent_inode,
            file_device,
            file_inode,
            name: name.into(),
            artifact,
        })
        .map_err(|e| anyhow::anyhow!("invalid prepared publication: {e:?}"))?;
        Ok(Self {
            parent,
            file,
            path: path.to_owned(),
            state,
        })
    }
    fn current_parent(&self) -> anyhow::Result<()> {
        ensure!(
            id(&super::pair::open_parent(&self.path)?)? == id(&self.parent)?,
            "publication parent replaced"
        );
        Ok(())
    }
    fn verify_linked(&mut self) -> anyhow::Result<()> {
        let target = CString::new(self.state.evidence().name.as_bytes())?;
        let named = open_at(
            &self.parent,
            &target,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )?;
        ensure!(
            id(&named)? == id(&self.file)?,
            "published name no longer identifies owned inode"
        );
        let stat = self.file.metadata()?;
        let artifact = &self.state.evidence().artifact;
        ensure!(
            stat.is_file() && stat.nlink() == 1 && stat.len() == artifact.bytes,
            "published inode changed"
        );
        self.file.seek(SeekFrom::Start(0))?;
        let mut digest = Sha256::new();
        let mut total = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            let n = self.file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            ensure!(total <= artifact.bytes, "published inode grew");
            digest.update(&buffer[..n]);
        }
        ensure!(
            total == artifact.bytes && format!("{:x}", digest.finalize()) == artifact.sha256,
            "published inode digest changed"
        );
        Ok(())
    }
    pub(super) fn publish(
        self,
        cancel: &AtomicBool,
        after_link: impl FnOnce() -> anyhow::Result<()>,
    ) -> Outcome {
        self.publish_inner(cancel, None, after_link)
    }
    /// Fixed reservation deadline, never a caller-supplied authorization hook.
    /// Refuse before link if the original seal expired; after a successful link
    /// always finish the existing durability path, even if it expires meanwhile.
    pub(super) fn publish_until(
        self,
        cancel: &AtomicBool,
        deadline: Instant,
        after_link: impl FnOnce() -> anyhow::Result<()>,
    ) -> Outcome {
        self.publish_inner(cancel, Some(deadline), after_link)
    }
    fn publish_inner(
        self,
        cancel: &AtomicBool,
        deadline: Option<Instant>,
        after_link: impl FnOnce() -> anyhow::Result<()>,
    ) -> Outcome {
        // Preserve the convenience publisher's ownership/result semantics. The
        // authorization-sensitive path uses retain + the fixed borrowed seam.
        let mut outcome = self.retain();
        // Keep even destruction of an unused legacy callback inside the unwind
        // boundary while the actual owner is still outside it and retained.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            outcome
                .prepare_link(cancel, deadline)?
                .publish_inner(after_link)
        }));
        let _ = outcome.retain_result(result);
        outcome
    }
    /// Move the sealed anonymous file into terminal custody before preflight.
    pub(super) fn retain(self) -> Outcome {
        Outcome {
            owner: self,
            error: None,
            preflight_started: false,
            mutable_stage: false,
            #[cfg(test)]
            fail_parent_sync: false,
            #[cfg(test)]
            panic_preflight: false,
            #[cfg(test)]
            panic_link_attempt: false,
        }
    }
}
impl PreparedLink<'_> {
    /// No callbacks, new authorization, ownership transfer, hashing, descriptor
    /// resolution or pathname preparation occur before this one create-link.
    pub(super) fn publish(self) -> anyhow::Result<()> {
        self.publish_inner(|| Ok(()))
    }
    fn publish_inner(self, after_link: impl FnOnce() -> anyhow::Result<()>) -> anyhow::Result<()> {
        let outcome = &mut *self.outcome;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            outcome.refuse_cancelled_or_expired(self.cancel, self.deadline)?;
            outcome.owner.state.apply(Input::BeginLink).unwrap();
            #[cfg(test)]
            assert!(!outcome.panic_link_attempt, "injected link-attempt panic");
            let rc = unsafe {
                libc::linkat(
                    self.proc.as_raw_fd(),
                    self.source.as_ptr(),
                    outcome.owner.parent.as_raw_fd(),
                    self.target.as_ptr(),
                    libc::AT_SYMLINK_FOLLOW,
                )
            };
            if rc != 0 {
                let error = io::Error::last_os_error();
                outcome
                    .owner
                    .state
                    .apply(if error.raw_os_error() == Some(libc::EEXIST) {
                        Input::DestinationAlreadyExists
                    } else {
                        Input::LinkOutcomeUnknown
                    })
                    .unwrap();
                return Err(error.into());
            }
            outcome.owner.state.apply(Input::LinkSucceeded).unwrap();
            // Once linked, cancellation/expiry cannot claim absence or skip
            // durability. Any error or unwind leaves the exact owner retained.
            if let Err(error) = after_link()
                .and_then(|_| outcome.owner.verify_linked())
                .and_then(|_| outcome.sync_parent())
                .and_then(|_| outcome.owner.current_parent())
            {
                outcome
                    .owner
                    .state
                    .apply(Input::DirectorySyncFailed)
                    .unwrap();
                return Err(error);
            }
            outcome.owner.state.apply(Input::DirectorySynced).unwrap();
            Ok(())
        }));
        outcome.retain_result(result)
    }
}
impl Drop for PreparedLink<'_> {
    fn drop(&mut self) {
        // Dropping an unauthorised/unused continuation is terminal. Pure state
        // only: no allocation, IO, unlink, retry or ownership transfer in Drop.
        if self.outcome.phase() == Phase::Ready {
            let _ = self.outcome.owner.state.apply(Input::Cancel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn stage(path: &Path, name: &str) -> Sealed {
        Sealed::create(path, name, &AtomicBool::new(false), |out| {
            out.write_all(b"verified-container")?;
            Ok(Artifact {
                bytes: 18,
                sha256: catabolic_core::sha256(b"verified-container"),
            })
        })
        .unwrap()
    }
    #[test]
    fn retained_preflight_is_one_shot_and_dropped_continuation_abandons_only_anonymous_stage() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let deadline = Instant::now() + std::time::Duration::from_secs(30);
        let mut outcome = stage(dir.path(), "prepared.bundle").retain();
        let original = outcome.evidence().clone();
        let fd = outcome.original_record_fd();
        let prepared = outcome.prepare_link_until(&cancel, deadline).unwrap();
        assert!(!dir.path().join("prepared.bundle").exists());
        drop(prepared); // Caller authorization refused; no link may follow.
        assert_eq!(outcome.phase(), Phase::Abandoned);
        assert_eq!(outcome.original_record_fd(), fd);
        assert_eq!(outcome.evidence(), &original);
        assert_eq!(outcome.owner.file.metadata().unwrap().nlink(), 0);
        assert!(outcome.prepare_link_until(&cancel, deadline).is_err());
    }
    #[test]
    fn retained_fixed_link_rechecks_cancel_and_original_deadline_after_preflight() {
        for expired in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let cancel = AtomicBool::new(false);
            let deadline = Instant::now() + std::time::Duration::from_millis(100);
            let mut outcome = stage(dir.path(), "fence.bundle").retain();
            let original = outcome.evidence().clone();
            let prepared = outcome.prepare_link_until(&cancel, deadline).unwrap();
            if expired {
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            } else {
                cancel.store(true, Ordering::Release);
            }
            assert!(prepared.publish().is_err());
            assert_eq!(outcome.phase(), Phase::Abandoned);
            assert_eq!(outcome.evidence(), &original);
            assert_eq!(outcome.owner.file.metadata().unwrap().nlink(), 0);
            assert!(!dir.path().join("fence.bundle").exists());
        }
    }
    #[test]
    fn retained_link_and_parent_sync_uncertainty_keep_exact_owner_without_retry() {
        for fail_link in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let cancel = AtomicBool::new(false);
            let deadline = Instant::now() + std::time::Duration::from_secs(30);
            let mut outcome = stage(dir.path(), "uncertain.bundle").retain();
            let original = outcome.evidence().clone();
            let fd = outcome.original_record_fd();
            if !fail_link {
                outcome.inject_parent_sync_failure();
            }
            let mut prepared = outcome.prepare_link_until(&cancel, deadline).unwrap();
            if fail_link {
                // Private test-only corruption of opaque preflight output:
                // actual linkat reports failure, never a guessed clean absence.
                prepared.source = CString::new("not-an-open-fd").unwrap();
            }
            assert!(prepared.publish().is_err());
            assert_eq!(
                outcome.phase(),
                if fail_link {
                    Phase::OutcomeUnknown
                } else {
                    Phase::LinkedUnconfirmed
                }
            );
            assert_eq!(outcome.evidence(), &original);
            assert_eq!(outcome.original_record_fd(), fd);
            assert!(outcome.error().is_some());
            assert!(outcome.prepare_link_until(&cancel, deadline).is_err());
            if !fail_link {
                assert_eq!(
                    id(&File::open(dir.path().join("uncertain.bundle")).unwrap()).unwrap(),
                    (original.file_device, original.file_inode)
                );
            }
        }
    }
    #[test]
    fn retained_preflight_failure_and_postlink_panic_do_not_drop_exact_owner() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let deadline = Instant::now() + std::time::Duration::from_secs(30);
        let mut failed = stage(dir.path(), "corrupt.bundle").retain();
        let original = failed.evidence().clone();
        failed.owner.file.seek(SeekFrom::Start(0)).unwrap();
        failed.owner.file.write_all(b"corrupt").unwrap();
        assert!(failed.prepare_link_until(&cancel, deadline).is_err());
        assert_eq!(failed.evidence(), &original);
        assert_eq!(failed.owner.file.metadata().unwrap().nlink(), 0);
        assert!(failed.prepare_link_until(&cancel, deadline).is_err());
        let mut panicked = stage(dir.path(), "panic.bundle").retain();
        let original = panicked.evidence().clone();
        assert!(
            panicked
                .prepare_link_until(&cancel, deadline)
                .unwrap()
                .publish_inner(|| panic!("postlink panic with retained Outcome"))
                .is_err()
        );
        assert_eq!(panicked.phase(), Phase::LinkedUnconfirmed);
        assert_eq!(panicked.evidence(), &original);
        assert_eq!(
            id(&panicked.owner.file).unwrap(),
            (original.file_device, original.file_inode)
        );
        assert!(panicked.error().unwrap().contains("panicked"));
        assert!(dir.path().join("panic.bundle").exists());
    }
    #[test]
    fn retained_preflight_and_link_attempt_panics_keep_owner_and_forbid_retry() {
        for during_link in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let cancel = AtomicBool::new(false);
            let deadline = Instant::now() + std::time::Duration::from_secs(30);
            let mut outcome = stage(dir.path(), "panic-stage.bundle").retain();
            let original = outcome.evidence().clone();
            outcome.panic_preflight = !during_link;
            outcome.panic_link_attempt = during_link;
            if during_link {
                assert!(
                    outcome
                        .prepare_link_until(&cancel, deadline)
                        .unwrap()
                        .publish()
                        .is_err()
                );
                assert_eq!(outcome.phase(), Phase::OutcomeUnknown);
            } else {
                assert!(outcome.prepare_link_until(&cancel, deadline).is_err());
                assert_eq!(outcome.phase(), Phase::Ready);
            }
            assert_eq!(outcome.evidence(), &original);
            assert_eq!(
                id(&outcome.owner.file).unwrap(),
                (original.file_device, original.file_inode)
            );
            assert!(outcome.error().unwrap().contains("panicked"));
            assert!(outcome.prepare_link_until(&cancel, deadline).is_err());
        }
    }
    #[test]
    fn legacy_unused_callback_drop_panic_still_returns_retained_owner() {
        struct PanicOnDrop;
        impl Drop for PanicOnDrop {
            fn drop(&mut self) {
                panic!("unused legacy callback destructor");
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let sealed = stage(dir.path(), "legacy-panic.bundle");
        let exact = sealed.evidence().clone();
        let bomb = PanicOnDrop;
        let outcome = sealed.publish(&AtomicBool::new(true), move || {
            drop(bomb);
            Ok(())
        });
        assert_eq!(outcome.phase(), Phase::Abandoned);
        assert_eq!(outcome.evidence(), &exact);
        assert_eq!(
            id(&outcome.owner.file).unwrap(),
            (exact.file_device, exact.file_inode)
        );
        assert!(outcome.error().unwrap().contains("panicked"));
        assert!(!dir.path().join("legacy-panic.bundle").exists());
    }
    #[test]
    fn restore_descriptor_bridge_is_fixed_and_os_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut outcome = stage(dir.path(), RestorePublicationKind::Reservation.name())
            .publish(&AtomicBool::new(false), || Ok(()));
        let (parent, mut file) = outcome
            .restore_descriptors(RestorePublicationKind::Reservation)
            .unwrap();
        assert_eq!(
            id(&parent).unwrap(),
            id(&File::open(dir.path()).unwrap()).unwrap()
        );
        assert!(file.write_all(b"overwrite").is_err());
        assert!(
            outcome
                .restore_descriptors(RestorePublicationKind::NativeArtifact)
                .is_err()
        );
        assert_eq!(
            std::fs::read(dir.path().join(RestorePublicationKind::Reservation.name())).unwrap(),
            b"verified-container"
        );
    }
    #[test]
    fn restore_descriptor_bridge_refuses_uncertain_link_and_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let mut uncertain = stage(dir.path(), RestorePublicationKind::NativeArtifact.name())
            .publish(&AtomicBool::new(false), || {
                anyhow::bail!("injected post-link failure")
            });
        assert!(
            uncertain
                .restore_descriptors(RestorePublicationKind::NativeArtifact)
                .is_err()
        );
        let mut record = stage(
            dir.path(),
            RestorePublicationKind::NativeArtifactRecord.name(),
        )
        .publish(&AtomicBool::new(false), || Ok(()));
        std::fs::rename(
            dir.path()
                .join(RestorePublicationKind::NativeArtifactRecord.name()),
            dir.path().join("retained-record"),
        )
        .unwrap();
        std::fs::write(
            dir.path()
                .join(RestorePublicationKind::NativeArtifactRecord.name()),
            b"unrelated",
        )
        .unwrap();
        assert!(
            record
                .restore_descriptors(RestorePublicationKind::NativeArtifactRecord)
                .is_err()
        );
        assert_eq!(
            std::fs::read(
                dir.path()
                    .join(RestorePublicationKind::NativeArtifactRecord.name())
            )
            .unwrap(),
            b"unrelated"
        );
    }
    #[test]
    fn mutable_stage_identity_never_reinterprets_initial_hash_or_adopts_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let mut outcome = stage(dir.path(), "stage.db").publish(&AtomicBool::new(false), || Ok(()));
        assert!(outcome.error.is_none());
        let original = outcome.evidence().clone();
        outcome.verify_mutable_stage_identity().unwrap();
        std::fs::write(
            dir.path().join("stage.db"),
            b"legitimate phase-owned new bytes",
        )
        .unwrap();
        outcome.verify_mutable_stage_identity().unwrap();
        assert!(outcome.read_verified(|_| Ok(())).is_err());
        assert_eq!(outcome.evidence().artifact.sha256, original.artifact.sha256);
        std::fs::rename(dir.path().join("stage.db"), dir.path().join("held-old.db")).unwrap();
        std::fs::write(dir.path().join("stage.db"), b"unrelated same name").unwrap();
        assert!(outcome.verify_mutable_stage_identity().is_err());
        assert_eq!(
            std::fs::read(dir.path().join("stage.db")).unwrap(),
            b"unrelated same name"
        );
    }
    #[test]
    fn actual_descriptor_create_only_link_is_visible_once_and_fsynced() {
        let dir = tempfile::tempdir().unwrap();
        let s = stage(dir.path(), "one.bundle");
        let exact = s.state.evidence().clone();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        let result = s.publish(&AtomicBool::new(false), || Ok(()));
        assert!(result.error.is_none(), "{:?}", result.error);
        assert_eq!(result.owner.state.phase(), Phase::Durable);
        let named = File::open(dir.path().join("one.bundle")).unwrap();
        assert_eq!(id(&named).unwrap(), (exact.file_device, exact.file_inode));
        assert_eq!(
            std::fs::read(dir.path().join("one.bundle")).unwrap(),
            b"verified-container"
        );
        let collision = stage(dir.path(), "one.bundle").publish(&AtomicBool::new(false), || Ok(()));
        assert_eq!(collision.owner.state.phase(), Phase::Collision);
        assert_eq!(id(&named).unwrap(), (exact.file_device, exact.file_inode));
    }
    #[test]
    fn original_deadline_refuses_before_link_and_retains_exact_stage() {
        let dir = tempfile::tempdir().unwrap();
        let sealed = stage(dir.path(), RestorePublicationKind::Reservation.name());
        let original = sealed.evidence().clone();
        let result = sealed.publish_until(&AtomicBool::new(false), Instant::now(), || {
            panic!("expired publication must never reach after-link work")
        });
        assert_eq!(result.phase(), Phase::Abandoned);
        assert!(result.error().unwrap().contains("original deadline"));
        assert_eq!(result.evidence(), &original);
        assert_eq!(
            id(&result.owner.file).unwrap(),
            (original.file_device, original.file_inode)
        );
        assert_eq!(result.owner.file.metadata().unwrap().nlink(), 0);
        assert!(
            !dir.path()
                .join(RestorePublicationKind::Reservation.name())
                .exists()
        );
    }
    #[test]
    fn expiry_after_link_does_not_skip_directory_durability() {
        let dir = tempfile::tempdir().unwrap();
        let sealed = stage(dir.path(), RestorePublicationKind::Reservation.name());
        let deadline = Instant::now() + std::time::Duration::from_secs(1);
        let result = sealed.publish_until(&AtomicBool::new(false), deadline, || {
            // Use the actual original deadline, not a second/new interval.
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            Ok(())
        });
        assert!(Instant::now() >= deadline);
        assert_eq!(result.phase(), Phase::Durable);
        assert!(result.error().is_none());
        assert_eq!(
            std::fs::read(dir.path().join(RestorePublicationKind::Reservation.name())).unwrap(),
            b"verified-container"
        );
        // The reservation wrapper separately refuses the expired continuation
        // while retaining this durable Outcome, never pretending it is absent.
    }
    #[test]
    fn prelink_cancel_and_parent_replacement_never_adopt_or_remove_names() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("parent");
        std::fs::create_dir(&parent).unwrap();
        let cancelled = stage(&parent, "cancel.bundle").publish(&AtomicBool::new(true), || Ok(()));
        assert_eq!(cancelled.owner.state.phase(), Phase::Abandoned);
        drop(cancelled);
        let s = stage(&parent, "pair.bundle");
        std::fs::rename(&parent, dir.path().join("old")).unwrap();
        std::fs::create_dir(&parent).unwrap();
        std::fs::write(parent.join("unowned"), b"keep").unwrap();
        let result = s.publish(&AtomicBool::new(false), || Ok(()));
        assert!(result.error.is_some());
        assert_eq!(result.owner.state.phase(), Phase::Ready);
        drop(result);
        assert_eq!(std::fs::read(parent.join("unowned")).unwrap(), b"keep");
        assert_eq!(
            std::fs::read_dir(dir.path().join("old")).unwrap().count(),
            0
        );
    }
    #[test]
    fn postlink_failure_retains_visible_identity_and_never_unlinks() {
        let dir = tempfile::tempdir().unwrap();
        let s = stage(dir.path(), "pair.bundle");
        let evidence = s.state.evidence().clone();
        let result = s.publish(&AtomicBool::new(false), || {
            anyhow::bail!("injected directory sync failure")
        });
        assert_eq!(result.owner.state.phase(), Phase::LinkedUnconfirmed);
        assert!(result.error.is_some());
        assert_eq!(result.owner.state.evidence(), &evidence);
        drop(result);
        assert_eq!(
            std::fs::read(dir.path().join("pair.bundle")).unwrap(),
            b"verified-container"
        );
    }
    #[test]
    fn postlink_cancellation_finishes_durability_but_replacement_is_uncertain() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let result = stage(dir.path(), "cancel.bundle").publish(&cancel, || {
            cancel.store(true, Ordering::Release);
            Ok(())
        });
        assert_eq!(result.owner.state.phase(), Phase::Durable);
        let replacement =
            stage(dir.path(), "replace.bundle").publish(&AtomicBool::new(false), || {
                std::fs::rename(
                    dir.path().join("replace.bundle"),
                    dir.path().join("owned-moved"),
                )?;
                std::fs::write(dir.path().join("replace.bundle"), b"unowned")?;
                Ok(())
            });
        assert!(replacement.error.is_some());
        assert_eq!(replacement.owner.state.phase(), Phase::LinkedUnconfirmed);
        drop(replacement);
        assert_eq!(
            std::fs::read(dir.path().join("replace.bundle")).unwrap(),
            b"unowned"
        );
        assert_eq!(
            std::fs::read(dir.path().join("owned-moved")).unwrap(),
            b"verified-container"
        );
        assert!(require_procfs(&super::super::pair::open_parent(dir.path()).unwrap()).is_err());
    }
    #[test]
    fn symlink_collision_is_not_followed_and_bad_bytes_never_link() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("victim"), b"keep").unwrap();
        std::os::unix::fs::symlink("victim", dir.path().join("pair.bundle")).unwrap();
        let result = stage(dir.path(), "pair.bundle").publish(&AtomicBool::new(false), || Ok(()));
        assert_eq!(result.owner.state.phase(), Phase::Collision);
        assert_eq!(std::fs::read(dir.path().join("victim")).unwrap(), b"keep");
        assert!(
            Sealed::create(dir.path(), "bad.bundle", &AtomicBool::new(false), |out| {
                out.write_all(b"bad")?;
                Ok(Artifact {
                    bytes: 3,
                    sha256: "0".repeat(64),
                })
            })
            .is_err()
        );
        assert!(!dir.path().join("bad.bundle").exists());
    }
}
