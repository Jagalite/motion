//! Library-only acceptance invoked by the real original-download fixture.
//! Public manifest, generated and delivery routes remain separately gated.
use super::native_application_fixture::{Fixture, OPERATOR};
use axum::http::StatusCode;
use serde_json::json;

pub async fn exercise(f: &mut Fixture, source: &str, bytes: &[u8]) {
    let operator = format!("Bearer {OPERATOR}");
    let mut library_ids = Vec::new();
    for (name, sources, key) in [
        ("Shared scoped source", vec![source], "scope-shared-library"),
        ("Other empty library", vec![], "scope-other-library"),
    ] {
        let library = f
            .call(
                "POST",
                "/api/v2/libraries",
                Some(json!({"name":name,"kind":"mixed","language":"en","source_ids":sources})),
                &[("authorization", &operator), ("idempotency-key", key)],
            )
            .await;
        assert_eq!(library.status, StatusCode::CREATED, "{:?}", library.body);
        library_ids.push(library.body["id"].as_str().unwrap().to_owned());
    }
    let legacy: i64 =
        sqlx::query_scalar("SELECT count(*) FROM catalog_libraries WHERE id=? OR id=?")
            .bind(&library_ids[0])
            .bind(&library_ids[1])
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(legacy, 0, "new scoped definitions must remain native-owned");
    let permissions = ["catalog:read", "viewing:write", "downloads:manage"];
    let (device, token) = f.pair(&["default"], &permissions).await;
    let auth = format!("Bearer {token}");
    let revision = catabolic_core::sha256(bytes);
    let mut download = None::<String>;
    // Real current policy mutation, not compatibility membership manipulation.
    for (index, library) in [
        library_ids[0].clone(),
        library_ids[1].clone(),
        library_ids[0].clone(),
    ]
    .into_iter()
    .enumerate()
    {
        let current = f
            .call(
                "GET",
                &format!("/api/v2/devices/{device}"),
                None,
                &[("authorization", &operator)],
            )
            .await;
        assert_eq!(current.status, StatusCode::OK, "{:?}", current.body);
        let changed=f.call("PUT",&format!("/api/v2/devices/{device}/policy"),Some(json!({"library_ids":[library],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":permissions})),
            &[("authorization",&operator),("if-match",current.headers["etag"].to_str().unwrap())]).await;
        assert_eq!(changed.status, StatusCode::OK, "{:?}", changed.body);
        let allowed = index != 1;
        let viewing = f.viewing_adapter("GET", &auth, None, None).await;
        assert_eq!(
            viewing.status,
            if allowed {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            },
            "{:?}",
            viewing.body
        );
        let (status, body) = f.content_adapter(&auth, &revision).await;
        assert_eq!(
            status,
            if allowed {
                StatusCode::PARTIAL_CONTENT
            } else {
                StatusCode::NOT_FOUND
            }
        );
        if allowed {
            assert_eq!(body, &bytes[..16]);
        }
        if index == 0 {
            let proof: String = sqlx::query_scalar("SELECT p.proof_json FROM catabolic_projection_file_libraries p JOIN catabolic_projection_state s ON s.generation=p.generation WHERE p.file_id='file' AND p.library_id=?")
                .bind(&library_ids[0]).fetch_one(&f.app.db).await.unwrap();
            let proof: serde_json::Value = serde_json::from_str(&proof).unwrap();
            assert_eq!(
                proof["library_native_id"],
                format!("motion.library:{}", library_ids[0])
            );
            assert!(
                proof["scope"]["query_id"]
                    .as_str()
                    .is_some_and(|id| !id.is_empty())
            );
            let changed = f
                .viewing_adapter(
                    "PUT",
                    &auth,
                    Some(json!({"manual_watched":true,"reset_position":false})),
                    Some(viewing.headers["etag"].to_str().unwrap()),
                )
                .await;
            assert_eq!(changed.status, StatusCode::OK, "{:?}", changed.body);
            assert_eq!(changed.body["manual_epoch"], "3");
            let created=f.download_adapter("POST","/downloads",&auth,Some(json!({"device_id":device,"profile_id":"default","timeline_id":"timeline","version_id":"version","recipe_id":null,
                "tracks":{"audio_component_id":null,"subtitle_component_id":null,"subtitle_policy":"off","audio_track_id":null,"subtitle_track_id":null}})),Some("scoped-original-download")).await;
            assert_eq!(created.status, StatusCode::ACCEPTED, "{:?}", created.body);
            assert_eq!(created.body["state"], "ready");
            download = Some(created.body["id"].as_str().unwrap().to_owned());
        }
        let download = download.as_deref().unwrap();
        let observed = f
            .download_adapter("GET", &format!("/downloads/{download}"), &auth, None, None)
            .await;
        assert_eq!(observed.status, StatusCode::OK, "{:?}", observed.body);
        assert_eq!(
            observed.body["state"],
            if allowed { "ready" } else { "expired" }
        );
        let manifest = f.manifest_adapter(download, &auth).await;
        assert_eq!(
            manifest.status,
            if allowed {
                StatusCode::OK
            } else {
                StatusCode::CONFLICT
            },
            "{:?}",
            manifest.body
        );
        // No public manifest/ticket/generation route is opened by this cohort.
        f.close().await;
        f.open().await;
        let (status, body) = f.content_adapter(&auth, &revision).await;
        assert_eq!(
            status,
            if allowed {
                StatusCode::PARTIAL_CONTENT
            } else {
                StatusCode::NOT_FOUND
            }
        );
        if allowed {
            assert_eq!(body, &bytes[..16]);
        }
    }
    let before:(i64,i64)=sqlx::query_as("SELECT position_ms,manual_epoch FROM timeline_viewing WHERE profile_id='default' AND timeline_id='timeline'").fetch_one(&f.app.db).await.unwrap();
    assert_eq!(
        before,
        (250, 3),
        "policy changes/restarts do not rewrite history"
    );
}
