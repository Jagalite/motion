//! Unregistered relocation verifier primitive. No schema, native effect or grant.
//! ExpectedCandidates deliberately has no production constructor: the eventual
//! immutable relocation admission owner must mint the complete expected set.
//! A request, deserialized plan or processing-job subset cannot mint it.
use anyhow::{Context, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

struct ExpectedFile {
    sha256: String,
    bytes: u64,
}
struct ExpectedCandidates {
    files: BTreeMap<PathBuf, ExpectedFile>,
}
struct Candidate {
    file: File,
    fingerprint: String,
}

/// Owns actual descriptors and caller custody, not transferable JSON authority.
/// The synchronous verifier must run in the worker that owns the writer/I/O
/// fence; cancellation of an outer waiter must not detach that custody.
struct RetainedCandidateSet<C> {
    root: cap_std::fs::Dir,
    root_path: PathBuf,
    root_fingerprint: String,
    files: BTreeMap<PathBuf, Candidate>,
    _custody: C,
}
fn normalized(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    !text.is_empty()
        && path.is_relative()
        && path.components().all(|c| matches!(c, Component::Normal(_)))
        && !text.contains(['\\', '\0'])
        && text
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}
// Like scan::open_file, this follows intra-root symlinks using cap-std's
// containment rules. It does not claim O_NOFOLLOW. Escaping links are refused;
// retained target identity and locator reopening are checked independently.
fn open(root: &cap_std::fs::Dir, path: &Path) -> anyhow::Result<File> {
    ensure!(normalized(path), "malformed candidate locator");
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = root.open_with(path, &options)?.into_std();
    ensure!(file.metadata()?.is_file(), "candidate is not regular");
    Ok(file)
}
fn root_fingerprint(root: &cap_std::fs::Dir) -> anyhow::Result<String> {
    Ok(crate::scan::fingerprint(
        &root.try_clone()?.into_std_file().metadata()?,
    ))
}
fn verify<C>(
    root_path: &Path,
    expected: &ExpectedCandidates,
    candidates: &[PathBuf],
    stop: &CancellationToken,
    custody: C,
) -> anyhow::Result<RetainedCandidateSet<C>> {
    verify_inner(
        root_path,
        expected,
        candidates,
        stop,
        custody,
        #[cfg(test)]
        None,
    )
}
fn verify_inner<C>(
    root_path: &Path,
    expected: &ExpectedCandidates,
    candidates: &[PathBuf],
    stop: &CancellationToken,
    custody: C,
    #[cfg(test)] mut after_read: Option<Box<dyn FnOnce(&File)>>,
) -> anyhow::Result<RetainedCandidateSet<C>> {
    // scan::fingerprint includes dev/inode/ctime on Unix. Do not silently use
    // its weaker non-Unix representation as relocation identity evidence.
    ensure!(
        cfg!(unix),
        "candidate identity verification requires Unix metadata"
    );
    ensure!(
        !expected.files.is_empty() && expected.files.len() <= 1024,
        "unsupported candidate set size"
    );
    // Bound the supplied slice before allocating even for duplicate-heavy input.
    ensure!(
        candidates.len() == expected.files.len() && candidates.len() <= 1024,
        "candidate count differs from bounded admitted set"
    );
    let supplied: std::collections::BTreeSet<_> = candidates.iter().collect();
    ensure!(
        supplied.len() == candidates.len()
            && supplied.len() == expected.files.len()
            && supplied.iter().all(|p| expected.files.contains_key(*p)),
        "candidate set missing, extra or duplicate"
    );
    for (path, value) in &expected.files {
        ensure!(
            normalized(path)
                && value.sha256.len() == 64
                && value
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "malformed admitted commitment"
        );
    }
    let root = cap_std::fs::Dir::open_ambient_dir(root_path, cap_std::ambient_authority())?;
    let root_fingerprint = root_fingerprint(&root)?;
    let mut files = BTreeMap::new();
    for (path, commitment) in &expected.files {
        ensure!(!stop.is_cancelled(), "candidate verification cancelled");
        let mut file = open(&root, path)?;
        let before = file.metadata()?;
        ensure!(before.len() == commitment.bytes, "candidate length differs");
        let fingerprint = crate::scan::fingerprint(&before);
        let mut hash = Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            ensure!(!stop.is_cancelled(), "candidate verification cancelled");
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            bytes = bytes
                .checked_add(n as u64)
                .context("candidate length overflow")?;
            ensure!(bytes <= commitment.bytes, "candidate grew during hashing");
            hash.update(&buffer[..n]);
            #[cfg(test)]
            if let Some(action) = after_read.take() {
                action(&file);
            }
        }
        ensure!(
            bytes == commitment.bytes && format!("{:x}", hash.finalize()) == commitment.sha256,
            "candidate content differs"
        );
        ensure!(
            crate::scan::fingerprint(&file.metadata()?) == fingerprint,
            "candidate changed during hashing"
        );
        file.seek(SeekFrom::Start(0))?;
        files.insert(path.clone(), Candidate { file, fingerprint });
    }
    let proof = RetainedCandidateSet {
        root,
        root_path: root_path.to_owned(),
        root_fingerprint,
        files,
        _custody: custody,
    };
    proof.revalidate(stop)?;
    Ok(proof)
}
impl<C> RetainedCandidateSet<C> {
    fn revalidate(&self, stop: &CancellationToken) -> anyhow::Result<()> {
        ensure!(!stop.is_cancelled(), "candidate verification cancelled");
        let current_root =
            cap_std::fs::Dir::open_ambient_dir(&self.root_path, cap_std::ambient_authority())?;
        ensure!(
            root_fingerprint(&self.root)? == self.root_fingerprint
                && root_fingerprint(&current_root)? == self.root_fingerprint,
            "candidate root changed"
        );
        for (path, candidate) in &self.files {
            ensure!(!stop.is_cancelled(), "candidate verification cancelled");
            ensure!(
                crate::scan::fingerprint(&candidate.file.metadata()?) == candidate.fingerprint
                    && crate::scan::fingerprint(&open(&self.root, path)?.metadata()?)
                        == candidate.fingerprint,
                "candidate descriptor or locator changed"
            );
        }
        ensure!(
            root_fingerprint(&self.root)? == self.root_fingerprint,
            "candidate root changed during validation"
        );
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, ExpectedCandidates, Vec<PathBuf>) {
        let dir = tempfile::tempdir().unwrap();
        let mut files = BTreeMap::new();
        for (name, bytes) in [
            ("a.wav", b"first".as_slice()),
            ("b.wav", b"second".as_slice()),
        ] {
            std::fs::write(dir.path().join(name), bytes).unwrap();
            files.insert(
                PathBuf::from(name),
                ExpectedFile {
                    sha256: format!("{:x}", Sha256::digest(bytes)),
                    bytes: bytes.len() as u64,
                },
            );
        }
        let paths = files.keys().cloned().collect();
        (dir, ExpectedCandidates { files }, paths)
    }
    #[test]
    fn exact_set_hashes_and_retains_caller_custody() {
        let (dir, expected, paths) = fixture();
        let custody = std::sync::Arc::new(());
        let proof = verify(
            dir.path(),
            &expected,
            &paths,
            &CancellationToken::new(),
            custody.clone(),
        )
        .unwrap();
        assert_eq!(std::sync::Arc::strong_count(&custody), 2);
        proof.revalidate(&CancellationToken::new()).unwrap();
        drop(proof);
        assert_eq!(std::sync::Arc::strong_count(&custody), 1);
    }
    #[test]
    fn malformed_missing_extra_duplicate_and_changed_bytes_refuse() {
        let (dir, mut expected, paths) = fixture();
        let stop = CancellationToken::new();
        for supplied in [
            vec![],
            vec![paths[0].clone()],
            vec![paths[0].clone(), paths[0].clone()],
            vec![paths[0].clone(), paths[1].clone(), "extra".into()],
            vec![paths[0].clone(); 1025],
        ] {
            assert!(verify(dir.path(), &expected, &supplied, &stop, ()).is_err());
        }
        std::fs::write(dir.path().join(&paths[0]), b"other").unwrap();
        assert!(verify(dir.path(), &expected, &paths, &stop, ()).is_err());
        expected.files.get_mut(&paths[0]).unwrap().sha256 = "malformed".into();
        assert!(verify(dir.path(), &expected, &paths, &stop, ()).is_err());
    }
    #[test]
    fn replacement_root_swap_and_cancellation_refuse() {
        let (dir, expected, paths) = fixture();
        let stop = CancellationToken::new();
        let proof = verify(dir.path(), &expected, &paths, &stop, ()).unwrap();
        let replace = dir.path().join("replacement");
        std::fs::write(&replace, b"first").unwrap();
        std::fs::rename(replace, dir.path().join(&paths[0])).unwrap();
        assert!(proof.revalidate(&stop).is_err());
        let (dir, expected, paths) = fixture();
        let proof = verify(dir.path(), &expected, &paths, &stop, ()).unwrap();
        let moved = dir.path().with_extension("moved");
        std::fs::rename(dir.path(), &moved).unwrap();
        std::fs::create_dir(dir.path()).unwrap();
        assert!(proof.revalidate(&stop).is_err());
        std::fs::remove_dir_all(moved).unwrap();
        stop.cancel();
        assert!(verify(dir.path(), &expected, &paths, &stop, ()).is_err());
    }
    #[test]
    fn mutation_and_cancel_during_hash_release_custody_and_descriptors() {
        for cancel in [false, true] {
            let (dir, expected, paths) = fixture();
            let stop = CancellationToken::new();
            let stop_hook = stop.clone();
            let target = dir.path().join(&paths[0]);
            let custody = std::sync::Arc::new(());
            let fd = std::sync::Arc::new(std::sync::atomic::AtomicI32::new(-1));
            let hook_fd = fd.clone();
            let result = verify_inner(
                dir.path(),
                &expected,
                &paths,
                &stop,
                custody.clone(),
                Some(Box::new(move |file| {
                    use std::os::fd::AsRawFd;
                    hook_fd.store(file.as_raw_fd(), std::sync::atomic::Ordering::SeqCst);
                    if cancel {
                        stop_hook.cancel();
                    } else {
                        std::fs::write(target, b"other").unwrap();
                    }
                })),
            );
            assert!(result.is_err());
            let closed = fd.load(std::sync::atomic::Ordering::SeqCst);
            assert!(closed >= 0);
            // No descriptor-opening operation occurs between return and check.
            assert_eq!(unsafe { libc::fcntl(closed, libc::F_GETFD) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
            assert_eq!(std::sync::Arc::strong_count(&custody), 1);
        }
    }
    #[test]
    fn in_place_change_missing_file_escape_and_nonregular_refuse() {
        let (dir, expected, paths) = fixture();
        let stop = CancellationToken::new();
        let proof = verify(dir.path(), &expected, &paths, &stop, ()).unwrap();
        std::fs::write(dir.path().join(&paths[0]), b"other").unwrap();
        assert!(proof.revalidate(&stop).is_err());
        std::fs::remove_file(dir.path().join(&paths[0])).unwrap();
        assert!(verify(dir.path(), &expected, &paths, &stop, ()).is_err());
        std::fs::create_dir(dir.path().join(&paths[0])).unwrap();
        assert!(verify(dir.path(), &expected, &paths, &stop, ()).is_err());
        for malformed in ["", "../outside", "/absolute", "a//b", "a/./b", "a/", "a\\b"] {
            assert!(!normalized(Path::new(malformed)));
        }
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"first").unwrap();
        std::fs::remove_dir(dir.path().join(&paths[0])).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join(&paths[0]))
            .unwrap();
        assert!(verify(dir.path(), &expected, &paths, &stop, ()).is_err());
    }
}
