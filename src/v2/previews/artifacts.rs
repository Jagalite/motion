//! Descriptor-relative, exclusively-created preview files. Directories are
//! held containers, not proof of creation ownership or deletion authority.
//! The managed output root must not be redirected to unrelated storage.
//! Names are not artifact
//! authority: durable digests are checked again on every native sheet read.
use playscale_core::previews::{self as core, Layout, Sheet};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};

pub(super) struct Artifact {
    pub bytes: Vec<u8>,
    pub digest: String,
    pub sheet: Sheet,
}

#[cfg(unix)]
fn at(parent: &File, name: &str, flags: i32) -> anyhow::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    anyhow::ensure!(
        !name.is_empty() && !name.contains('/') && name != "." && name != "..",
        "invalid artifact component"
    );
    let name = std::ffi::CString::new(name)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub(super) fn directory(path: &Path) -> anyhow::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    Ok(std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?)
}

fn same(a: &File, b: &File) -> anyhow::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let (a, b) = (a.metadata()?, b.metadata()?);
    Ok(a.dev() == b.dev() && a.ino() == b.ino())
}

pub(super) fn named_directory(path: &Path, held: &File) -> anyhow::Result<()> {
    anyhow::ensure!(same(&directory(path)?, held)?, "preview directory replaced");
    Ok(())
}

fn read(dir: &File, name: &str, bound: u64) -> anyhow::Result<Vec<u8>> {
    let mut file = at(dir, name, libc::O_RDONLY)?;
    let before = file.metadata()?;
    anyhow::ensure!(
        before.is_file() && before.len() > 0 && before.len() <= bound,
        "invalid preview artifact size"
    );
    let mut bytes = Vec::new();
    (&mut file).take(bound + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 == before.len()
            && crate::scan::fingerprint(&before) == crate::scan::fingerprint(&file.metadata()?),
        "preview artifact changed"
    );
    Ok(bytes)
}

pub(super) fn assemble(dir: &File, layout: &Layout) -> anyhow::Result<Vec<Artifact>> {
    anyhow::ensure!(
        layout.tiles > 0 && u64::from(layout.tiles) <= core::MAX_TILES && layout.sheets <= 20,
        "invalid preview layout"
    );
    let mut artifacts = Vec::new();
    let per_sheet = layout.columns * layout.rows;
    for sheet in 0..layout.sheets {
        let mut canvas = image::RgbImage::new(
            layout.tile_width * layout.columns,
            layout.tile_height * layout.rows,
        );
        for within in 0..per_sheet {
            let n = sheet * per_sheet + within;
            if n >= layout.tiles {
                break;
            }
            let bytes = read(
                dir,
                &format!("thumb-{}.jpg", n + 1),
                core::MAX_THUMBNAIL_BYTES,
            )?;
            let dimensions = image::ImageReader::new(std::io::Cursor::new(&bytes))
                .with_guessed_format()?
                .into_dimensions()?;
            anyhow::ensure!(
                dimensions == (layout.tile_width, layout.tile_height),
                "invalid thumbnail dimensions"
            );
            let tile = image::load_from_memory(&bytes)?.to_rgb8();
            image::imageops::replace(
                &mut canvas,
                &tile,
                i64::from(within % layout.columns * layout.tile_width),
                i64::from(within / layout.columns * layout.tile_height),
            );
        }
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 80).encode_image(&canvas)?;
        anyhow::ensure!(
            !bytes.is_empty() && bytes.len() as u64 <= core::MAX_SHEET_BYTES,
            "oversized preview sheet"
        );
        artifacts.push(Artifact {
            digest: super::super::sha256(&bytes),
            sheet: Sheet {
                width: canvas.width(),
                height: canvas.height(),
                bytes: bytes.len() as u64,
            },
            bytes,
        });
    }
    // An unexpected extra tile is not silently discarded.
    match at(
        dir,
        &format!("thumb-{}.jpg", layout.tiles + 1),
        libc::O_RDONLY,
    ) {
        Ok(_) => anyhow::bail!("unexpected preview thumbnail"),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
        Err(error) => return Err(error),
    }
    Ok(artifacts)
}

/// Never adopt or delete an existing target. Failed ACKs leave an orphan,
/// not a destructive path-based cleanup. All bytes originate from assembly.
fn mkdir(parent: &File, name: &str, allow_existing: bool) -> anyhow::Result<File> {
    mkdir_after(parent, name, allow_existing, || Ok(()))
}

fn mkdir_after(
    parent: &File,
    name: &str,
    allow_existing: bool,
    after: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<File> {
    use std::os::fd::AsRawFd;
    anyhow::ensure!(
        !name.is_empty() && !name.contains('/') && name != "." && name != "..",
        "invalid preview component"
    );
    let c = std::ffi::CString::new(name)?;
    if unsafe { libc::mkdirat(parent.as_raw_fd(), c.as_ptr(), 0o700) } != 0 {
        let error = std::io::Error::last_os_error();
        anyhow::ensure!(
            allow_existing && error.kind() == std::io::ErrorKind::AlreadyExists,
            "preview output collision: {error}"
        );
    }
    after()?;
    at(parent, name, libc::O_RDONLY | libc::O_DIRECTORY)
}

pub(super) fn create_staging(root: &Path, name: &str) -> anyhow::Result<File> {
    match std::fs::create_dir(root) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let root_fd = directory(root)?;
    let staging = mkdir(&root_fd, ".staging", true)?;
    let attempt = mkdir(&staging, name, false)?;
    // A directory substituted before open is a container, not trusted output.
    // Rendering starts only into an empty held container.
    capacity_directory(&attempt, 0, &mut 0, &mut 0, 1, 0, false)?;
    named_directory(root, &root_fd)?;
    anyhow::ensure!(
        same(
            &at(&root_fd, ".staging", libc::O_RDONLY | libc::O_DIRECTORY)?,
            &staging
        )?,
        "preview staging ancestor changed"
    );
    Ok(attempt)
}

pub(super) struct PublicationCustody {
    _directories: (File, File, File),
    _files: Vec<File>,
}

pub(super) fn publish(
    root: &Path,
    file: &str,
    revision: &str,
    artifacts: &[Artifact],
) -> anyhow::Result<PublicationCustody> {
    std::fs::create_dir_all(root)?;
    let root_fd = directory(root)?;
    let parent = mkdir(&root_fd, file, true)?;
    let target = mkdir(&parent, revision, false)?;
    let mut files = Vec::new();
    for (i, artifact) in artifacts.iter().enumerate() {
        let name = format!("sheet-{}.jpg", i + 1);
        let mut output = at(
            &target,
            &name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        )?;
        output.write_all(&artifact.bytes)?;
        output.sync_all()?;
        anyhow::ensure!(
            same(&output, &at(&target, &name, libc::O_RDONLY)?)?,
            "preview sheet name replaced"
        );
        anyhow::ensure!(
            super::super::sha256(&read(&target, &name, core::MAX_SHEET_BYTES)?) == artifact.digest,
            "published preview changed"
        );
        files.push(output);
    }
    target.sync_all()?;
    parent.sync_all()?;
    root_fd.sync_all()?;
    named_directory(root, &root_fd)?;
    anyhow::ensure!(
        same(
            &at(&root_fd, file, libc::O_RDONLY | libc::O_DIRECTORY)?,
            &parent
        )? && same(
            &at(&parent, revision, libc::O_RDONLY | libc::O_DIRECTORY)?,
            &target
        )?,
        "preview output directory replaced"
    );
    Ok(PublicationCustody {
        _directories: (root_fd, parent, target),
        _files: files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact() -> Artifact {
        let bytes = b"exact test-owned bytes".to_vec();
        Artifact {
            digest: super::super::super::sha256(&bytes),
            sheet: Sheet {
                width: 1,
                height: 1,
                bytes: bytes.len() as u64,
            },
            bytes,
        }
    }

    #[test]
    fn publication_is_create_only_and_does_not_delete_collision() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("previews");
        publish(&root, "file", "revision", &[artifact()]).unwrap();
        let path = root.join("file/revision/sheet-1.jpg");
        std::fs::write(&path, b"replacement must survive").unwrap();
        assert!(publish(&root, "file", "revision", &[artifact()]).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"replacement must survive");
    }

    #[test]
    fn replaced_staging_directory_is_not_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("staging");
        std::fs::create_dir(&path).unwrap();
        let held = directory(&path).unwrap();
        std::fs::rename(&path, dir.path().join("old")).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(named_directory(&path, &held).is_err());
    }

    #[test]
    fn orphan_apparent_bytes_and_hardlinks_consume_or_refuse_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("previews");
        std::fs::create_dir(&root).unwrap();
        let orphan = root.join("orphan");
        let file = File::create(&orphan).unwrap();
        file.set_len(2 * 1024 * 1024 * 1024).unwrap();
        assert!(capacity(&root, 1).is_err());
        file.set_len(1).unwrap();
        std::fs::hard_link(&orphan, root.join("alias")).unwrap();
        assert!(capacity(&root, 1).is_err());
    }

    #[test]
    fn directory_swap_between_mkdir_and_open_cannot_overwrite_foreign_sheet() {
        let temp = tempfile::tempdir().unwrap();
        let parent = directory(temp.path()).unwrap();
        let name = temp.path().join("revision");
        let target = mkdir_after(&parent, "revision", false, || {
            std::fs::rename(&name, temp.path().join("ours"))?;
            std::fs::create_dir(&name)?;
            std::fs::write(name.join("sheet-1.jpg"), b"foreign sheet")?;
            Ok(())
        })
        .unwrap();
        assert!(
            at(
                &target,
                "sheet-1.jpg",
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(name.join("sheet-1.jpg")).unwrap(),
            b"foreign sheet"
        );
    }

    #[test]
    fn empty_swapped_container_only_receives_exclusively_created_files() {
        let temp = tempfile::tempdir().unwrap();
        let parent = directory(temp.path()).unwrap();
        let name = temp.path().join("revision");
        let target = mkdir_after(&parent, "revision", false, || {
            std::fs::rename(&name, temp.path().join("ours"))?;
            std::fs::create_dir(&name)?;
            Ok(())
        })
        .unwrap();
        let mut file = at(
            &target,
            "sheet-1.jpg",
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        )
        .unwrap();
        file.write_all(b"our exact artifact").unwrap();
        assert!(same(&file, &at(&target, "sheet-1.jpg", libc::O_RDONLY).unwrap()).unwrap());
        assert_eq!(
            std::fs::read(name.join("sheet-1.jpg")).unwrap(),
            b"our exact artifact"
        );
        assert!(
            temp.path().join("ours").is_dir(),
            "no directory cleanup authority was inferred"
        );
    }

    #[test]
    fn symlink_output_directory_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("previews");
        std::fs::create_dir(&root).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("file")).unwrap();
        assert!(publish(&root, "file", "revision", &[artifact()]).is_err());
        assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
    }
}

/// Bounded admission accounting includes unacknowledged/stale files. Cleanup
/// deferral must not permit unlimited retries to accumulate disk output.
pub(super) fn capacity(root: &Path, active: u64) -> anyhow::Result<()> {
    const BUDGET: u64 = 2 * 1024 * 1024 * 1024;
    const ENTRIES: usize = 32_768;
    let reserve = core::MAX_TILES * core::MAX_THUMBNAIL_BYTES + 20 * core::MAX_SHEET_BYTES;
    let mut bytes = reserve
        .checked_mul(active.max(1))
        .ok_or_else(|| anyhow::anyhow!("preview capacity overflow"))?;
    anyhow::ensure!(
        bytes <= BUDGET,
        "preview retained-output capacity exhausted"
    );
    let held = match directory(root) {
        Ok(root) => root,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let mut entries = 0;
    capacity_directory(&held, 0, &mut entries, &mut bytes, ENTRIES, BUDGET, true)?;
    named_directory(root, &held)?;
    Ok(())
}

fn capacity_directory(
    dir: &File,
    depth: usize,
    entries: &mut usize,
    bytes: &mut u64,
    max_entries: usize,
    budget: u64,
    reserve_witnesses: bool,
) -> anyhow::Result<()> {
    use std::os::{fd::AsRawFd, unix::fs::MetadataExt};
    anyhow::ensure!(depth <= 4, "preview inventory depth exceeded");
    struct Entries(*mut libc::DIR);
    impl Drop for Entries {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let duplicate = unsafe { libc::dup(dir.as_raw_fd()) };
    anyhow::ensure!(duplicate >= 0, "preview inventory descriptor unavailable");
    let raw = unsafe { libc::fdopendir(duplicate) };
    if raw.is_null() {
        unsafe { libc::close(duplicate) };
        return Err(std::io::Error::last_os_error().into());
    }
    let stream = Entries(raw);
    loop {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let errno = unsafe { libc::__errno_location() };
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        ))]
        let errno = unsafe { libc::__error() };
        unsafe {
            *errno = 0;
        }
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            if unsafe { *errno } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            break;
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_str()?;
        if name == "." || name == ".." {
            continue;
        }
        *entries += 1;
        anyhow::ensure!(
            *entries < max_entries,
            "preview retained-output inventory exhausted"
        );
        let file = at(dir, name, libc::O_RDONLY)?;
        let metadata = file.metadata()?;
        if metadata.is_dir() {
            capacity_directory(
                &file,
                depth + 1,
                entries,
                bytes,
                max_entries,
                budget,
                reserve_witnesses,
            )?;
        } else {
            anyhow::ensure!(
                metadata.is_file() && metadata.nlink() == 1,
                "preview inventory special or hard-linked entry"
            );
            if reserve_witnesses && name == ".owner" {
                // Recovery can requeue a job while its old child is still
                // witnessed. Count that process even if its old DB phase is
                // no longer running. Double-counting live attempts is safe.
                let path = std::path::PathBuf::from(format!("/dev/fd/{}", file.as_raw_fd()));
                let witness = crate::execution::Witness::open(&path)?;
                if !witness.released() {
                    let reserve =
                        core::MAX_TILES * core::MAX_THUMBNAIL_BYTES + 20 * core::MAX_SHEET_BYTES;
                    *bytes = bytes
                        .checked_add(reserve)
                        .ok_or_else(|| anyhow::anyhow!("preview witness capacity overflow"))?;
                }
            }
            *bytes = bytes
                .checked_add(metadata.len())
                .ok_or_else(|| anyhow::anyhow!("preview capacity overflow"))?;
            anyhow::ensure!(
                *bytes <= budget,
                "preview retained-output capacity exhausted"
            );
        }
    }
    Ok(())
}
