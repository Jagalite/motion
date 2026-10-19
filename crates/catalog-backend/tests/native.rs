use catabolic_store::{Store, catalog_commands::CommandOutcome};
use motion_catalog_backend::{NativeCatalog, initialize, inspect};
use serde_json::json;
use std::{path::PathBuf, time::Duration};

struct Fixture {
    _root: tempfile::TempDir,
    path: PathBuf,
    db: sqlx::SqlitePool,
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        // Catabolic rejects symlink directory components; macOS /var is one.
        let base = root.path().canonicalize().unwrap();
        let path = base.join("native.sqlite3");
        initialize(path.clone()).await.unwrap();
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(base.join("journal.sqlite3"))
            .create_if_missing(true);
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/0066_catabolic_command_journal.sql"
        ))
        .execute(&db)
        .await
        .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/0067_catabolic_import_snapshots.sql"
        ))
        .execute(&db)
        .await
        .unwrap();
        Self {
            _root: root,
            path,
            db,
        }
    }
    async fn backend(&self) -> NativeCatalog {
        let identity = inspect(self.path.clone()).await.unwrap().identity;
        NativeCatalog::bind(self.db.clone(), self.path.clone(), identity)
            .await
            .unwrap()
    }
    fn count(&self, table: &str) -> i64 {
        let store = Store::open(&self.path, false, false).unwrap();
        store
            .db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
}
fn item(title: &str) -> Vec<serde_json::Value> {
    vec![json!({"id":"motion-work-1", "kind":"video", "metadata":{"title": title}})]
}

#[tokio::test]
async fn durable_intent_survives_lost_acknowledgment_and_replays_once() {
    let f = Fixture::new().await;
    let backend = f.backend().await;
    backend.prepare_items("create-1", item("A")).await.unwrap();
    assert_eq!(f.count("items"), 0);
    let saved: String = sqlx::query_scalar(
        "SELECT envelope_json FROM catabolic_commands WHERE operation_id='create-1'",
    )
    .fetch_one(&f.db)
    .await
    .unwrap();
    let first = backend.dispatch("create-1").await.unwrap();
    let CommandOutcome::Committed {
        result: first_result,
        committed_generation: first_generation,
        ..
    } = first
    else {
        panic!("{first:?}")
    };
    // Simulate loss of the returned receipt: the host saves no success marker.
    backend.drain(Duration::from_secs(1)).await.unwrap();
    drop(backend);
    let resumed = f.backend().await;
    resumed.prepare_items("create-1", item("A")).await.unwrap();
    let replay = resumed.dispatch("create-1").await.unwrap();
    let CommandOutcome::Replay {
        result,
        committed_generation,
        ..
    } = replay
    else {
        panic!("{replay:?}")
    };
    assert_eq!(result, first_result);
    assert_eq!(committed_generation, first_generation);
    assert_eq!(f.count("items"), 1);
    assert_eq!(f.count("catalog_command_receipts"), 1);
    let unchanged: String = sqlx::query_scalar(
        "SELECT envelope_json FROM catabolic_commands WHERE operation_id='create-1'",
    )
    .fetch_one(&f.db)
    .await
    .unwrap();
    assert_eq!(saved, unchanged);
    f.db.close().await;
}

#[tokio::test]
async fn same_key_different_payload_is_refused_without_replacing_intent() {
    let f = Fixture::new().await;
    let backend = f.backend().await;
    backend
        .prepare_items("key", item("Original"))
        .await
        .unwrap();
    assert!(backend.prepare_items("key", item("Changed")).await.is_err());
    assert!(matches!(
        backend.dispatch("key").await.unwrap(),
        CommandOutcome::Committed { .. }
    ));
    assert_eq!(f.count("items"), 1);
    assert!(
        sqlx::query("DELETE FROM catabolic_commands")
            .execute(&f.db)
            .await
            .is_err()
    );
    f.db.close().await;
}

#[tokio::test]
async fn stale_intent_remains_stale_instead_of_silently_rebasing() {
    let f = Fixture::new().await;
    let backend = f.backend().await;
    backend.prepare_items("first", item("A")).await.unwrap();
    backend.prepare_items("stale", item("B")).await.unwrap();
    assert!(matches!(
        backend.dispatch("first").await.unwrap(),
        CommandOutcome::Committed { .. }
    ));
    assert_eq!(
        backend.dispatch("stale").await.unwrap(),
        CommandOutcome::StaleGeneration
    );
    backend.prepare_items("stale", item("B")).await.unwrap();
    assert_eq!(
        backend.dispatch("stale").await.unwrap(),
        CommandOutcome::StaleGeneration
    );
    assert_eq!(f.count("items"), 1);
    f.db.close().await;
}

#[tokio::test]
async fn different_catalog_cannot_replace_a_persisted_binding() {
    let f = Fixture::new().await;
    let _backend = f.backend().await;
    let other = f.path.with_file_name("other.sqlite3");
    initialize(other.clone()).await.unwrap();
    let observed = inspect(other.clone()).await.unwrap();
    assert!(
        NativeCatalog::bind(f.db.clone(), other, observed.identity)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM catabolic_binding")
            .execute(&f.db)
            .await
            .is_err()
    );
    f.db.close().await;
}

#[tokio::test]
async fn inspect_never_creates_and_drain_permanently_closes_admission() {
    let f = Fixture::new().await;
    let absent = f.path.with_file_name("absent.sqlite3");
    assert!(inspect(absent.clone()).await.is_err());
    assert!(!absent.exists());
    assert!(initialize(f.path.clone()).await.is_err());
    let backend = f.backend().await;
    backend.drain(Duration::from_secs(1)).await.unwrap();
    assert!(backend.status().await.is_err());
    assert!(backend.dispatch("never-recorded").await.is_err());
    f.db.close().await;
}

#[tokio::test]
async fn native_runtime_opens_current_schema_with_explicit_host_authority() {
    use catabolic_core::catalog_host_lease::HostAuthorization;
    use catabolic_runtime::host::{CONTRACT_VERSION, HostSession};
    let f = Fixture::new().await;
    let path = f.path.clone();
    let observed = inspect(path.clone()).await.unwrap();
    let sqlite: String = sqlx::query_scalar("SELECT sqlite_source_id()")
        .fetch_one(&f.db)
        .await
        .unwrap();
    assert_eq!(sqlite, observed.sqlite_source_id);
    let math: f64 = sqlx::query_scalar("SELECT sqrt(9)")
        .fetch_one(&f.db)
        .await
        .unwrap();
    assert_eq!(math, 3.0);
    assert!(
        sqlx::query_scalar::<_, String>("SELECT soundex('a')")
            .fetch_one(&f.db)
            .await
            .is_err()
    );
    println!(
        "native schema {}; shared SQLite source {}",
        observed.identity.schema_version, sqlite
    );
    let expected = observed.identity;
    tokio::task::spawn_blocking(move || {
        let gate = HostAuthorization::new("motion-fixture".into(), "revision-1".into()).unwrap();
        let mut host =
            HostSession::open_authorized(&path, "default", false, CONTRACT_VERSION, gate).unwrap();
        assert_eq!(host.identity().unwrap()["catalog_id"], expected.catalog_id);
        host.close().unwrap();
    })
    .await
    .unwrap();
    f.db.close().await;
}

#[tokio::test]
async fn concurrent_conflicting_preparations_persist_one_intent() {
    let f = Fixture::new().await;
    let backend = f.backend().await;
    let (a, b) = tokio::join!(
        backend.prepare_items("racing-key", item("A")),
        backend.prepare_items("racing-key", item("B")),
    );
    assert_ne!(
        a.is_ok(),
        b.is_ok(),
        "exactly one different payload owns the key"
    );
    assert_eq!(f.count("items"), 0);
    assert!(matches!(
        backend.dispatch("racing-key").await.unwrap(),
        CommandOutcome::Committed { .. }
    ));
    assert!(matches!(
        backend.dispatch("racing-key").await.unwrap(),
        CommandOutcome::Replay { .. }
    ));
    assert_eq!(f.count("items"), 1);
    assert_eq!(f.count("catalog_command_receipts"), 1);
    f.db.close().await;
}

fn import_snapshot(evidence: bool) -> motion_catalog_backend::migration::Snapshot {
    use catabolic_store::catalog_import::*;
    let batches = vec![ImportBatch {
        items: vec![ImportItem {
            source_id: "motion-work-1".into(),
            item_id: "motion-work-1".into(),
            kind: "video".into(),
            legacy_metadata: json!({"title":"A"}),
        }],
        unresolved_evidence: if evidence {
            vec![UnresolvedEvidence {
                evidence_id: "physical-1".into(),
                kind: EvidenceKind::PhysicalOccurrence,
                data: json!({"path":"/must/not/be/opened"}),
            }]
        } else {
            vec![]
        },
        typed: None,
    }];
    motion_catalog_backend::migration::Snapshot {
        manifest: SnapshotManifest {
            contract_version: IMPORT_CONTRACT_VERSION,
            import_id: "migration-1".into(),
            source_namespace: "motion".into(),
            source_snapshot_id: "snapshot-1".into(),
            batches: batches.iter().map(|b| describe_batch(b).unwrap()).collect(),
        },
        batches,
    }
}

#[tokio::test]
async fn staged_import_resumes_from_durable_bytes_without_publishing_items() {
    use catabolic_store::catalog_import::ImportReceipt;
    let f = Fixture::new().await;
    let backend = f.backend().await;
    backend
        .prepare_import(import_snapshot(false))
        .await
        .unwrap();
    assert_eq!(f.count("catalog_command_receipts"), 0);
    assert_eq!(f.count("items"), 0);
    let first = backend.stage_import("migration-1").await.unwrap();
    assert_eq!(first.len(), 2);
    assert_eq!(f.count("catalog_command_receipts"), 2);
    backend.drain(Duration::from_secs(1)).await.unwrap();
    drop(backend);
    let resumed = f.backend().await;
    assert_eq!(resumed.stage_import("migration-1").await.unwrap(), first);
    assert_eq!(f.count("catalog_command_receipts"), 2);
    let validated = resumed.validate_import("migration-1", None).await.unwrap();
    let ImportReceipt::ValidatedStaging {
        activation_supported,
        unresolved_evidence,
        proposed_mappings,
        ..
    } = &validated
    else {
        panic!("{validated:?}")
    };
    assert!(*activation_supported);
    assert_eq!(*unresolved_evidence, 0);
    assert_eq!(proposed_mappings.len(), 1);
    assert_eq!(proposed_mappings[0].source_id, "motion-work-1");
    assert_eq!(proposed_mappings[0].item_id, "motion-work-1");
    assert_eq!(
        resumed.validate_import("migration-1", None).await.unwrap(),
        validated
    );
    assert_eq!(f.count("catalog_command_receipts"), 3);
    assert_eq!(f.count("items"), 0);
}

#[tokio::test]
async fn changed_or_corrupt_import_snapshot_cannot_replace_durable_source() {
    let f = Fixture::new().await;
    let backend = f.backend().await;
    let original = import_snapshot(false);
    let mut corrupt = original.clone();
    corrupt.batches[0].items[0].legacy_metadata = json!({"title":"changed"});
    assert!(
        backend
            .prepare_import(corrupt.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("descriptor mismatch")
    );
    backend.prepare_import(original.clone()).await.unwrap();
    backend.prepare_import(original).await.unwrap();
    corrupt.manifest.batches[0] =
        catabolic_store::catalog_import::describe_batch(&corrupt.batches[0]).unwrap();
    assert!(
        backend
            .prepare_import(corrupt)
            .await
            .unwrap_err()
            .to_string()
            .contains("different snapshot")
    );
    assert!(
        sqlx::query("DELETE FROM catabolic_import_snapshots")
            .execute(&f.db)
            .await
            .is_err()
    );
    assert_eq!(f.count("catalog_command_receipts"), 0);
    backend.stage_import("migration-1").await.unwrap();
}

#[tokio::test]
async fn unresolved_migration_evidence_is_retained_and_blocks_native_activation() {
    use catabolic_store::catalog_import::ImportReceipt;
    let f = Fixture::new().await;
    let backend = f.backend().await;
    backend.prepare_import(import_snapshot(true)).await.unwrap();
    let staged = backend.stage_import("migration-1").await.unwrap();
    let ImportReceipt::Staged { batch, .. } = &staged[1] else {
        panic!("{:?}", staged[1])
    };
    assert_eq!(
        batch.unresolved_evidence[0].data["path"],
        "/must/not/be/opened"
    );
    let result = backend.validate_import("migration-1", None).await.unwrap();
    assert!(matches!(
        result,
        ImportReceipt::ValidatedStaging {
            activation_supported: false,
            unresolved_evidence: 1,
            ..
        }
    ));
    assert_eq!(f.count("items"), 0);
}

#[tokio::test]
async fn typed_import_requires_independent_owner_grants_and_only_rehearses_writes() {
    use catabolic_core::catalog_metadata::SourceKind;
    use catabolic_store::{
        catalog_import::{self, ImportReceipt},
        catalog_import_typed::{
            ImportedContribution, ImportedMetadata, ImportedSourceKind, TypedAuthority, TypedBatch,
        },
        catalog_metadata_store::{MetadataAccess, MetadataAuthority},
    };
    let f = Fixture::new().await;
    let backend = f.backend().await;
    let mut snapshot = import_snapshot(false);
    snapshot.manifest.contract_version = catalog_import::TYPED_IMPORT_CONTRACT_VERSION;
    snapshot.batches[0].typed = Some(TypedBatch {
        metadata: vec![ImportedMetadata {
            item_id: "motion-work-1".into(),
            policies: json!([{"field":"title","value_type":"string","nullable":false,"normalization":"exact"}]),
            contributions: vec![ImportedContribution {
                namespace: "motion.scan".into(),
                record_id: "origin".into(),
                source_revision: 7,
                kind: ImportedSourceKind::Scan,
                fields: json!({"title":"Original title"}),
                evidence: json!({"source":"item_origins"}),
            }],
        }],
        ..Default::default()
    });
    snapshot.manifest.batches[0] = catalog_import::describe_batch(&snapshot.batches[0]).unwrap();
    backend.prepare_import(snapshot).await.unwrap();
    backend.stage_import("migration-1").await.unwrap();
    assert!(backend.validate_import("migration-1", None).await.is_err());
    assert_eq!(f.count("catalog_command_receipts"), 2);
    let grants = TypedAuthority {
        metadata: vec![
            MetadataAuthority {
                host_namespace: "motion.catalog.operator.v1".into(),
                subject: "motion-work-1".into(),
                access: MetadataAccess::Initialize,
            },
            MetadataAuthority {
                host_namespace: "motion.catalog.operator.v1".into(),
                subject: "motion-work-1".into(),
                access: MetadataAccess::Source {
                    namespace: "motion.scan".into(),
                    record_id: "origin".into(),
                    kind: SourceKind::Scan,
                },
            },
        ],
        ..Default::default()
    };
    let result = backend
        .validate_import("migration-1", Some(grants))
        .await
        .unwrap();
    let ImportReceipt::ValidatedStaging {
        activation_supported,
        proposed_typed_mappings,
        ..
    } = result
    else {
        panic!("{result:?}")
    };
    assert!(activation_supported);
    assert_eq!(proposed_typed_mappings.len(), 1);
    assert_eq!(proposed_typed_mappings[0]["source_revision"], 7);
    assert_eq!(proposed_typed_mappings[0]["namespace"], "motion.scan");
    assert_eq!(f.count("items"), 0);
    assert_eq!(f.count("catalog_command_receipts"), 3);
}

#[tokio::test]
async fn cross_batch_duplicate_identity_is_rejected_at_native_validation() {
    let f = Fixture::new().await;
    let backend = f.backend().await;
    let mut snapshot = import_snapshot(false);
    snapshot.batches.push(snapshot.batches[0].clone());
    snapshot
        .manifest
        .batches
        .push(snapshot.manifest.batches[0].clone());
    backend.prepare_import(snapshot).await.unwrap();
    backend.stage_import("migration-1").await.unwrap();
    assert!(backend.validate_import("migration-1", None).await.is_err());
    assert_eq!(f.count("items"), 0);
    assert_eq!(f.count("catalog_command_receipts"), 3);
}

#[tokio::test]
async fn interrupted_import_resumes_after_a_committed_batch_without_duplicate_effects() {
    use catabolic_store::catalog_import::{self, ImportReceipt, ImportStep};
    let f = Fixture::new().await;
    let backend = f.backend().await;
    let mut snapshot = import_snapshot(false);
    let mut second = snapshot.batches[0].clone();
    second.items[0].source_id = "motion-work-2".into();
    second.items[0].item_id = "motion-work-2".into();
    snapshot
        .manifest
        .batches
        .push(catalog_import::describe_batch(&second).unwrap());
    snapshot.batches.push(second);
    let second_key = catalog_import::operation_id("migration-1", ImportStep::Batch(1)).unwrap();
    // Simulate the journal becoming unavailable after the prior batch committed.
    // The key is produced by native code and hex-only, not user-supplied SQL.
    sqlx::raw_sql(&format!("CREATE TRIGGER reject_next_batch BEFORE INSERT ON catabolic_commands WHEN NEW.operation_id='{second_key}' BEGIN SELECT RAISE(ABORT,'injected journal failure'); END;"))
        .execute(&f.db).await.unwrap();
    backend.prepare_import(snapshot).await.unwrap();
    assert!(
        backend
            .stage_import("migration-1")
            .await
            .unwrap_err()
            .to_string()
            .contains("injected journal failure")
    );
    assert_eq!(f.count("catalog_command_receipts"), 2);
    assert_eq!(f.count("items"), 0);
    backend.drain(Duration::from_secs(1)).await.unwrap();
    drop(backend);
    sqlx::raw_sql("DROP TRIGGER reject_next_batch")
        .execute(&f.db)
        .await
        .unwrap();
    let resumed = f.backend().await;
    assert_eq!(resumed.stage_import("migration-1").await.unwrap().len(), 3);
    assert_eq!(f.count("catalog_command_receipts"), 3);
    let result = resumed.validate_import("migration-1", None).await.unwrap();
    let ImportReceipt::ValidatedStaging {
        proposed_mappings,
        activation_supported,
        ..
    } = result
    else {
        panic!("{result:?}")
    };
    assert!(activation_supported);
    assert_eq!(proposed_mappings.len(), 2);
    assert_eq!(f.count("catalog_command_receipts"), 4);
    assert_eq!(f.count("items"), 0);
}
