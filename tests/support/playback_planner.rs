use super::*;

#[tokio::test]
async fn planner_is_read_only_strict_and_revision_bound() {
    let f = Fixture::new().await;
    std::fs::write(f.root.join("original.mp4"), b"original").unwrap();
    std::fs::write(f.root.join("rendition.mp4"), b"rendition").unwrap();
    assert_eq!(f.scan().await.phase, "completed");
    let items = f.items().await;
    let source = &items[0];
    let output = &items[1];
    let id = source["id"].as_str().unwrap();
    let path = format!("/api/v1/profiles/default/items/{id}/playback-plan");
    // Synthetic metadata isolates planning from actual encoder execution.
    sqlx::query("UPDATE media_files SET duration_seconds=10")
        .execute(&f.app.db)
        .await
        .unwrap();
    let register = format!("/api/v1/items/{id}/renditions/external/test");
    let (status, _) = f
        .json(
            "PUT",
            &register,
            Some(json!({
                "file_id":output["file_id"],"file_revision":output["revision"],
                "source_file_id":source["file_id"],"source_revision":source["revision"],
                "label":"Prepared", "recipe":{"version":1,"recipe":"h264720p"}
            })),
            true,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let failed = json!([{"file_id":source["file_id"],"revision":source["revision"]}]);
    let (status, plan) = f.json("POST", &path, Some(json!({})), false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(plan["selection"]["delivery"], "original");
    assert_eq!(plan["reason"], "client_check_required");
    let (_, plan) = f
        .json(
            "POST",
            &path,
            Some(json!({"mode":"original","failed_versions":failed})),
            false,
        )
        .await;
    assert_eq!(plan["status"], "blocked");
    assert!(plan["preparation"].is_null());
    let (_, plan) = f
        .json(
            "POST",
            &path,
            Some(json!({"failed_versions":failed})),
            false,
        )
        .await;
    assert_eq!(plan["selection"]["file_id"], output["file_id"]);
    assert_eq!(plan["selection"]["operation"], "unknown");
    // Informational external recipe metadata must not satisfy the fixed local contract.
    let (_, plan) = f
        .json("POST", &path, Some(json!({"mode":"convert"})), false)
        .await;
    assert_eq!(plan["status"], "preparation_required");
    assert_eq!(plan["preparation"]["requires_admin"], true);
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM processing_jobs")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM playback_sessions")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!((jobs, sessions), (0, 0));
    let (_, stale_evidence) = f.json("POST", &path, Some(json!({"mode":"original","client_support":[{"file_id":source["file_id"],"revision":"old","support":"unsupported"}]})), false).await;
    assert_eq!(stale_evidence["status"], "ready");
    let (status, _) = f
        .json(
            "POST",
            &path,
            Some(json!({"selected":{"file_id":source["file_id"],"revision":"old"}})),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    for body in [
        json!({"mode":"invalid"}),
        json!({"recipe":"shell_command"}),
        json!({"max_average_bitrate":0}),
        json!({"client_support":[{"file_id":"a","revision":"b","support":"maybe"}]}),
    ] {
        assert_eq!(
            f.json("POST", &path, Some(body), false).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let (_, plan) = f
        .json(
            "POST",
            &path,
            Some(json!({"mode":"original","max_average_bitrate":1})),
            false,
        )
        .await;
    assert_eq!(plan["status"], "blocked");
    // A replaced source invalidates its rendition, even if the output still exists.
    sqlx::query("UPDATE media_files SET revision='replacement' WHERE id=?")
        .bind(source["file_id"].as_str().unwrap())
        .execute(&f.app.db)
        .await
        .unwrap();
    let (_, plan) = f
        .json(
            "POST",
            &path,
            Some(json!({"selected":{"file_id":output["file_id"],"revision":output["revision"]}})),
            false,
        )
        .await;
    assert_eq!(plan["status"], "blocked");
}

#[tokio::test]
async fn planner_preferences_conversion_reuse_and_active_job() {
    let f = Fixture::new().await;
    std::fs::write(f.root.join("movie.mp4"), b"source").unwrap();
    std::fs::write(f.root.join("prepared.mp4"), b"output").unwrap();
    f.scan().await;
    let items = f.items().await;
    let source = &items[0];
    let output = &items[1];
    sqlx::query("UPDATE media_files SET duration_seconds=10")
        .execute(&f.app.db)
        .await
        .unwrap();
    let path = format!(
        "/api/v1/profiles/default/items/{}/playback-plan",
        source["id"].as_str().unwrap()
    );
    let pref_path = "/api/v1/profiles/default/playback-preferences";
    let (status, prefs)=f.json("PUT",pref_path,Some(json!({"expected_revision":0,"preferences":{"audio_languages":[],"subtitle_languages":[],"subtitle_mode":"off","quality":"convert","conversion_recipe":"audio_aac"}})),false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(prefs["preferences"]["conversion_recipe"], "audio_aac");
    let (_, plan) = f.json("POST", &path, Some(json!({})), false).await;
    assert_eq!(plan["preparation"]["recipe"], "audio_aac");
    assert_eq!(
        f.json("POST", &path, Some(json!({"mode":"original"})), false)
            .await
            .1["status"],
        "ready"
    );
    sqlx::query("INSERT INTO processing_jobs(id,source_file_id,source_revision,recipe,backend,idempotency_key,phase,created_at,updated_at) VALUES ('planner-job',?,?,'audio_aac','software','planner-key','queued',1,1)")
        .bind(source["file_id"].as_str().unwrap()).bind(source["revision"].as_str().unwrap()).execute(&f.app.db).await.unwrap();
    let (_, plan) = f.json("POST", &path, Some(json!({})), false).await;
    assert_eq!(plan["preparation"]["job_id"], "planner-job");
    let register = format!(
        "/api/v1/items/{}/renditions/playscale/planner-job",
        source["id"].as_str().unwrap()
    );
    assert_eq!(f.json("PUT",&register,Some(json!({"file_id":output["file_id"],"file_revision":output["revision"],"source_file_id":source["file_id"],"source_revision":source["revision"],"label":"AAC", "recipe":{}})),true).await.0,StatusCode::OK);
    sqlx::query(
        "UPDATE processing_jobs SET phase='completed',output_file_id=? WHERE id='planner-job'",
    )
    .bind(output["file_id"].as_str().unwrap())
    .execute(&f.app.db)
    .await
    .unwrap();
    let (_, plan) = f.json("POST", &path, Some(json!({})), false).await;
    assert_eq!(plan["selection"]["file_id"], output["file_id"]);
    assert_eq!(plan["selection"]["operation"], "audio_conversion");
    let (_, plan) = f
        .json("POST", &path, Some(json!({"recipe":"h264720p"})), false)
        .await;
    assert_eq!(plan["status"], "preparation_required");
    // A completed job only qualifies the exact bytes it published. Re-registering
    // changed bytes under another rendition identity must not inherit its recipe.
    let replacement = "f".repeat(64);
    sqlx::query("UPDATE media_files SET revision=? WHERE id=?")
        .bind(&replacement)
        .bind(output["file_id"].as_str().unwrap())
        .execute(&f.app.db)
        .await
        .unwrap();
    let changed = format!(
        "/api/v1/items/{}/renditions/external/replaced",
        source["id"].as_str().unwrap()
    );
    assert_eq!(f.json("PUT",&changed,Some(json!({"file_id":output["file_id"],"file_revision":replacement,"source_file_id":source["file_id"],"source_revision":source["revision"],"label":"Changed bytes", "recipe":{}})),true).await.0,StatusCode::OK);
    let (_, plan) = f
        .json(
            "POST",
            &path,
            Some(json!({"mode":"convert","recipe":"audio_aac"})),
            false,
        )
        .await;
    assert_eq!(plan["status"], "preparation_required");
    // Old documents and old clients get the default recipe without a migration.
    sqlx::query("UPDATE playback_preferences SET document_json=? WHERE profile_id='default'")
        .bind(r#"{"audio_languages":[],"subtitle_languages":[],"subtitle_mode":"off","quality":"original"}"#).execute(&f.app.db).await.unwrap();
    assert_eq!(
        f.json("GET", pref_path, None, false).await.1["preferences"]["conversion_recipe"],
        "h264720p"
    );
    let (_, spec) = f.json("GET", "/api/v1/openapi.json", None, false).await;
    assert!(
        spec["paths"]["/api/v1/profiles/{profile}/items/{id}/playback-plan"]["post"].is_object()
    );
}

#[tokio::test]
async fn planner_source_pin_preserves_version_and_rejects_stale_identity() {
    let f = Fixture::new().await;
    for name in ["a.mp4", "b.mp4", "c.mp4"] {
        std::fs::write(f.root.join(name), name).unwrap();
    }
    f.scan().await;
    let items = f.items().await;
    let a = &items[0];
    let b = &items[1];
    let output = &items[2];
    sqlx::query("UPDATE editions SET item_id=? WHERE id=?")
        .bind(a["id"].as_str().unwrap())
        .bind(b["edition_id"].as_str().unwrap())
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("UPDATE media_files SET duration_seconds=10")
        .execute(&f.app.db)
        .await
        .unwrap();
    let path = format!(
        "/api/v1/profiles/default/items/{}/playback-plan",
        a["id"].as_str().unwrap()
    );
    let source = json!({"file_id":b["file_id"],"revision":b["revision"]});
    let (_, plan) = f
        .json("POST", &path, Some(json!({"source":source})), false)
        .await;
    assert_eq!(plan["selection"]["file_id"], b["file_id"]);
    assert_eq!(plan["selection"]["source_file_id"], b["file_id"]);
    let (_, plan) = f
        .json(
            "POST",
            &path,
            Some(json!({"source":source,"mode":"convert"})),
            false,
        )
        .await;
    assert_eq!(plan["preparation"]["source_file_id"], b["file_id"]);
    let register = format!(
        "/api/v1/items/{}/renditions/external/other-cut",
        a["id"].as_str().unwrap()
    );
    assert_eq!(f.json("PUT", &register, Some(json!({"file_id":output["file_id"],"file_revision":output["revision"],"source_file_id":a["file_id"],"source_revision":a["revision"],"label":"Other cut", "recipe":{}})), true).await.0, StatusCode::OK);
    let (_, plan) = f
        .json(
            "POST",
            &path,
            Some(json!({"source":source,"failed_versions":[source]})),
            false,
        )
        .await;
    assert_eq!(plan["status"], "preparation_required");
    assert_eq!(plan["preparation"]["source_file_id"], b["file_id"]);
    let (_, plan) = f
        .json(
            "POST",
            &path,
            Some(json!({"source":source,"mode":"original","failed_versions":[source]})),
            false,
        )
        .await;
    assert_eq!(plan["status"], "blocked");
    for bad in [
        json!({"file_id":b["file_id"],"revision":"stale"}),
        json!({"file_id":output["file_id"],"revision":output["revision"]}),
    ] {
        assert_eq!(
            f.json("POST", &path, Some(json!({"source":bad})), false)
                .await
                .0,
            StatusCode::CONFLICT
        );
    }
    assert_eq!(f.json("POST", &path, Some(json!({"source":source,"selected":{"file_id":a["file_id"],"revision":a["revision"]}})), false).await.0, StatusCode::CONFLICT);
}

#[tokio::test]
async fn session_file_switch_is_atomic_ordered_and_revision_bound() {
    let f = Fixture::new().await;
    std::fs::write(f.root.join("first.mp4"), b"first").unwrap();
    std::fs::write(f.root.join("second.mp4"), b"second").unwrap();
    f.scan().await;
    let items = f.items().await;
    let source = &items[0];
    let output = &items[1];
    sqlx::query("UPDATE media_files SET duration_seconds=100")
        .execute(&f.app.db)
        .await
        .unwrap();
    let id = source["id"].as_str().unwrap();
    let registration = json!({"file_id":output["file_id"],"file_revision":output["revision"],"source_file_id":source["file_id"],"source_revision":source["revision"],"label":"Alternative","recipe":{}});
    assert_eq!(
        f.json(
            "PUT",
            &format!("/api/v1/items/{id}/renditions/external/switch"),
            Some(registration),
            true
        )
        .await
        .0,
        200
    );
    let sessions = "/api/v1/profiles/default/playback-sessions";
    let (status, started) = f
        .json("POST", sessions, Some(session_request(source, 0)), false)
        .await;
    assert_eq!(status, 201);
    let path = format!("{sessions}/{}", started["id"].as_str().unwrap());
    let file = json!({"file_id":output["file_id"],"file_revision":output["revision"]});
    let event = json!({"sequence":1,"position_seconds":35,"status":"playing","file":file});
    let (status, switched) = f.json("PUT", &path, Some(event.clone()), false).await;
    assert_eq!(status, 200);
    assert_eq!(switched["id"], started["id"]);
    assert_eq!(switched["file_id"], output["file_id"]);
    assert_eq!(switched["position_seconds"], 35.0);
    assert_eq!(
        f.json("PUT", &path, Some(event.clone()), false).await.1,
        switched
    );
    let mut stale = event.clone();
    stale["file"]["file_id"] = source["file_id"].clone();
    stale["file"]["file_revision"] = source["revision"].clone();
    assert_eq!(f.json("PUT", &path, Some(stale), false).await.0, 409);
    let mut invalid = event.clone();
    invalid["sequence"] = json!(2);
    invalid["file"]["file_revision"] = json!("stale");
    assert_eq!(f.json("PUT", &path, Some(invalid), false).await.0, 409);
    let (_, advanced) = f
        .json(
            "PUT",
            &path,
            Some(json!({"sequence":2,"position_seconds":36,"status":"paused"})),
            false,
        )
        .await;
    assert_eq!(advanced["file_id"], output["file_id"]);
    assert_eq!(advanced["sequence"], 2);
    let view = f
        .json(
            "GET",
            &format!("/api/v1/profiles/default/viewing/{id}"),
            None,
            false,
        )
        .await
        .1;
    assert_eq!(view["session_id"], started["id"]);
    assert_eq!(view["watched"], false);
    assert_eq!(view["position_seconds"], 36.0);
    sqlx::query("UPDATE media_files SET revision='changed' WHERE id=?")
        .bind(source["file_id"].as_str().unwrap())
        .execute(&f.app.db)
        .await
        .unwrap();
    let mut changed = event;
    changed["sequence"] = json!(3);
    assert_ne!(f.json("PUT", &path, Some(changed), false).await.0, 200);
}
