use catabolic_store::catalog_import::ImportReceipt;
use motion_catalog_backend::{NativeCatalog, initialize, inspect, migration::Snapshot};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use std::path::PathBuf;

struct Fixture {
    _root: tempfile::TempDir,
    path: PathBuf,
    db: SqlitePool,
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let path = base.join("native.sqlite3");
        initialize(path.clone()).await.unwrap();
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(base.join("motion.sqlite3"))
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        // Real embedded schema; full application upgrade hooks remain covered by
        // the root crate's separate db::connect integration test.
        sqlx::migrate!("../../migrations").run(&db).await.unwrap();
        Self {
            _root: root,
            path,
            db,
        }
    }
    async fn backend(&self) -> NativeCatalog {
        NativeCatalog::bind(
            self.db.clone(),
            self.path.clone(),
            inspect(self.path.clone()).await.unwrap().identity,
        )
        .await
        .unwrap()
    }
    async fn source(&self) {
        sqlx::raw_sql("INSERT INTO items(id,title,kind) VALUES('work-1','Cached','video'); INSERT INTO item_origins VALUES('work-1','Scanned'); INSERT INTO progress VALUES('default','work-1',42,1);")
            .execute(&self.db).await.unwrap();
        sqlx::query("INSERT INTO metadata_documents VALUES('work-1','local',7,NULL,?,1)")
            .bind(json!({"values":{"title":"Local title"},"tags":["favorite"],"excluded_tags":["drama"]}).to_string()).execute(&self.db).await.unwrap();
    }
    async fn snapshot(&self, id: &str) -> Snapshot {
        let raw: String = sqlx::query_scalar(
            "SELECT snapshot_json FROM catabolic_import_snapshots WHERE import_id=?",
        )
        .bind(id)
        .fetch_one(&self.db)
        .await
        .unwrap();
        serde_json::from_str(&raw).unwrap()
    }
}
#[tokio::test]
async fn captures_original_metadata_and_references_then_replays_saved_source() {
    let f = Fixture::new().await;
    f.source().await;
    let backend = f.backend().await;
    let first = backend.capture_motion_import("motion-1").await.unwrap();
    let saved = f.snapshot("motion-1").await;
    let docs: Vec<_> = saved
        .batches
        .iter()
        .filter_map(|b| b.typed.as_ref())
        .flat_map(|t| &t.metadata)
        .collect();
    assert_eq!(docs.len(), 1);
    let local = docs[0]
        .contributions
        .iter()
        .find(|c| c.namespace == "motion.local")
        .unwrap();
    assert_eq!(local.source_revision, 7);
    assert_eq!(local.fields["title"], "Local title");
    let evidence: Vec<_> = saved
        .batches
        .iter()
        .flat_map(|b| &b.unresolved_evidence)
        .collect();
    assert!(
        evidence
            .iter()
            .any(|e| e.data["tags"] == json!(["favorite"])
                && e.data["excluded_tags"] == json!(["drama"]))
    );
    let reference = evidence
        .iter()
        .find(|e| e.data["table"] == "progress")
        .unwrap();
    assert_eq!(
        reference.data["reference_columns"],
        json!({"profile_id":"default","item_id":"work-1"})
    );
    assert!(
        evidence
            .iter()
            .any(|e| e.evidence_id == "motion.reference-audit")
    );
    sqlx::query("UPDATE metadata_documents SET revision=8,document_json=?")
        .bind(json!({"values":{"title":"Later title"},"tags":[],"excluded_tags":[]}).to_string())
        .execute(&f.db)
        .await
        .unwrap();
    assert_eq!(
        backend.capture_motion_import("motion-1").await.unwrap(),
        first
    );
    assert_eq!(
        serde_json::to_value(f.snapshot("motion-1").await).unwrap(),
        serde_json::to_value(saved).unwrap()
    );
    backend.stage_import("motion-1").await.unwrap();
    assert!(backend.validate_import("motion-1", None).await.is_err());
    let result = backend.validate_motion_import("motion-1").await.unwrap();
    let ImportReceipt::ValidatedStaging {
        activation_supported,
        proposed_typed_mappings,
        unresolved_evidence,
        ..
    } = result
    else {
        panic!("{result:?}")
    };
    assert!(!activation_supported);
    assert!(unresolved_evidence >= 4);
    assert!(
        proposed_typed_mappings
            .iter()
            .any(|m| m["namespace"] == "motion.local" && m["source_revision"] == 7)
    );
}
#[tokio::test]
async fn untrusted_snapshot_cannot_claim_motion_export_grants() {
    let f = Fixture::new().await;
    f.source().await;
    let backend = f.backend().await;
    backend.capture_motion_import("trusted").await.unwrap();
    let mut copied = f.snapshot("trusted").await;
    copied.manifest.import_id = "untrusted".into();
    backend.prepare_import(copied).await.unwrap();
    backend.stage_import("untrusted").await.unwrap();
    assert!(
        backend
            .validate_motion_import("untrusted")
            .await
            .unwrap_err()
            .to_string()
            .contains("trusted source export")
    );
    assert!(backend.capture_motion_import("untrusted").await.is_err());
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM catabolic_motion_exports WHERE import_id='untrusted'",
    )
    .fetch_one(&f.db)
    .await
    .unwrap();
    assert_eq!(count, 0);
    assert!(
        sqlx::query("DELETE FROM catabolic_motion_exports")
            .execute(&f.db)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn oversized_source_aborts_without_partial_snapshot_or_provenance() {
    let f = Fixture::new().await;
    f.source().await;
    sqlx::query("UPDATE items SET title=?")
        .bind("x".repeat(60001))
        .execute(&f.db)
        .await
        .unwrap();
    let backend = f.backend().await;
    assert!(
        backend
            .capture_motion_import("oversized")
            .await
            .unwrap_err()
            .to_string()
            .contains("exceeds")
    );
    for table in [
        "catabolic_import_snapshots",
        "catabolic_motion_exports",
        "catabolic_commands",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&f.db)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}
#[tokio::test]
async fn empty_catalog_still_requires_an_external_reference_and_freeze_audit() {
    let f = Fixture::new().await;
    let backend = f.backend().await;
    backend.capture_motion_import("empty").await.unwrap();
    backend.stage_import("empty").await.unwrap();
    let result = backend.validate_motion_import("empty").await.unwrap();
    assert!(matches!(
        result,
        ImportReceipt::ValidatedStaging {
            activation_supported: false,
            ..
        }
    ));
    let saved = f.snapshot("empty").await;
    assert!(saved.batches.iter().all(|b| b.items.is_empty()));
    let facts: Vec<Value> = saved
        .batches
        .iter()
        .flat_map(|b| &b.unresolved_evidence)
        .map(|e| e.data.clone())
        .collect();
    assert!(
        facts
            .iter()
            .any(|e| e["writer_freeze"] == "not_established")
    );
}

#[tokio::test]
async fn oversized_metadata_is_not_silently_omitted_from_export() {
    let f = Fixture::new().await;
    f.source().await;
    sqlx::query("UPDATE metadata_documents SET document_json=?")
        .bind(
            json!({"values":{"title":"x".repeat(60001)},"tags":[],"excluded_tags":[]}).to_string(),
        )
        .execute(&f.db)
        .await
        .unwrap();
    let error = f
        .backend()
        .await
        .capture_motion_import("large-document")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("metadata input exceeds"));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_import_snapshots")
        .fetch_one(&f.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn source_freeze_blocks_legacy_writers_and_survives_rebinding() {
    let f = Fixture::new().await;
    f.source().await;
    let backend = f.backend().await;
    let frozen = backend.freeze_motion_source("frozen", 0).await.unwrap();
    assert!(!frozen.writes_allowed());
    assert_eq!(frozen.active_freeze(), Some(("frozen", 1)));
    assert_eq!(
        backend.freeze_motion_source("frozen", 0).await.unwrap(),
        frozen
    );
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_source_events")
        .fetch_one(&f.db)
        .await
        .unwrap();
    assert_eq!(events, 1);
    for statement in [
        "UPDATE items SET title='changed'",
        "DELETE FROM progress",
        "INSERT INTO items(id,title,kind) VALUES('new','New','video')",
        "UPDATE _sqlx_migrations SET success=0",
    ] {
        let error = sqlx::query(statement).execute(&f.db).await.unwrap_err();
        assert!(error.to_string().contains("source is frozen"), "{error}");
    }
    backend.capture_motion_import("frozen").await.unwrap();
    assert_eq!(
        backend.require_frozen_export("frozen").await.unwrap(),
        frozen
    );
    backend.stage_import("frozen").await.unwrap();
    assert!(backend.capture_motion_import("another").await.is_err());
    drop(backend);
    let resumed = f.backend().await;
    assert_eq!(resumed.source_state().await.unwrap(), frozen);
    assert!(resumed.abandon_motion_freeze(99).await.is_err());
    let live = resumed.abandon_motion_freeze(1).await.unwrap();
    assert!(live.writes_allowed());
    sqlx::query("UPDATE items SET title='after abandon'")
        .execute(&f.db)
        .await
        .unwrap();
    assert!(resumed.require_frozen_export("frozen").await.is_err());
    assert!(
        resumed
            .freeze_motion_source("frozen", live.revision)
            .await
            .is_err()
    );
    let next = resumed
        .freeze_motion_source("next", live.revision)
        .await
        .unwrap();
    assert_eq!(next.active_freeze(), Some(("next", 3)));
    assert!(resumed.abandon_motion_freeze(1).await.is_err());
}
#[tokio::test]
async fn sealed_source_cannot_be_abandoned_and_guard_drift_is_detected() {
    let f = Fixture::new().await;
    f.source().await;
    let backend = f.backend().await;
    backend.freeze_motion_source("sealed", 0).await.unwrap();
    backend.capture_motion_import("sealed").await.unwrap();
    let sealed = backend.seal_motion_freeze(1).await.unwrap();
    assert_eq!(backend.seal_motion_freeze(1).await.unwrap(), sealed);
    assert!(backend.abandon_motion_freeze(1).await.is_err());
    assert!(
        sqlx::query("UPDATE items SET title='blocked'")
            .execute(&f.db)
            .await
            .is_err()
    );
    sqlx::raw_sql("DROP TRIGGER motion_source_freeze_update_items")
        .execute(&f.db)
        .await
        .unwrap();
    assert!(
        backend
            .source_state()
            .await
            .unwrap_err()
            .to_string()
            .contains("guard inventory")
    );
    assert!(backend.require_frozen_export("sealed").await.is_err());
}
#[tokio::test]
async fn prefreeze_snapshot_cannot_be_reused_as_a_frozen_source() {
    let f = Fixture::new().await;
    let backend = f.backend().await;
    backend.capture_motion_import("preview").await.unwrap();
    assert!(
        backend
            .freeze_motion_source("preview", 0)
            .await
            .unwrap_err()
            .to_string()
            .contains("predates freeze")
    );
    assert!(backend.source_state().await.unwrap().writes_allowed());
    let guards: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_source_guards")
        .fetch_one(&f.db)
        .await
        .unwrap();
    assert_eq!(guards, 0);
}

#[tokio::test]
async fn competing_freezes_commit_one_owner_and_one_event() {
    let f = Fixture::new().await;
    f.source().await;
    let backend = f.backend().await;
    let (left, right) = tokio::join!(
        backend.freeze_motion_source("left", 0),
        backend.freeze_motion_source("right", 0)
    );
    assert_ne!(left.is_ok(), right.is_ok());
    let winner = left.or(right).unwrap();
    assert_eq!(backend.source_state().await.unwrap(), winner);
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_source_events")
        .fetch_one(&f.db)
        .await
        .unwrap();
    assert_eq!(events, 1);
    assert!(
        sqlx::query("UPDATE items SET title='blocked'")
            .execute(&f.db)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn failed_guard_installation_rolls_back_partial_ddl_and_admission() {
    let f = Fixture::new().await;
    f.source().await;
    let backend = f.backend().await;
    // A later guard collides after earlier guards have already been created.
    sqlx::raw_sql("CREATE TRIGGER motion_source_freeze_insert_items BEFORE INSERT ON items BEGIN SELECT 1; END")
        .execute(&f.db).await.unwrap();
    assert!(
        backend
            .freeze_motion_source("retry-install", 0)
            .await
            .is_err()
    );
    assert!(backend.source_state().await.unwrap().writes_allowed());
    for table in ["catabolic_source_guards", "catabolic_source_events"] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&f.db)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
    let triggers:i64=sqlx::query_scalar("SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND name LIKE 'motion_source_freeze_%'").fetch_one(&f.db).await.unwrap();
    assert_eq!(triggers, 1);
    sqlx::raw_sql("DROP TRIGGER motion_source_freeze_insert_items")
        .execute(&f.db)
        .await
        .unwrap();
    assert_eq!(
        backend
            .freeze_motion_source("retry-install", 0)
            .await
            .unwrap()
            .active_freeze(),
        Some(("retry-install", 1))
    );
}

#[tokio::test]
async fn manual_pins_use_native_owner_and_survive_batch_packing_and_restart() {
    let f = Fixture::new().await;
    for (id, state) in [
        ("a", "manual"),
        ("b", "manual"),
        ("c", "matched"),
        ("d", "unmatched"),
        ("e", "ambiguous"),
        ("retired", "manual"),
    ] {
        sqlx::query("INSERT INTO items(id,title,kind,match_state) VALUES(?,?,'video',?)")
            .bind(id)
            .bind(id)
            .bind(state)
            .execute(&f.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO item_origins VALUES(?,?)")
            .bind(id)
            .bind(id)
            .execute(&f.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO metadata_documents VALUES(?,'tmdb',7,?, ?,1)")
            .bind(id)
            .bind(format!("id-{id}"))
            .bind(json!({"values":{},"tags":[],"excluded_tags":[]}).to_string())
            .execute(&f.db)
            .await
            .unwrap();
    }
    sqlx::raw_sql("INSERT INTO catalog_receipts VALUES('merge','merge',1,'{}'); INSERT INTO item_aliases VALUES('retired','a','merge');")
        .execute(&f.db).await.unwrap();
    let backend = f.backend().await;
    backend.capture_motion_import("pins").await.unwrap();
    let saved = f.snapshot("pins").await;
    assert_eq!(saved.manifest.source_namespace, "motion.schema68.export.v2");
    let pins: Vec<_> = saved
        .batches
        .iter()
        .filter_map(|b| b.typed.as_ref())
        .flat_map(|t| &t.pins)
        .collect();
    assert_eq!(pins.len(), 2);
    assert_eq!(
        pins.iter().map(|p| p.item_id.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert!(
        pins.iter()
            .all(|p| p.namespace == "tmdb" && p.history[0].source_revision == 7)
    );
    // Capture is immutable even if the source subsequently releases a pin.
    sqlx::query("UPDATE items SET match_state='unmatched' WHERE id='a'")
        .execute(&f.db)
        .await
        .unwrap();
    backend.stage_import("pins").await.unwrap();
    drop(backend);
    let backend = f.backend().await;
    assert!(backend.validate_import("pins", None).await.is_err());
    let ImportReceipt::ValidatedStaging {
        proposed_typed_mappings,
        activation_supported,
        ..
    } = backend.validate_motion_import("pins").await.unwrap()
    else {
        panic!("expected validation")
    };
    let mapped: Vec<_> = proposed_typed_mappings
        .iter()
        .filter(|v| v["owner"] == "pins")
        .collect();
    assert_eq!(mapped.len(), 2);
    assert!(
        mapped
            .iter()
            .all(|v| v["source_revision"] == 7 && v["catalog_revision"] == 1)
    );
    assert!(!activation_supported);
    // A newly captured snapshot observes the release, never a retained set decision.
    backend.capture_motion_import("released").await.unwrap();
    let saved = f.snapshot("released").await;
    let pins: Vec<_> = saved
        .batches
        .iter()
        .filter_map(|b| b.typed.as_ref())
        .flat_map(|t| &t.pins)
        .collect();
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].item_id, "b");
}
