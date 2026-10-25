//! Compare public reads with a genuine native worker's observed durable row.
//! Call only after an existing fixture establishes its actual worker boundary.
use super::native_application_fixture::Fixture;
use axum::http::StatusCode;
use serde_json::{Value, json};

pub async fn observed(f: &Fixture, id: &str, auth: &str) {
    let saved: (i64, String, i64, Option<String>, Option<String>, String) = sqlx::query_as(
        "SELECT revision,phase,attempt,error,output_file_id,requester_id FROM processing_jobs WHERE id=?",
    ).bind(id).fetch_one(&f.app.db).await.unwrap();
    let detail = f
        .call(
            "GET",
            &format!("/api/v2/jobs/{id}"),
            None,
            &[("authorization", auth)],
        )
        .await;
    assert_eq!(detail.status, StatusCode::OK, "{}", detail.body);
    assert_eq!(
        detail.body,
        json!({
            "id":id,"revision":saved.0.to_string(),"kind":"rendition","phase":saved.1,
            "attempt_generation":saved.2.to_string(),"progress":Value::Null,
            "error_code":saved.3.as_ref().map(|_| "worker_failed"),
            "result_ids":saved.4.iter().collect::<Vec<_>>(),"requester_id":saved.5
        })
    );
    assert_eq!(
        detail.headers["etag"].to_str().unwrap(),
        format!("\"r-{}\"", saved.0)
    );
    let list = f
        .call("GET", "/api/v2/jobs", None, &[("authorization", auth)])
        .await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    assert_eq!(
        list.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id),
        Some(&detail.body)
    );
    let unchanged: (i64, String, i64, Option<String>, Option<String>, String) = sqlx::query_as(
        "SELECT revision,phase,attempt,error,output_file_id,requester_id FROM processing_jobs WHERE id=?",
    ).bind(id).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(
        unchanged, saved,
        "a read cannot ACK, complete, retry or reinterpret the worker"
    );
}
