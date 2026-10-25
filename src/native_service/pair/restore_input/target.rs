//! Consumed inspected-pair to fresh target custody. No conversion, rotation or DB
//! link authority is minted. The original pair remains owned and unchanged.
mod conversion;
use super::*;
use crate::native_service::{pair_install, publication_file::Outcome};
pub(in crate::native_service) use conversion::{RestoreProof, RestoreSupervisor};
use playscale_core::paired_backup::publication::Phase;
struct Custody {
    input: InspectedRestoreInput,
    path: PathBuf,
    parent: File,
    lock: File,
    install_id: String,
    reservation: Outcome,
}
pub(in crate::native_service::pair) struct FreshRestoreTarget {
    held: Option<Custody>,
}
static RECOVERY: std::sync::OnceLock<std::sync::Mutex<Vec<Custody>>> = std::sync::OnceLock::new();
impl Drop for FreshRestoreTarget {
    fn drop(&mut self) {
        let Some(held) = self.held.take() else { return };
        if matches!(
            held.reservation.phase(),
            Phase::LinkAttempted | Phase::LinkedUnconfirmed | Phase::OutcomeUnknown
        ) {
            tracing::error!(install_id=%held.install_id, evidence=?held.reservation.evidence(),"restore target reservation requires recovery; retaining exact source and target custody");
            RECOVERY
                .get_or_init(Default::default)
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(held);
        }
    }
}
// Retain actual ancestor FDs, not canonical strings. openat(..) handles mount
// aliases without assuming two different path prefixes mean disjoint roots.
fn ancestors(start: &File, budget: &mut usize) -> anyhow::Result<Vec<File>> {
    let mut out = vec![start.try_clone()?];
    loop {
        ensure!(
            *budget > 0 && out.len() <= 128,
            "restore target ancestry budget exceeded"
        );
        *budget -= 1;
        let current = out.last().context("ancestor missing")?;
        let fd = unsafe {
            libc::openat(
                current.as_raw_fd(),
                c"..".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        ensure!(
            fd >= 0,
            "restore ancestry unavailable: {}",
            std::io::Error::last_os_error()
        );
        let parent = unsafe { File::from_raw_fd(fd) };
        let next = identity(&parent)?;
        if next == identity(current)? {
            return Ok(out);
        }
        ensure!(
            !out.iter()
                .map(identity)
                .collect::<anyhow::Result<Vec<_>>>()?
                .contains(&next),
            "restore ancestry cycle"
        );
        out.push(parent);
    }
}
fn disjoint(target: &[File], source: &File, budget: &mut usize) -> anyhow::Result<()> {
    let source_id = identity(source)?;
    let target_id = identity(&target[0])?;
    ensure!(
        !target
            .iter()
            .map(identity)
            .collect::<anyhow::Result<Vec<_>>>()?
            .contains(&source_id),
        "restore target lies in source/output ancestry"
    );
    let source_ancestors = ancestors(source, budget)?;
    ensure!(
        !source_ancestors
            .iter()
            .map(identity)
            .collect::<anyhow::Result<Vec<_>>>()?
            .contains(&target_id),
        "restore target contains source/output ancestry"
    );
    Ok(())
}
fn verify_target_lock(parent: &File, lock: &File) -> anyhow::Result<()> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    ensure!(
        unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                c"server.lock".as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0,
        "restore target lock is unobservable"
    );
    let stat = unsafe { stat.assume_init() };
    let held = lock.metadata()?;
    ensure!(
        held.is_file()
            && held.nlink() == 1
            && stat.st_mode & libc::S_IFMT == libc::S_IFREG
            && (stat.st_dev, stat.st_ino) == (held.dev(), held.ino()),
        "restore target lock was substituted"
    );
    Ok(())
}
fn require_fresh(parent: &File) -> anyhow::Result<()> {
    require_fresh_leaves(parent, false)
}
fn require_fresh_leaves(parent: &File, retained_lock: bool) -> anyhow::Result<()> {
    for name in [
        "server.lock",
        "playscale.sqlite3",
        "catabolic.sqlite3",
        "playscale.sqlite3-wal",
        "playscale.sqlite3-shm",
        "playscale.sqlite3-journal",
        "catabolic.sqlite3-wal",
        "catabolic.sqlite3-shm",
        "catabolic.sqlite3-journal",
        "paired-install.reservation.json",
        "paired-install.conversion.json",
        "paired-install.intent.json",
        "paired-install.motion.json",
        "paired-install.native.json",
        "paired-install.complete.json",
    ] {
        if retained_lock && name == "server.lock" {
            continue;
        }
        let name = CString::new(name)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let result = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        ensure!(
            result < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT),
            "fresh restore target occupied or unobservable"
        );
    }
    Ok(())
}
impl InspectedRestoreInput {
    pub(in crate::native_service::pair) fn reserve_fresh_target(
        mut self,
        path: &Path,
        flag: &AtomicBool,
    ) -> anyhow::Result<FreshRestoreTarget> {
        cancelled(flag)?;
        self.verify_archive(flag)?;
        let held = &self.current;
        held._stopped.verify_lock()?;
        let parent = super::super::open_parent(path)?;
        let mut budget = 16384;
        let target = ancestors(&parent, &mut budget)?;
        disjoint(&target, &held.parent, &mut budget)?;
        for archive_parent in self.additional_archive_parents() {
            disjoint(&target, archive_parent, &mut budget)?;
        }
        let (state, native_parent) = held._stopped.source_parents()?;
        disjoint(&target, &state, &mut budget)?;
        disjoint(&target, &native_parent, &mut budget)?;
        for root in self._native_facts.binding_roots().iter().chain(
            self.current_native_facts
                .iter()
                .flat_map(|facts| facts.binding_roots().iter()),
        ) {
            cancelled(flag)?;
            let expected = root
                .identity()
                .context("restore root identity proof missing")?;
            let root = super::super::open_parent(Path::new(root.path()))?;
            ensure!(
                identity(&root)? == expected,
                "restore source/output root identity changed"
            );
            disjoint(&target, &root, &mut budget)?;
        }
        // Refuse all preexisting deployment/install leaves. Never adopt a lock or
        // retry an earlier install whose ownership is not this consumed attempt.
        require_fresh(&parent)?;
        ensure!(
            identity(&super::super::open_parent(path)?)? == identity(&parent)?,
            "restore target directory changed"
        );
        cancelled(flag)?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                c"server.lock".as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        ensure!(
            fd >= 0,
            "create-only restore target lock failed: {}",
            std::io::Error::last_os_error()
        );
        let lock = unsafe { File::from_raw_fd(fd) };
        fs2::FileExt::try_lock_exclusive(&lock)?;
        lock.sync_all()?;
        parent.sync_all()?;
        verify_target_lock(&parent, &lock)?;
        let install_id = uuid::Uuid::new_v4().to_string();
        let (parent_device, parent_inode) = identity(&parent)?;
        let reservation = pair_install::reserve_target(
            path,
            &pair_install::Reservation {
                contract: 1,
                install_id: install_id.clone(),
                pair_sha256: self.pair_sha256().to_owned(),
                parent_device,
                parent_inode,
            },
            flag,
        )?;
        let durable = reservation.phase() == Phase::Durable;
        let target = FreshRestoreTarget {
            held: Some(Custody {
                input: self,
                path: path.into(),
                parent,
                lock,
                install_id,
                reservation,
            }),
        };
        {
            let retained = target.held.as_ref().context("target custody missing")?;
            verify_target_lock(&retained.parent, &retained.lock)?;
            ensure!(
                identity(&super::super::open_parent(path)?)? == identity(&retained.parent)?,
                "reserved target parent changed"
            );
        }
        ensure!(
            durable,
            "fresh target reservation visibility/durability unresolved; explicit recovery required"
        );
        Ok(target)
    }
}
#[cfg(test)]
impl FreshRestoreTarget {
    pub(in crate::native_service::pair) fn assert_reserved(&self) {
        let held = self.held.as_ref().unwrap();
        assert!(
            crate::native_service::require_complete_pair_install(
                &held.path,
                &held.path.join("catabolic.sqlite3"),
                &held.lock
            )
            .is_err()
        );
        assert!(!held.path.join("playscale.sqlite3").exists());
        assert!(!held.path.join("catabolic.sqlite3").exists());
        assert_eq!(
            identity(&held.parent).unwrap(),
            identity(&super::super::open_parent(&held.path).unwrap()).unwrap()
        );
        held.input.current._stopped.verify_lock().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn descriptor_ancestry_refuses_same_parent_child_symlink_and_missing_roots() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::create_dir(a.path().join("child")).unwrap();
        let root = super::super::super::open_parent(a.path()).unwrap();
        let child = super::super::super::open_parent(&a.path().join("child")).unwrap();
        let other = super::super::super::open_parent(b.path()).unwrap();
        let mut budget = 1000;
        let roots = ancestors(&root, &mut budget).unwrap();
        assert!(disjoint(&roots, &root, &mut budget).is_err());
        assert!(disjoint(&roots, &child, &mut budget).is_err());
        assert!(disjoint(&ancestors(&child, &mut budget).unwrap(), &root, &mut budget).is_err());
        disjoint(&roots, &other, &mut budget).unwrap();
        std::os::unix::fs::symlink(a.path(), b.path().join("alias")).unwrap();
        assert!(super::super::super::open_parent(&b.path().join("alias")).is_err());
        assert!(super::super::super::open_parent(&b.path().join("missing")).is_err());
        assert!(ancestors(&root, &mut 0).is_err());
    }
    #[test]
    fn existing_rollback_journals_refuse_before_target_lock_or_reservation() {
        for leaf in ["playscale.sqlite3-journal", "catabolic.sqlite3-journal"] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join(leaf), b"unrelated hot journal").unwrap();
            let parent = super::super::super::open_parent(dir.path()).unwrap();
            assert!(require_fresh(&parent).is_err());
            assert!(!dir.path().join("server.lock").exists());
            assert!(!dir.path().join("paired-install.reservation.json").exists());
            assert_eq!(
                std::fs::read(dir.path().join(leaf)).unwrap(),
                b"unrelated hot journal"
            );
        }
    }
}
