use super::*;
use std::{
    fs,
    os::unix::fs::{OpenOptionsExt, symlink},
};

struct Fixture {
    root: tempfile::TempDir,
    directory: RecordDirectory,
    anchor: RecordAnchor,
    reference: ClosedReference,
    guards: Vec<(String, (String, String))>,
    additions: ControlAdditions,
}

impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let parent = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(root.path())
            .unwrap();
        let stage = File::create(root.path().join("stage.sqlite3")).unwrap();
        let metadata = stage.metadata().unwrap();
        let directory = RecordDirectory::from_retained(parent).unwrap();
        let reference = ClosedReference::build().await.unwrap();
        // This primitive test supplies the real guard producer with the three
        // relevant table names. Full closed97 admission is the parent owner's
        // responsibility and is exercised by its real migration tests.
        let schema = rusqlite::Connection::open_in_memory().unwrap();
        schema
            .execute_batch(
                "CREATE TABLE catabolic_cutover(singleton INTEGER,phase TEXT);
             CREATE TABLE catabolic_cutover_sets(singleton INTEGER,phase TEXT);
             CREATE TABLE preview_sheet_artifacts(file_id TEXT);",
            )
            .unwrap();
        let guards = crate::source_freeze::preview97_guard_definitions_in(&schema).unwrap();
        let additions = ControlAdditions {
            ledger: reference
                .suffix()
                .iter()
                .enumerate()
                .map(|(index, migration)| {
                    vec![
                        Value::Integer(100 + index as i64),
                        Value::Integer(migration.version),
                        Value::Text(migration.description.to_string()),
                        Value::Text("2026-10-21 14:00:00".into()),
                        Value::Integer(1),
                        Value::Blob(migration.checksum.to_vec()),
                        Value::Integer(0),
                    ]
                })
                .collect(),
            guards: guards
                .iter()
                .enumerate()
                .map(|(index, (name, (table, sql)))| {
                    vec![
                        Value::Integer(200 + index as i64),
                        Value::Text(name.clone()),
                        Value::Text(table.clone()),
                        Value::Text(sql.clone()),
                    ]
                })
                .collect(),
        };
        let anchor = RecordAnchor {
            operation_id: "control-upgrade-test".into(),
            selected_pair_sha256: "a".repeat(64),
            original93_image_sha256: "b".repeat(64),
            original93_inventory_sha256: "c".repeat(64),
            stage_device: metadata.dev(),
            stage_inode: metadata.ino(),
            frozen_binding_sha256: "d".repeat(64),
        };
        Self {
            root,
            directory,
            anchor,
            reference,
            guards,
            additions,
        }
    }

    fn prepared(&self) -> PreparedRecord {
        PreparedRecord::new(
            self.anchor.clone(),
            &self.additions,
            &self.reference,
            &self.guards,
        )
        .unwrap()
    }
    fn load(&self) -> anyhow::Result<Option<DurableRecord>> {
        self.directory
            .load_exact(&self.anchor, &self.reference, &self.guards)
    }
    fn path(&self) -> std::path::PathBuf {
        self.root.path().join(FILE_NAME)
    }
}

#[tokio::test]
async fn create_fsync_reload_preserves_exact_typed_additions_without_completion_authority() {
    let fixture = Fixture::new().await;
    fixture.directory.require_absent().unwrap();
    assert!(fixture.load().unwrap().is_none());
    let prepared = fixture.prepared();
    let created = fixture.directory.create_only(&prepared).unwrap();
    assert!(fixture.directory.require_absent().is_err());
    created.verify().unwrap();
    let expected_bytes = fs::read(fixture.path()).unwrap();
    assert_eq!(created.sha256(), catabolic_core::sha256(&expected_bytes));
    assert_eq!(created.anchor(), &fixture.anchor);
    assert_eq!(created.additions().ledger, fixture.additions.ledger);
    assert_eq!(created.additions().guards, fixture.additions.guards);
    drop(created);
    let loaded = fixture.load().unwrap().unwrap();
    loaded.verify().unwrap();
    assert_eq!(loaded.additions().ledger, fixture.additions.ledger);
    assert_eq!(loaded.additions().guards, fixture.additions.guards);
    assert!(fixture.directory.create_only(&prepared).is_err());
    assert_eq!(fs::read(fixture.path()).unwrap(), expected_bytes);
}

#[tokio::test]
async fn changed_operation_pair_original_stage_or_binding_cannot_load_record() {
    let fixture = Fixture::new().await;
    fixture.directory.create_only(&fixture.prepared()).unwrap();
    for field in 0..7 {
        let mut changed = fixture.anchor.clone();
        match field {
            0 => changed.operation_id.push_str("-changed"),
            1 => changed.selected_pair_sha256 = "1".repeat(64),
            2 => changed.original93_image_sha256 = "2".repeat(64),
            3 => changed.original93_inventory_sha256 = "3".repeat(64),
            4 => changed.stage_device ^= 1,
            5 => changed.stage_inode += 1,
            _ => changed.frozen_binding_sha256 = "4".repeat(64),
        }
        assert!(
            fixture
                .directory
                .load_exact(&changed, &fixture.reference, &fixture.guards)
                .is_err()
        );
    }
}

#[tokio::test]
async fn every_ledger_authority_field_and_storage_class_is_checked_before_write() {
    let fixture = Fixture::new().await;
    for (column, replacement) in [
        (0, Value::Integer(101)),
        (1, Value::Integer(93)),
        (2, Value::Text("unreviewed resource".into())),
        (3, Value::Text("2026-02-30 14:00:00".into())),
        (4, Value::Integer(0)),
        (5, Value::Blob(vec![0; 48])),
        (6, Value::Integer(1)),
        (0, Value::Real(100.0)),
        (1, Value::Text("94".into())),
        (3, Value::Null),
        (4, Value::Text("1".into())),
        (5, Value::Text("00".repeat(48))),
        (6, Value::Real(0.0)),
    ] {
        let mut additions = ControlAdditions {
            ledger: fixture.additions.ledger.clone(),
            guards: fixture.additions.guards.clone(),
        };
        additions.ledger[0][column] = replacement;
        assert!(
            PreparedRecord::new(
                fixture.anchor.clone(),
                &additions,
                &fixture.reference,
                &fixture.guards
            )
            .is_err()
        );
    }
    assert!(!fixture.path().exists());
}

#[tokio::test]
async fn exact_guard_producer_rows_and_order_are_required() {
    let fixture = Fixture::new().await;
    for (column, replacement) in [
        (0, Value::Integer(201)),
        (1, Value::Text("unknown_guard".into())),
        (2, Value::Text("items".into())),
        (3, Value::Text("CREATE TRIGGER weakened".into())),
        (3, Value::Blob(b"guard is not a BLOB".to_vec())),
    ] {
        let mut additions = ControlAdditions {
            ledger: fixture.additions.ledger.clone(),
            guards: fixture.additions.guards.clone(),
        };
        additions.guards[0][column] = replacement;
        assert!(
            PreparedRecord::new(
                fixture.anchor.clone(),
                &additions,
                &fixture.reference,
                &fixture.guards
            )
            .is_err()
        );
    }
    let mut additions = ControlAdditions {
        ledger: fixture.additions.ledger.clone(),
        guards: fixture.additions.guards.clone(),
    };
    additions.guards.swap(0, 1);
    assert!(
        PreparedRecord::new(
            fixture.anchor.clone(),
            &additions,
            &fixture.reference,
            &fixture.guards
        )
        .is_err()
    );
    additions.guards.pop();
    assert!(
        PreparedRecord::new(
            fixture.anchor.clone(),
            &additions,
            &fixture.reference,
            &fixture.guards
        )
        .is_err()
    );
}

#[tokio::test]
async fn canonical_reload_revalidates_compiled_authority_and_refuses_unknown_fields() {
    let fixture = Fixture::new().await;
    let prepared = fixture.prepared();
    for alteration in 0..8 {
        let mut stored: Stored = serde_json::from_slice(&prepared.bytes).unwrap();
        match alteration {
            0 => stored.contract_version = 2,
            1 => stored.ledger[0][1] = Cell::Integer(93),
            2 => stored.ledger[0][2] = Cell::Text("changed".into()),
            3 => stored.ledger[0][5] = Cell::Blob(vec![1; 48]),
            4 => stored.ledger[0][4] = Cell::Integer(0),
            5 => stored.ledger[0][6] = Cell::Integer(7),
            6 => stored.guards[0][3] = Cell::Text("changed".into()),
            _ => {
                stored.ledger.pop();
            }
        }
        fs::write(fixture.path(), serde_json::to_vec(&stored).unwrap()).unwrap();
        assert!(fixture.load().is_err());
    }
    let mut unknown: serde_json::Value = serde_json::from_slice(&prepared.bytes).unwrap();
    unknown
        .as_object_mut()
        .unwrap()
        .insert("completed".into(), true.into());
    fs::write(fixture.path(), serde_json::to_vec(&unknown).unwrap()).unwrap();
    assert!(fixture.load().is_err());
    let mut noncanonical = prepared.bytes.clone();
    noncanonical.push(b'\n');
    fs::write(fixture.path(), noncanonical).unwrap();
    assert!(fixture.load().is_err());
    fs::write(fixture.path(), &prepared.bytes[..prepared.bytes.len() - 1]).unwrap();
    assert!(fixture.load().is_err());
    assert!(fixture.directory.create_only(&prepared).is_err());
}

#[tokio::test]
async fn every_creation_failure_retains_named_evidence_and_never_overwrites_it() {
    for point in 0..4 {
        let fixture = Fixture::new().await;
        let prepared = fixture.prepared();
        assert!(
            fixture
                .directory
                .create_inner(&prepared, |at| {
                    ensure!(at != point, "injected record durability failure");
                    Ok(())
                })
                .is_err()
        );
        assert!(fixture.path().exists());
        assert!(fixture.directory.require_absent().is_err());
        let retained = fs::read(fixture.path()).unwrap();
        assert!(fixture.directory.create_only(&prepared).is_err());
        assert_eq!(fs::read(fixture.path()).unwrap(), retained);
        if point == 0 {
            assert!(fixture.load().is_err());
        } else {
            // A complete record can be made durable on reload. It still carries
            // no claim that a Motion transaction ever committed.
            let loaded = fixture.load().unwrap().unwrap();
            assert_eq!(loaded.additions().ledger, fixture.additions.ledger);
        }
    }
}

#[tokio::test]
async fn symlinks_directories_hardlinks_and_oversize_records_are_refused() {
    let fixture = Fixture::new().await;
    let target = fixture.root.path().join("external.json");
    fs::write(&target, b"untouched").unwrap();
    symlink(&target, fixture.path()).unwrap();
    assert!(fixture.directory.require_absent().is_err());
    assert!(fixture.load().is_err());
    assert!(fixture.directory.create_only(&fixture.prepared()).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"untouched");
    fs::remove_file(fixture.path()).unwrap();
    fs::create_dir(fixture.path()).unwrap();
    assert!(fixture.directory.require_absent().is_err());
    assert!(fixture.load().is_err());
    assert!(fixture.directory.create_only(&fixture.prepared()).is_err());
    fs::remove_dir(fixture.path()).unwrap();
    let durable = fixture.directory.create_only(&fixture.prepared()).unwrap();
    fs::hard_link(fixture.path(), fixture.root.path().join("linked.json")).unwrap();
    assert!(durable.verify().is_err());
    assert!(fixture.load().is_err());
    fs::remove_file(fixture.root.path().join("linked.json")).unwrap();
    File::options()
        .write(true)
        .open(fixture.path())
        .unwrap()
        .set_len(MAX_BYTES as u64 + 1)
        .unwrap();
    assert!(fixture.load().is_err());
    assert!(durable.verify().is_err());
}

#[tokio::test]
async fn held_named_replacement_and_in_place_byte_changes_are_detected() {
    let fixture = Fixture::new().await;
    let prepared = fixture.prepared();
    let durable = fixture.directory.create_only(&prepared).unwrap();
    let old = fixture.root.path().join("old.json");
    fs::rename(fixture.path(), &old).unwrap();
    fs::write(fixture.path(), &prepared.bytes).unwrap();
    assert!(durable.verify().is_err());
    fs::remove_file(fixture.path()).unwrap();
    fs::rename(&old, fixture.path()).unwrap();
    durable.verify().unwrap();
    let mut changed = prepared.bytes.clone();
    changed[0] = b'[';
    fs::write(fixture.path(), changed).unwrap();
    assert!(durable.verify().is_err());
}

#[test]
fn sqlite_timestamp_format_is_exact_and_calendar_bounded() {
    for valid in [
        "2026-10-21 14:00:00",
        "2024-02-29 23:59:59",
        "2000-02-29 00:00:00",
    ] {
        assert!(timestamp(valid));
    }
    for invalid in [
        "2026-02-29 00:00:00",
        "1900-02-29 00:00:00",
        "2026-04-31 00:00:00",
        "2026-13-01 00:00:00",
        "2026-01-01 24:00:00",
        "2026-01-01 00:60:00",
        "2026-01-01 00:00:60",
        "2026-1-01 00:00:00",
        "2026-01-01T00:00:00",
        "2026-01-01 00:00:00Z",
        "0000-01-01 00:00:00",
        "２０２６-01-01 00:00:00",
    ] {
        assert!(!timestamp(invalid), "{invalid}");
    }
}

#[tokio::test]
async fn escaped_encoding_budget_and_all_dimensions_are_checked_before_value_clones() {
    let fixture = Fixture::new().await;
    let prepared = fixture.prepared();
    let mut stored: Stored = serde_json::from_slice(&prepared.bytes).unwrap();
    for row in &mut stored.guards {
        row[3] = Cell::Text("\0".repeat(MAX_CELL_BYTES));
    }
    assert!(canonical_bytes(&stored).is_err());
    stored = serde_json::from_slice(&prepared.bytes).unwrap();
    stored.ledger[3].pop();
    assert!(decode_additions(&stored).is_err());
    stored = serde_json::from_slice(&prepared.bytes).unwrap();
    stored.guards[2].push(Cell::Integer(7));
    assert!(decode_additions(&stored).is_err());
}

#[tokio::test]
async fn retained_record_counts_capacity_and_exact_verification_buffers() {
    let fixture = Fixture::new().await;
    let prepared = fixture.prepared();
    let mut record = fixture.directory.create_only(&prepared).unwrap();
    let before = record.retained_and_verification_bytes().unwrap();
    assert!(before >= record.bytes.capacity() as u64 + 2 * record.bytes.len() as u64);
    let capacity = record.bytes.capacity();
    record.bytes.reserve(4096);
    let growth = record.bytes.capacity() - capacity;
    assert!(growth > 0);
    assert_eq!(
        record.retained_and_verification_bytes().unwrap(),
        before + growth as u64
    );
    record.verify().unwrap();
    let before = record.retained_and_verification_bytes().unwrap();
    let Value::Text(value) = &mut record.additions.ledger[0][2] else {
        panic!("description must be TEXT")
    };
    let capacity = value.capacity();
    value.reserve(4096);
    let growth = value.capacity() - capacity;
    assert!(growth > 0);
    assert_eq!(
        record.retained_and_verification_bytes().unwrap(),
        before + growth as u64
    );
    record.verify().unwrap();
}

#[test]
fn verification_read_refuses_changed_length_before_copy() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"record").unwrap();
    assert_eq!(read_bounded(file.as_file(), Some(6)).unwrap(), b"record");
    file.as_file().set_len(4096).unwrap();
    let error = read_bounded(file.as_file(), Some(6)).unwrap_err();
    assert!(error.to_string().contains("size changed before read"));
    file.as_file().set_len(2).unwrap();
    let error = read_bounded(file.as_file(), Some(6)).unwrap_err();
    assert!(error.to_string().contains("size changed before read"));
}
