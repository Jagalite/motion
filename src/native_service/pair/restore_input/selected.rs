//! Fresh-process selected-artifact input. Independent inspection returns owned
//! descriptive copies, never current authorization, conversion or restore grants.
use super::*;
const MAX_CONTAINER: u64 = 128 * 1024 * 1024;
// Final native descriptions retain <=4096 worker IDs (<=256 bytes each,
// <=1MiB total), kind strings <=11 bytes,32-byte incarnations,64-byte tuple
// digests and four-String entries; <=1024 roots with <=256KiB total paths;
// <=1000 native inventory entries with <=256-byte names/64-byte digests.
// Motion retains only inventory digests and bounded verified identity, not
// raw binding JSON or inventory row vectors. Eight MiB conservatively covers
// these strings, collection capacity and metadata, not allocator/RSS overhead.
// Reinspection precharges three simultaneous sets (old selected, new selected,
// current native); the consumed owner retains two. The256MiB cap is unchanged.
pub(super) const RETAINED_METADATA: u64 = 8 * 1024 * 1024;
pub(super) struct SelectedArtifact {
    selected: File,
    parent: File,
    parent_path: PathBuf,
    leaf: CString,
    source: Artifact,
    staging: File,
}
pub(in crate::native_service::pair) struct SelectedPairInput {
    artifact: SelectedArtifact,
    motion: File,
    native: File,
    manifest: PairManifest,
    motion_facts: motion_catalog_backend::motion_snapshot::image_read::InspectedImage,
    native_facts: catabolic_store::catalog_backup::image_read::InspectedImage,
    current_native_facts: Option<catabolic_store::catalog_backup::image_read::InspectedImage>,
}
// This bounded input-path share is charged with the actual supervisor layout
// BEFORE native export/pair acquisition. It covers the caller buffers, not new
// clones. Later inspection scratch and metadata have their own measured charge.
pub(in crate::native_service::pair) const SERVICE_INPUT_PATH_STORAGE: u64 =
    2 * libc::PATH_MAX as u64;

/// A service phase, not a request result or restore grant. The current pair is
/// retained separately in the same PhaseOwner and is never consumed here.
pub(in crate::native_service::pair) struct ServiceSelection {
    path: PathBuf,
    staging: PathBuf,
    deadline: std::time::Instant,
    selected: Option<SelectedPairInput>,
    high_water: u64,
    bound: bool,
}
fn measured_current(current: &mut PreparedPair) -> anyhow::Result<u64> {
    current
        ._writer
        .retained_allocations_blocking()?
        .checked_add(current.retained_native_allocations()?)
        .and_then(|n| n.checked_add(OVERHEAD))
        .context("selected/current pair accounting overflow")
}
impl ServiceSelection {
    /// No allocation/clone precedes this check. Moving the supplied buffers is
    /// allowed only within the specific input share already admitted by export.
    pub(in crate::native_service::pair) fn new(
        path: PathBuf,
        staging: PathBuf,
        deadline: std::time::Instant,
    ) -> anyhow::Result<Self> {
        ensure!(
            path.is_absolute() && staging.is_absolute(),
            "service selection requires absolute paths"
        );
        let supplied = u64::try_from(path.capacity())?
            .checked_add(u64::try_from(staging.capacity())?)
            .context("selected input path capacity overflow")?;
        ensure!(
            supplied <= SERVICE_INPUT_PATH_STORAGE,
            "selected input path share exhausted"
        );
        ensure!(
            std::time::Instant::now() < deadline,
            "selected caller deadline already expired"
        );
        Ok(Self {
            path,
            staging,
            deadline,
            selected: None,
            high_water: 0,
            bound: false,
        })
    }
    pub(in crate::native_service::pair) fn target_deadline(
        &self,
        flag: &AtomicBool,
    ) -> anyhow::Result<std::time::Instant> {
        self.alive(flag)?;
        ensure!(
            self.bound && self.selected.is_some(),
            "bound service selection required"
        );
        Ok(self.deadline)
    }
    /// Borrow the exact service-retained selection, never consume/reconstruct
    /// its current pair. This is descriptive binding, not mutation permission.
    pub(in crate::native_service::pair) fn target_input(
        &mut self,
        flag: &AtomicBool,
    ) -> anyhow::Result<&mut SelectedPairInput> {
        self.target_deadline(flag)?;
        let selected = self.selected.as_mut().expect("bound selected input");
        selected.revalidate(flag)?;
        Ok(selected)
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn evidence_for_fixture(
        &self,
    ) -> (Option<(&File, &str)>, bool) {
        (
            self.selected
                .as_ref()
                .map(|input| (&input.artifact.selected, input.artifact.sha256())),
            self.bound,
        )
    }
    pub(in crate::native_service::pair) fn alive(&self, flag: &AtomicBool) -> anyhow::Result<()> {
        cancelled(flag)?;
        ensure!(
            std::time::Instant::now() < self.deadline,
            "original selected caller deadline exhausted"
        );
        Ok(())
    }
    /// Additional host high water, not the already-admitted caller path share.
    /// Retain it monotonically in Shared even after transient work is gone.
    pub(in crate::native_service::pair) fn admit(
        &mut self,
        current: &mut PreparedPair,
        flag: &AtomicBool,
    ) -> anyhow::Result<u64> {
        self.alive(flag)?;
        use std::os::unix::ffi::OsStrExt;
        let bytes = |n: usize| u64::try_from(n).context("selected path storage width overflow");
        let path = bytes(self.path.as_os_str().as_bytes().len())?;
        let staging = bytes(self.staging.as_os_str().as_bytes().len())?;
        let parent = bytes(
            self.path
                .parent()
                .context("selected parent missing")?
                .as_os_str()
                .as_bytes()
                .len(),
        )?;
        let leaf = bytes(
            self.path
                .file_name()
                .context("selected leaf missing")?
                .as_bytes()
                .len(),
        )?
        .checked_add(1)
        .context("selected leaf storage overflow")?;
        let component = self
            .path
            .components()
            .chain(self.staging.components())
            .filter_map(|part| match part {
                Component::Normal(name) => Some(name.as_bytes().len()),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let component = bytes(component)?
            .checked_add(1)
            .context("selected component storage overflow")?;
        // Absolute selected copy + retained parent copy + leaf CString overlap
        // open_parent's absolute scratch and component CString. Twofold terms
        // cover construction growth, not an assumed allocator/RSS bound. Caller
        // buffer capacities are absent here: the fixed input share covers them.
        let mut additional = 0u64;
        for next in [
            path,
            path,
            parent,
            parent,
            leaf,
            leaf,
            path.max(staging),
            path.max(staging),
            component,
            component,
            bytes(std::mem::size_of::<SelectedPairInput>())?,
            bytes(std::mem::size_of::<PathBuf>() * 2)?,
            bytes(std::mem::size_of::<CString>() * 2)?,
            bytes(std::mem::size_of::<[u8; 65536]>())?,
            bytes(std::mem::size_of::<Sha256>())?,
            bytes(std::mem::size_of::<std::fs::Metadata>() * 4)?,
            128, // retained source digest and its simultaneous verification copy
            RETAINED_METADATA * 3,
        ] {
            additional = additional
                .checked_add(next)
                .context("selected host storage overflow")?;
        }
        self.high_water = measured_current(current)?
            .checked_add(additional)
            .context("selected service admission overflow")?;
        ensure!(
            self.high_water <= 256 * 1024 * 1024,
            "selected service admission exhausted"
        );
        Ok(additional)
    }
    pub(in crate::native_service::pair) fn inspect(
        &mut self,
        current: &mut PreparedPair,
        flag: &AtomicBool,
    ) -> anyhow::Result<()> {
        self.alive(flag)?;
        current._stopped.verify_lock()?;
        ensure!(
            identity(&current.parent)? == current.parent_identity,
            "current pair parent changed"
        );
        // Shared now forwards the admitted additional high water. Do not add
        // its metadata or the fixed input share again at either inspection.
        self.high_water = self.high_water.max(measured_current(current)?);
        let selected = SelectedPairInput::prepare_precharged(
            &self.path,
            &self.staging,
            flag,
            self.high_water,
            || Ok(()),
        )?;
        self.selected = Some(selected);
        // The successful initial facts and exact selected descriptors are in
        // the service phase before this or any later fallible revalidation.
        self.selected
            .as_mut()
            .expect("installed selected input")
            .revalidate(flag)?;
        self.alive(flag)
    }
    pub(in crate::native_service::pair) fn bind(
        &mut self,
        current: &mut PreparedPair,
        flag: &AtomicBool,
    ) -> anyhow::Result<()> {
        self.alive(flag)?;
        let selected = self
            .selected
            .as_mut()
            .context("service selected input missing")?;
        selected.validate_current_prefix(current, flag)?;
        self.high_water = self.high_water.max(measured_current(current)?);
        selected.bind_current_after_validation(current, flag, self.high_water)?;
        self.alive(flag)?;
        self.bound = true;
        Ok(())
    }
}
fn regular(file: &File) -> anyhow::Result<std::fs::Metadata> {
    let m = file.metadata()?;
    ensure!(
        m.is_file() && m.nlink() == 1 && m.len() > 0 && m.len() <= MAX_CONTAINER,
        "selected pair requires bounded regular single-link file"
    );
    Ok(m)
}
fn open_selected(parent: &File, leaf: &std::ffi::CStr) -> anyhow::Result<File> {
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            leaf.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    ensure!(
        fd >= 0,
        "selected pair open failed: {}",
        std::io::Error::last_os_error()
    );
    let file = unsafe { File::from_raw_fd(fd) };
    regular(&file)?;
    Ok(file)
}
fn snapshot(file: &mut File, flag: &AtomicBool) -> anyhow::Result<Artifact> {
    let initial = regular(file)?;
    file.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        cancelled(flag)?;
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        total = total
            .checked_add(n as u64)
            .context("selected pair length overflow")?;
        ensure!(total <= initial.len(), "selected pair grew");
        hash.update(&buffer[..n]);
    }
    let final_state = regular(file)?;
    ensure!(
        total == initial.len()
            && (
                initial.dev(),
                initial.ino(),
                initial.len(),
                initial.ctime(),
                initial.ctime_nsec()
            ) == (
                final_state.dev(),
                final_state.ino(),
                final_state.len(),
                final_state.ctime(),
                final_state.ctime_nsec()
            ),
        "selected pair changed during observation"
    );
    Ok(Artifact {
        bytes: total,
        sha256: format!("{:x}", hash.finalize()),
    })
}
// This finite cohort proves matching frozen binding/89 and migration resources,
// catalog/incarnation/profile, and nondecreasing generation only. A same-
// incarnation branched copy can satisfy it: it is NOT proof that every selected
// command is a prefix/ancestor of current commands. Historical target bytes and
// fresh current authorization remain independently validated and retained.
fn require_same_frozen_lineage(
    selected: &PairManifest,
    current: &PairManifest,
) -> anyhow::Result<()> {
    ensure!(
        selected.motion_schema_version == 93
            && current.motion_schema_version == 93
            && selected.native_identity.schema_version == 40
            && current.native_identity.schema_version == 40,
        "selected/current pair requires exact93/native40 history cohort"
    );
    ensure!(
        selected.native_identity.catalog_id == current.native_identity.catalog_id
            && selected.native_identity.incarnation == current.native_identity.incarnation
            && selected.native_identity.profile == current.native_identity.profile,
        "selected/current pair catalog, incarnation or profile differs"
    );
    ensure!(
        selected.native_identity.generation <= current.native_identity.generation,
        "selected pair is newer than current stopped native history"
    );
    // Exact independently verified full binding/89 evidence includes the frozen
    // binding commitment. No incarnation rewrite or cross-lineage inference.
    ensure!(
        selected.motion_binding_sha256 == current.motion_binding_sha256
            && selected.motion_migration_ledger_sha256 == current.motion_migration_ledger_sha256
            && selected.native_migration_ledger_sha256 == current.native_migration_ledger_sha256,
        "selected/current pair frozen binding or migration history differs"
    );
    Ok(())
}
impl SelectedArtifact {
    pub(super) fn sha256(&self) -> &str {
        &self.source.sha256
    }
    pub(super) fn parents(&self) -> [&File; 2] {
        [&self.parent, &self.staging]
    }
    pub(super) fn revalidate(&mut self, flag: &AtomicBool) -> anyhow::Result<()> {
        ensure!(
            identity(&super::super::open_parent(&self.parent_path)?)? == identity(&self.parent)?,
            "selected pair parent replaced"
        );
        let named = open_selected(&self.parent, &self.leaf)?;
        ensure!(
            identity(&named)? == identity(&self.selected)?,
            "selected pair pathname replaced"
        );
        ensure!(
            snapshot(&mut self.selected, flag)? == self.source,
            "selected pair bytes changed"
        );
        Ok(())
    }
}
impl PreparedPair {
    /// The original artifact is selected while this freshly stopped current
    /// owner is retained. Initial inspection and repeated binding both charge
    /// its measured caches, not a caller-provided authorization/reservation.
    pub(in crate::native_service::pair) async fn select_previous(
        mut self,
        path: PathBuf,
        staging: PathBuf,
        flag: Arc<AtomicBool>,
    ) -> anyhow::Result<InspectedRestoreInput> {
        let mut cancellation = PairCancellation(flag.clone(), true);
        let result = tokio::task::spawn_blocking(move || {
            let external = self
                ._writer
                .retained_allocations_blocking()?
                .checked_add(self.retained_native_allocations()?)
                .and_then(|n| n.checked_add(OVERHEAD))
                .context("selected initial inspection accounting overflow")?;
            let selected = SelectedPairInput::inspect_with_reservation(
                &path,
                &staging,
                &flag,
                external,
                || Ok(()),
            )?;
            selected.bind_current_blocking(self, &flag)
        })
        .await
        .context("current selected-artifact owner panicked")?;
        cancellation.1 = false;
        result
    }
}
impl SelectedPairInput {
    /// Fixed borrowed views for the actual service staging bridge. These never
    /// surrender an image FD or produce a source/target authorization grant.
    /// Borrow original immutable93 and its inspected facts without surrendering
    /// either selected descriptor. Distinct from the current source writer.
    pub(in crate::native_service::pair) fn service_control_input(
        &mut self,
    ) -> (&mut File, &motion_catalog_backend::motion_snapshot::image_read::InspectedImage) {
        (&mut self.motion, &self.motion_facts)
    }
    pub(in crate::native_service::pair) fn service_manifest(&self) -> &PairManifest {
        &self.manifest
    }
    pub(in crate::native_service::pair) fn prepare_service_motion(
        &mut self,
        preparation: &mut crate::native_service::publication_file::Preparation,
        parent: &File,
        parent_identity: (u64, u64),
        cancel: &AtomicBool,
        deadline: std::time::Instant,
    ) -> anyhow::Result<()> {
        preparation.prepare_image(
            parent,
            parent_identity,
            &mut self.motion,
            &self.manifest.motion_database,
            cancel,
            deadline,
        )
    }
    pub(in crate::native_service::pair) fn prepare_service_native(
        &mut self,
        preparation: &mut crate::native_service::publication_file::Preparation,
        parent: &File,
        parent_identity: (u64, u64),
        cancel: &AtomicBool,
        deadline: std::time::Instant,
    ) -> anyhow::Result<()> {
        preparation.prepare_image(
            parent,
            parent_identity,
            &mut self.native,
            &self.manifest.native_database,
            cancel,
            deadline,
        )
    }
    pub(in crate::native_service::pair) fn target_revalidate(
        &mut self,
        flag: &AtomicBool,
    ) -> anyhow::Result<()> {
        self.revalidate(flag)
    }
    pub(in crate::native_service::pair) fn target_archive(&self) -> &SelectedArtifact {
        &self.artifact
    }
    pub(in crate::native_service::pair) fn target_roots(
        &self,
    ) -> impl Iterator<Item = &catabolic_store::catalog_backup::image_read::BindingRoot> {
        self.native_facts.binding_roots().iter().chain(
            self.current_native_facts
                .iter()
                .flat_map(|facts| facts.binding_roots().iter()),
        )
    }
    pub(in crate::native_service::pair) fn target_binding(
        &mut self,
        current: &PreparedPair,
        flag: &AtomicBool,
    ) -> anyhow::Result<()> {
        self.validate_current_prefix(current, flag)?;
        ensure!(
            self.current_native_facts.is_some(),
            "bound current root inventory missing"
        );
        Ok(())
    }
    pub(in crate::native_service::pair) fn inspect(
        path: &Path,
        staging: &Path,
        flag: &AtomicBool,
    ) -> anyhow::Result<Self> {
        Self::inspect_with(path, staging, flag, || Ok(()))
    }
    /// Connects historical immutable bytes to independently fresh stopped
    /// authorization custody. Neither input alone grants target mutation.
    pub(in crate::native_service::pair) async fn bind_current(
        self,
        current: PreparedPair,
        flag: Arc<AtomicBool>,
    ) -> anyhow::Result<InspectedRestoreInput> {
        let mut cancellation = PairCancellation(flag.clone(), true);
        let result =
            tokio::task::spawn_blocking(move || self.bind_current_blocking(current, &flag))
                .await
                .context("selected/current pair composition owner panicked")?;
        cancellation.1 = false;
        result
    }
    fn bind_current_blocking(
        mut self,
        mut current: PreparedPair,
        flag: &AtomicBool,
    ) -> anyhow::Result<InspectedRestoreInput> {
        // Preserve the original early-refusal order: cancelled/invalid inputs
        // must not acquire the writer lease merely to measure its allocations.
        self.validate_current_prefix(&current, flag)?;
        let reserved = measured_current(&mut current)?
            .checked_add(RETAINED_METADATA * 3)
            .context("selected/current pair accounting overflow")?;
        self.bind_current_after_validation(&mut current, flag, reserved)?;
        Ok(InspectedRestoreInput {
            motion: self.motion,
            native: self.native,
            manifest: self.manifest,
            _motion_facts: self.motion_facts,
            _native_facts: self.native_facts,
            archive: ArchiveCustody::Selected(self.artifact),
            current_native_facts: self.current_native_facts,
            current,
        })
    }
    /// Shared exact original binding prefix, run once before its measurement.
    fn validate_current_prefix(
        &mut self,
        current: &PreparedPair,
        flag: &AtomicBool,
    ) -> anyhow::Result<()> {
        cancelled(flag)?;
        current._stopped.verify_lock()?;
        ensure!(
            identity(&current.parent)? == current.parent_identity,
            "current pair parent changed"
        );
        self.revalidate(flag)?;
        require_same_frozen_lineage(&self.manifest, &current.manifest)?;
        Ok(())
    }
    // All authority and resources remain borrowed from the service phase. Both
    // callers have just run the exact prefix, without duplicating its I/O.
    // This private scalar is measured by the owner, not supplied by a request.
    fn bind_current_after_validation(
        &mut self,
        current: &mut PreparedPair,
        flag: &AtomicBool,
        reserved: u64,
    ) -> anyhow::Result<()> {
        let (motion_facts, native_facts) = inspect_images(
            &mut self.motion,
            &mut self.native,
            &self.manifest,
            reserved,
            flag,
        )?;
        // Install successful selected observations before any later fallible work.
        self.motion_facts = motion_facts;
        self.native_facts = native_facts;
        // Current may have additional source/output roots after the archived
        // snapshot. Independently inspect its genuine verified export too so
        // fresh-target ancestry cannot overlap either complete root inventory.
        let mut current_native = anonymous(&current.parent)?;
        current.native.copy_verified_to(&mut current_native, flag)?;
        current_native.seek(SeekFrom::Start(0))?;
        let current_native_facts = catabolic_store::catalog_backup::image_read::inspect(
            &mut current_native,
            current.native.bytes(),
            reserved,
            flag,
        )?;
        // Keep the new current observation in the same owner even if its
        // commitments or a subsequent revalidation refuse this selection.
        self.current_native_facts = Some(current_native_facts);
        let current_native_facts = self
            .current_native_facts
            .as_ref()
            .expect("installed current facts");
        ensure!(
            current_native_facts.sha256() == current.native.sha256()
                && current_native_facts.inventory_sha256() == current.native.inventory_sha256()
                && current_native_facts.catalog_id() == current.manifest.native_identity.catalog_id
                && current_native_facts.incarnation()
                    == current.manifest.native_identity.incarnation
                && u64::try_from(current_native_facts.generation())?
                    == current.manifest.native_identity.generation,
            "current root inventory differs from retained export"
        );
        self.revalidate(flag)?;
        current._stopped.verify_lock()?;
        cancelled(flag)?;
        Ok(())
    }
    fn inspect_with(
        path: &Path,
        staging: &Path,
        flag: &AtomicBool,
        after_pin: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<Self> {
        Self::inspect_with_reservation(path, staging, flag, 0, after_pin)
    }
    fn inspect_with_reservation(
        path: &Path,
        staging: &Path,
        flag: &AtomicBool,
        external: u64,
        after_pin: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<Self> {
        let reserved = external
            .checked_add(RETAINED_METADATA)
            .context("selected inspection reservation overflow")?;
        let mut input = Self::prepare_precharged(path, staging, flag, reserved, after_pin)?;
        input.revalidate(flag)?;
        Ok(input)
    }
    fn prepare_precharged(
        path: &Path,
        staging: &Path,
        flag: &AtomicBool,
        reserved: u64,
        after_pin: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<Self> {
        use std::os::unix::ffi::OsStrExt;
        ensure!(
            reserved <= 256 * 1024 * 1024,
            "selected inspection reservation exhausted"
        );
        cancelled(flag)?;
        let path = std::path::absolute(path)?;
        let parent_path = path
            .parent()
            .context("selected pair parent missing")?
            .to_owned();
        let leaf = CString::new(
            path.file_name()
                .context("selected pair leaf missing")?
                .as_bytes(),
        )?;
        let parent = super::super::open_parent(&parent_path)?;
        let mut selected = open_selected(&parent, &leaf)?;
        let source = snapshot(&mut selected, flag)?;
        after_pin()?;
        let staging = super::super::open_parent(staging)?;
        let mut motion = anonymous(&staging)?;
        let mut native = anonymous(&staging)?;
        selected.seek(SeekFrom::Start(0))?;
        let manifest = crate::native_service::pair_container::extract(
            &mut selected,
            &mut motion,
            &mut native,
            flag,
        )?;
        motion.sync_all()?;
        native.sync_all()?;
        let (motion_facts, native_facts) =
            inspect_images(&mut motion, &mut native, &manifest, reserved, flag)?;
        Ok(Self {
            artifact: SelectedArtifact {
                selected,
                parent,
                parent_path,
                leaf,
                source,
                staging,
            },
            motion,
            native,
            manifest,
            motion_facts,
            native_facts,
            current_native_facts: None,
        })
    }
    fn revalidate(&mut self, flag: &AtomicBool) -> anyhow::Result<()> {
        self.artifact.revalidate(flag)?;
        verify_file(&mut self.motion, &self.manifest.motion_database, flag)?;
        verify_file(&mut self.native, &self.manifest.native_database, flag)?;
        Ok(())
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn assert_identity(&self) {
        assert_eq!(
            self.native_facts.catalog_id(),
            self.manifest.native_identity.catalog_id
        );
        assert_eq!(
            self.motion_facts.binding().effective_identity().catalog_id,
            self.manifest.native_identity.catalog_id
        );
        assert!(self.artifact.staging.metadata().unwrap().is_dir());
    }
}

#[cfg(test)]
pub(in crate::native_service::pair) fn exercise_process_fixture(original: &Path) {
    let staging = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    let input = SelectedPairInput::inspect(original, staging.path(), &cancel).unwrap();
    input.assert_identity();
    require_same_frozen_lineage(&input.manifest, &input.manifest).unwrap();
    for change in 0..8 {
        let mut other = input.manifest.clone();
        match change {
            0 => other.native_identity.catalog_id.push('x'),
            1 => other.native_identity.incarnation.push('x'),
            2 => other.native_identity.profile.push('x'),
            3 => other.native_identity.schema_version = 41,
            4 => other.motion_schema_version = 97,
            5 => other.motion_binding_sha256 = "a".repeat(64),
            6 => other.motion_migration_ledger_sha256 = "b".repeat(64),
            _ => other.native_migration_ledger_sha256 = "c".repeat(64),
        }
        assert!(require_same_frozen_lineage(&input.manifest, &other).is_err());
    }
    let mut newer = input.manifest.clone();
    newer.native_identity.generation = newer.native_identity.generation.checked_add(1).unwrap();
    require_same_frozen_lineage(&input.manifest, &newer).unwrap();
    assert!(require_same_frozen_lineage(&newer, &input.manifest).is_err());
    assert!(
        SelectedPairInput::inspect_with_reservation(
            original,
            staging.path(),
            &cancel,
            u64::MAX,
            || Ok(())
        )
        .is_err()
    );
    drop(input);
    assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 0);
    let cases = tempfile::tempdir().unwrap();
    let copy = |name: &str| {
        let p = cases.path().join(name);
        std::fs::copy(original, &p).unwrap();
        p
    };
    // A successful inspection is not lasting authority over mutable source
    // bytes or extracted stages. Future conversion must revalidate this owner.
    for change in 0..5 {
        let path = copy(&format!("after-inspection-{change}.pair"));
        let mut input = SelectedPairInput::inspect(&path, staging.path(), &cancel).unwrap();
        match change {
            0 => {
                std::fs::rename(&path, cases.path().join("after-inspection-old.pair")).unwrap();
                std::fs::copy(original, &path).unwrap();
            }
            1 => {
                use std::os::unix::fs::FileExt;
                let file = File::options().write(true).open(&path).unwrap();
                file.write_all_at(b"X", 0).unwrap();
            }
            2 => {
                std::fs::hard_link(&path, cases.path().join("after-inspection-alias.pair"))
                    .unwrap();
            }
            3 => {
                use std::os::unix::fs::FileExt;
                input.motion.write_all_at(b"X", 0).unwrap();
            }
            _ => {
                use std::os::unix::fs::FileExt;
                input.native.write_all_at(b"X", 0).unwrap();
            }
        }
        assert!(input.revalidate(&cancel).is_err());
        drop(input);
        assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 0);
    }
    let path = copy("replace.pair");
    assert!(
        SelectedPairInput::inspect_with(&path, staging.path(), &cancel, || {
            std::fs::rename(&path, cases.path().join("retained-old.pair"))?;
            std::fs::copy(original, &path)?;
            Ok(())
        })
        .is_err()
    );
    let path = copy("truncate.pair");
    assert!(
        SelectedPairInput::inspect_with(&path, staging.path(), &cancel, || {
            File::options().write(true).open(&path)?.set_len(17)?;
            Ok(())
        })
        .is_err()
    );
    let path = copy("extra-link.pair");
    std::fs::hard_link(&path, cases.path().join("another-link.pair")).unwrap();
    assert!(SelectedPairInput::inspect(&path, staging.path(), &cancel).is_err());
    let alias = cases.path().join("symlink.pair");
    std::os::unix::fs::symlink(original, &alias).unwrap();
    assert!(SelectedPairInput::inspect(&alias, staging.path(), &cancel).is_err());
    assert!(SelectedPairInput::inspect(cases.path(), staging.path(), &cancel).is_err());
    let fifo = cases.path().join("fifo.pair");
    use std::os::unix::ffi::OsStrExt;
    let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(SelectedPairInput::inspect(&fifo, staging.path(), &cancel).is_err());
    cancel.store(true, Ordering::Release);
    assert!(SelectedPairInput::inspect(original, staging.path(), &cancel).is_err());
    cancel.store(false, Ordering::Release);
    assert!(
        SelectedPairInput::inspect_with(original, staging.path(), &cancel, || {
            cancel.store(true, Ordering::Release);
            Ok(())
        })
        .is_err()
    );
    assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 0);
    // Every refusal leaves the genuinely published source pair untouched.
    cancel.store(false, Ordering::Release);
    SelectedPairInput::inspect(original, staging.path(), &cancel)
        .unwrap()
        .assert_identity();
}
