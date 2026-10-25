//! Service-boundary refusal is independent of HTTP gates or runtime presence.
use crate as playscale_app;
#[path = "../tests/support/native_application_fixture.rs"]
mod native_application_fixture;
use native_application_fixture::Fixture;
use std::sync::Arc;

async fn permit(app: &crate::App) -> Arc<tokio::sync::OwnedSemaphorePermit> {
    Arc::new(app.storage.backup_io.clone().acquire_owned().await.unwrap())
}
fn paired_required(error: anyhow::Error) {
    assert!(
        error.to_string().contains("paired native/Motion"),
        "{error:#}"
    );
}
#[tokio::test]
async fn native_binding_refuses_single_database_backup_reuse_and_restore_before_artifacts() {
    let mut fixture = Fixture::new().await;
    // The existing single-database path remains valid for an actual legacy DB.
    let saved = super::requested_backup(&fixture.app, "legacy", false, permit(&fixture.app).await)
        .await
        .unwrap();
    assert!(saved.database_verified && saved.inventory_complete && saved.destination_durable);
    let backup_root = fixture.app.storage.root.join("backups");
    let completed = backup_root.join("request-legacy");
    let manifest = std::fs::read(completed.join("manifest.json")).unwrap();
    let legacy_restore = fixture._dir.path().join("legacy-restored.sqlite3");
    crate::db::restore_into(&completed.join("playscale.sqlite3"), &legacy_restore)
        .await
        .unwrap();
    assert!(legacy_restore.is_file());

    fixture.cutover().await;
    // Even an App whose runtime is missing must obey the durable binding.
    let mut absent_runtime = fixture.app.clone();
    absent_runtime.native_catalog = None;
    let partial = backup_root.join("request-new.partial");
    std::fs::create_dir(&partial).unwrap();
    std::fs::write(partial.join(".playscale-backup"), b"playscale-scheduled-v1").unwrap();
    std::fs::write(partial.join("keep"), b"untouched stage").unwrap();
    for id in ["legacy", "new"] {
        let error =
            super::requested_backup(&absent_runtime, id, false, permit(&absent_runtime).await)
                .await
                .err()
                .expect("native binding must refuse single DB backup");
        paired_required(error);
    }
    paired_required(
        super::make_snapshot(
            &absent_runtime,
            permit(&absent_runtime).await,
            Some("direct".into()),
            false,
        )
        .await
        .err()
        .expect("direct snapshot must refuse native binding"),
    );
    paired_required(
        super::backup(&absent_runtime)
            .await
            .err()
            .expect("automatic backup must refuse native binding"),
    );
    assert_eq!(
        std::fs::read(completed.join("manifest.json")).unwrap(),
        manifest
    );
    assert_eq!(
        std::fs::read(partial.join("keep")).unwrap(),
        b"untouched stage"
    );
    assert!(!backup_root.join("direct").exists());
    assert!(!backup_root.join("direct.partial").exists());
    assert!(!backup_root.join("request-new").exists());
    assert_eq!(std::fs::read_dir(&backup_root).unwrap().count(), 2);

    let restore_parent = fixture._dir.path().join("native-restore-denied");
    paired_required(
        crate::db::restore_into(
            &fixture._dir.path().join("db.sqlite"),
            &restore_parent.join("motion.sqlite3"),
        )
        .await
        .err()
        .expect("native-bound restore requires paired owner"),
    );
    assert!(
        !restore_parent.exists(),
        "refusal precedes directory or staging creation"
    );
}

#[tokio::test]
async fn native_binding_cannot_cross_legacy_publication_custody() {
    let fixture = Fixture::new().await;
    let path = fixture._dir.path().join("binding-race.sqlite3");
    motion_catalog_backend::initialize(path.clone())
        .await
        .unwrap();
    let identity = motion_catalog_backend::inspect(path.clone())
        .await
        .unwrap()
        .identity;
    let custody = super::legacy_publication_custody(&fixture.app.db)
        .await
        .unwrap();
    let db = fixture.app.db.clone();
    let (started, waiting) = tokio::sync::oneshot::channel();
    let binding = tokio::spawn(async move {
        started.send(()).unwrap();
        motion_catalog_backend::NativeCatalog::bind(db, path, identity).await
    });
    waiting.await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !binding.is_finished(),
        "real native binding cannot commit during publication custody"
    );
    custody.rollback().await.unwrap();
    let backend = tokio::time::timeout(std::time::Duration::from_secs(10), binding)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(backend);
    paired_required(
        super::legacy_publication_custody(&fixture.app.db)
            .await
            .err()
            .expect("new or reused artifact acknowledgement must recheck the committed binding"),
    );
}
