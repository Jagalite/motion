//! Synchronous copied-image admission and live SQLx admission must make the same
//! decision from identical retained upgrade rows. No migration or restore runs.
use super::*;
use serde_json::json;
use sqlx::Connection as _;

fn original() -> Identity {
    Identity {
        catalog_id: "catalog".into(),
        incarnation: "original-incarnation".into(),
        schema_version: 39,
    }
}
fn retained_intent() -> Intent {
    let from = original();
    let to = Identity {
        schema_version: 40,
        ..from.clone()
    };
    let native_path = PathBuf::from("/retained/native.sqlite3");
    let mut plan = json!({
        "from_schema":39,"to_schema":40,
        "input":{"database":native_path,"catalog_identity":{"catalog_id":from.catalog_id,"incarnation":from.incarnation}},
        "input_fingerprint":"retained-input","native_schema_fingerprint":"retained-schema"
    });
    plan["plan_fingerprint"] = json!(catabolic_core::sha256(catabolic_core::encode(&plan)));
    Intent {
        contract_version: 1,
        from,
        to,
        native_path,
        plan,
    }
}
fn proof(intent: &Intent) -> Value {
    json!({"contract_version":1,"catalog_id":intent.from.catalog_id,
        "incarnation":intent.from.incarnation,"from_schema":39,"to_schema":40,
        "plan_fingerprint":intent.plan["plan_fingerprint"],
        "input_fingerprint":intent.plan["input_fingerprint"],
        "native_schema_fingerprint":intent.plan["native_schema_fingerprint"]})
}
async fn compare(db: &Connection, path: &Path, original: &Identity) -> anyhow::Result<Identity> {
    let sync = effective_identity_sync(db, original);
    let mut live = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .read_only(true),
    )
    .await
    .unwrap();
    let asynchronous = effective_identity_in(&mut live, original).await;
    match (&sync, &asynchronous) {
        (Ok(a), Ok(b)) => assert_eq!(a, b),
        (Err(a), Err(b)) => assert_eq!(a.to_string(), b.to_string()),
        _ => panic!("sync/async identity decision differs: {sync:?} / {asynchronous:?}"),
    }
    sync
}
#[tokio::test]
async fn copied_upgrade_identity_matches_live_empty_pending_ack_and_foreign_binding() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("motion.sqlite3");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(include_str!(
        "../../../../migrations/0089_catabolic_native_upgrade.sql"
    ))
    .unwrap();
    let original = original();
    assert_eq!(compare(&db, &path, &original).await.unwrap(), original);
    let intent = retained_intent();
    insert(&db, "intent", serde_json::to_string(&intent).unwrap()).unwrap();
    assert!(
        compare(&db, &path, &original)
            .await
            .unwrap_err()
            .to_string()
            .contains("pending")
    );
    insert(&db, "ack", proof(&intent).to_string()).unwrap();
    assert_eq!(compare(&db, &path, &original).await.unwrap(), intent.to);
    let foreign = Identity {
        incarnation: "different".into(),
        ..original
    };
    assert!(compare(&db, &path, &foreign).await.is_err());
}
#[tokio::test]
async fn copied_upgrade_refuses_changed_ack_and_capability_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("motion.sqlite3");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(include_str!(
        "../../../../migrations/0089_catabolic_native_upgrade.sql"
    ))
    .unwrap();
    let intent = retained_intent();
    insert(&db, "intent", serde_json::to_string(&intent).unwrap()).unwrap();
    let mut bad = proof(&intent);
    bad["input_fingerprint"] = json!("wrong");
    insert(&db, "ack", bad.to_string()).unwrap();
    assert!(
        compare(&db, &path, &original())
            .await
            .unwrap_err()
            .to_string()
            .contains("ACK")
    );
    db.execute_batch("DROP TRIGGER catabolic_native_upgrade_ack_update")
        .unwrap();
    assert!(
        compare(&db, &path, &original())
            .await
            .unwrap_err()
            .to_string()
            .contains("schema or capability")
    );
}
#[tokio::test]
async fn copied_upgrade_missing_journal_accepts_only_reviewed_historical_schemas() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("motion.sqlite3");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(
        "CREATE TABLE _sqlx_migrations(version INTEGER NOT NULL,success INTEGER NOT NULL)",
    )
    .unwrap();
    for (schema, accepted) in [
        (68, false),
        (69, true),
        (88, true),
        (89, false),
        (93, false),
    ] {
        db.execute("DELETE FROM _sqlx_migrations", []).unwrap();
        db.execute("INSERT INTO _sqlx_migrations VALUES(?,1)", [schema])
            .unwrap();
        assert_eq!(compare(&db, &path, &original()).await.is_ok(), accepted);
    }
}
