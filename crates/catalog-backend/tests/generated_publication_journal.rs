//! Ordering and immutability of private publication control records. These do
//! not qualify generated native effects, subprocess retirement or HTTP routes.
use sqlx::{Connection, Executor};

async fn fixture() -> sqlx::SqliteConnection {
    let mut db = sqlx::SqliteConnection::connect("sqlite::memory:")
        .await
        .unwrap();
    db.execute(include_str!(
        "../../../migrations/0087_catabolic_generated_publications.sql"
    ))
    .await
    .unwrap();
    db
}

#[tokio::test]
async fn publication_intent_proofs_and_acknowledgment_are_monotonic() {
    let mut db = fixture().await;
    db.execute("INSERT INTO catabolic_generated_publications(operation_id,job_id,attempt,source_json,output_relative,created_at) VALUES('op','job',1,'{}','job/1/output.mp4',1)").await.unwrap();
    for forbidden in [
        "UPDATE catabolic_generated_publications SET receipt_json='{}'",
        "UPDATE catabolic_generated_publications SET command_json='{}'",
        "UPDATE catabolic_generated_publications SET acknowledged=1",
        "UPDATE catabolic_generated_publications SET source_json='{\"other\":1}'",
        "UPDATE catabolic_generated_publications SET output_relative='other.mp4'",
        "UPDATE catabolic_generated_publications SET attempt=2",
        "DELETE FROM catabolic_generated_publications",
    ] {
        assert!(db.execute(forbidden).await.is_err(), "accepted {forbidden}");
    }
    db.execute("UPDATE catabolic_generated_publications SET output_json='{\"sha256\":\"exact\"}',command_json='{\"request\":1}'").await.unwrap();
    db.execute("UPDATE catabolic_generated_publications SET receipt_json='{\"receipt\":1}'")
        .await
        .unwrap();
    db.execute("UPDATE catabolic_generated_publications SET acknowledged=1")
        .await
        .unwrap();
    for forbidden in [
        "UPDATE catabolic_generated_publications SET output_json=NULL",
        "UPDATE catabolic_generated_publications SET command_json='{\"request\":2}'",
        "UPDATE catabolic_generated_publications SET receipt_json=NULL",
        "UPDATE catabolic_generated_publications SET acknowledged=0",
    ] {
        assert!(db.execute(forbidden).await.is_err(), "accepted {forbidden}");
    }
    // Exact repeated recording is harmless; no new identity or effect exists.
    db.execute("UPDATE catabolic_generated_publications SET acknowledged=1")
        .await
        .unwrap();
}

#[tokio::test]
async fn generated_root_is_private_singleton_and_failed_ack_rolls_back() {
    let mut db = fixture().await;
    assert!(
        db.execute("INSERT INTO catabolic_generated_source VALUES('public-source','{}',1)")
            .await
            .is_err()
    );
    db.execute("INSERT INTO catabolic_generated_source VALUES('motion:generated-output','{\"root\":\"retained\"}',1)").await.unwrap();
    assert!(
        db.execute("UPDATE catabolic_generated_source SET mapping_json='{}'")
            .await
            .is_err()
    );
    assert!(
        db.execute("DELETE FROM catabolic_generated_source")
            .await
            .is_err()
    );
    let mut tx = db.begin().await.unwrap();
    tx.execute("INSERT INTO catabolic_generated_publications(operation_id,job_id,attempt,source_json,output_relative,created_at) VALUES('op','job',1,'{}','job/1/output.mp4',1)").await.unwrap();
    assert!(
        tx.execute("UPDATE catabolic_generated_publications SET acknowledged=1")
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_generated_publications")
        .fetch_one(&mut db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
