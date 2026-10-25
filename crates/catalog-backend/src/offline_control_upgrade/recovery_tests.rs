use super::*;
use std::io::Cursor;

async fn setup() -> (
    tempfile::TempDir,
    PathBuf,
    Vec<u8>,
    Original93Preservation,
    RecordDirectory,
    RecordAnchor,
) {
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    let reference = UpgradeReference::build().await.unwrap();
    let original = super::super::original::capture(
        &reference,
        Cursor::new(&bytes),
        &inspected,
        0,
        &AtomicBool::new(false),
    )
    .unwrap();
    let stage = root.path().join("conversion-stage.sqlite3");
    std::fs::write(&stage, &bytes).unwrap();
    let identity = std::fs::metadata(&stage).unwrap();
    let directory = RecordDirectory::from_retained(File::open(root.path()).unwrap()).unwrap();
    // A private host-attestation commitment, not a paired-container admission
    // fixture. Original image and stage identities are independently verified.
    let anchor = RecordAnchor {
        operation_id: "actual-control-upgrade".into(),
        selected_pair_sha256: catabolic_core::sha256(b"retained selected-pair attestation"),
        original93_image_sha256: original.image_sha256().into(),
        original93_inventory_sha256: original.inventory_sha256().into(),
        stage_device: identity.dev(),
        stage_inode: identity.ino(),
        frozen_binding_sha256: original.frozen_binding_sha256().into(),
    };
    (root, stage, bytes, original, directory, anchor)
}
fn hash(path: &Path) -> String {
    catabolic_core::sha256(std::fs::read(path).unwrap())
}

#[tokio::test]
async fn completed97_recovers_only_from_original_and_exact_durable_additions() {
    let (_root, stage, _bytes, original, directory, anchor) = setup().await;
    let reference = UpgradeReference::build().await.unwrap();
    let upgraded = reference
        .upgrade_recorded(
            File::open(&stage).unwrap(),
            &stage,
            &original,
            &directory,
            anchor.clone(),
            0,
            &AtomicBool::new(false),
        )
        .unwrap();
    drop(upgraded);
    let recovered = reference
        .recover_completed97(
            File::open(&stage).unwrap(),
            &stage,
            &hash(&stage),
            &original,
            &directory,
            &anchor,
            0,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(
        recovered
            .db
            .query_row("SELECT name FROM profiles WHERE id='retained'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
        "Original profile"
    );
    assert!(
        recovered
            .db
            .execute(
                "INSERT INTO catabolic_native_bootstrap_intents VALUES('unowned','{}')",
                []
            )
            .is_err()
    );
    drop(recovered);
    let mut wrong = anchor.clone();
    wrong.selected_pair_sha256 = catabolic_core::sha256(b"foreign pair");
    assert!(
        reference
            .recover_completed97(
                File::open(&stage).unwrap(),
                &stage,
                &hash(&stage),
                &original,
                &directory,
                &wrong,
                0,
                &AtomicBool::new(false)
            )
            .is_err()
    );
}

#[tokio::test]
async fn completed97_changed_old_row_or_added_timestamp_and_replaced_stage_refuse() {
    let reference = UpgradeReference::build().await.unwrap();
    for kind in 0..3 {
        let (_root, stage, _bytes, original, directory, anchor) = setup().await;
        drop(
            reference
                .upgrade_recorded(
                    File::open(&stage).unwrap(),
                    &stage,
                    &original,
                    &directory,
                    anchor.clone(),
                    0,
                    &AtomicBool::new(false),
                )
                .unwrap(),
        );
        if kind == 2 {
            let old = stage.with_extension("old");
            std::fs::rename(&stage, &old).unwrap();
            std::fs::copy(old, &stage).unwrap();
        } else {
            let db = Connection::open(&stage).unwrap();
            set_ledger(&db, false).unwrap();
            db.execute_batch("BEGIN IMMEDIATE").unwrap();
            if kind == 0 {
                // Test-only corrupt image. The ordinary UPDATE remains refused.
                // Its AFTER event would also hit the frozen change_events table,
                // so suppress that exact event only while altering this one row.
                assert!(
                    db.execute("UPDATE profiles SET name='corrupt' WHERE id='retained'", [])
                        .is_err()
                );
                let names = [
                    "motion_source_freeze_update_profiles",
                    "event_profiles_update",
                ];
                let triggers: Vec<String> = names
                    .iter()
                    .map(|name| {
                        db.query_row(
                            "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name=?1",
                            [name],
                            |row| row.get(0),
                        )
                        .unwrap()
                    })
                    .collect();
                let events: i64 = db
                    .query_row("SELECT count(*) FROM change_events", [], |row| row.get(0))
                    .unwrap();
                db.execute_batch("DROP TRIGGER motion_source_freeze_update_profiles; DROP TRIGGER event_profiles_update").unwrap();
                assert_eq!(
                    db.execute("UPDATE profiles SET name='corrupt' WHERE id='retained'", [])
                        .unwrap(),
                    1
                );
                for trigger in &triggers {
                    db.execute_batch(trigger).unwrap();
                }
                for (name, expected) in names.iter().zip(&triggers) {
                    let actual: String = db
                        .query_row(
                            "SELECT sql FROM sqlite_schema WHERE name=?1",
                            [name],
                            |row| row.get(0),
                        )
                        .unwrap();
                    assert_eq!(&actual, expected);
                }
                assert_eq!(
                    db.query_row("SELECT count(*) FROM change_events", [], |row| row
                        .get::<_, i64>(0))
                        .unwrap(),
                    events
                );
            } else {
                assert!(db.execute("UPDATE _sqlx_migrations SET installed_on='different exact timestamp' WHERE version=94", []).is_err());
                // Existing connection-local capability admits only the ledger.
                set_ledger(&db, true).unwrap();
                assert_eq!(db.execute("UPDATE _sqlx_migrations SET installed_on='different exact timestamp' WHERE version=94", []).unwrap(), 1);
                set_ledger(&db, false).unwrap();
            }
            crate::source_freeze::verify_sync(&db).unwrap();
            db.execute_batch("COMMIT").unwrap();
        }
        assert!(
            reference
                .recover_completed97(
                    File::open(&stage).unwrap(),
                    &stage,
                    &hash(&stage),
                    &original,
                    &directory,
                    &anchor,
                    0,
                    &AtomicBool::new(false)
                )
                .is_err()
        );
    }
}

#[tokio::test]
async fn durable_record_before_commit_is_not_completion_or_implicit_retry() {
    let (_root, stage, _bytes, original, directory, anchor) = setup().await;
    let reference = UpgradeReference::build().await.unwrap();
    FAULT.with(|f| f.set(Some((100, false))));
    let failed = reference.upgrade_recorded(
        File::open(&stage).unwrap(),
        &stage,
        &original,
        &directory,
        anchor.clone(),
        0,
        &AtomicBool::new(false),
    );
    FAULT.with(|f| f.set(None));
    assert!(failed.is_err());
    let db = Connection::open(&stage).unwrap();
    assert_eq!(
        db.query_row("SELECT max(version) FROM _sqlx_migrations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        93
    );
    drop(db);
    assert!(
        reference
            .recover_completed97(
                File::open(&stage).unwrap(),
                &stage,
                &hash(&stage),
                &original,
                &directory,
                &anchor,
                0,
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert!(
        reference
            .upgrade_recorded(
                File::open(&stage).unwrap(),
                &stage,
                &original,
                &directory,
                anchor,
                0,
                &AtomicBool::new(false)
            )
            .is_err(),
        "durable pending record must not be replaced by new timestamps"
    );
}

#[test]
#[ignore = "actual post-COMMIT child process invoked by parent fixture"]
fn recorded_commit_crash_child() {
    let root = PathBuf::from(std::env::var_os("MOTION_RECORDED_ROOT").unwrap());
    let stage = root.join("conversion-stage.sqlite3");
    let bytes = std::fs::read(root.join("original93.image")).unwrap();
    let cancel = AtomicBool::new(false);
    let inspected = crate::motion_snapshot::image_read::inspect(
        Cursor::new(&bytes),
        bytes.len() as u64,
        0,
        &cancel,
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let reference = runtime.block_on(UpgradeReference::build()).unwrap();
    let original =
        super::super::original::capture(&reference, Cursor::new(&bytes), &inspected, 0, &cancel)
            .unwrap();
    let anchor: RecordAnchor =
        serde_json::from_slice(&std::fs::read(root.join("anchor.json")).unwrap()).unwrap();
    let directory = RecordDirectory::from_retained(File::open(&root).unwrap()).unwrap();
    FAULT.with(|f| f.set(Some((101, true))));
    let _ = reference.upgrade_recorded(
        File::open(&stage).unwrap(),
        &stage,
        &original,
        &directory,
        anchor,
        0,
        &cancel,
    );
    panic!("post-COMMIT process checkpoint not reached");
}
#[tokio::test]
async fn actual_process_loss_after_commit_recovers_exact97() {
    let (root, stage, bytes, original, directory, anchor) = setup().await;
    std::fs::write(root.path().join("original93.image"), &bytes).unwrap();
    std::fs::write(
        root.path().join("anchor.json"),
        serde_json::to_vec(&anchor).unwrap(),
    )
    .unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "offline_control_upgrade::recovery::tests::recorded_commit_crash_child",
            "--nocapture",
        ])
        .env("MOTION_RECORDED_ROOT", root.path())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(77));
    let reference = UpgradeReference::build().await.unwrap();
    let recovered = reference
        .recover_completed97(
            File::open(&stage).unwrap(),
            &stage,
            &hash(&stage),
            &original,
            &directory,
            &anchor,
            0,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(
        reference.0.verify_in(&recovered.db).unwrap(),
        ControlVersion::Complete97
    );
    assert_eq!(
        recovered
            .db
            .query_row(
                "SELECT count(*) FROM _sqlx_migrations WHERE version BETWEEN 94 AND 97",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        4
    );
}
