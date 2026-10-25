use super::*;
use std::time::Duration;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}
struct Fixture {
    directory: tempfile::TempDir,
    parent: File,
    source: File,
    expected: Artifact,
}
impl Fixture {
    fn image() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let parent = crate::native_service::pair::open_parent(directory.path()).unwrap();
        let mut source = tempfile::tempfile_in(directory.path()).unwrap();
        let bytes = vec![b'v'; 2 * COPY_BYTES + 23];
        source.write_all(&bytes).unwrap();
        source.sync_all().unwrap();
        let expected = Artifact {
            bytes: bytes.len() as u64,
            sha256: catabolic_core::sha256(&bytes),
        };
        Self {
            directory,
            parent,
            source,
            expected,
        }
    }
    fn preparation(&self, name: &str) -> Preparation {
        Preparation::new(self.directory.path(), name).unwrap()
    }
    fn prepare(
        &mut self,
        preparation: &mut Preparation,
        cancel: &AtomicBool,
        until: Instant,
    ) -> anyhow::Result<()> {
        preparation.prepare_image(
            &self.parent,
            id(&self.parent).unwrap(),
            &mut self.source,
            &self.expected,
            cancel,
            until,
        )
    }
}
const POINTS: [PreparationPoint; 9] = [
    PreparationPoint::ParentAcquired,
    PreparationPoint::FileAcquired,
    PreparationPoint::CopyChunk,
    PreparationPoint::BeforeHash,
    PreparationPoint::HashChunk,
    PreparationPoint::BeforeSync,
    PreparationPoint::AfterSync,
    PreparationPoint::BeforeReady,
    PreparationPoint::EvidencePrepared,
];
fn assert_partial(preparation: &Preparation, fixture: &Fixture, point: PreparationPoint) {
    assert_eq!(preparation.phase(), PreparationPhase::Failed);
    assert_eq!(preparation.original_fds(), preparation.fault_fds.unwrap());
    assert_eq!(
        id(preparation.parent.as_ref().unwrap()).unwrap(),
        id(&fixture.parent).unwrap()
    );
    if point == PreparationPoint::ParentAcquired {
        assert!(preparation.file.is_none());
    } else {
        let file = preparation.file.as_ref().unwrap();
        let stat = file.metadata().unwrap();
        assert!(stat.is_file());
        assert_eq!(stat.nlink(), 0);
        assert_ne!(id(file).unwrap(), id(&fixture.source).unwrap());
        match point {
            PreparationPoint::FileAcquired => assert_eq!(stat.len(), 0),
            PreparationPoint::CopyChunk => {
                assert!(stat.len() > 0 && stat.len() <= COPY_BYTES as u64);
                assert!(stat.len() < fixture.expected.bytes);
            }
            _ => assert_eq!(stat.len(), fixture.expected.bytes),
        }
    }
    assert_eq!(fixture.source.metadata().unwrap().nlink(), 0);
    assert_eq!(
        std::fs::read_dir(fixture.directory.path()).unwrap().count(),
        0
    );
}

#[test]
fn retained_preparation_errors_preserve_exact_parent_and_partial_file_without_retry() {
    for point in POINTS {
        let mut fixture = Fixture::image();
        let mut preparation = fixture.preparation("image.sqlite3");
        preparation.inject(point, PreparationFault::Error);
        let cancel = AtomicBool::new(false);
        assert!(
            fixture
                .prepare(&mut preparation, &cancel, deadline())
                .is_err()
        );
        assert_partial(&preparation, &fixture, point);
        let fds = preparation.original_fds();
        assert!(preparation.finish().is_none());
        assert!(
            fixture
                .prepare(&mut preparation, &cancel, deadline())
                .is_err()
        );
        assert_eq!(preparation.original_fds(), fds);
    }
}

#[test]
fn retained_preparation_unwind_leaves_failed_owner_and_original_partial_descriptors() {
    for point in POINTS {
        let mut fixture = Fixture::image();
        let mut preparation = fixture.preparation("panic.sqlite3");
        preparation.inject(point, PreparationFault::Panic);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            fixture.prepare(&mut preparation, &AtomicBool::new(false), deadline())
        }));
        assert!(result.is_err());
        assert_partial(&preparation, &fixture, point);
        assert!(preparation.finish().is_none());
    }
}

#[test]
fn retained_preparation_cancellation_is_checked_through_copy_hash_and_sync() {
    for point in POINTS {
        let mut fixture = Fixture::image();
        let mut preparation = fixture.preparation("cancel.sqlite3");
        preparation.inject(point, PreparationFault::Cancel);
        let cancel = AtomicBool::new(false);
        assert!(
            fixture
                .prepare(&mut preparation, &cancel, deadline())
                .is_err()
        );
        assert!(cancel.load(Ordering::Acquire));
        assert_partial(&preparation, &fixture, point);
        cancel.store(false, Ordering::Release);
        assert!(
            fixture
                .prepare(&mut preparation, &cancel, deadline())
                .is_err()
        );
        assert!(preparation.finish().is_none());
    }
}

#[test]
fn retained_preparation_uses_original_deadline_even_after_successful_fsync() {
    for point in [
        PreparationPoint::CopyChunk,
        PreparationPoint::HashChunk,
        PreparationPoint::AfterSync,
    ] {
        let mut fixture = Fixture::image();
        let mut preparation = fixture.preparation("expired.sqlite3");
        preparation.inject(point, PreparationFault::Expire);
        let until = Instant::now() + Duration::from_millis(100);
        let error = fixture
            .prepare(&mut preparation, &AtomicBool::new(false), until)
            .unwrap_err();
        assert!(error.to_string().contains("original deadline"));
        assert_partial(&preparation, &fixture, point);
        assert!(
            fixture
                .prepare(&mut preparation, &AtomicBool::new(false), deadline())
                .is_err()
        );
        assert!(preparation.finish().is_none());
    }
    let mut fixture = Fixture::image();
    let mut preparation = fixture.preparation("already-expired.sqlite3");
    assert!(
        fixture
            .prepare(&mut preparation, &AtomicBool::new(false), Instant::now())
            .is_err()
    );
    assert_eq!(preparation.phase(), PreparationPhase::Failed);
    assert_eq!(preparation.original_fds(), (None, None));
}

#[test]
fn retained_preparation_real_parent_and_tmpfile_failures_keep_acquired_parent() {
    let mut fixture = Fixture::image();
    let mut mismatch = fixture.preparation("wrong-parent.sqlite3");
    let (device, inode) = id(&fixture.parent).unwrap();
    assert!(
        mismatch
            .prepare_image(
                &fixture.parent,
                (device, inode + 1),
                &mut fixture.source,
                &fixture.expected,
                &AtomicBool::new(false),
                deadline(),
            )
            .is_err()
    );
    assert_eq!(mismatch.phase(), PreparationPhase::Failed);
    assert_eq!(
        id(mismatch.parent.as_ref().unwrap()).unwrap(),
        (device, inode)
    );
    assert!(mismatch.file.is_none());
    let mut not_directory = fixture.preparation("record.json");
    let regular = tempfile::tempfile_in(fixture.directory.path()).unwrap();
    // A real successful descriptor clone followed by real failing openat.
    assert!(
        not_directory
            .prepare_record(
                &regular,
                id(&regular).unwrap(),
                b"record",
                &AtomicBool::new(false),
                deadline(),
            )
            .is_err()
    );
    assert_eq!(
        id(not_directory.parent.as_ref().unwrap()).unwrap(),
        id(&regular).unwrap()
    );
    assert!(not_directory.file.is_none());
    assert_eq!(not_directory.phase(), PreparationPhase::Failed);
}

#[test]
fn retained_image_refuses_existing_leaf_and_all_sqlite_sidecars_without_overwrite() {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut fixture = Fixture::image();
        let leaf = fixture
            .directory
            .path()
            .join(format!("image.sqlite3{suffix}"));
        std::fs::write(&leaf, b"existing-owned-by-someone-else").unwrap();
        let mut preparation = fixture.preparation("image.sqlite3");
        assert!(
            fixture
                .prepare(&mut preparation, &AtomicBool::new(false), deadline())
                .is_err()
        );
        assert_eq!(preparation.phase(), PreparationPhase::Failed);
        assert!(preparation.parent.is_some());
        assert!(preparation.file.is_none());
        assert_eq!(
            std::fs::read(leaf).unwrap(),
            b"existing-owned-by-someone-else"
        );
    }
}

#[test]
fn retained_source_and_copied_stage_hash_failures_preserve_anonymous_owner() {
    let mut fixture = Fixture::image();
    let mut wrong_source = fixture.preparation("wrong-source.sqlite3");
    fixture.expected.sha256 = "0".repeat(64);
    assert!(
        fixture
            .prepare(&mut wrong_source, &AtomicBool::new(false), deadline())
            .is_err()
    );
    assert_eq!(wrong_source.retained_length(), Some(0));
    assert_eq!(wrong_source.phase(), PreparationPhase::Failed);
    assert!(wrong_source.finish().is_none());

    let mut fixture = Fixture::image();
    let mut corrupt = fixture.preparation("corrupt.sqlite3");
    corrupt.inject(PreparationPoint::BeforeHash, PreparationFault::Corrupt);
    let error = fixture
        .prepare(&mut corrupt, &AtomicBool::new(false), deadline())
        .unwrap_err();
    assert!(error.to_string().contains("stage digest mismatch"));
    assert_partial(&corrupt, &fixture, PreparationPoint::BeforeHash);
    assert!(corrupt.finish().is_none());
}

#[test]
fn retained_source_read_syscall_failure_keeps_new_stage_and_original_source() {
    let mut fixture = Fixture::image();
    let fd = unsafe {
        libc::openat(
            fixture.parent.as_raw_fd(),
            c".".as_ptr(),
            libc::O_TMPFILE | libc::O_EXCL | libc::O_WRONLY | libc::O_CLOEXEC,
            0o600,
        )
    };
    assert!(fd >= 0);
    let mut write_only = unsafe { File::from_raw_fd(fd) };
    write_only
        .write_all(&vec![b'v'; fixture.expected.bytes as usize])
        .unwrap();
    fixture.source = write_only;
    let mut preparation = fixture.preparation("unreadable.sqlite3");
    let source_fd = fixture.source.as_raw_fd();
    let error = fixture
        .prepare(&mut preparation, &AtomicBool::new(false), deadline())
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<io::Error>().unwrap().raw_os_error(),
        Some(libc::EBADF)
    );
    assert_eq!(fixture.source.as_raw_fd(), source_fd);
    assert_eq!(fixture.source.metadata().unwrap().nlink(), 0);
    assert_eq!(preparation.phase(), PreparationPhase::Failed);
    assert!(preparation.parent.is_some());
    assert_eq!(preparation.retained_length(), Some(0));
    assert!(preparation.finish().is_none());
    assert_eq!(
        std::fs::read_dir(fixture.directory.path()).unwrap().count(),
        0
    );
}

#[test]
fn retained_record_partial_error_panic_and_cancel_keep_exact_acquisitions() {
    let record = b"bounded service record";
    for point in POINTS {
        for fault in [
            PreparationFault::Error,
            PreparationFault::Panic,
            PreparationFault::Cancel,
        ] {
            let fixture = Fixture::image();
            let mut preparation = fixture.preparation("partial-record.json");
            let cancel = AtomicBool::new(false);
            preparation.inject(point, fault);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                preparation.prepare_record(
                    &fixture.parent,
                    id(&fixture.parent).unwrap(),
                    record,
                    &cancel,
                    deadline(),
                )
            }));
            if matches!(fault, PreparationFault::Panic) {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_err());
            }
            assert_eq!(preparation.phase(), PreparationPhase::Failed);
            assert_eq!(preparation.original_fds(), preparation.fault_fds.unwrap());
            assert!(preparation.parent.is_some());
            match point {
                PreparationPoint::ParentAcquired => assert!(preparation.file.is_none()),
                PreparationPoint::FileAcquired => {
                    assert_eq!(preparation.retained_length(), Some(0))
                }
                PreparationPoint::CopyChunk => {
                    assert!(preparation.retained_length().unwrap() > 0);
                    assert!(preparation.retained_length().unwrap() <= record.len() as u64);
                }
                _ => assert_eq!(preparation.retained_length(), Some(record.len() as u64)),
            }
            if let Some(file) = &preparation.file {
                assert_eq!(file.metadata().unwrap().nlink(), 0);
            }
            assert!(preparation.finish().is_none());
            assert_eq!(
                std::fs::read_dir(fixture.directory.path()).unwrap().count(),
                0
            );
        }
    }
}

#[test]
fn retained_record_bounds_are_enforced_before_any_descriptor_acquisition() {
    let fixture = Fixture::image();
    for bytes in [Vec::new(), vec![1; RECORD_BYTES + 1]] {
        let mut preparation = fixture.preparation("record.json");
        assert!(
            preparation
                .prepare_record(
                    &fixture.parent,
                    id(&fixture.parent).unwrap(),
                    &bytes,
                    &AtomicBool::new(false),
                    deadline(),
                )
                .is_err()
        );
        assert_eq!(preparation.phase(), PreparationPhase::Failed);
        assert_eq!(preparation.original_fds(), (None, None));
        assert!(preparation.finish().is_none());
    }
    let bytes = vec![b'r'; RECORD_BYTES];
    let mut preparation = fixture.preparation("maximum.json");
    preparation
        .prepare_record(
            &fixture.parent,
            id(&fixture.parent).unwrap(),
            &bytes,
            &AtomicBool::new(false),
            deadline(),
        )
        .unwrap();
    assert_eq!(preparation.retained_length(), Some(RECORD_BYTES as u64));
    assert_eq!(
        preparation
            .publication
            .as_ref()
            .unwrap()
            .evidence()
            .artifact
            .sha256,
        catabolic_core::sha256(&bytes)
    );
}

#[test]
fn retained_preparation_finish_moves_exact_descriptors_and_string_allocations() {
    let mut fixture = Fixture::image();
    let mut preparation = fixture.preparation("ready.sqlite3");
    assert!(preparation.finish().is_none());
    fixture
        .prepare(&mut preparation, &AtomicBool::new(false), deadline())
        .unwrap();
    let fds = preparation.original_fds();
    let publication = preparation.publication.as_ref().unwrap();
    let name = publication.evidence().name.as_ptr();
    let digest = publication.evidence().artifact.sha256.as_ptr();
    let path = preparation
        .path
        .as_ref()
        .unwrap()
        .as_os_str()
        .as_bytes()
        .as_ptr();
    let mut outcome = preparation.finish().unwrap();
    assert_eq!(preparation.phase(), PreparationPhase::Finished);
    assert_eq!(preparation.original_fds(), (None, None));
    assert!(preparation.finish().is_none());
    assert_eq!(outcome.owner.parent.as_raw_fd(), fds.0.unwrap());
    assert_eq!(outcome.original_record_fd(), fds.1.unwrap());
    assert_eq!(outcome.evidence().name.as_ptr(), name);
    assert_eq!(outcome.evidence().artifact.sha256.as_ptr(), digest);
    assert_eq!(outcome.owner.path.as_os_str().as_bytes().as_ptr(), path);
    assert_eq!(&outcome.evidence().artifact, &fixture.expected);
    assert!(!fixture.directory.path().join("ready.sqlite3").exists());
    let cancel = AtomicBool::new(false);
    outcome
        .prepare_link_until(&cancel, deadline())
        .unwrap()
        .publish()
        .unwrap();
    assert_eq!(outcome.phase(), Phase::Durable);
    assert_eq!(outcome.original_record_fd(), fds.1.unwrap());
    assert_eq!(
        id(&File::open(fixture.directory.path().join("ready.sqlite3")).unwrap()).unwrap(),
        id(&outcome.owner.file).unwrap()
    );
    assert_eq!(fixture.source.metadata().unwrap().nlink(), 0);
}

#[test]
fn retained_finished_record_keeps_existing_publication_collision_and_sync_uncertainty() {
    for collision in [false, true] {
        let fixture = Fixture::image();
        let mut preparation = fixture.preparation("record.json");
        let cancel = AtomicBool::new(false);
        preparation
            .prepare_record(
                &fixture.parent,
                id(&fixture.parent).unwrap(),
                b"bounded-record",
                &cancel,
                deadline(),
            )
            .unwrap();
        let fd = preparation.original_fds().1.unwrap();
        let mut outcome = preparation.finish().unwrap();
        if collision {
            std::fs::write(fixture.directory.path().join("record.json"), b"other").unwrap();
        } else {
            outcome.inject_parent_sync_failure();
        }
        assert!(
            outcome
                .prepare_link_until(&cancel, deadline())
                .unwrap()
                .publish()
                .is_err()
        );
        assert_eq!(
            outcome.phase(),
            if collision {
                Phase::Collision
            } else {
                Phase::LinkedUnconfirmed
            }
        );
        assert_eq!(outcome.original_record_fd(), fd);
        assert_eq!(
            outcome.owner.file.metadata().unwrap().nlink(),
            if collision { 0 } else { 1 }
        );
        assert!(outcome.prepare_link_until(&cancel, deadline()).is_err());
        if collision {
            assert_eq!(
                std::fs::read(fixture.directory.path().join("record.json")).unwrap(),
                b"other"
            );
        }
    }
}

#[test]
fn retained_preparation_storage_names_every_buffer_and_preflights_invalid_paths() {
    let layout = Preparation::storage_layout();
    assert_eq!(layout.total(), Preparation::storage_reservation());
    assert_eq!(layout.copy_hash_buffer, COPY_BYTES as u64);
    assert_eq!(layout.record_bytes, RECORD_BYTES as u64);
    assert_eq!(layout.parent_descriptor, std::mem::size_of::<File>() as u64);
    assert_eq!(layout.stage_descriptor, std::mem::size_of::<File>() as u64);
    assert!(3 * layout.total() < 1024 * 1024);
    assert!(Preparation::new(Path::new("relative"), "file.sqlite3").is_err());
    assert!(Preparation::new(Path::new("/tmp"), "../file.sqlite3").is_err());
    assert!(
        Preparation::new(
            Path::new(&format!("/{}", "p".repeat(PATH_BYTES))),
            "file.sqlite3"
        )
        .is_err()
    );
    let fixture = Fixture::image();
    let preparation = fixture.preparation("bounded.sqlite3");
    assert!(preparation.path.as_ref().unwrap().capacity() as u64 <= layout.path);
    assert!(preparation.name.as_ref().unwrap().capacity() as u64 <= layout.name);
    assert!(preparation.digest.capacity() as u64 <= layout.digest);
    assert!(preparation.buffer.capacity() as u64 <= layout.copy_hash_buffer);
    assert!(preparation.leaf.capacity() as u64 <= layout.sidecar_leaf);
}
