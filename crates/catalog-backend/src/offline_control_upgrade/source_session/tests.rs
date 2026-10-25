use super::*;
use std::{borrow::Cow, fs::File, sync::atomic::AtomicBool, time::Duration};
async fn upgraded_fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    AdmittedControlStore,
) {
    let (root, path, native, upgraded, _) = upgraded_fixture_with_record(false).await;
    (root, path, native, upgraded)
}
pub(super) async fn upgraded_fixture_with_record(
    recorded: bool,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    AdmittedControlStore,
    Option<RestoreSealSelection>,
) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("motion.sqlite3");
    let native = root.path().join("native.sqlite3");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .after_connect(|db, _| Box::pin(crate::projection::configure_connection(db)))
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    let mut migrations = sqlx::migrate!("../../migrations");
    migrations.migrations = Cow::Owned(
        migrations
            .iter()
            .filter(|m| m.version <= 93)
            .cloned()
            .collect(),
    );
    migrations.run(&pool).await.unwrap();
    crate::initialize(native.clone()).await.unwrap();
    let backend = crate::NativeCatalog::bind(
        pool.clone(),
        native.clone(),
        crate::inspect(native.clone()).await.unwrap().identity,
    )
    .await
    .unwrap();
    backend
        .freeze_motion_source("empty-source-custody", 0)
        .await
        .unwrap();
    backend
        .capture_motion_import("empty-source-custody")
        .await
        .unwrap();
    backend.stage_import("empty-source-custody").await.unwrap();
    backend
        .prepare_motion_cutover("empty-source-custody")
        .await
        .unwrap();
    backend
        .activate_motion_cutover(Duration::from_secs(30))
        .await
        .unwrap();
    drop(backend);
    let binding: String =
        sqlx::query_scalar("SELECT identity_json FROM catabolic_binding WHERE id=1")
            .fetch_one(&pool)
            .await
            .unwrap();
    pool.close().await;
    let reference = UpgradeReference::build().await.unwrap();
    let flag = AtomicBool::new(false);
    let bytes = std::fs::read(&path).unwrap();
    let mut selected = None;
    let upgraded = if recorded {
        use crate::offline_control_upgrade::{
            original,
            record::{RecordAnchor, RecordDirectory},
        };
        use std::{io::Cursor, os::unix::fs::MetadataExt};
        let inspected = crate::motion_snapshot::image_read::inspect(
            Cursor::new(&bytes),
            bytes.len() as u64,
            0,
            &flag,
        )
        .unwrap();
        let original =
            original::capture(&reference, Cursor::new(&bytes), &inspected, 0, &flag).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        let anchor = RecordAnchor {
            operation_id: "reservation-original97".into(),
            // Private selected-host attestation, not a paired-container fixture.
            selected_pair_sha256: catabolic_core::sha256(b"reservation selected pair"),
            original93_image_sha256: original.image_sha256().into(),
            original93_inventory_sha256: original.inventory_sha256().into(),
            stage_device: meta.dev(),
            stage_inode: meta.ino(),
            frozen_binding_sha256: original.frozen_binding_sha256().into(),
        };
        selected = Some(RestoreSealSelection {
            operation_id: "reservation-operation".into(),
            install_id: "reservation-install".into(),
            selected_pair_sha256: anchor.selected_pair_sha256.clone(),
            source_motion_image_sha256: anchor.original93_image_sha256.clone(),
            source_motion_inventory_sha256: anchor.original93_inventory_sha256.clone(),
            source_motion_binding_sha256: original.full_binding_sha256().into(),
            source_native_inventory_sha256: catabolic_core::sha256(
                b"selected native inventory host attestation",
            ),
        });
        let directory = RecordDirectory::from_retained(File::open(root.path()).unwrap()).unwrap();
        reference
            .upgrade_recorded(
                File::open(&path).unwrap(),
                &path,
                &original,
                &directory,
                anchor,
                0,
                &flag,
            )
            .unwrap()
    } else {
        reference
            .upgrade(
                File::open(&path).unwrap(),
                &path,
                &catabolic_core::sha256(&bytes),
                &catabolic_core::sha256(binding.as_bytes()),
                0,
                &flag,
            )
            .unwrap()
    };
    (root, path, native, upgraded, selected)
}
async fn fixture() -> (tempfile::TempDir, std::path::PathBuf, StageSourceSession) {
    fixture_with_archive(false).await
}
async fn fixture_with_archive(
    capture: bool,
) -> (tempfile::TempDir, std::path::PathBuf, StageSourceSession) {
    let (root, path, native, upgraded) = upgraded_fixture().await;
    let q = catabolic_store::migration::NativeUpgradeQuiescence {
        admission_stopped: true,
        readers_and_workers_stopped: true,
        exclusive_operational_ownership: true,
    };
    let prepared = if capture {
        upgraded.prepare_native_with_archive_reserved(
            &native,
            q,
            "empty-source-transition",
            1024 * 1024,
            std::time::Instant::now() + Duration::from_secs(60),
            std::sync::Arc::new(AtomicBool::new(false)),
        )
    } else {
        upgraded.prepare_native_reserved(&native, q, "empty-source-transition", 1024 * 1024)
    };
    let session = prepared
        .unwrap()
        .apply(None)
        .unwrap()
        .save_command("empty-source-bootstrap", &[])
        .unwrap()
        .execute_unacknowledged(
            &[],
            catabolic_store::catalog_commands::ReceiptPolicy {
                retention: Duration::from_secs(3600),
                max_receipts: 100,
                max_hold_records: 100,
                max_logical_bytes: 16 * 1024 * 1024,
            },
        )
        .unwrap()
        .acknowledge()
        .unwrap()
        .into_source_session()
        .await
        .unwrap();
    (root, path, session)
}
async fn pool(path: &std::path::Path) -> sqlx::SqlitePool {
    sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .after_connect(|db, _| Box::pin(crate::projection::configure_connection(db)))
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(path)
                .foreign_keys(true),
        )
        .await
        .unwrap()
}
#[tokio::test]
async fn genuine_completed_pair_rejects_foreign_and_ended_writer() {
    let (root, path, mut session) = fixture().await;
    let original = pool(&path).await;
    let mut tx = original.begin_with("BEGIN IMMEDIATE").await.unwrap();
    session.verify_pair_in(&mut tx).await.unwrap();
    tx.rollback().await.unwrap();
    let mut idle = original.acquire().await.unwrap();
    assert!(session.verify_pair_in(&mut idle).await.is_err());
    drop(idle);
    original.close().await;
    let foreign_path = root.path().join("foreign.sqlite3");
    std::fs::copy(&path, &foreign_path).unwrap();
    let foreign = pool(&foreign_path).await;
    let mut tx = foreign.begin_with("BEGIN IMMEDIATE").await.unwrap();
    assert!(session.verify_pair_in(&mut tx).await.is_err());
    tx.rollback().await.unwrap();
    foreign.close().await;
    session.close().unwrap();
}
#[tokio::test]
async fn genuine_completed_pair_rejects_same_path_replacement() {
    let (root, path, mut session) = fixture().await;
    let original = pool(&path).await;
    let mut tx = original.begin_with("BEGIN IMMEDIATE").await.unwrap();
    session.verify_pair_in(&mut tx).await.unwrap();
    let displaced = root.path().join("displaced.sqlite3");
    std::fs::rename(&path, &displaced).unwrap();
    std::fs::copy(&displaced, &path).unwrap();
    assert!(session.verify_pair_in(&mut tx).await.is_err());
    tx.rollback().await.unwrap();
    original.close().await;
    assert!(session.close().is_err());
}

#[tokio::test]
async fn public_reserved_preparation_refuses_overflow_before_intent_and_native_effect() {
    let (_root, path, native, owner) = upgraded_fixture().await;
    let before = catabolic_core::sha256(std::fs::read(&native).unwrap());
    let upgraded = super::super::facade::UpgradedControlStore { owner };
    assert!(
        upgraded
            .prepare_native_reserved(
                &native,
                catabolic_store::migration::NativeUpgradeQuiescence {
                    admission_stopped: true,
                    readers_and_workers_stopped: true,
                    exclusive_operational_ownership: true,
                },
                "overflow-source-transition",
                u64::MAX,
            )
            .is_err()
    );
    assert_eq!(
        before,
        catabolic_core::sha256(std::fs::read(&native).unwrap())
    );
    assert_eq!(
        crate::inspect(native)
            .await
            .unwrap()
            .identity
            .schema_version,
        40
    );
    let db = Connection::open(&path).unwrap();
    let journal_rows: i64 = db
        .query_row(
            "SELECT (SELECT count(*) FROM catabolic_native_bootstrap_intents)
          + (SELECT count(*) FROM catabolic_native_bootstrap_commands)
          + (SELECT count(*) FROM catabolic_native_bootstrap_acks)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(journal_rows, 0);
}

#[tokio::test]
async fn current_image_retains_original_custody_across_cancel_and_discard() {
    let (_root, path, session) = fixture().await;
    let pool = pool(&path).await;
    let tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let failure = match session.into_current_image(tx, &AtomicBool::new(true)).await {
        Ok(_) => panic!("cancelled capture accepted"),
        Err(failure) => failure,
    };
    assert!(failure.error().to_string().contains("cancelled"));
    let (session, tx, _) = failure.into_parts();
    let mut image = match session
        .into_current_image(tx, &AtomicBool::new(false))
        .await
    {
        Ok(image) => image,
        Err(failure) => panic!("genuine empty current image: {}", failure.error()),
    };
    assert_eq!(image.effect_count(), 0);
    assert_eq!(image.native_inventory_sha256().len(), 64);
    image.verify_current(&AtomicBool::new(false)).await.unwrap();
    let (session, tx) = image.discard_copy();
    let baseline = session.proof.allocation_provenance();
    assert!(baseline.0 < baseline.1);
    assert_eq!(
        baseline.1,
        session.native.external_retained_reservation().unwrap()
    );
    let image = match session
        .into_current_image(tx, &AtomicBool::new(false))
        .await
    {
        Ok(image) => image,
        Err(failure) => panic!("recapture compounded retained scratch: {}", failure.error()),
    };
    let (session, mut tx) = image.discard_copy();
    let after = session.proof.allocation_provenance();
    assert_eq!(after.0, baseline.0, "owned forwarding became a host claim");
    assert!(after.1 >= baseline.1);
    assert_eq!(
        after.1,
        session.native.external_retained_reservation().unwrap()
    );
    session.verify_pair_in(&mut tx).await.unwrap();
    tx.rollback().await.unwrap();
    session.close().unwrap();
    pool.close().await;
}

#[tokio::test]
async fn current_image_refuses_foreign_writer_without_losing_original_session() {
    let (root, path, session) = fixture().await;
    let foreign_path = root.path().join("foreign-current-image.sqlite3");
    std::fs::copy(&path, &foreign_path).unwrap();
    let foreign = pool(&foreign_path).await;
    let tx = foreign.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let failure = match session
        .into_current_image(tx, &AtomicBool::new(false))
        .await
    {
        Ok(_) => panic!("foreign writer accepted"),
        Err(failure) => failure,
    };
    assert!(failure.error().to_string().contains("filename differs"));
    let (session, tx, _) = failure.into_parts();
    tx.rollback().await.unwrap();
    foreign.close().await;
    let original = pool(&path).await;
    let mut tx = original.begin_with("BEGIN IMMEDIATE").await.unwrap();
    session.verify_pair_in(&mut tx).await.unwrap();
    tx.rollback().await.unwrap();
    session.close().unwrap();
    original.close().await;
}

#[tokio::test]
async fn restore_seal_requires_recorded_original_and_returns_same_custody() {
    let (_root, path, session) = fixture().await;
    let pool = pool(&path).await;
    let tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let mut image = match session
        .into_current_image(tx, &AtomicBool::new(false))
        .await
    {
        Ok(image) => image,
        Err(failure) => panic!("genuine current image: {}", failure.error()),
    };
    let provenance = image.allocation_provenance_for_test();
    // Descriptive values are deliberately insufficient: this genuine fixture
    // used the unrecorded control owner and cannot mint a restore reservation.
    let error = image
        .admit_restore_operation(0, 1024, &AtomicBool::new(false))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("durable original97"));
    let image = match image.into_restore_operation() {
        Err(image) => image,
        Ok(_) => panic!("failed admission permitted owned handoff"),
    };
    assert_eq!(
        image.allocation_provenance_for_test(),
        provenance,
        "failed seal must not record an unaccepted reservation"
    );
    let (session, mut tx) = image.discard_copy();
    session.verify_pair_in(&mut tx).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_paired_restore_intents")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(count, 0);
    tx.rollback().await.unwrap();
    session.close().unwrap();
    pool.close().await;
}

#[tokio::test]
async fn current_image_aggregate_refusal_precedes_capture_and_preserves_pair() {
    let (_root, path, mut session) = fixture().await;
    let pool = pool(&path).await;
    let tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    session
        .native
        .refresh_external_reservation(
            catabolic_store::migration::offline_bootstrap::OfflineAllocationReservation::new(
                224 * 1024 * 1024,
            )
            .unwrap(),
        )
        .unwrap();
    let before = session.native.external_retained_reservation().unwrap();
    assert!(session.proof.allocation_provenance().1 < before);
    let failure = match session
        .into_current_image(tx, &AtomicBool::new(false))
        .await
    {
        Ok(_) => panic!("capture admitted independent full allocation allowance"),
        Err(failure) => failure,
    };
    // Current-native capture does not charge the later Motion snapshot scratch.
    // The retained 224 MiB leaves insufficient aggregate remainder for the
    // bounded seal scan. Require this exact pre-capture allocation refusal;
    // unchanged forwarding and original pair custody are checked below.
    assert_eq!(
        failure.error().to_string(),
        "seal transitive allocation exceeds remaining ceiling"
    );
    let (session, mut tx, _) = failure.into_parts();
    assert_eq!(
        session.proof.allocation_provenance(),
        (before, before),
        "unknown direct native claim must remain wholly retained"
    );
    assert_eq!(
        session.native.external_retained_reservation().unwrap(),
        before
    );
    session.verify_pair_in(&mut tx).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_paired_restore_intents")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(count, 0);
    tx.rollback().await.unwrap();
    session.close().unwrap();
    pool.close().await;
}

#[tokio::test]
async fn genuine_archive_moves_through_completed_source_custody_without_grant() {
    let (_root, path, mut session) = fixture_with_archive(true).await;
    let (parts, retained) = session.proof.imported_archive_summary().unwrap().unwrap();
    assert_eq!(parts, 1);
    assert!(retained > 0);
    let (host, forwarded) = session.proof.allocation_provenance();
    assert_eq!(host, 1024 * 1024);
    assert!(forwarded >= host + retained);
    assert!(session.proof.retained_allocations().unwrap() >= retained);
    let pool = pool(&path).await;
    let mut writer = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    session.verify_pair_in(&mut writer).await.unwrap();
    writer.rollback().await.unwrap();
    pool.close().await;
    session.close().unwrap();
}
#[tokio::test]
async fn cancelled_archive_capture_refuses_before_native_acquisition_or_intent() {
    let (_root, path, native, upgraded) = upgraded_fixture().await;
    let before = catabolic_core::sha256(std::fs::read(&native).unwrap());
    assert!(
        upgraded
            .prepare_native_with_archive_reserved(
                &native,
                catabolic_store::migration::NativeUpgradeQuiescence {
                    admission_stopped: true,
                    readers_and_workers_stopped: true,
                    exclusive_operational_ownership: true,
                },
                "cancelled-archive",
                0,
                std::time::Instant::now() + Duration::from_secs(60),
                std::sync::Arc::new(AtomicBool::new(true)),
            )
            .is_err()
    );
    assert_eq!(
        before,
        catabolic_core::sha256(std::fs::read(&native).unwrap())
    );
    assert_eq!(
        crate::inspect(native)
            .await
            .unwrap()
            .identity
            .schema_version,
        40
    );
    let db = Connection::open(path).unwrap();
    let count: i64 = db.query_row("SELECT (SELECT count(*) FROM catabolic_native_bootstrap_intents) + (SELECT count(*) FROM catabolic_native_bootstrap_commands) + (SELECT count(*) FROM catabolic_native_bootstrap_acks)", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn retained_source_close_refuses_genuine_legacy_source_without_admission() {
    use std::os::fd::AsRawFd;
    let (_root, _path, source) = fixture().await;
    let before = (
        source.witness.file.as_raw_fd(),
        source.proof.retained_bootstrap_slot_for_test(),
    );
    assert!(before.1.0.is_none());
    let source = match source.begin_retained_close() {
        Err(source) => source,
        Ok(_) => panic!("legacy proof minted close admission"),
    };
    assert_eq!(
        (
            source.witness.file.as_raw_fd(),
            source.proof.retained_bootstrap_slot_for_test()
        ),
        before
    );
    source.witness.verify(false).unwrap();
}
