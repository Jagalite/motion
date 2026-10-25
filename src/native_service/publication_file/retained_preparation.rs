//! Fixed anonymous-stage preparation for a service-owned slot. This is custody,
//! not publication or authorization: no named effect occurs here. The existing
//! core Publication remains the sole visibility/durability state machine.
use super::*;
use std::os::unix::ffi::OsStrExt;

const PATH_BYTES: usize = libc::PATH_MAX as usize;
const NAME_BYTES: usize = 200;
const DIGEST_BYTES: usize = 64;
const LEAF_BYTES: usize = NAME_BYTES + 9 + 1;
const COPY_BYTES: usize = 64 * 1024;
pub(in crate::native_service) const RECORD_BYTES: usize = 16 * 1024;

/// Named logical storage, not allocator overhead, kernel FD memory or RSS.
/// Record bytes belong to the caller's retained encoding slot; include that
/// share here so admission precedes encoding as well as preparation. Transient
/// work includes the existing Outcome preflight's 64KiB hash buffer, bounded
/// path/procfs arguments, digest state, metadata and synchronous call frames.
#[derive(Clone, Copy, Debug)]
pub(in crate::native_service) struct PreparationStorage {
    pub(in crate::native_service) owners: u64,
    pub(in crate::native_service) parent_descriptor: u64,
    pub(in crate::native_service) stage_descriptor: u64,
    pub(in crate::native_service) path: u64,
    pub(in crate::native_service) name: u64,
    pub(in crate::native_service) digest: u64,
    pub(in crate::native_service) sidecar_leaf: u64,
    pub(in crate::native_service) copy_hash_buffer: u64,
    pub(in crate::native_service) record_bytes: u64,
    pub(in crate::native_service) transient_work: u64,
}
impl PreparationStorage {
    pub(in crate::native_service) const fn total(self) -> u64 {
        self.owners
            + self.parent_descriptor
            + self.stage_descriptor
            + self.path
            + self.name
            + self.digest
            + self.sidecar_leaf
            + self.copy_hash_buffer
            + self.record_bytes
            + self.transient_work
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::native_service) enum PreparationPhase {
    Inputs,
    Failed,
    Ready,
    Finished,
}

/// Install this owner in the actual ServiceTarget before calling either fixed
/// prepare method. Neither an error nor an unwind consumes this owner. Every
/// returned parent/stage File is installed before the next fallible operation.
pub(in crate::native_service) struct Preparation {
    phase: PreparationPhase,
    parent: Option<File>,
    file: Option<File>,
    path: Option<PathBuf>,
    name: Option<String>,
    digest: String,
    buffer: Vec<u8>,
    leaf: Vec<u8>,
    publication: Option<Publication>,
    parent_identity: Option<(u64, u64)>,
    file_identity: Option<(u64, u64)>,
    #[cfg(test)]
    fault: Option<(PreparationPoint, PreparationFault)>,
    #[cfg(test)]
    fault_fds: Option<(Option<std::os::fd::RawFd>, Option<std::os::fd::RawFd>)>,
}

impl Preparation {
    pub(in crate::native_service) const fn storage_layout() -> PreparationStorage {
        PreparationStorage {
            // Conservative construction/move overlap, including eventual
            // Outcome layout. Descriptor shares below are named separately.
            owners: 2 * std::mem::size_of::<Self>() as u64
                + 2 * std::mem::size_of::<Outcome>() as u64,
            parent_descriptor: std::mem::size_of::<File>() as u64,
            stage_descriptor: std::mem::size_of::<File>() as u64,
            path: PATH_BYTES as u64,
            name: NAME_BYTES as u64,
            digest: DIGEST_BYTES as u64,
            sidecar_leaf: LEAF_BYTES as u64,
            copy_hash_buffer: COPY_BYTES as u64,
            record_bytes: RECORD_BYTES as u64,
            transient_work: 96 * 1024,
        }
    }
    pub(in crate::native_service) const fn storage_reservation() -> u64 {
        Self::storage_layout().total()
    }
    /// Bounded allocation only, with no filesystem observation or effect. The
    /// caller admits storage_reservation() before constructing this owner.
    pub(in crate::native_service) fn new(path: &Path, name: &str) -> anyhow::Result<Self> {
        ensure!(
            path.is_absolute() && path.as_os_str().as_bytes().len() <= PATH_BYTES,
            "retained preparation path exceeds absolute path share"
        );
        ensure!(valid_name(name), "invalid retained preparation leaf");
        let path = path.to_owned();
        ensure!(
            path.capacity() <= PATH_BYTES,
            "preparation path capacity exceeded"
        );
        let mut retained_name = String::with_capacity(NAME_BYTES);
        retained_name.push_str(name);
        Ok(Self {
            phase: PreparationPhase::Inputs,
            parent: None,
            file: None,
            path: Some(path),
            name: Some(retained_name),
            digest: String::with_capacity(DIGEST_BYTES),
            buffer: vec![0; COPY_BYTES],
            leaf: Vec::with_capacity(LEAF_BYTES),
            publication: None,
            parent_identity: None,
            file_identity: None,
            #[cfg(test)]
            fault: None,
            #[cfg(test)]
            fault_fds: None,
        })
    }
    pub(in crate::native_service) fn phase(&self) -> PreparationPhase {
        self.phase
    }
    fn begin(&mut self) -> anyhow::Result<()> {
        ensure!(
            self.phase == PreparationPhase::Inputs,
            "retained preparation cannot be retried"
        );
        // Pessimistic and terminal before any IO/hash work. Unwind propagates
        // to the service boundary without allocating or removing either FD.
        self.phase = PreparationPhase::Failed;
        Ok(())
    }
    /// Copy only the selected, already retained anonymous image descriptor and
    /// exact artifact. No source pathname, connection or effect callback exists.
    pub(in crate::native_service) fn prepare_image(
        &mut self,
        parent: &File,
        expected_parent: (u64, u64),
        source: &mut File,
        expected: &Artifact,
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> anyhow::Result<()> {
        self.begin()?;
        {
            checkpoint(cancel, deadline)?;
            ensure!(
                expected.bytes > 0
                    && expected.bytes <= MAX_CONTAINER
                    && expected.sha256.len() == DIGEST_BYTES
                    && expected
                        .sha256
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "invalid retained image artifact"
            );
            self.digest.push_str(&expected.sha256);
            self.acquire(parent, expected_parent, true, cancel, deadline)?;
            verify_source(source, expected, &mut self.buffer, cancel, deadline)?;
            checkpoint(cancel, deadline)?;
            source.seek(SeekFrom::Start(0))?;
            checkpoint(cancel, deadline)?;
            let mut copied = 0u64;
            loop {
                checkpoint(cancel, deadline)?;
                let count = source.read(&mut self.buffer)?;
                checkpoint(cancel, deadline)?;
                if count == 0 {
                    break;
                }
                copied = copied
                    .checked_add(count as u64)
                    .ok_or_else(|| anyhow::anyhow!("retained image length overflow"))?;
                ensure!(copied <= expected.bytes, "retained image source grew");
                self.write_buffer(count, cancel, deadline)?;
            }
            ensure!(copied == expected.bytes, "retained image source truncated");
            self.seal(expected.bytes, cancel, deadline)?;
        }
        self.phase = PreparationPhase::Ready;
        Ok(())
    }
    /// Only a bounded, already encoded service record is accepted. The caller
    /// retains both the record and encoded bytes before this fixed operation.
    pub(in crate::native_service) fn prepare_record(
        &mut self,
        parent: &File,
        expected_parent: (u64, u64),
        record: &[u8],
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> anyhow::Result<()> {
        self.begin()?;
        {
            checkpoint(cancel, deadline)?;
            ensure!(
                !record.is_empty() && record.len() <= RECORD_BYTES,
                "retained record exceeds nonempty 16KiB bound"
            );
            self.acquire(parent, expected_parent, false, cancel, deadline)?;
            checkpoint(cancel, deadline)?;
            for byte in Sha256::digest(record) {
                self.digest.push(HEX[(byte >> 4) as usize] as char);
                self.digest.push(HEX[(byte & 15) as usize] as char);
            }
            checkpoint(cancel, deadline)?;
            self.buffer[..record.len()].copy_from_slice(record);
            self.write_buffer(record.len(), cancel, deadline)?;
            self.seal(record.len() as u64, cancel, deadline)?;
        }
        self.phase = PreparationPhase::Ready;
        Ok(())
    }
    fn acquire(
        &mut self,
        parent: &File,
        expected_parent: (u64, u64),
        image: bool,
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> anyhow::Result<()> {
        checkpoint(cancel, deadline)?;
        self.parent = Some(parent.try_clone()?);
        self.boundary(PreparationPoint::ParentAcquired, cancel, deadline)?;
        let actual = id(self.parent.as_ref().unwrap())?;
        checkpoint(cancel, deadline)?;
        ensure!(
            actual == expected_parent,
            "retained target parent identity changed"
        );
        self.parent_identity = Some(actual);
        if image {
            for suffix in ["", "-wal", "-shm", "-journal"] {
                checkpoint(cancel, deadline)?;
                self.leaf.clear();
                self.leaf
                    .extend_from_slice(self.name.as_ref().unwrap().as_bytes());
                self.leaf.extend_from_slice(suffix.as_bytes());
                self.leaf.push(0);
                let name = std::ffi::CStr::from_bytes_with_nul(&self.leaf)?;
                let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
                let rc = unsafe {
                    libc::fstatat(
                        self.parent.as_ref().unwrap().as_raw_fd(),
                        name.as_ptr(),
                        stat.as_mut_ptr(),
                        libc::AT_SYMLINK_NOFOLLOW,
                    )
                };
                let absent =
                    rc < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT);
                checkpoint(cancel, deadline)?;
                ensure!(
                    absent,
                    "retained image stage or SQLite sidecar already exists"
                );
            }
        }
        checkpoint(cancel, deadline)?;
        // This NEW file alone is linkable. The source image's O_EXCL anonymous
        // descriptor is never adopted, reopened for writing or published.
        let fd = unsafe {
            libc::openat(
                self.parent.as_ref().unwrap().as_raw_fd(),
                c".".as_ptr(),
                libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC,
                0o600,
            )
        };
        ensure!(
            fd >= 0,
            "retained anonymous stage unavailable: {}",
            io::Error::last_os_error()
        );
        // No observation, checkpoint, allocation or other fallible work may
        // occur between successful openat and this ownership installation.
        self.file = Some(unsafe { File::from_raw_fd(fd) });
        self.boundary(PreparationPoint::FileAcquired, cancel, deadline)?;
        self.file_identity = Some(id(self.file.as_ref().unwrap())?);
        checkpoint(cancel, deadline)?;
        Ok(())
    }
    fn write_buffer(
        &mut self,
        count: usize,
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> anyhow::Result<()> {
        let mut offset = 0;
        while offset < count {
            checkpoint(cancel, deadline)?;
            let written = self
                .file
                .as_mut()
                .unwrap()
                .write(&self.buffer[offset..count])?;
            self.boundary(PreparationPoint::CopyChunk, cancel, deadline)?;
            ensure!(written != 0, "retained stage write made no progress");
            offset += written;
        }
        Ok(())
    }
    fn seal(&mut self, bytes: u64, cancel: &AtomicBool, deadline: Instant) -> anyhow::Result<()> {
        self.boundary(PreparationPoint::BeforeHash, cancel, deadline)?;
        let stat = self.file.as_ref().unwrap().metadata()?;
        checkpoint(cancel, deadline)?;
        ensure!(
            stat.is_file()
                && stat.nlink() == 0
                && stat.len() == bytes
                && Some((stat.dev(), stat.ino())) == self.file_identity,
            "retained stage identity or length changed"
        );
        self.file.as_mut().unwrap().seek(SeekFrom::Start(0))?;
        checkpoint(cancel, deadline)?;
        let mut hasher = Sha256::new();
        let mut total = 0u64;
        loop {
            checkpoint(cancel, deadline)?;
            let count = self.file.as_mut().unwrap().read(&mut self.buffer)?;
            self.boundary(PreparationPoint::HashChunk, cancel, deadline)?;
            if count == 0 {
                break;
            }
            total = total
                .checked_add(count as u64)
                .ok_or_else(|| anyhow::anyhow!("retained stage length overflow"))?;
            ensure!(total <= bytes, "retained stage grew during hashing");
            hasher.update(&self.buffer[..count]);
            checkpoint(cancel, deadline)?;
        }
        ensure!(
            total == bytes && digest_matches(&hasher.finalize(), &self.digest),
            "retained stage digest mismatch"
        );
        self.boundary(PreparationPoint::BeforeSync, cancel, deadline)?;
        self.file.as_ref().unwrap().sync_all()?;
        self.boundary(PreparationPoint::AfterSync, cancel, deadline)?;
        let parent_identity = id(self.parent.as_ref().unwrap())?;
        checkpoint(cancel, deadline)?;
        let stat = self.file.as_ref().unwrap().metadata()?;
        checkpoint(cancel, deadline)?;
        ensure!(
            Some(parent_identity) == self.parent_identity
                && Some((stat.dev(), stat.ino())) == self.file_identity
                && stat.is_file()
                && stat.nlink() == 0
                && stat.len() == bytes,
            "retained stage or parent changed before completion"
        );
        self.boundary(PreparationPoint::BeforeReady, cancel, deadline)?;
        self.publication = Some(
            Publication::prepared(Evidence {
                parent_device: parent_identity.0,
                parent_inode: parent_identity.1,
                file_device: stat.dev(),
                file_inode: stat.ino(),
                name: self.name.take().unwrap(),
                artifact: Artifact {
                    bytes,
                    sha256: std::mem::take(&mut self.digest),
                },
            })
            .map_err(|error| anyhow::anyhow!("invalid retained stage evidence: {error:?}"))?,
        );
        self.boundary(PreparationPoint::EvidencePrepared, cancel, deadline)
    }
    /// All IO, hashing, allocation and evidence validation finished in prepare.
    /// Success is moves only; install the returned Outcome immediately, before
    /// its existing preflight and the caller's genuine fresh temporal fence.
    pub(in crate::native_service) fn finish(&mut self) -> Option<Outcome> {
        if self.phase != PreparationPhase::Ready
            || self.parent.is_none()
            || self.file.is_none()
            || self.path.is_none()
            || self.publication.is_none()
        {
            return None;
        }
        self.phase = PreparationPhase::Finished;
        Some(
            Sealed {
                parent: self.parent.take().unwrap(),
                file: self.file.take().unwrap(),
                path: self.path.take().unwrap(),
                state: self.publication.take().unwrap(),
            }
            .retain(),
        )
    }
    fn boundary(
        &mut self,
        point: PreparationPoint,
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> anyhow::Result<()> {
        #[cfg(not(test))]
        let _ = point;
        #[cfg(test)]
        if let Some((at, fault)) = self.fault {
            if at == point {
                self.fault = None;
                self.fault_fds = Some(self.original_fds());
                match fault {
                    PreparationFault::Error => {
                        anyhow::bail!("injected retained preparation failure")
                    }
                    PreparationFault::Panic => panic!("injected retained preparation panic"),
                    PreparationFault::Cancel => cancel.store(true, Ordering::Release),
                    PreparationFault::Expire => {
                        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                    }
                    PreparationFault::Corrupt => {
                        use std::os::unix::fs::FileExt;
                        self.file.as_ref().unwrap().write_at(b"!", 0)?;
                    }
                }
            }
        }
        checkpoint(cancel, deadline)
    }
    #[cfg(test)]
    pub(in crate::native_service) fn inject(
        &mut self,
        point: PreparationPoint,
        fault: PreparationFault,
    ) {
        self.fault = Some((point, fault));
    }
    #[cfg(test)]
    pub(in crate::native_service) fn original_fds(
        &self,
    ) -> (Option<std::os::fd::RawFd>, Option<std::os::fd::RawFd>) {
        (
            self.parent.as_ref().map(AsRawFd::as_raw_fd),
            self.file.as_ref().map(AsRawFd::as_raw_fd),
        )
    }
    #[cfg(test)]
    pub(in crate::native_service) fn retained_length(&self) -> Option<u64> {
        self.file
            .as_ref()
            .map(|file| file.metadata().unwrap().len())
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";
fn digest_matches(bytes: &[u8], expected: &str) -> bool {
    expected.len() == bytes.len() * 2
        && bytes.iter().enumerate().all(|(index, byte)| {
            expected.as_bytes()[index * 2] == HEX[(byte >> 4) as usize]
                && expected.as_bytes()[index * 2 + 1] == HEX[(byte & 15) as usize]
        })
}
fn checkpoint(cancel: &AtomicBool, deadline: Instant) -> anyhow::Result<()> {
    ensure!(
        !cancel.load(Ordering::Acquire),
        "retained preparation cancelled"
    );
    ensure!(
        Instant::now() < deadline,
        "retained preparation original deadline exhausted"
    );
    Ok(())
}
fn verify_source(
    source: &mut File,
    expected: &Artifact,
    buffer: &mut [u8],
    cancel: &AtomicBool,
    deadline: Instant,
) -> anyhow::Result<()> {
    checkpoint(cancel, deadline)?;
    let original = source.metadata()?;
    checkpoint(cancel, deadline)?;
    ensure!(
        original.is_file() && original.nlink() == 0 && original.len() == expected.bytes,
        "retained source must be the exact anonymous image"
    );
    source.seek(SeekFrom::Start(0))?;
    checkpoint(cancel, deadline)?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    loop {
        checkpoint(cancel, deadline)?;
        let count = source.read(buffer)?;
        checkpoint(cancel, deadline)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| anyhow::anyhow!("retained source length overflow"))?;
        ensure!(total <= expected.bytes, "retained source image grew");
        hasher.update(&buffer[..count]);
        checkpoint(cancel, deadline)?;
    }
    ensure!(
        total == expected.bytes && digest_matches(&hasher.finalize(), &expected.sha256),
        "retained source image digest mismatch"
    );
    let final_stat = source.metadata()?;
    checkpoint(cancel, deadline)?;
    ensure!(
        final_stat.is_file()
            && final_stat.nlink() == 0
            && final_stat.len() == expected.bytes
            && (final_stat.dev(), final_stat.ino()) == (original.dev(), original.ino()),
        "retained source image changed during verification"
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::native_service) enum PreparationPoint {
    ParentAcquired,
    FileAcquired,
    CopyChunk,
    BeforeHash,
    HashChunk,
    BeforeSync,
    AfterSync,
    BeforeReady,
    EvidencePrepared,
}
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(in crate::native_service) enum PreparationFault {
    Error,
    Panic,
    Cancel,
    Expire,
    Corrupt,
}

#[cfg(test)]
mod tests;
