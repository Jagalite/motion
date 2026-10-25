//! Filesystem evidence only. The refresh owner separately proves native source,
//! occurrence, work membership, authorization and attempt custody. This value is
//! deliberately not deserializable and is not a native mutation grant.
use super::Sidecar;
use anyhow::{Context, ensure};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

pub(crate) struct Observation {
    root: PathBuf,
    root_identity: String,
    relative: PathBuf,
    fingerprint: Option<String>,
    retained: Option<File>,
    sidecar: Sidecar,
}
fn root_identity(root: &Path) -> anyhow::Result<String> {
    let metadata = std::fs::metadata(root)?;
    ensure!(metadata.is_dir(), "NFO root is not a directory");
    Ok(crate::db::root_identity(&metadata))
}
fn open(root: &Path, relative: &Path) -> anyhow::Result<Option<File>> {
    match crate::scan::open_file(root, relative) {
        Ok(file) => Ok(Some(file)),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}
impl Observation {
    pub(crate) fn sidecar(&self) -> &Sidecar {
        &self.sidecar
    }

    /// Recheck the same path and retained descriptor. Equal file contents at a
    /// different inode are not an unchanged observation. Absence is rechecked,
    /// never inferred from a previous read or an unreadable special file.
    pub(crate) fn revalidate(&self) -> anyhow::Result<()> {
        ensure!(
            root_identity(&self.root)? == self.root_identity,
            "NFO root identity changed"
        );
        let current = open(&self.root, &self.relative)?;
        match (&self.fingerprint, &self.retained, current) {
            (None, None, None) => {}
            (Some(expected), Some(retained), Some(current)) => {
                ensure!(
                    crate::scan::fingerprint(&retained.metadata()?) == *expected
                        && crate::scan::fingerprint(&current.metadata()?) == *expected,
                    "NFO sidecar identity or contents changed"
                );
            }
            _ => anyhow::bail!("NFO sidecar presence changed"),
        }
        ensure!(
            root_identity(&self.root)? == self.root_identity,
            "NFO root changed during validation"
        );
        Ok(())
    }
}

pub(crate) fn observe(
    root: &Path,
    media_relative: &Path,
    expected_root_identity: &str,
    budget: &mut usize,
) -> anyhow::Result<Observation> {
    ensure!(
        media_relative.is_relative()
            && media_relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_))),
        "invalid NFO media path"
    );
    let root_identity = root_identity(root)?;
    ensure!(
        root_identity == expected_root_identity,
        "NFO root differs from admitted source binding"
    );
    let relative = media_relative.with_extension("nfo");
    let mut retained = open(root, &relative)?;
    let (sidecar, fingerprint) = if let Some(file) = retained.as_mut() {
        let before = crate::scan::fingerprint(&file.metadata()?);
        let allowance = (*budget).min(playscale_core::nfo::MAX_BYTES);
        // Eligibility is the existing pure parser's decision. A safely opened
        // regular sidecar can remain unusable without losing its exact custody
        // evidence. Exhaustion performs no content read (not even a sentinel).
        let sidecar = if allowance == 0 {
            Sidecar::Unusable
        } else {
            let mut bytes = Vec::new();
            let read = (&mut *file)
                .take(allowance as u64 + 1)
                .read_to_end(&mut bytes);
            *budget = budget.saturating_sub(bytes.len());
            // A read failure is not evidence of absent or safely observed
            // unusable contents. Keep that filesystem cohort an explicit error.
            read?;
            if bytes.len() > allowance {
                Sidecar::Unusable
            } else {
                match playscale_core::nfo::parse(&bytes) {
                    Ok(parsed) => Sidecar::Parsed(parsed),
                    Err(_) => Sidecar::Unusable,
                }
            }
        };
        ensure!(
            crate::scan::fingerprint(&file.metadata()?) == before,
            "NFO changed during read"
        );
        (sidecar, Some(before))
    } else {
        (Sidecar::Absent, None)
    };
    let observed = Observation {
        root: root.into(),
        root_identity,
        relative,
        fingerprint,
        retained,
        sidecar,
    };
    observed
        .revalidate()
        .context("NFO observation became stale")?;
    Ok(observed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_sidecar_and_absence_are_revalidated_without_substitution() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("film.nfo");
        let mut budget = super::super::SCAN_BUDGET;
        let expected_root = root_identity(root.path()).unwrap();
        let absent = observe(
            root.path(),
            Path::new("film.mp4"),
            &expected_root,
            &mut budget,
        )
        .unwrap();
        assert_eq!(absent.sidecar, Sidecar::Absent);
        absent.revalidate().unwrap();
        std::fs::write(&path, b"<movie><title>Original</title></movie>").unwrap();
        assert!(absent.revalidate().is_err());
        let present = observe(
            root.path(),
            Path::new("film.mp4"),
            &expected_root,
            &mut budget,
        )
        .unwrap();
        present.revalidate().unwrap();
        assert!(matches!(present.sidecar, Sidecar::Parsed(_)));
        std::fs::rename(&path, root.path().join("retained.nfo")).unwrap();
        std::fs::write(&path, b"<movie><title>Original</title></movie>").unwrap();
        assert!(present.revalidate().is_err());
        assert!(
            observe(
                root.path(),
                Path::new("../film.mp4"),
                &expected_root,
                &mut budget
            )
            .is_err()
        );
    }
    fn retained_position(observed: &Observation) -> u64 {
        use std::io::Seek;
        let mut retained = observed.retained.as_ref().unwrap();
        retained.stream_position().unwrap()
    }

    #[cfg(unix)]
    fn assert_replacement_refused(root: &Path, observed: &Observation, bytes: &[u8]) {
        let path = root.join("film.nfo");
        let old = root.join(format!("retained-{}.nfo", uuid::Uuid::new_v4()));
        std::fs::rename(&path, old).unwrap();
        std::fs::write(&path, bytes).unwrap();
        assert!(observed.revalidate().is_err());
    }

    #[test]
    fn malformed_regular_contents_are_unusable_with_retained_exact_identity() {
        let root = tempfile::tempdir().unwrap();
        let expected_root = root_identity(root.path()).unwrap();
        for bytes in [
            b"<movie><title>broken".as_slice(),
            b"\xff\xfe",
            b"<!DOCTYPE movie><movie/>",
        ] {
            let path = root.path().join("film.nfo");
            std::fs::write(&path, bytes).unwrap();
            let exact = crate::scan::fingerprint(&std::fs::metadata(&path).unwrap());
            let mut budget = super::super::SCAN_BUDGET;
            let observed = observe(
                root.path(),
                Path::new("film.mp4"),
                &expected_root,
                &mut budget,
            )
            .unwrap();
            assert_eq!(observed.sidecar, Sidecar::Unusable);
            assert_eq!(observed.fingerprint.as_deref(), Some(exact.as_str()));
            assert!(observed.retained.is_some());
            assert_eq!(retained_position(&observed), bytes.len() as u64);
            assert_eq!(budget, super::super::SCAN_BUDGET - bytes.len());
            observed.revalidate().unwrap();
            #[cfg(unix)]
            assert_replacement_refused(root.path(), &observed, bytes);
        }
    }

    #[test]
    fn bounded_oversize_retains_identity_and_charges_only_the_bounded_read() {
        let root = tempfile::tempdir().unwrap();
        let expected_root = root_identity(root.path()).unwrap();
        for (length, initial_budget) in [
            (
                playscale_core::nfo::MAX_BYTES + 16,
                super::super::SCAN_BUDGET,
            ),
            (128, 7),
        ] {
            let bytes = vec![b'x'; length];
            let path = root.path().join("film.nfo");
            std::fs::write(&path, &bytes).unwrap();
            let exact = crate::scan::fingerprint(&std::fs::metadata(&path).unwrap());
            let mut budget = initial_budget;
            let allowance = budget.min(playscale_core::nfo::MAX_BYTES);
            let observed = observe(
                root.path(),
                Path::new("film.mp4"),
                &expected_root,
                &mut budget,
            )
            .unwrap();
            assert_eq!(observed.sidecar, Sidecar::Unusable);
            assert_eq!(observed.fingerprint.as_deref(), Some(exact.as_str()));
            assert_eq!(retained_position(&observed), (allowance + 1) as u64);
            assert_eq!(budget, initial_budget.saturating_sub(allowance + 1));
            observed.revalidate().unwrap();
            #[cfg(unix)]
            assert_replacement_refused(root.path(), &observed, &bytes);
        }
    }

    #[test]
    fn exhausted_budget_retains_regular_descriptor_without_reading_contents() {
        let root = tempfile::tempdir().unwrap();
        let expected_root = root_identity(root.path()).unwrap();
        let bytes = b"<movie><title>Otherwise usable</title></movie>";
        let path = root.path().join("film.nfo");
        std::fs::write(&path, bytes).unwrap();
        let exact = crate::scan::fingerprint(&std::fs::metadata(&path).unwrap());
        let mut budget = 0;
        let observed = observe(
            root.path(),
            Path::new("film.mp4"),
            &expected_root,
            &mut budget,
        )
        .unwrap();
        assert_eq!(observed.sidecar, Sidecar::Unusable);
        assert_eq!(observed.fingerprint.as_deref(), Some(exact.as_str()));
        assert_eq!(
            retained_position(&observed),
            0,
            "no read or sentinel at exhaustion"
        );
        assert_eq!(budget, 0);
        observed.revalidate().unwrap();
        assert_eq!(retained_position(&observed), 0);
        #[cfg(unix)]
        assert_replacement_refused(root.path(), &observed, bytes);
        // Exhaustion does not turn proven absence into unusable content either.
        let absent = observe(
            root.path(),
            Path::new("missing.mp4"),
            &expected_root,
            &mut budget,
        )
        .unwrap();
        assert_eq!(absent.sidecar, Sidecar::Absent);
        assert!(absent.retained.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn special_sidecars_and_replaced_roots_never_prove_absence() {
        use std::os::unix::ffi::OsStrExt;
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("source");
        std::fs::create_dir(&root).unwrap();
        let expected = root_identity(&root).unwrap();
        let mut budget = super::super::SCAN_BUDGET;
        let path = std::ffi::CString::new(root.join("film.nfo").as_os_str().as_bytes()).unwrap();
        // A FIFO must fail as a special file without waiting for a writer.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(observe(&root, Path::new("film.mp4"), &expected, &mut budget).is_err());
        let mut exhausted = 0;
        assert!(observe(&root, Path::new("film.mp4"), &expected, &mut exhausted).is_err());
        std::fs::remove_file(root.join("film.nfo")).unwrap();
        let absent = observe(&root, Path::new("film.mp4"), &expected, &mut budget).unwrap();
        std::fs::rename(&root, base.path().join("old-source")).unwrap();
        std::fs::create_dir(&root).unwrap();
        assert!(absent.revalidate().is_err());
        assert!(observe(&root, Path::new("film.mp4"), &expected, &mut budget).is_err());
    }
}
