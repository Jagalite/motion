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
            .after_connect(|conn, _| {
                Box::pin(motion_catalog_backend::projection::configure_connection(
                    conn,
                ))
            })
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
    let baseline_triggers: Vec<(String,String)>=sqlx::query_as("SELECT name,sql FROM sqlite_schema WHERE type='trigger' AND name LIKE 'motion_source_freeze_%' ORDER BY name").fetch_all(&f.db).await.unwrap();
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
    assert_eq!(triggers, baseline_triggers.len() as i64 + 1);
    let retained: Vec<(String,String)>=sqlx::query_as("SELECT name,sql FROM sqlite_schema WHERE type='trigger' AND name LIKE 'motion_source_freeze_%' AND name!='motion_source_freeze_insert_items' ORDER BY name").fetch_all(&f.db).await.unwrap();
    assert_eq!(retained, baseline_triggers);
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
    assert_eq!(saved.manifest.source_namespace, "motion.schema68.export.v4");
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

#[tokio::test]
async fn edition_owners_and_timelines_rehearse_without_inventing_equivalence() {
    let f = Fixture::new().await;
    f.source().await;
    sqlx::raw_sql("INSERT INTO editions(id,item_id,label) VALUES('edition-a','work-1','Theatrical'),('edition-b','work-1','Alternate'); INSERT INTO timelines(id,edition_id,revision,duration_ms) VALUES('timeline-a','edition-a',7,90000),('timeline-b','edition-b',3,NULL);")
        .execute(&f.db).await.unwrap();
    let backend = f.backend().await;
    backend.capture_motion_import("structure").await.unwrap();
    let snapshot = f.snapshot("structure").await;
    let editions: Vec<_> = snapshot
        .batches
        .iter()
        .flat_map(|b| &b.items)
        .filter(|i| i.kind == "custom:motion_edition")
        .collect();
    assert_eq!(editions.len(), 2);
    assert_eq!(editions[0].item_id, "motion.edition:edition-a");
    let mutations: Vec<_> = snapshot
        .batches
        .iter()
        .filter_map(|b| b.typed.as_ref())
        .filter_map(|t| t.native.as_ref())
        .flat_map(|n| &n.mutations)
        .collect();
    assert_eq!(mutations.len(), 1);
    assert_eq!(mutations[0]["context"]["context_key"], "work-1");
    let timelines = mutations[0]["timelines"].as_array().unwrap();
    assert_eq!(timelines.len(), 2);
    assert!(
        timelines
            .iter()
            .all(|t| t["identity_state"] == "provisional" && t["coverage"].is_null())
    );
    assert_eq!(mutations[0]["versions"], json!([]));
    assert_eq!(mutations[0]["evidence"], json!([]));
    assert!(
        snapshot
            .batches
            .iter()
            .flat_map(|b| &b.unresolved_evidence)
            .any(|e| e.data["table"] == "timelines" && e.data["row"]["revision"] == 7)
    );
    backend.stage_import("structure").await.unwrap();
    drop(backend);
    let backend = f.backend().await;
    assert!(backend.validate_import("structure", None).await.is_err());
    let ImportReceipt::ValidatedStaging {
        activation_supported,
        proposed_typed_mappings,
        ..
    } = backend.validate_motion_import("structure").await.unwrap()
    else {
        panic!("expected validation")
    };
    assert!(!activation_supported);
    let mapped: Vec<_> = proposed_typed_mappings
        .iter()
        .filter(|m| m["owner"] == "identity")
        .collect();
    assert_eq!(mapped.len(), 1);
    assert_eq!(mapped[0]["result"]["context_id"], "motion.work:work-1");
    assert_eq!(
        mapped[0]["result"]["timelines"],
        json!([
            {"timeline_id":"timeline-a","edition_item_id":"motion.edition:edition-a","revision":1,"identity_state":"provisional","coverage":null},
            {"timeline_id":"timeline-b","edition_item_id":"motion.edition:edition-b","revision":1,"identity_state":"provisional","coverage":null}
        ])
    );
    assert_eq!(mapped[0]["result"]["versions"], json!([]));
}

#[tokio::test]
async fn structural_key_collision_and_timeline_overflow_abort_capture() {
    let f = Fixture::new().await;
    f.source().await;
    sqlx::raw_sql("INSERT INTO editions(id,item_id,label) VALUES('a','work-1','Edition'); INSERT INTO items(id,title,kind) VALUES('motion.edition:a','Collision','video'); INSERT INTO item_origins VALUES('motion.edition:a','Collision');")
        .execute(&f.db).await.unwrap();
    let backend = f.backend().await;
    assert!(
        backend
            .capture_motion_import("collision")
            .await
            .unwrap_err()
            .to_string()
            .contains("collides")
    );
    sqlx::query("DELETE FROM item_origins WHERE item_id='motion.edition:a'")
        .execute(&f.db)
        .await
        .unwrap();
    sqlx::query("DELETE FROM items WHERE id='motion.edition:a'")
        .execute(&f.db)
        .await
        .unwrap();
    for n in 0..65 {
        sqlx::query("INSERT INTO timelines(id,edition_id) VALUES(?,'a')")
            .bind(format!("t{n:03}"))
            .execute(&f.db)
            .await
            .unwrap();
    }
    assert!(
        backend
            .capture_motion_import("overflow")
            .await
            .unwrap_err()
            .to_string()
            .contains("timeline mutation bound")
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_import_snapshots")
        .fetch_one(&f.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_motion_exports")
        .fetch_one(&f.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn v4_preserves_unknown_metadata_and_nested_reference_bytes() {
    let f = Fixture::new().await;
    f.source().await;
    let raw = r#"{ "values":{"title":"Local","extension":{"timeline_id":"historic-timeline"}}, "tags":["favorite"], "excluded_tags":["drama"], "future_schema":{"file_id":"historical-file","revision":8} }"#;
    sqlx::query("UPDATE metadata_documents SET document_json=?")
        .bind(raw)
        .execute(&f.db)
        .await
        .unwrap();
    let backend = f.backend().await;
    backend.capture_motion_import("metadata-v4").await.unwrap();
    let snapshot = f.snapshot("metadata-v4").await;
    let local = snapshot
        .batches
        .iter()
        .filter_map(|b| b.typed.as_ref())
        .flat_map(|t| &t.metadata)
        .flat_map(|m| &m.contributions)
        .find(|c| c.namespace == "motion.local")
        .unwrap();
    assert_eq!(local.evidence["document_json"], raw);
    assert_eq!(
        local.evidence["document"]["future_schema"]["file_id"],
        "historical-file"
    );
    let inventory = backend
        .motion_reference_inventory("metadata-v4")
        .await
        .unwrap();
    assert!(inventory.schema_recorded);
    assert!(!inventory.activation_ready);
    assert!(
        inventory
            .unknown_metadata_fields
            .iter()
            .any(|f| f.field == "future_schema")
    );
    let evidence = snapshot
        .batches
        .iter()
        .flat_map(|b| &b.unresolved_evidence)
        .find(|e| {
            e.evidence_id
                .starts_with("embedded-reference:metadata_documents:")
        })
        .unwrap();
    assert!(
        evidence.data["references"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["path"] == "/values/extension/timeline_id"
                && r["source_id"] == "historic-timeline")
    );
    assert!(
        evidence.data["references"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["path"] == "/future_schema/file_id" && r["source_id"] == "historical-file")
    );
}

#[tokio::test]
async fn reference_inventory_preserves_non_fk_history_without_credentials() {
    let f = Fixture::new().await;
    // Catalog-independent history intentionally retains IDs no longer present in
    // the live catalog. These must not disappear merely because there is no FK.
    sqlx::query("INSERT INTO downloads(id,revision,principal_id,device_id,profile_id,timeline_id,timeline_revision,version_id,file_id,file_revision,content_type,size_bytes,duration_ms,state,created_at,updated_at) VALUES('download',1,'principal','device','default','old-timeline',9,'old-version','old-file','revision','video/mp4',1,1,'ready',1,1)").execute(&f.db).await.unwrap();
    sqlx::query("INSERT INTO content_tickets(id,token_hash,principal_id,credential_hash,file_id,file_revision,purpose,expires_at,revoked) VALUES('ticket','DO-NOT-COPY-TOKEN','principal',NULL,'old-file','revision','playback',99,0)").execute(&f.db).await.unwrap();
    let backend = f.backend().await;
    backend.capture_motion_import("historical").await.unwrap();
    let inventory = backend
        .motion_reference_inventory("historical")
        .await
        .unwrap();
    assert!(
        inventory.slots.iter().any(|s| s.table == "downloads"
            && s.column == "timeline_id"
            && !s.declared_foreign_key)
    );
    assert!(
        inventory.slots.iter().any(|s| s.table == "content_tickets"
            && s.column == "file_id"
            && !s.declared_foreign_key)
    );
    assert!(
        inventory
            .slots
            .iter()
            .any(|s| s.table == "library_sources" && s.target_table == "catalog_libraries")
    );
    let snapshot = f.snapshot("historical").await;
    let raw = serde_json::to_string(&snapshot).unwrap();
    assert!(raw.contains("old-timeline"));
    assert!(raw.contains("old-version"));
    assert!(raw.contains("old-file"));
    assert!(!raw.contains("DO-NOT-COPY-TOKEN"));
    assert!(!inventory.activation_ready);
    sqlx::query("DELETE FROM downloads")
        .execute(&f.db)
        .await
        .unwrap();
    let replay = backend
        .motion_reference_inventory("historical")
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(inventory).unwrap(),
        serde_json::to_value(replay).unwrap()
    );
}

#[tokio::test]
async fn copied_snapshot_cannot_acquire_reference_inventory_authority() {
    let f = Fixture::new().await;
    f.source().await;
    let backend = f.backend().await;
    backend.capture_motion_import("original").await.unwrap();
    let mut copied = f.snapshot("original").await;
    copied.manifest.import_id = "copied".into();
    backend.prepare_import(copied).await.unwrap();
    assert!(
        backend
            .motion_reference_inventory("copied")
            .await
            .unwrap_err()
            .to_string()
            .contains("trusted source export")
    );
}

#[tokio::test]
async fn reserved_native_projection_preserves_source_wire_structure() {
    let f = Fixture::new().await;
    f.source().await;
    sqlx::query("INSERT INTO item_structure(item_id,media_type,parent_id,number,revision) VALUES('work-1','movie',NULL,NULL,7)").execute(&f.db).await.unwrap();
    let backend = f.backend().await;
    backend.capture_motion_import("projection").await.unwrap();
    let snapshot = f.snapshot("projection").await;
    let projection = snapshot
        .batches
        .iter()
        .filter_map(|b| b.typed.as_ref())
        .flat_map(|t| &t.metadata)
        .flat_map(|m| &m.contributions)
        .find(|c| c.namespace == "motion.structure")
        .unwrap();
    assert_eq!(projection.fields["motion_catalog"]["contract_version"], 1);
    assert_eq!(projection.fields["motion_catalog"]["media_type"], "movie");
    assert_eq!(projection.fields["motion_catalog"]["structure_revision"], 7);
    assert_eq!(projection.fields["motion_catalog"]["item_id"], "work-1");
    backend.stage_import("projection").await.unwrap();
    backend.validate_motion_import("projection").await.unwrap();
    for field in [
        "motion_catalog",
        "motion_edition",
        "motion_version",
        "motion_historical_version",
        "motion_historical_file",
        "motion_historical_timeline",
        "motion_source_history",
        "motion_reference_tombstone",
    ] {
        sqlx::query("UPDATE metadata_documents SET document_json=?")
            .bind(
                json!({"values":{field:{"item_id":"forged"}},"tags":[],"excluded_tags":[]})
                    .to_string(),
            )
            .execute(&f.db)
            .await
            .unwrap();
        assert!(
            backend
                .capture_motion_import(&format!("collision-{field}"))
                .await
                .unwrap_err()
                .to_string()
                .contains("reserved native projection")
        );
    }
}

#[tokio::test]
async fn frozen_empty_catalog_uses_no_phantom_items_and_retains_exact_audit() {
    let f = Fixture::new().await;
    let backend = f.backend().await;
    backend
        .freeze_motion_source("empty-cutover", 0)
        .await
        .unwrap();
    let manifest = backend
        .capture_motion_import("empty-cutover")
        .await
        .unwrap();
    assert_eq!(manifest.contract_version, 3);
    assert!(manifest.batches.is_empty());
    assert!(f.snapshot("empty-cutover").await.batches.is_empty());
    assert!(
        backend
            .frozen_motion_sources("empty-cutover")
            .await
            .unwrap()
            .is_empty()
    );
    let inventory = backend
        .motion_reference_inventory("empty-cutover")
        .await
        .unwrap();
    assert!(inventory.empty_catalog);
    assert!(inventory.activation_ready);
    assert_eq!(inventory.unresolved_evidence, 0);
    assert!(!inventory.slots.is_empty());
    assert!(
        sqlx::query("UPDATE catabolic_reference_audits SET audit_json='{}'")
            .execute(&f.db)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM catabolic_reference_audits")
            .execute(&f.db)
            .await
            .is_err()
    );
    backend.stage_import("empty-cutover").await.unwrap();
    assert!(matches!(
        backend
            .validate_motion_import("empty-cutover")
            .await
            .unwrap(),
        ImportReceipt::ValidatedStaging {
            activation_supported: true,
            unresolved_evidence: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn unknown_schema_or_historical_rows_prevent_empty_cutover() {
    let f = Fixture::new().await;
    sqlx::query("CREATE TABLE unknown_extension(id TEXT PRIMARY KEY, timeline_id TEXT)")
        .execute(&f.db)
        .await
        .unwrap();
    let backend = f.backend().await;
    backend
        .freeze_motion_source("unknown-schema", 0)
        .await
        .unwrap();
    let manifest = backend
        .capture_motion_import("unknown-schema")
        .await
        .unwrap();
    assert_eq!(manifest.contract_version, 2);
    let inventory = backend
        .motion_reference_inventory("unknown-schema")
        .await
        .unwrap();
    assert!(!inventory.empty_catalog);
    assert!(!inventory.activation_ready);
    assert_eq!(inventory.unknown_tables, vec!["unknown_extension"]);
    // A schema mutation invalidates installed writer guards, so use a fresh
    // legitimate host instead of silently rebuilding an existing guard seal.
    let f = Fixture::new().await;
    let backend = f.backend().await;
    sqlx::query("INSERT INTO timeline_viewing(profile_id,timeline_id,revision,updated_at) VALUES('default','historic',1,1)").execute(&f.db).await.unwrap();
    backend
        .freeze_motion_source("historical-empty", 0)
        .await
        .unwrap();
    let manifest = backend
        .capture_motion_import("historical-empty")
        .await
        .unwrap();
    assert_eq!(manifest.contract_version, 2);
    let inventory = backend
        .motion_reference_inventory("historical-empty")
        .await
        .unwrap();
    assert!(!inventory.empty_catalog);
    assert!(inventory.activation_ready);
    assert!(inventory.physical_coverage.tombstone_ids["timelines"].contains("historic"));
    assert!(!inventory.physical_coverage.preserved_ids["timelines"].contains("historic"));
}

#[tokio::test]
async fn column_drift_in_exempt_table_blocks_empty_qualification() {
    let f = Fixture::new().await;
    sqlx::query("ALTER TABLE profiles ADD COLUMN opaque_future_reference TEXT")
        .execute(&f.db)
        .await
        .unwrap();
    let backend = f.backend().await;
    backend
        .freeze_motion_source("schema-columns", 0)
        .await
        .unwrap();
    backend
        .capture_motion_import("schema-columns")
        .await
        .unwrap();
    let audit = backend
        .motion_reference_inventory("schema-columns")
        .await
        .unwrap();
    assert!(!audit.activation_ready);
    assert_eq!(
        audit.schema_mismatches,
        vec!["profiles", "profiles:column_or_reference_rules"]
    );
}

#[tokio::test]
async fn frozen_logical_only_metadata_tags_are_qualified_without_discarding_source() {
    let f = Fixture::new().await;
    sqlx::raw_sql("INSERT INTO items(id,title,kind) VALUES('work','Cached','video'); INSERT INTO item_origins VALUES('work','Scanned');")
        .execute(&f.db).await.unwrap();
    sqlx::query("INSERT INTO metadata_documents VALUES('work','local',7,NULL,?,1)")
        .bind(
            json!({"values":{"title":"Curated"},"tags":["favorite"],"excluded_tags":["drama"]})
                .to_string(),
        )
        .execute(&f.db)
        .await
        .unwrap();
    let backend = f.backend().await;
    backend.freeze_motion_source("logical", 0).await.unwrap();
    backend.capture_motion_import("logical").await.unwrap();
    let inventory = backend.motion_reference_inventory("logical").await.unwrap();
    assert!(inventory.activation_ready);
    assert!(!inventory.empty_catalog);
    assert_eq!(inventory.qualification, "logical_only");
    assert_eq!(inventory.unresolved_evidence, 0);
    let snapshot = f.snapshot("logical").await;
    assert_eq!(
        snapshot
            .batches
            .iter()
            .filter_map(|b| b.typed.as_ref())
            .flat_map(|t| &t.tags)
            .count(),
        1
    );
    let audit: String = sqlx::query_scalar(
        "SELECT audit_json FROM catabolic_reference_audits WHERE import_id='logical'",
    )
    .fetch_one(&f.db)
    .await
    .unwrap();
    assert!(audit.contains("item:work"));
    backend.stage_import("logical").await.unwrap();
    assert!(matches!(
        backend.validate_motion_import("logical").await.unwrap(),
        ImportReceipt::ValidatedStaging {
            activation_supported: true,
            unresolved_evidence: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn frozen_export_plan_packs_all_owners_once_and_replays() {
    let f = Fixture::new().await;
    let mut tx = f.db.begin().await.unwrap();
    for index in 0..1100 {
        sqlx::query("INSERT INTO items(id,title,kind) VALUES(?,?,'video')")
            .bind(format!("work-{index:04}"))
            .bind(format!("Work {index}"))
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("INSERT INTO item_origins(item_id,title) VALUES(?,?)")
            .bind(format!("work-{index:04}"))
            .bind(format!("Work {index}"))
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
    let backend = f.backend().await;
    backend.freeze_motion_source("large-plan", 0).await.unwrap();
    let plan = backend
        .capture_motion_export_plan("large-plan")
        .await
        .unwrap();
    assert!(plan.partitions.len() > 1);
    let mut owners = std::collections::BTreeSet::new();
    for part in &plan.partitions {
        let snapshot = f.snapshot(&part.import_id).await;
        assert!(snapshot.batches.len() <= catabolic_store::catalog_import::MAX_IMPORT_BATCHES);
        assert_eq!(snapshot.validate().unwrap(), part.snapshot_digest);
        for owner in snapshot.batches.iter().flat_map(|b| &b.items) {
            assert!(owners.insert(owner.item_id.clone()));
        }
        assert!(
            backend
                .motion_reference_inventory(&part.import_id)
                .await
                .unwrap()
                .activation_ready
        );
    }
    assert_eq!(owners.len(), 1100);
    assert_eq!(
        backend.motion_export_plan("large-plan").await.unwrap(),
        plan
    );
    assert_eq!(
        backend
            .capture_motion_export_plan("large-plan")
            .await
            .unwrap(),
        plan
    );
    assert!(
        sqlx::query("UPDATE catabolic_export_plan_parts SET ordinal=900 WHERE ordinal=0")
            .execute(&f.db)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn logical_qualification_keeps_unknown_or_embedded_catalog_obligations_closed() {
    for (name, document) in [
        (
            "unknown-doc",
            json!({"values":{"title":"Title"},"tags":[],"excluded_tags":[],"future_contract":{"decision":true}}),
        ),
        (
            "nested-ref",
            json!({"values":{"custom":{"timeline_id":"historical"}},"tags":[],"excluded_tags":[]}),
        ),
    ] {
        let f = Fixture::new().await;
        sqlx::raw_sql("INSERT INTO items(id,title,kind) VALUES('work','Title','video'); INSERT INTO item_origins VALUES('work','Title');").execute(&f.db).await.unwrap();
        sqlx::query("INSERT INTO metadata_documents VALUES('work','local',1,NULL,?,1)")
            .bind(document.to_string())
            .execute(&f.db)
            .await
            .unwrap();
        let backend = f.backend().await;
        backend.freeze_motion_source(name, 0).await.unwrap();
        backend.capture_motion_import(name).await.unwrap();
        let inventory = backend.motion_reference_inventory(name).await.unwrap();
        assert!(!inventory.activation_ready);
        assert!(inventory.unresolved_evidence > 0);
    }
}

#[tokio::test]
async fn one_frozen_root_cannot_mix_direct_and_partitioned_exports() {
    for aggregate_first in [false, true] {
        let f = Fixture::new().await;
        let backend = f.backend().await;
        backend.freeze_motion_source("one-mode", 0).await.unwrap();
        if aggregate_first {
            backend
                .capture_motion_export_plan("one-mode")
                .await
                .unwrap();
            assert!(
                backend
                    .capture_motion_import("one-mode")
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("aggregate plan")
            );
        } else {
            backend.capture_motion_import("one-mode").await.unwrap();
            assert!(
                backend
                    .capture_motion_export_plan("one-mode")
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("single import")
            );
        }
    }
}

#[tokio::test]
async fn logical_work_history_and_collection_references_keep_their_exact_ids() {
    let f = Fixture::new().await;
    f.source().await;
    sqlx::raw_sql("INSERT INTO viewing_state(profile_id,item_id,automatic_watched,manual_watched,revision) VALUES('default','work-1',1,1,9); INSERT INTO collections VALUES('collection',3,'default','Favorites','manual',NULL); INSERT INTO collection_items VALUES('collection','work-1');")
        .execute(&f.db).await.unwrap();
    let backend = f.backend().await;
    backend.freeze_motion_source("history", 0).await.unwrap();
    backend.capture_motion_import("history").await.unwrap();
    let inventory = backend.motion_reference_inventory("history").await.unwrap();
    assert!(inventory.activation_ready, "{inventory:?}");
    let raw: String = sqlx::query_scalar(
        "SELECT audit_json FROM catabolic_reference_audits WHERE import_id='history'",
    )
    .fetch_one(&f.db)
    .await
    .unwrap();
    let audit: Value = serde_json::from_str(&raw).unwrap();
    let evidence = audit["resolved_evidence"].as_array().unwrap();
    for table in [
        "progress",
        "viewing_state",
        "collections",
        "collection_items",
        "change_events",
    ] {
        assert!(evidence.iter().any(|e| e["application_table"] == table));
    }
    backend.stage_import("history").await.unwrap();
    assert!(matches!(
        backend.validate_motion_import("history").await.unwrap(),
        ImportReceipt::ValidatedStaging {
            activation_supported: true,
            ..
        }
    ));
    let row: (String, i64) = sqlx::query_as("SELECT item_id,revision FROM viewing_state")
        .fetch_one(&f.db)
        .await
        .unwrap();
    assert_eq!(row, ("work-1".into(), 9));
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT item_id FROM collection_items")
            .fetch_one(&f.db)
            .await
            .unwrap(),
        "work-1"
    );
}

#[tokio::test]
async fn artwork_projection_preserves_suppression_and_content_identity_without_publication() {
    let f = Fixture::new().await;
    f.source().await;
    sqlx::raw_sql("INSERT INTO artwork_assets VALUES('asset','image/png',1,1,X'010203'); INSERT INTO artwork_contributions VALUES('work-1','poster','provider','asset',4); INSERT INTO artwork_selections VALUES('work-1','poster',NULL,6); INSERT INTO item_artwork_assets VALUES('work-1','asset');").execute(&f.db).await.unwrap();
    let backend = f.backend().await;
    backend.freeze_motion_source("art", 0).await.unwrap();
    backend.capture_motion_import("art").await.unwrap();
    let snapshot = f.snapshot("art").await;
    let art = snapshot
        .batches
        .iter()
        .filter_map(|b| b.typed.as_ref())
        .flat_map(|t| &t.metadata)
        .flat_map(|m| &m.contributions)
        .find(|c| c.namespace == "motion.artwork")
        .unwrap();
    assert_eq!(
        art.fields["motion_artwork"]["selections"][0]["asset_id"],
        Value::Null
    );
    assert_eq!(art.fields["motion_artwork"]["selections"][0]["revision"], 6);
    assert_eq!(
        art.fields["motion_artwork"]["assets"][0]["content_sha256"],
        catabolic_core::sha256(&[1, 2, 3])
    );
    assert_eq!(
        art.fields["motion_artwork"]["assets"][0]["owner"],
        "motion_asset_service"
    );
    assert!(
        backend
            .motion_reference_inventory("art")
            .await
            .unwrap()
            .activation_ready
    );
    assert_eq!(
        sqlx::query_scalar::<_, Vec<u8>>("SELECT bytes FROM artwork_assets WHERE id='asset'")
            .fetch_one(&f.db)
            .await
            .unwrap(),
        vec![1, 2, 3]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn exact_observed_file_rehearses_a_complete_version_without_using_legacy_revision_as_native()
{
    use catabolic_runtime::helper::{ClosedObservationProtocol, TrustedClosedObservationArtifact};
    use std::{
        fs,
        os::unix::fs::MetadataExt,
        time::{Duration, UNIX_EPOCH},
    };
    let f = Fixture::new().await;
    let media = f._root.path().join("media");
    fs::create_dir(&media).unwrap();
    fs::write(media.join("one.txt"), b"exact bytes").unwrap();
    let root = fs::metadata(&media).unwrap();
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
    sqlx::query("INSERT INTO libraries(id,name,root,root_identity) VALUES('source','Source',?,?)")
        .bind(media.to_str().unwrap())
        .bind(format!("{}:{}", root.dev(), root.ino()))
        .execute(&f.db)
        .await
        .unwrap();
    sqlx::raw_sql("INSERT INTO items(id,title,kind) VALUES('work','Work','video'); INSERT INTO item_origins VALUES('work','Work'); INSERT INTO editions(id,item_id,label) VALUES('edition','work','Original'); INSERT INTO timelines(id,edition_id,revision,duration_ms) VALUES('timeline','edition',7,1000); INSERT INTO media_versions(id,timeline_id,revision,label,origin,equivalence) VALUES('version','timeline',9,'Source','original','declared');").execute(&f.db).await.unwrap();
    sqlx::query("INSERT INTO media_files(id,edition_id,library_id,relative_path,revision,fingerprint,bytes,duration_seconds) VALUES('file','edition','source','one.txt','legacy-content-revision',?,11,1.0)").bind(fingerprint).execute(&f.db).await.unwrap();
    sqlx::query("INSERT INTO version_files(version_id,part,file_id,file_revision) VALUES('version',1,'file','legacy-content-revision')").execute(&f.db).await.unwrap();
    let backend = f.backend().await;
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
            motion_catalog_backend::sources::ScanCancellation::default(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    sqlx::query("INSERT INTO timeline_viewing(profile_id,timeline_id,revision,manual_epoch,position_ms,updated_at) VALUES('default','timeline',4,2,250,1)").execute(&f.db).await.unwrap();
    sqlx::raw_sql("INSERT INTO catalog_libraries(id,name,kind) VALUES('logical-b','B','mixed'),('logical-a','A','movies'); INSERT INTO library_sources VALUES('logical-b','source'),('logical-a','source');").execute(&f.db).await.unwrap();

    // Old pins remain history. Their revisions deliberately differ from current
    // source/native heads and must not be rebased into a current byte grant.
    sqlx::raw_sql("INSERT INTO devices(id,name,client_name,revision,profile_ids,permissions,policy,created_at) VALUES('history-device','Device','Client',1,'[]','[]','{}',1); INSERT INTO downloads(id,revision,principal_id,device_id,profile_id,timeline_id,timeline_revision,version_id,file_id,file_revision,content_type,size_bytes,duration_ms,state,created_at,updated_at) VALUES('old-download',2,'principal','history-device','default','timeline',3,'version','file','old-content-revision','video/mp4',11,1000,'cancelled',1,1); INSERT INTO viewing_sessions(id,profile_id,timeline_id,delivery_id,principal_id,revision,manual_epoch,sequence,position_ms,status,created_at,updated_at) VALUES('old-session','default','timeline','old-delivery','principal',3,2,5,250,'ended',1,1); INSERT INTO offline_sequences(device_id,profile_id,next_sequence) VALUES('history-device','default',2); INSERT INTO offline_lineage VALUES('history-device','default','timeline',1,1,4);").execute(&f.db).await.unwrap();
    let ack = json!({"session":{"id":"old-session","revision":"2","profile_id":"default","timeline_id":"timeline","delivery_id":"older-delivery","sequence":"4","manual_epoch":"2","position_ms":250,"status":"paused"},"viewing":{"profile_id":"default","timeline_id":"timeline","revision":"3","manual_epoch":"2","position_ms":250,"watched":false,"manual_watched":null,"session_id":"old-session"},"duplicate":false});
    sqlx::query(
        "INSERT INTO viewing_events VALUES('old-session',4,'view-event',1,250,'paused',?,1)",
    )
    .bind(ack.to_string())
    .execute(&f.db)
    .await
    .unwrap();
    let receipt = json!({"id":"old-download","revision":"1","device_id":"history-device","profile_id":"default","timeline_id":"timeline","state":"ready","manifest_asset_id":null,"size_bytes":"11","job_id":null,"subtitle_track_id":null,"device_transfer":null});
    sqlx::query("INSERT INTO idempotency_records(principal_id,operation,target,key,digest,status,body,expires_at) VALUES('principal','createDownload','history-device','key','digest',202,?,999)").bind(receipt.to_string()).execute(&f.db).await.unwrap();
    let offline = json!({"event":{"id":"old-event","sequence":1,"download_id":"old-download","timeline_id":"timeline","timeline_revision":3,"base_viewing_revision":1,"base_manual_epoch":1,"position_ms":250,"status":"paused","device_time":"old-clock"}});
    sqlx::query("INSERT INTO offline_events VALUES('history-device','default',1,'old-event','timeline',?,'history_only','timeline_changed',1)").bind(offline.to_string()).execute(&f.db).await.unwrap();

    backend
        .freeze_motion_source("physical-version", 0)
        .await
        .unwrap();
    backend
        .capture_motion_import("physical-version")
        .await
        .unwrap();
    let snapshot = f.snapshot("physical-version").await;
    let mutations: Vec<catabolic_store::catalog_identity_mutation::IdentityMutation> = snapshot
        .batches
        .iter()
        .filter_map(|b| b.typed.as_ref())
        .filter_map(|t| t.native.as_ref())
        .flat_map(|n| &n.mutations)
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect();
    let version = &mutations[0].versions[0];
    assert_eq!(version.version.version_id, "version");
    assert_eq!(
        version.version.association_item_id,
        "motion.version:version"
    );
    assert_eq!(version.version.edition_item_id, "motion.edition:edition");
    assert_eq!(version.expected_timeline_revision, 1);
    assert_ne!(version.version.bindings[0].file.file_id, "file");
    assert_ne!(
        version.version.bindings[0].file.revision,
        "legacy-content-revision"
    );
    assert_eq!(
        mutations[0].evidence[0].evidence["source_version"]["revision"],
        9
    );
    backend.stage_import("physical-version").await.unwrap();
    let ImportReceipt::ValidatedStaging {
        proposed_typed_mappings,
        ..
    } = backend
        .validate_motion_import("physical-version")
        .await
        .unwrap()
    else {
        panic!("expected rehearsal")
    };
    assert!(
        proposed_typed_mappings
            .iter()
            .any(|m| m["owner"] == "identity"
                && m["result"]["versions"][0]["version_id"] == "version")
    );
    // Complete source ownership can be admitted without claiming HTTP serving.
    let inventory = backend
        .motion_reference_inventory("physical-version")
        .await
        .unwrap();
    assert!(inventory.activation_ready);
    assert_eq!(inventory.qualification, "physical_source");
    assert!(
        inventory
            .physical_coverage
            .missing_ids
            .values()
            .all(|ids| ids.is_empty())
    );
    assert!(
        inventory
            .physical_coverage
            .unresolved_occurrences
            .is_empty()
    );
    assert_eq!(
        inventory.physical_coverage.serving_projection_contract,
        None
    );
    assert!(inventory.physical_coverage.preserved_ids["media_versions"].contains("version"));
    assert_eq!(
        inventory.physical_coverage.source_reference_contract,
        Some(1)
    );
    assert_eq!(
        inventory.physical_coverage.reference_slots["timeline_viewing.timeline_id->timelines.id"]
            .resolved,
        1
    );
    let old_pin: (i64, String) = sqlx::query_as(
        "SELECT timeline_revision,file_revision FROM downloads WHERE id='old-download'",
    )
    .fetch_one(&f.db)
    .await
    .unwrap();
    assert_eq!(old_pin, (3, "old-content-revision".into()));
    assert_eq!(
        inventory.physical_coverage.reference_slots["downloads.file_id->media_files.id"].resolved,
        1
    );
    let history: (i64,i64,i64) = sqlx::query_as("SELECT revision,manual_epoch,position_ms FROM timeline_viewing WHERE timeline_id='timeline'").fetch_one(&f.db).await.unwrap();
    assert_eq!(history, (4, 2, 250));
    let frozen_sources = backend
        .frozen_motion_sources("physical-version")
        .await
        .unwrap();
    assert_eq!(
        frozen_sources,
        motion_catalog_backend::motion_export::frozen_motion_sources_from_journal(
            &f.db,
            backend.identity(),
            "physical-version"
        )
        .await
        .unwrap()
    );
    let mut connection = f.db.acquire().await.unwrap();
    assert_eq!(
        frozen_sources,
        motion_catalog_backend::motion_export::frozen_motion_sources_in(
            &mut connection,
            backend.identity(),
            "physical-version"
        )
        .await
        .unwrap()
    );
    drop(connection);
    let sync = rusqlite::Connection::open_with_flags(
        f.path.parent().unwrap().join("motion.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        frozen_sources,
        motion_catalog_backend::motion_export::frozen_motion_sources_from_connection(
            &sync,
            backend.identity(),
            "physical-version"
        )
        .unwrap()
    );
    let mut wrong_identity = backend.identity().clone();
    wrong_identity.incarnation = "another-incarnation".into();
    assert!(
        motion_catalog_backend::motion_export::frozen_motion_sources_from_connection(
            &sync,
            &wrong_identity,
            "physical-version"
        )
        .is_err()
    );
    assert!(
        motion_catalog_backend::motion_export::frozen_motion_sources_from_journal(
            &f.db,
            &wrong_identity,
            "physical-version"
        )
        .await
        .is_err()
    );
    assert!(
        motion_catalog_backend::motion_export::frozen_motion_sources_from_connection(
            &sync,
            backend.identity(),
            "uncaptured"
        )
        .is_err()
    );
    drop(sync);
    assert_eq!(frozen_sources.len(), 1);
    assert_eq!(frozen_sources[0].mapping.library_id, "source");
    assert_eq!(frozen_sources[0].source_row["name"], "Source");
    assert_eq!(
        frozen_sources[0].logical_library_ids,
        vec!["logical-a", "logical-b"]
    );
    assert_eq!(frozen_sources[0].audit_digest.len(), 64);

    backend.abandon_motion_freeze(1).await.unwrap();
    sqlx::query("UPDATE media_files SET duration_seconds=NULL WHERE id='file'")
        .execute(&f.db)
        .await
        .unwrap();
    sqlx::query("UPDATE media_versions SET equivalence='unknown' WHERE id='version'")
        .execute(&f.db)
        .await
        .unwrap();
    backend
        .freeze_motion_source("draft-version", 2)
        .await
        .unwrap();
    backend
        .capture_motion_import("draft-version")
        .await
        .unwrap();
    let draft = f.snapshot("draft-version").await;
    let mutations: Vec<catabolic_store::catalog_identity_mutation::IdentityMutation> = draft
        .batches
        .iter()
        .filter_map(|b| b.typed.as_ref())
        .filter_map(|t| t.native.as_ref())
        .flat_map(|n| &n.mutations)
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect();
    let version = &mutations[0].versions[0].version;
    assert_eq!(
        version.state,
        catabolic_core::catalog_identity::VersionState::Draft
    );
    assert!(version.declared_coverage.is_none());
    assert!(version.bindings[0].timeline_interval.is_none());
    assert!(matches!(
        version.bindings[0].source,
        catabolic_core::catalog_identity::SourceExtent::WholeFile { duration_us: None }
    ));
    assert!(mutations[0].evidence.is_empty());
    backend.stage_import("draft-version").await.unwrap();
    assert!(matches!(
        backend
            .validate_motion_import("draft-version")
            .await
            .unwrap(),
        ImportReceipt::ValidatedStaging { .. }
    ));
    backend.abandon_motion_freeze(3).await.unwrap();
    sqlx::query("UPDATE media_files SET revision='different-content' WHERE id='file'")
        .execute(&f.db)
        .await
        .unwrap();
    backend
        .freeze_motion_source("historical-version", 4)
        .await
        .unwrap();
    backend
        .capture_motion_import("historical-version")
        .await
        .unwrap();
    let historical = f.snapshot("historical-version").await;
    let mutations: Vec<catabolic_store::catalog_identity_mutation::IdentityMutation> = historical
        .batches
        .iter()
        .filter_map(|b| b.typed.as_ref())
        .filter_map(|t| t.native.as_ref())
        .flat_map(|n| &n.mutations)
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect();
    assert!(mutations.iter().all(|m| m.versions.is_empty()));
    let inventory = backend
        .motion_reference_inventory("historical-version")
        .await
        .unwrap();
    assert!(inventory.physical_coverage.missing_ids["media_versions"].contains("version"));
    assert!(inventory.physical_coverage.historical_ids["media_versions"].contains("version"));
    assert!(
        historical
            .batches
            .iter()
            .flat_map(|batch| &batch.items)
            .any(|item| item.item_id == "motion.version:version"
                && item.kind == "custom:motion_historical_version")
    );
    assert!(
        historical
            .batches
            .iter()
            .flat_map(|batch| &batch.items)
            .any(|item| item.item_id == "motion.file-history:file"
                && item.kind == "custom:motion_historical_file")
    );
    assert!(
        historical
            .batches
            .iter()
            .filter_map(|batch| batch.typed.as_ref())
            .flat_map(|typed| &typed.relationships)
            .any(|edge| edge.source_item_id == "motion.source:source"
                && edge.target_item_id == "work"
                && edge.kind == "custom:motion_source_work")
    );
    assert!(
        historical
            .batches
            .iter()
            .filter_map(|batch| batch.typed.as_ref())
            .filter_map(|typed| typed.native.as_ref())
            .all(|native| native.occurrences.is_empty() && native.associations.is_empty())
    );

    assert!(inventory.activation_ready);
    assert_eq!(
        inventory.physical_coverage.source_reference_contract,
        Some(2)
    );
    let retained: String = sqlx::query_scalar(
        "SELECT audit_json FROM catabolic_reference_audits WHERE import_id='historical-version'",
    )
    .fetch_one(&f.db)
    .await
    .unwrap();
    assert!(retained.contains("version_files") && retained.contains("legacy-content-revision"));
    backend.stage_import("historical-version").await.unwrap();
    backend
        .prepare_motion_cutover("historical-version")
        .await
        .unwrap();
    backend
        .activate_motion_cutover(std::time::Duration::from_secs(30))
        .await
        .unwrap();
    let mut server = motion_catalog_backend::cutover::open_server(
        f.db.clone(),
        f.path.clone(),
        "default".into(),
    )
    .await
    .unwrap();
    let host = server.admission.admitted_host().unwrap();
    motion_catalog_backend::projection::ProjectionWriter::reserve(
        &f._root
            .path()
            .canonicalize()
            .unwrap()
            .join("motion.sqlite3"),
    )
    .unwrap()
    .rebuild(host)
    .unwrap();
    let historic: (String, i64, i64) =
        sqlx::query_as("SELECT revision,available,native_history FROM media_files WHERE id='file'")
            .fetch_one(&f.db)
            .await
            .unwrap();
    assert_eq!(historic, ("different-content".into(), 0, 1));
    let current: i64 =
        sqlx::query_scalar("SELECT count(*) FROM catabolic_projection_files WHERE file_id='file'")
            .fetch_one(&f.db)
            .await
            .unwrap();
    assert_eq!(current, 0);
    host.close().unwrap();
}

#[tokio::test]
async fn retained_motion_auth_never_exports_secrets_or_invents_native_grants() {
    let f = Fixture::new().await;
    sqlx::query("INSERT INTO signin_providers(id,revision,name,issuer,client_id,secret) VALUES('provider',1,'Provider','https://issuer.invalid','client','secret-not-an-import-grant')").execute(&f.db).await.unwrap();
    let backend = f.backend().await;
    backend
        .freeze_motion_source("auth-retained", 0)
        .await
        .unwrap();
    backend
        .capture_motion_import("auth-retained")
        .await
        .unwrap();
    let inventory = backend
        .motion_reference_inventory("auth-retained")
        .await
        .unwrap();
    assert!(inventory.activation_ready);
    assert_eq!(inventory.retained_motion_auth_rows["signin_providers"], 1);
    let raw: String = sqlx::query_scalar(
        "SELECT audit_json FROM catabolic_reference_audits WHERE import_id='auth-retained'",
    )
    .fetch_one(&f.db)
    .await
    .unwrap();
    assert!(!raw.contains("secret-not-an-import-grant"));
    assert!(
        !serde_json::to_string(&f.snapshot("auth-retained").await)
            .unwrap()
            .contains("secret-not-an-import-grant")
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT secret FROM signin_providers WHERE id='provider'")
            .fetch_one(&f.db)
            .await
            .unwrap(),
        "secret-not-an-import-grant"
    );
}

#[tokio::test]
async fn exact_zero_effect_upgrade_receipts_are_retained_without_phantom_catalog() {
    let f = Fixture::new().await;
    for (id, kind, body) in [
        (
            "legacy",
            "migration:legacy_progress",
            json!({"outcomes":{}}),
        ),
        (
            "repair",
            "migration:timeline_progress_repair",
            json!({"imported":0,"retracted_unattributed":0}),
        ),
    ] {
        sqlx::query("INSERT INTO catalog_receipts VALUES(?,?,17,?)")
            .bind(id)
            .bind(kind)
            .bind(body.to_string())
            .execute(&f.db)
            .await
            .unwrap();
    }
    let before: Vec<(String, String, i64, String)> =
        sqlx::query_as("SELECT * FROM catalog_receipts ORDER BY id")
            .fetch_all(&f.db)
            .await
            .unwrap();
    let backend = f.backend().await;
    backend
        .freeze_motion_source("empty-upgraded", 0)
        .await
        .unwrap();
    backend
        .capture_motion_import("empty-upgraded")
        .await
        .unwrap();
    let inventory = backend
        .motion_reference_inventory("empty-upgraded")
        .await
        .unwrap();
    assert!(inventory.empty_catalog && inventory.activation_ready);
    assert!(f.snapshot("empty-upgraded").await.batches.is_empty());
    let audit: String = sqlx::query_scalar(
        "SELECT audit_json FROM catabolic_reference_audits WHERE import_id='empty-upgraded'",
    )
    .fetch_one(&f.db)
    .await
    .unwrap();
    let audit: Value = serde_json::from_str(&audit).unwrap();
    assert_eq!(
        audit["retained_zero_effect_receipts"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    for (_, _, _, raw) in &before {
        assert!(
            audit["retained_zero_effect_receipts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["document_json"] == *raw)
        );
    }
    let after: Vec<(String, String, i64, String)> =
        sqlx::query_as("SELECT * FROM catalog_receipts ORDER BY id")
            .fetch_all(&f.db)
            .await
            .unwrap();
    assert_eq!(before, after);
    backend.stage_import("empty-upgraded").await.unwrap();
    assert!(matches!(
        backend
            .validate_motion_import("empty-upgraded")
            .await
            .unwrap(),
        ImportReceipt::ValidatedStaging { .. }
    ));
}

#[tokio::test]
async fn receipt_exemption_rejects_nonzero_unknown_duplicate_and_affected_history() {
    for (kind, raw, duplicate, history) in [
        (
            "migration:legacy_progress",
            r#"{"outcomes":{"exact":1}}"#,
            false,
            false,
        ),
        (
            "migration:timeline_progress_repair",
            r#"{"imported":1,"retracted_unattributed":0}"#,
            false,
            false,
        ),
        ("migration:other", r#"{"outcomes":{}}"#, false, false),
        (
            "migration:legacy_progress",
            r#"{"outcomes":{},"extra":0}"#,
            false,
            false,
        ),
        (
            "migration:legacy_progress",
            r#"{"outcomes":{"exact":1},"outcomes":{}}"#,
            false,
            false,
        ),
        (
            "migration:legacy_progress",
            r#"{"outcomes":{}}"#,
            true,
            false,
        ),
        (
            "migration:legacy_progress",
            r#"{"outcomes":{}}"#,
            false,
            true,
        ),
    ] {
        let f = Fixture::new().await;
        sqlx::query("INSERT INTO catalog_receipts VALUES('receipt',?,1,?)")
            .bind(kind)
            .bind(raw)
            .execute(&f.db)
            .await
            .unwrap();
        if duplicate {
            sqlx::query("INSERT INTO catalog_receipts VALUES('duplicate',?,2,?)")
                .bind(kind)
                .bind(raw)
                .execute(&f.db)
                .await
                .unwrap();
        }
        if history {
            sqlx::query("INSERT INTO timeline_viewing(profile_id,timeline_id,revision,updated_at) VALUES('default','historical',4,1)").execute(&f.db).await.unwrap();
        }
        let backend = f.backend().await;
        backend
            .freeze_motion_source("denied-receipt", 0)
            .await
            .unwrap();
        backend
            .capture_motion_import("denied-receipt")
            .await
            .unwrap();
        let inventory = backend
            .motion_reference_inventory("denied-receipt")
            .await
            .unwrap();
        if history {
            assert!(!inventory.empty_catalog);
            assert!(inventory.activation_ready);
            assert_eq!(
                inventory.physical_coverage.source_reference_contract,
                Some(2)
            );
            assert!(inventory.physical_coverage.tombstone_ids["timelines"].contains("historical"));
        } else {
            assert!(
                !inventory.activation_ready,
                "{kind} {raw} duplicate={duplicate}"
            );
            assert!(inventory.unresolved_evidence > 0);
        }
        let unresolved_receipt = f
            .snapshot("denied-receipt")
            .await
            .batches
            .iter()
            .flat_map(|b| &b.unresolved_evidence)
            .any(|e| e.data["table"] == "catalog_receipts");
        assert_eq!(
            unresolved_receipt, !history,
            "{kind} {raw} duplicate={duplicate} history={history}"
        );
        // The valid zero-effect receipt is discharged only with the complete
        // historical tombstone proof, and its original bytes remain retained.
        let retained: String =
            sqlx::query_scalar("SELECT document_json FROM catalog_receipts WHERE id='receipt'")
                .fetch_one(&f.db)
                .await
                .unwrap();
        assert_eq!(retained, raw);
    }
}

#[tokio::test]
async fn logical_library_owner_preserves_distinct_physical_ids_without_granting_scope() {
    let f = Fixture::new().await;
    sqlx::raw_sql("INSERT INTO libraries(id,name,root,root_identity) VALUES('physical-b','B','/not-opened/b','old-b'),('physical-a','A','/not-opened/a','old-a'); INSERT INTO catalog_libraries(id,revision,name,kind,language) VALUES('logical',7,'Movies','movies','en'); INSERT INTO library_sources VALUES('logical','physical-b'),('logical','physical-a');")
        .execute(&f.db).await.unwrap();
    let backend = f.backend().await;
    backend
        .capture_motion_import("library-definition")
        .await
        .unwrap();
    let snapshot = f.snapshot("library-definition").await;
    let library = snapshot
        .batches
        .iter()
        .flat_map(|b| &b.items)
        .find(|i| i.item_id == "motion.library:logical")
        .unwrap();
    assert_eq!(library.kind, "custom:motion_library");
    let metadata = snapshot
        .batches
        .iter()
        .filter_map(|b| b.typed.as_ref())
        .flat_map(|t| &t.metadata)
        .find(|m| m.item_id == library.item_id)
        .unwrap();
    assert_eq!(metadata.contributions[0].source_revision, 7);
    assert_eq!(
        metadata.contributions[0].fields["motion_library"],
        json!({"contract_version":1,"id":"logical","catalog_revision":7,"name":"Movies","kind":"movies","language":"en","source_ids":["physical-a","physical-b"]})
    );
    let inventory = backend
        .motion_reference_inventory("library-definition")
        .await
        .unwrap();
    assert!(!inventory.activation_ready);
    assert!(inventory.physical_coverage.preserved_ids["catalog_libraries"].contains("logical"));
    assert_eq!(
        inventory.physical_coverage.missing_ids["libraries"].len(),
        2
    );
    let logical = &inventory.physical_coverage.reference_slots["library_sources.library_id->catalog_libraries.id"];
    assert_eq!(logical.resolved, 2);
    let physical =
        &inventory.physical_coverage.reference_slots["library_sources.source_id->libraries.id"];
    assert_eq!(physical.unmapped, 2);
    let scope_count: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_server_scopes")
        .fetch_one(&f.db)
        .await
        .unwrap();
    assert_eq!(scope_count, 0);
    backend.stage_import("library-definition").await.unwrap();
    assert!(matches!(
        backend
            .validate_motion_import("library-definition")
            .await
            .unwrap(),
        ImportReceipt::ValidatedStaging { .. }
    ));
}

#[tokio::test]
async fn retained_history_without_native_owners_uses_empty_native_contract() {
    let f = Fixture::new().await;
    sqlx::query("INSERT INTO change_events(topic,resource_id) VALUES('sources','deleted-source')")
        .execute(&f.db)
        .await
        .unwrap();
    let backend = f.backend().await;
    backend
        .freeze_motion_source("history-only", 0)
        .await
        .unwrap();
    backend.capture_motion_import("history-only").await.unwrap();
    let snapshot = f.snapshot("history-only").await;
    assert_eq!(
        snapshot.manifest.contract_version,
        catabolic_store::catalog_import::EMPTY_IMPORT_CONTRACT_VERSION
    );
    assert!(snapshot.batches.is_empty());
    let inventory = backend
        .motion_reference_inventory("history-only")
        .await
        .unwrap();
    assert!(inventory.activation_ready);
    assert!(!inventory.empty_catalog);
    assert_eq!(inventory.qualification, "logical_only");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM change_events WHERE resource_id='deleted-source'")
            .fetch_one(&f.db)
            .await
            .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn root_structure_is_qualified_but_parent_relationship_is_not_inferred() {
    for nested in [false, true] {
        let f = Fixture::new().await;
        f.source().await;
        if nested {
            sqlx::raw_sql("INSERT INTO items(id,title,kind) VALUES('parent','Parent','video'); INSERT INTO item_origins VALUES('parent','Parent'); INSERT INTO item_structure VALUES('work-1','season','parent',0,7);").execute(&f.db).await.unwrap();
        } else {
            sqlx::query("INSERT INTO item_structure VALUES('work-1','movie',NULL,NULL,7)")
                .execute(&f.db)
                .await
                .unwrap();
        }
        let backend = f.backend().await;
        backend
            .freeze_motion_source("root-structure", 0)
            .await
            .unwrap();
        backend
            .capture_motion_import("root-structure")
            .await
            .unwrap();
        let inventory = backend
            .motion_reference_inventory("root-structure")
            .await
            .unwrap();
        assert_eq!(inventory.activation_ready, !nested);
        let snapshot = f.snapshot("root-structure").await;
        let model = snapshot
            .batches
            .iter()
            .filter_map(|b| b.typed.as_ref())
            .flat_map(|t| &t.metadata)
            .find(|m| m.item_id == "work-1")
            .unwrap()
            .contributions
            .iter()
            .find(|c| c.record_id == "source_projection")
            .unwrap()
            .fields["motion_catalog"]
            .clone();
        assert_eq!(model["structure_revision"], 7);
        assert_eq!(
            model["parent_id"],
            if nested { json!("parent") } else { Value::Null }
        );
    }
}

#[tokio::test]
async fn auth_pairing_replay_is_retained_without_exporting_secrets_or_catalog_replays() {
    for operation in ["approvePairing", "createItem"] {
        let f = Fixture::new().await;
        sqlx::raw_sql("INSERT INTO devices(id,name,client_name,revision,profile_ids,permissions,policy,created_at) VALUES('device','Device','Client',1,'[\"default\"]','[]','{}',1); INSERT INTO pairings(id,device_code_hash,user_code,device_name,client_name,expires_at,phase,device_id) VALUES('pair','hash','123','Device','Client',99,'approved','device');").execute(&f.db).await.unwrap();
        sqlx::query("INSERT INTO idempotency_records VALUES('operator',?,'pair','key','digest',200,?,'secret-nonce',99)")
            .bind(operation).bind(r#"{"secret":"secret-ack-body","item_id":"unmapped"}"#).execute(&f.db).await.unwrap();
        let backend = f.backend().await;
        backend
            .freeze_motion_source("auth-replay", 0)
            .await
            .unwrap();
        backend.capture_motion_import("auth-replay").await.unwrap();
        let inventory = backend
            .motion_reference_inventory("auth-replay")
            .await
            .unwrap();
        assert_eq!(inventory.activation_ready, operation == "approvePairing");
        let raw = serde_json::to_string(&f.snapshot("auth-replay").await).unwrap();
        let audit: String = sqlx::query_scalar(
            "SELECT audit_json FROM catabolic_reference_audits WHERE import_id='auth-replay'",
        )
        .fetch_one(&f.db)
        .await
        .unwrap();
        for secret in ["secret-nonce", "secret-ack-body"] {
            assert!(!raw.contains(secret));
            assert!(!audit.contains(secret));
        }
        let retained: Option<String> =
            sqlx::query_scalar("SELECT issued_nonce FROM idempotency_records")
                .fetch_one(&f.db)
                .await
                .unwrap();
        assert_eq!(retained.as_deref(), Some("secret-nonce"));
    }
}

#[tokio::test]
async fn same_column_reference_constraint_drift_keeps_admission_closed() {
    for nullable in [true, false] {
        let f = Fixture::new().await;
        // An empty fixture can expose same-column DDL drift without discarding
        // any historical data. Names alone must not certify nullable references.
        sqlx::query("DROP TABLE timeline_viewing")
            .execute(&f.db)
            .await
            .unwrap();
        let target = if nullable { "profiles" } else { "items" };
        let notnull = if nullable { "" } else { "NOT NULL" };
        sqlx::query(&format!("CREATE TABLE timeline_viewing(profile_id TEXT NOT NULL REFERENCES {target}(id),timeline_id TEXT {notnull},revision INTEGER NOT NULL,manual_epoch INTEGER NOT NULL DEFAULT 0,position_ms INTEGER NOT NULL DEFAULT 0,automatic_watched INTEGER NOT NULL DEFAULT 0,manual_watched INTEGER,session_id TEXT,updated_at INTEGER NOT NULL,PRIMARY KEY(profile_id,timeline_id))"))
            .execute(&f.db).await.unwrap();
        let backend = f.backend().await;
        backend
            .freeze_motion_source("constraint-drift", 0)
            .await
            .unwrap();
        backend
            .capture_motion_import("constraint-drift")
            .await
            .unwrap();
        let inventory = backend
            .motion_reference_inventory("constraint-drift")
            .await
            .unwrap();
        assert!(!inventory.activation_ready);
        assert!(
            inventory
                .schema_mismatches
                .iter()
                .any(|m| m == "timeline_viewing:column_or_reference_rules")
        );
    }
}

#[tokio::test]
async fn unavailable_unregistered_source_has_only_historical_native_owners() {
    let f = Fixture::new().await;
    sqlx::raw_sql("INSERT INTO libraries(id,name,root,root_identity,enabled) VALUES('offline-source','Offline','/does-not-exist','old-device:old-inode',0); INSERT INTO catalog_libraries(id,name,kind) VALUES('logical','Library','movies'); INSERT INTO library_sources VALUES('logical','offline-source'); INSERT INTO items(id,title,kind) VALUES('work','Offline film','video'); INSERT INTO item_origins VALUES('work','Offline film'); INSERT INTO editions(id,item_id,label) VALUES('edition','work','Original'); INSERT INTO timelines(id,edition_id,revision,duration_ms) VALUES('timeline','edition',7,1000); INSERT INTO media_versions(id,timeline_id,revision,label,origin,equivalence) VALUES('version','timeline',9,'Source','original','unknown'); INSERT INTO media_files(id,edition_id,library_id,relative_path,revision,fingerprint,bytes,available) VALUES('file','edition','offline-source','old/movie.mp4','old-revision','old-fingerprint',123,0); INSERT INTO version_files(version_id,part,file_id,file_revision) VALUES('version',1,'file','old-revision');").execute(&f.db).await.unwrap();
    let backend = f.backend().await;
    backend
        .freeze_motion_source("unavailable-history", 0)
        .await
        .unwrap();
    backend
        .capture_motion_import("unavailable-history")
        .await
        .unwrap();
    let snapshot = f.snapshot("unavailable-history").await;
    let inventory = backend
        .motion_reference_inventory("unavailable-history")
        .await
        .unwrap();
    assert!(inventory.activation_ready);
    assert_eq!(
        inventory.physical_coverage.source_reference_contract,
        Some(2)
    );
    assert!(
        backend
            .frozen_motion_sources("unavailable-history")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        inventory.physical_coverage.historical_reference_contract,
        Some(1)
    );
    assert_eq!(inventory.physical_coverage.historical_reference_slots["version_files.file_id->media_files.id"].resolved,1);
    assert!(inventory.physical_coverage.historical_ids["libraries"].contains("offline-source"));
    assert!(inventory.physical_coverage.missing_ids["media_files"].contains("file"));
    assert!(!inventory.physical_coverage.preserved_ids["media_files"].contains("file"));
    assert!(
        snapshot
            .batches
            .iter()
            .flat_map(|batch| &batch.items)
            .any(|item| item.item_id == "motion.version:version"
                && item.kind == "custom:motion_historical_version")
    );
    assert!(
        snapshot
            .batches
            .iter()
            .filter_map(|batch| batch.typed.as_ref())
            .filter_map(|typed| typed.native.as_ref())
            .all(|native| native.occurrences.is_empty() && native.associations.is_empty())
    );
    // A descriptive provisional timeline is preserved, but no version,
    // binding, physical association, coverage or equivalence is invented.
    let mutations: Vec<_> = snapshot
        .batches
        .iter()
        .filter_map(|batch| batch.typed.as_ref())
        .filter_map(|typed| typed.native.as_ref())
        .flat_map(|native| native.mutations.iter().cloned())
        .collect();
    assert_eq!(
        mutations,
        vec![json!({
            "contract_version": 1,
            "profile": "default",
            "context": {
                "id": "motion.work:work",
                "authority_namespace": "motion.catalog.operator.v1",
                "context_key": "work",
                "expected_revision": null
            },
            "timelines": [{
                "timeline_id": "timeline",
                "edition_item_id": "motion.edition:edition",
                "expected_revision": null,
                "identity_state": "provisional",
                "coverage": null
            }],
            "versions": [],
            "evidence": []
        })]
    );
    let metadata = snapshot
        .batches
        .iter()
        .filter_map(|batch| batch.typed.as_ref())
        .flat_map(|typed| &typed.metadata)
        .find(|item| item.item_id == "motion.file-history:file")
        .unwrap();
    assert_eq!(
        metadata.contributions[0].fields["motion_historical_file"]["source_file"]["relative_path"],
        "old/movie.mp4"
    );
    assert_eq!(
        metadata.contributions[0].fields["motion_historical_file"]["source_file"]["revision"],
        "old-revision"
    );
    assert!(
        metadata.contributions[0].fields["motion_historical_file"]["authority"]
            .as_str()
            .unwrap()
            .contains("no native occurrence")
    );
    backend.stage_import("unavailable-history").await.unwrap();
    assert!(matches!(
        backend
            .validate_motion_import("unavailable-history")
            .await
            .unwrap(),
        ImportReceipt::ValidatedStaging { .. }
    ));
    backend.abandon_motion_freeze(1).await.unwrap();
    sqlx::query("INSERT INTO timeline_viewing(profile_id,timeline_id,revision,updated_at) VALUES('default','unknown-historical-timeline',4,1)").execute(&f.db).await.unwrap();
    backend
        .freeze_motion_source("unknown-history", 2)
        .await
        .unwrap();
    backend
        .capture_motion_import("unknown-history")
        .await
        .unwrap();
    let unknown = backend
        .motion_reference_inventory("unknown-history")
        .await
        .unwrap();
    assert!(unknown.activation_ready);
    assert_eq!(
        unknown.physical_coverage.historical_reference_contract,
        Some(1)
    );
    assert!(
        unknown.physical_coverage.tombstone_ids["timelines"]
            .contains("unknown-historical-timeline")
    );
    assert_eq!(unknown.physical_coverage.historical_reference_slots["timeline_viewing.timeline_id->timelines.id"].unmapped,0);
    backend.abandon_motion_freeze(3).await.unwrap();
    sqlx::query("INSERT INTO timeline_viewing(profile_id,timeline_id,revision,manual_epoch,position_ms,updated_at) VALUES('default','timeline',4,2,250,1)").execute(&f.db).await.unwrap();
    backend
        .freeze_motion_source("historical-cutover", 4)
        .await
        .unwrap();
    backend
        .capture_motion_import("historical-cutover")
        .await
        .unwrap();
    backend.stage_import("historical-cutover").await.unwrap();
    backend
        .prepare_motion_cutover("historical-cutover")
        .await
        .unwrap();
    backend
        .activate_motion_cutover(std::time::Duration::from_secs(30))
        .await
        .unwrap();
    let mut server = motion_catalog_backend::cutover::open_server(
        f.db.clone(),
        f.path.clone(),
        "default".into(),
    )
    .await
    .unwrap();
    let host = server.admission.admitted_host().unwrap();
    let projection = motion_catalog_backend::projection::ProjectionWriter::reserve(
        &f._root
            .path()
            .canonicalize()
            .unwrap()
            .join("motion.sqlite3"),
    )
    .unwrap();
    projection.rebuild(host).unwrap();
    let row: (String, String, i64, i64) = sqlx::query_as(
        "SELECT relative_path,revision,available,native_history FROM media_files WHERE id='file'",
    )
    .fetch_one(&f.db)
    .await
    .unwrap();
    assert_eq!(row, ("old/movie.mp4".into(), "old-revision".into(), 0, 1));
    let binding: (String, String) = sqlx::query_as(
        "SELECT file_id,file_revision FROM version_files WHERE version_id='version' AND part=1",
    )
    .fetch_one(&f.db)
    .await
    .unwrap();
    assert_eq!(binding, ("file".into(), "old-revision".into()));
    let versions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM catabolic_projection_versions WHERE version_id='version'",
    )
    .fetch_one(&f.db)
    .await
    .unwrap();
    assert_eq!(versions, 0);
    let proofs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM catabolic_projection_files WHERE file_id='file'")
            .fetch_one(&f.db)
            .await
            .unwrap();
    assert_eq!(proofs, 0);
    let history:(i64,i64,i64)=sqlx::query_as("SELECT revision,manual_epoch,position_ms FROM timeline_viewing WHERE timeline_id='timeline'").fetch_one(&f.db).await.unwrap();
    assert_eq!(history, (4, 2, 250));
    let absent: i64 =
        sqlx::query_scalar("SELECT count(*) FROM timelines WHERE id='unknown-historical-timeline'")
            .fetch_one(&f.db)
            .await
            .unwrap();
    assert_eq!(absent, 0);
    let retained_deleted: i64 = sqlx::query_scalar(
        "SELECT revision FROM timeline_viewing WHERE timeline_id='unknown-historical-timeline'",
    )
    .fetch_one(&f.db)
    .await
    .unwrap();
    assert_eq!(retained_deleted, 4);

    motion_catalog_backend::projection::ProjectionWriter::reserve(
        &f._root
            .path()
            .canonicalize()
            .unwrap()
            .join("motion.sqlite3"),
    )
    .unwrap()
    .rebuild(host)
    .unwrap();
    let preserved: (String, String, i64, i64) = sqlx::query_as(
        "SELECT relative_path,revision,available,native_history FROM media_files WHERE id='file'",
    )
    .fetch_one(&f.db)
    .await
    .unwrap();
    assert_eq!(preserved, row);
    host.close().unwrap();
}

#[tokio::test]
async fn historical_tombstones_never_discharge_broken_catalog_foreign_keys() {
    let f = Fixture::new().await;
    sqlx::raw_sql("INSERT INTO items(id,title,kind) VALUES('work','Work','video'); INSERT INTO item_origins VALUES('work','Work'); INSERT INTO editions(id,item_id,label) VALUES('edition','work','Original'); INSERT INTO timelines(id,edition_id,revision) VALUES('timeline','edition',7); INSERT INTO media_versions(id,timeline_id,revision,label,origin,equivalence) VALUES('version','timeline',9,'Source','original','unknown');").execute(&f.db).await.unwrap();
    let mut connection = f.db.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys=OFF")
        .execute(&mut *connection)
        .await
        .unwrap();
    // Explicit interval satisfies the independent cross-edition shape trigger;
    // only the foreign key is deliberately corrupted in this isolated fixture.
    sqlx::query("INSERT INTO version_files(version_id,part,file_id,file_revision,start_ms,end_ms) VALUES('version',1,'missing-file','old',0,1000)").execute(&mut *connection).await.unwrap();
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&mut *connection)
        .await
        .unwrap();
    drop(connection);
    let backend = f.backend().await;
    backend
        .freeze_motion_source("broken-catalog-fk", 0)
        .await
        .unwrap();
    backend
        .capture_motion_import("broken-catalog-fk")
        .await
        .unwrap();
    let audit = backend
        .motion_reference_inventory("broken-catalog-fk")
        .await
        .unwrap();
    assert!(!audit.activation_ready);
    assert_eq!(audit.physical_coverage.historical_reference_contract, None);
    assert!(
        audit
            .physical_coverage
            .tombstone_ids
            .get("media_files")
            .is_none_or(|ids| !ids.contains("missing-file"))
    );
    assert_eq!(
        audit.physical_coverage.historical_reference_slots["version_files.file_id->media_files.id"]
            .unmapped,
        1
    );
}

#[tokio::test]
async fn frozen_audio_family_rehearses_without_inventing_recording_semantics() {
    let f = Fixture::new().await;
    sqlx::raw_sql("INSERT INTO items(id,title,kind) VALUES('audio-work','Voice memo','audio'); INSERT INTO item_origins VALUES('audio-work','Voice memo');")
        .execute(&f.db).await.unwrap();
    let backend = f.backend().await;
    backend
        .freeze_motion_source("audio-family", 0)
        .await
        .unwrap();
    backend.capture_motion_import("audio-family").await.unwrap();
    assert!(
        backend
            .motion_reference_inventory("audio-family")
            .await
            .unwrap()
            .activation_ready
    );
    let snapshot = f.snapshot("audio-family").await;
    let owners: Vec<_> = snapshot.batches.iter().flat_map(|b| &b.items).collect();
    assert_eq!(owners.len(), 1);
    assert_eq!(owners[0].item_id, "audio-work");
    assert_eq!(owners[0].kind, "custom:motion_audio");
    let source = snapshot
        .batches
        .iter()
        .filter_map(|b| b.typed.as_ref())
        .flat_map(|t| &t.metadata)
        .flat_map(|m| &m.contributions)
        .find(|c| c.namespace == "motion.structure" && c.record_id == "source_projection")
        .unwrap();
    assert_eq!(source.fields["motion_catalog"]["kind"], "audio");
    assert_eq!(
        source.fields["motion_catalog"]["media_type"],
        "unclassified"
    );
    backend.stage_import("audio-family").await.unwrap();
    assert!(matches!(
        backend
            .validate_motion_import("audio-family")
            .await
            .unwrap(),
        ImportReceipt::ValidatedStaging {
            activation_supported: true,
            unresolved_evidence: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn native40_import_preserves_hundred_field_sources_and_long_keys() {
    let f = Fixture::new().await;
    sqlx::raw_sql("INSERT INTO items(id,title,kind) VALUES('wide-work','Wide','video'); INSERT INTO item_origins VALUES('wide-work','Wide');")
        .execute(&f.db).await.unwrap();
    for (source, prefix) in [("local", "a"), ("provider", "b")] {
        let values: serde_json::Map<String, Value> = (0..100)
            .map(|index| {
                let key = if index == 0 {
                    format!("{prefix}{index:03}{}", "x".repeat(96))
                } else {
                    format!("{prefix}{index:03}")
                };
                assert!(key.len() <= 100);
                (key, json!(index))
            })
            .collect();
        sqlx::query("INSERT INTO metadata_documents VALUES('wide-work',?,7,NULL,?,1)")
            .bind(source)
            .bind(json!({"values":values,"tags":[],"excluded_tags":[]}).to_string())
            .execute(&f.db)
            .await
            .unwrap();
    }
    let backend = f.backend().await;
    assert!(backend.identity().schema_version >= 40);
    backend
        .freeze_motion_source("wide-import", 0)
        .await
        .unwrap();
    backend.capture_motion_import("wide-import").await.unwrap();
    let snapshot = f.snapshot("wide-import").await;
    let metadata = snapshot
        .batches
        .iter()
        .filter_map(|b| b.typed.as_ref())
        .flat_map(|t| &t.metadata)
        .find(|m| m.item_id == "wide-work")
        .unwrap();
    assert_eq!(metadata.contract_version, 2);
    assert_eq!(metadata.system_fields, vec!["motion_catalog"]);
    assert_eq!(metadata.policies.as_array().unwrap().len(), 202);
    for namespace in ["motion.local", "motion.provider"] {
        let source = metadata
            .contributions
            .iter()
            .find(|c| c.namespace == namespace)
            .unwrap();
        assert_eq!(source.fields.as_object().unwrap().len(), 100);
        assert!(
            source
                .fields
                .as_object()
                .unwrap()
                .keys()
                .any(|key| key.len() == 100)
        );
    }
    backend.stage_import("wide-import").await.unwrap();
    assert!(matches!(
        backend.validate_motion_import("wide-import").await.unwrap(),
        ImportReceipt::ValidatedStaging {
            activation_supported: true,
            unresolved_evidence: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn differing_native_tag_identity_never_silently_relabels_frozen_source() {
    let f = Fixture::new().await;
    sqlx::raw_sql("INSERT INTO items(id,title,kind) VALUES('unicode-tags','Tags','video'); INSERT INTO item_origins VALUES('unicode-tags','Tags');")
        .execute(&f.db).await.unwrap();
    sqlx::query("INSERT INTO metadata_documents VALUES('unicode-tags','local',1,NULL,?,1)")
        .bind(json!({"values":{},"tags":["straße","strasse"],"excluded_tags":[]}).to_string())
        .execute(&f.db)
        .await
        .unwrap();
    let backend = f.backend().await;
    backend
        .freeze_motion_source("unicode-tags", 0)
        .await
        .unwrap();
    let error = backend
        .capture_motion_import("unicode-tags")
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("tag identity differs"));
    let saved: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM catabolic_import_snapshots WHERE import_id='unicode-tags'",
    )
    .fetch_one(&f.db)
    .await
    .unwrap();
    assert_eq!(saved, 0);
    let document: String = sqlx::query_scalar(
        "SELECT document_json FROM metadata_documents WHERE item_id='unicode-tags'",
    )
    .fetch_one(&f.db)
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&document).unwrap()["tags"],
        json!(["straße", "strasse"])
    );
}

#[tokio::test]
async fn artwork_import_keeps_unknown_receipts_or_unowned_assets_unresolved() {
    for case in [
        "unknown_receipt",
        "foreign_upload_owner",
        "unowned_asset",
        "mismatched_receipt_digest",
    ] {
        let f = Fixture::new().await;
        sqlx::raw_sql("INSERT INTO items(id,title,kind) VALUES('work','Artwork','video'); INSERT INTO item_origins VALUES('work','Artwork');").execute(&f.db).await.unwrap();
        let asset = catabolic_core::sha256(&[1, 2, 3]);
        sqlx::query("INSERT INTO artwork_assets VALUES(?,'image/png',1,1,X'010203')")
            .bind(&asset)
            .execute(&f.db)
            .await
            .unwrap();
        if case != "unowned_asset" {
            sqlx::query("INSERT INTO item_artwork_assets(item_id,asset_id) VALUES('work',?)")
                .bind(&asset)
                .execute(&f.db)
                .await
                .unwrap();
        }
        if case != "unowned_asset" {
            let operation = if case == "unknown_receipt" {
                "unreviewedArtwork"
            } else {
                "uploadArtwork"
            };
            let target = if case == "foreign_upload_owner" {
                "foreign"
            } else {
                "work"
            };
            let body = json!({"id":asset,"revision":"1","digest":if case=="mismatched_receipt_digest" {"sha256:wrong".into()}else{format!("sha256:{asset}")},"kind":"artwork","mime_type":"image/png","size_bytes":"3","content_url":format!("/api/v2/media/assets/{asset}/content?revision=1"),"source_revision":null});
            sqlx::query("INSERT INTO idempotency_records(principal_id,operation,target,key,digest,status,body,issued_nonce,expires_at) VALUES('operator',?,?, 'retained-artwork-request','digest',201,?,NULL,9999999999)").bind(operation).bind(target).bind(body.to_string()).execute(&f.db).await.unwrap();
        }
        let backend = f.backend().await;
        backend.freeze_motion_source(case, 0).await.unwrap();
        backend.capture_motion_import(case).await.unwrap();
        assert!(
            !backend
                .motion_reference_inventory(case)
                .await
                .unwrap()
                .activation_ready,
            "{case}"
        );
    }
}
