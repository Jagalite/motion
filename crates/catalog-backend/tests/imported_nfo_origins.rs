//! Genuine direct activated archive provenance, never fabricated receipts.
#![cfg(unix)]
use catabolic_runtime::helper::{ClosedObservationProtocol, TrustedClosedObservationArtifact};
use motion_catalog_backend::{
    NativeCatalog,
    nfo_bootstrap::imported_origins::{self, ArchivedOrigin},
};
use serde_json::json;
use sqlx::SqlitePool;
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::MetadataExt,
    path::PathBuf,
    time::{Duration, UNIX_EPOCH},
};

struct Fixture {
    _root: tempfile::TempDir,
    native: PathBuf,
    db: SqlitePool,
    backend: NativeCatalog,
}
impl Fixture {
    async fn new(foreign: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let native = root.path().join("native.sqlite3");
        motion_catalog_backend::initialize(native.clone())
            .await
            .unwrap();
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .after_connect(|c, _| {
                Box::pin(motion_catalog_backend::projection::configure_connection(c))
            })
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(root.path().join("motion.sqlite3"))
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("../../migrations").run(&db).await.unwrap();
        let media = root.path().join("media");
        fs::create_dir(&media).unwrap();
        fs::write(media.join("one.txt"), b"exact bytes").unwrap();
        let directory = fs::metadata(&media).unwrap();
        let file = fs::metadata(media.join("one.txt")).unwrap();
        let fingerprint = format!(
            "{}:{}:{}:{}:{}:{}",
            file.len(),
            file.modified()
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            file.dev(),
            file.ino(),
            file.ctime(),
            file.ctime_nsec()
        );
        sqlx::query(
            "INSERT INTO libraries(id,name,root,root_identity) VALUES('source','Source',?,?)",
        )
        .bind(media.to_str().unwrap())
        .bind(format!("{}:{}", directory.dev(), directory.ino()))
        .execute(&db)
        .await
        .unwrap();
        sqlx::raw_sql("INSERT INTO items(id,title,kind) VALUES('work','Work','video'); INSERT INTO item_origins VALUES('work','Work'); INSERT INTO editions(id,item_id,label) VALUES('edition','work','Original'); INSERT INTO timelines(id,edition_id,revision,duration_ms) VALUES('timeline','edition',7,1000); INSERT INTO media_versions(id,timeline_id,revision,label,origin,equivalence) VALUES('version','timeline',9,'Source','original','declared');").execute(&db).await.unwrap();
        sqlx::query("INSERT INTO media_files(id,edition_id,library_id,relative_path,revision,fingerprint,bytes,duration_seconds) VALUES('file','edition','source','one.txt','original-revision',?,11,1.0)").bind(fingerprint).execute(&db).await.unwrap();
        sqlx::query("INSERT INTO version_files(version_id,part,file_id,file_revision) VALUES('version',1,'file','original-revision')").execute(&db).await.unwrap();
        if foreign {
            sqlx::raw_sql("INSERT INTO items(id,title,kind) VALUES('foreign','Foreign','video'); INSERT INTO item_origins VALUES('foreign','Foreign');").execute(&db).await.unwrap();
        }
        let subject = if foreign { "foreign" } else { "work" };
        sqlx::query("INSERT INTO metadata_documents VALUES(?,'nfo',3,NULL,?,1)")
            .bind(subject)
            .bind(json!({"values":{"title":"NFO"},"tags":["drama"],"excluded_tags":[]}).to_string())
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO nfo_origins VALUES(?,'file')")
            .bind(subject)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO timeline_viewing(profile_id,timeline_id,revision,manual_epoch,position_ms,updated_at) VALUES('default','timeline',4,2,250,1)").execute(&db).await.unwrap();
        let backend = NativeCatalog::bind(
            db.clone(),
            native.clone(),
            motion_catalog_backend::inspect(native.clone())
                .await
                .unwrap()
                .identity,
        )
        .await
        .unwrap();
        backend.register_motion_source("source").await.unwrap();
        let binary = std::path::Path::new(env!("CARGO_BIN_EXE_motion-catalog-observer"));
        let helper = TrustedClosedObservationArtifact::after_packaging_verification(
            binary,
            &catabolic_core::sha256(fs::read(binary).unwrap()),
            ClosedObservationProtocol::NoChildWithExactJoinV2,
        )
        .unwrap();
        backend
            .scan_source(
                "source",
                helper,
                Default::default(),
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        Self {
            _root: root,
            native,
            db,
            backend,
        }
    }
    async fn history(&self) -> (String, String, i64, String, i64, i64, i64) {
        sqlx::query_as("SELECT o.item_id,o.file_id,d.revision,d.document_json,v.revision,v.manual_epoch,v.position_ms FROM nfo_origins o JOIN metadata_documents d ON d.item_id=o.item_id AND d.source='nfo' JOIN timeline_viewing v ON v.timeline_id='timeline'").fetch_one(&self.db).await.unwrap()
    }
}

#[tokio::test]
async fn actual_scan_and_activated_direct_archive_preserve_physical_nfo_origin() {
    let f = Fixture::new(false).await;
    let before = f.history().await;
    f.backend.freeze_motion_source("origin", 0).await.unwrap();
    f.backend.capture_motion_import("origin").await.unwrap();
    let inventory = f
        .backend
        .motion_reference_inventory("origin")
        .await
        .unwrap();
    assert!(inventory.activation_ready);
    assert_eq!(inventory.unresolved_evidence, 0);
    for key in [
        "nfo_origins.item_id->items.id",
        "nfo_origins.file_id->media_files.id",
    ] {
        assert_eq!(inventory.physical_coverage.reference_slots[key].resolved, 1);
    }
    let saved:String=sqlx::query_scalar("SELECT mapping_json FROM catabolic_occurrence_mappings WHERE import_id='origin' AND motion_file_id='file'").fetch_one(&f.db).await.unwrap();
    let mapping: motion_catalog_backend::occurrence_bridge::OccurrenceMapping =
        serde_json::from_str(&saved).unwrap();
    assert_eq!(serde_json::to_string(&mapping).unwrap(), saved);
    assert!(
        sqlx::query(
            "UPDATE catabolic_occurrence_mappings SET mapping_json=? WHERE import_id='origin'"
        )
        .bind(format!(" {saved}"))
        .execute(&f.db)
        .await
        .is_err(),
        "owner output cannot be rewritten to noncanonical JSON"
    );
    f.backend.stage_import("origin").await.unwrap();
    f.backend.prepare_motion_cutover("origin").await.unwrap();
    f.backend
        .activate_motion_cutover(Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(f.history().await, before);
    for _ in 0..2 {
        let mut server = motion_catalog_backend::cutover::open_server(
            f.db.clone(),
            f.native.clone(),
            "default".into(),
        )
        .await
        .unwrap();
        let host = server.admission.admitted_host().unwrap();
        let generation = host.catalog_generation().unwrap();
        let mut tx = f.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let subjects = BTreeSet::from(["work".to_owned()]);
        let proof = imported_origins::prepare_in(&mut tx, host, generation, &subjects)
            .await
            .unwrap();
        let subject = &proof.subjects()["work"];
        assert_eq!(subject.metadata_part(), 0);
        assert_eq!(subject.original_item_id(), "work");
        match subject.origin() {
            ArchivedOrigin::Physical {
                physical_part,
                motion_file_id,
                saved_mapping_digest,
                physical,
            } => {
                assert_eq!(*physical_part, 0);
                assert_eq!(motion_file_id, "file");
                assert_eq!(
                    saved_mapping_digest,
                    &catabolic_core::sha256(saved.as_bytes())
                );
                assert_eq!(physical, &mapping.physical);
            }
            _ => panic!("real saved physical origin must not become audited absence"),
        }
        assert!(
            imported_origins::prepare_in(&mut tx, host, generation + 1, &subjects)
                .await
                .is_err()
        );
        assert!(
            imported_origins::prepare_in(
                &mut tx,
                host,
                generation,
                &BTreeSet::from(["missing".into()])
            )
            .await
            .is_err()
        );
        tx.rollback().await.unwrap();
        host.close().unwrap();
        drop(server);
        assert_eq!(f.history().await, before);
    }
}

#[tokio::test]
async fn foreign_origin_and_aggregate_corridor_remain_closed_without_partial_capture() {
    let f = Fixture::new(true).await;
    let before = f.history().await;
    f.backend
        .freeze_motion_source("foreign-origin", 0)
        .await
        .unwrap();
    f.backend
        .capture_motion_import("foreign-origin")
        .await
        .unwrap();
    assert!(
        !f.backend
            .motion_reference_inventory("foreign-origin")
            .await
            .unwrap()
            .activation_ready
    );
    f.backend.stage_import("foreign-origin").await.unwrap();
    assert!(
        f.backend
            .prepare_motion_cutover("foreign-origin")
            .await
            .is_err()
    );
    assert_eq!(f.history().await, before);
    let f = Fixture::new(false).await;
    let before = f.history().await;
    f.backend
        .freeze_motion_source("origin-plan", 0)
        .await
        .unwrap();
    assert!(
        f.backend
            .capture_motion_export_plan("origin-plan")
            .await
            .unwrap_err()
            .to_string()
            .contains("origin-bearing aggregate")
    );
    for table in [
        "catabolic_export_plans",
        "catabolic_import_snapshots",
        "catabolic_motion_exports",
        "catabolic_reference_audits",
        "catabolic_occurrence_mappings",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&f.db)
            .await
            .unwrap();
        assert_eq!(count, 0, "{table} rolled back");
    }
    assert_eq!(f.history().await, before);
}

#[tokio::test]
async fn missing_nfo_document_and_stale_observation_never_gain_origin_admission() {
    for stale in [false, true] {
        let f = Fixture::new(false).await;
        if stale {
            sqlx::query("UPDATE media_files SET fingerprint='stale-observation' WHERE id='file'")
                .execute(&f.db)
                .await
                .unwrap();
        } else {
            sqlx::query("DELETE FROM metadata_documents WHERE item_id='work' AND source='nfo'")
                .execute(&f.db)
                .await
                .unwrap();
        }
        f.backend
            .freeze_motion_source("incomplete-origin", 0)
            .await
            .unwrap();
        f.backend
            .capture_motion_import("incomplete-origin")
            .await
            .unwrap();
        let inventory = f
            .backend
            .motion_reference_inventory("incomplete-origin")
            .await
            .unwrap();
        assert!(!inventory.activation_ready);
        assert!(inventory.unresolved_evidence > 0);
        f.backend.stage_import("incomplete-origin").await.unwrap();
        assert!(
            f.backend
                .prepare_motion_cutover("incomplete-origin")
                .await
                .is_err()
        );
        let origin: (String, String) = sqlx::query_as("SELECT item_id,file_id FROM nfo_origins")
            .fetch_one(&f.db)
            .await
            .unwrap();
        assert_eq!(origin, ("work".into(), "file".into()));
    }
}
