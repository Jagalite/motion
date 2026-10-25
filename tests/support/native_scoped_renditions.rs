//! Actual public native rendition scope and replay authorization.
use super::native_application_fixture::{Fixture, OPERATOR};
use axum::http::StatusCode;
use serde_json::{Value, json};

pub async fn exercise(
    f: &mut Fixture,
    source: &str,
    timeline: &str,
    completed: &str,
    input: Value,
    plan_input: Value,
    output_bytes: &[u8],
    foreign_download_auth: &str,
) -> String {
    let operator = format!("Bearer {OPERATOR}");
    let original_recipe: String =
        sqlx::query_scalar("SELECT recipe FROM processing_jobs WHERE id=?")
            .bind(completed)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    let changed = sqlx::query("UPDATE processing_jobs SET recipe='audio_aac' WHERE id=?")
        .bind(completed)
        .execute(&f.app.db)
        .await;
    match changed {
        Ok(result) => {
            assert_eq!(result.rows_affected(), 1);
            let refused = f
                .call(
                    "GET",
                    &format!("/api/v2/processing/renditions?timeline_id={timeline}"),
                    None,
                    &[("authorization", &operator)],
                )
                .await;
            sqlx::query("UPDATE processing_jobs SET recipe=? WHERE id=?")
                .bind(&original_recipe)
                .bind(completed)
                .execute(&f.app.db)
                .await
                .unwrap();
            assert_eq!(
                refused.status,
                StatusCode::INTERNAL_SERVER_ERROR,
                "changed application recipe must not be presented as receipt provenance: {:?}",
                refused.body
            );
        }
        Err(error) => {
            assert!(
                error
                    .as_database_error()
                    .is_some_and(|e| e.code().as_deref() == Some("1811")),
                "only an immutable trigger refusal qualifies: {error}"
            );
        }
    }
    let recipe: String = sqlx::query_scalar("SELECT recipe FROM processing_jobs WHERE id=?")
        .bind(completed)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(recipe, original_recipe);
    let restored = f
        .call(
            "GET",
            &format!("/api/v2/processing/renditions?timeline_id={timeline}"),
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(restored.status, StatusCode::OK, "{:?}", restored.body);
    assert!(
        restored.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["id"] == completed && v["status"] == "ready")
    );

    let output = restored.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == completed)
        .unwrap()["output"]["file_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let completed_body = restored.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == completed)
        .unwrap();
    let version = completed_body["version_id"].as_str().unwrap().to_owned();
    let revision = completed_body["output"]["file_revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut libraries = Vec::new();
    for (name, sources, key) in [
        ("Rendition source", vec![source], "rendition-shared-library"),
        ("Unrelated renditions", vec![], "rendition-empty-library"),
    ] {
        let r = f
            .call(
                "POST",
                "/api/v2/libraries",
                Some(json!({"name":name,"kind":"mixed","language":"en","source_ids":sources})),
                &[("authorization", &operator), ("idempotency-key", key)],
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
        libraries.push(r.body["id"].as_str().unwrap().to_owned());
    }
    let legacy: i64 =
        sqlx::query_scalar("SELECT count(*) FROM catalog_libraries WHERE id=? OR id=?")
            .bind(&libraries[0])
            .bind(&libraries[1])
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(legacy, 0);
    let (device, token) = f
        .pair(
            &["default"],
            &[
                "catalog:read",
                "processing:request",
                "playback:request",
                "viewing:write",
                "downloads:manage",
            ],
        )
        .await;
    let auth = format!("Bearer {token}");
    let path = format!("/api/v2/processing/renditions?timeline_id={timeline}&limit=1");
    assert_eq!(
        f.call("GET", &path, None, &[]).await.status,
        StatusCode::UNAUTHORIZED
    );
    let mut queued = None;
    let mut scoped_download: Option<super::native_generated_downloads::Saved> = None;
    for (step, library, permission) in [
        (0, &libraries[0], true),
        (1, &libraries[1], true),
        (2, &libraries[0], false),
        (3, &libraries[0], true),
    ] {
        let current = f
            .call(
                "GET",
                &format!("/api/v2/devices/{device}"),
                None,
                &[("authorization", &operator)],
            )
            .await;
        assert_eq!(current.status, StatusCode::OK);
        let permissions = if permission {
            json!([
                "catalog:read",
                "processing:request",
                "playback:request",
                "viewing:write",
                "downloads:manage"
            ])
        } else {
            json!([
                "catalog:read",
                "playback:request",
                "viewing:write",
                "downloads:manage"
            ])
        };
        let changed=f.call("PUT",&format!("/api/v2/devices/{device}/policy"),Some(json!({"library_ids":[library],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":permissions})),&[("authorization",&operator),("if-match",current.headers["etag"].to_str().unwrap())]).await;
        assert_eq!(changed.status, StatusCode::OK, "{:?}", changed.body);
        f.close().await;
        f.open().await;
        if step == 0 {
            scoped_download = Some(
                super::native_generated_downloads::create(
                    f,
                    &auth,
                    foreign_download_auth,
                    timeline,
                    &version,
                    &output,
                    &revision,
                    &original_recipe,
                    output_bytes,
                    "scoped",
                )
                .await,
            );
        } else {
            let download = scoped_download.as_ref().unwrap();
            if step == 1 {
                let refused = f
                    .download_adapter(
                        "POST",
                        "/downloads",
                        &auth,
                        Some(download.input.clone()),
                        Some("wrong-scope-generated-download"),
                    )
                    .await;
                assert_eq!(refused.status, StatusCode::NOT_FOUND, "{:?}", refused.body);
                super::native_generated_downloads::check(f, &auth, download, "expired", None).await;
            } else {
                super::native_generated_downloads::check(
                    f,
                    &auth,
                    download,
                    "ready",
                    Some(output_bytes),
                )
                .await;
            }
        }

        if step == 1 {
            let (_, html) = super::native_presentation::player(f, timeline, &auth).await;
            assert!(
                !html.contains("value=\"a0\""),
                "wrong native library cannot disclose measured track choices"
            );
        }
        if step == 1 {
            let generated = f
                .call(
                    "GET",
                    &format!("/api/v2/media/files/{output}/content"),
                    None,
                    &[("authorization", &auth)],
                )
                .await;
            assert_eq!(
                generated.status,
                StatusCode::NOT_FOUND,
                "wrong native library must not obtain generated bytes: {:?}",
                generated.body
            );
        } else {
            super::assert_generated_range(f, &output, &revision, &auth, output_bytes).await;
            let proof:String=sqlx::query_scalar("SELECT m.proof_json FROM catabolic_projection_generated_libraries m JOIN catabolic_projection_state s ON s.generation=m.generation WHERE m.file_id=? AND m.library_id=?").bind(&output).bind(library).fetch_one(&f.app.db).await.unwrap();
            let proof: Value = serde_json::from_str(&proof).unwrap();
            assert_eq!(proof["contract_version"], 2);
            assert_eq!(proof["kind"], "generated");
            assert_eq!(proof["input_source_id"], source);
            assert_eq!(proof["lifecycle"]["state"], "active");
            assert!(proof["publication"]["admission_digest"].is_string());
            let refused =
                sqlx::query("DELETE FROM catabolic_projection_generated_libraries WHERE file_id=?")
                    .bind(&output)
                    .execute(&f.app.db)
                    .await;
            assert!(
                refused.is_err(),
                "ordinary SQL must not alter generated scope authority"
            );
            if step == 0 || step == 3 {
                super::native_generated_playback::exercise(
                    f,
                    plan_input.clone(),
                    &version,
                    &output,
                    &revision,
                    &auth,
                    &operator,
                    output_bytes,
                    &format!("scoped-{step}"),
                )
                .await;
            }
        }
        let listed = f
            .call("GET", &path, None, &[("authorization", &auth)])
            .await;
        if !permission {
            assert_eq!(listed.status, StatusCode::FORBIDDEN, "{:?}", listed.body);
        } else {
            assert_eq!(listed.status, StatusCode::OK, "{:?}", listed.body);
            if step == 1 {
                assert_eq!(listed.body["items"], json!([]));
            } else {
                // Follow real authorized keyset pages: a queued replay row may
                // sort before the completed output; neither may hide the other.
                let mut items = listed.body["items"].as_array().unwrap().clone();
                let mut cursor = listed.body["next_cursor"].as_str().map(str::to_owned);
                while let Some(c) = cursor {
                    let next = f
                        .call(
                            "GET",
                            &format!("{path}&cursor={c}"),
                            None,
                            &[("authorization", &auth)],
                        )
                        .await;
                    assert_eq!(next.status, StatusCode::OK, "{:?}", next.body);
                    items.extend(next.body["items"].as_array().unwrap().clone());
                    cursor = next.body["next_cursor"].as_str().map(str::to_owned);
                }
                let ready = items
                    .iter()
                    .find(|v| v["id"] == completed)
                    .expect("native library must disclose its real completed rendition");
                assert_eq!(ready["status"], "ready");
                assert!(ready["output"]["file_id"].is_string());
                assert!(ready["version_id"].is_string());
            }
        }
        let preparation = f
            .call(
                "POST",
                "/api/v2/processing/preparations",
                Some(input.clone()),
                &[
                    ("authorization", &auth),
                    ("idempotency-key", "scoped-rendition-replay"),
                ],
            )
            .await;
        assert_eq!(
            preparation.status,
            if !permission {
                StatusCode::FORBIDDEN
            } else if step == 1 {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::ACCEPTED
            },
            "{:?}",
            preparation.body
        );
        if step == 0 {
            queued = preparation.body["id"].as_str().map(str::to_owned);
        }
        if step == 3 {
            assert_eq!(preparation.body["id"], queued.as_deref().unwrap());
        }
    }
    auth
}
