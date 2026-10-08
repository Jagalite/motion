use super::*;

#[tokio::test]
async fn video_profile_jobs_are_immutable_and_plans_match_exact_profile() {
    let f = Fixture::new().await;
    std::fs::write(f.root.join("clip.mp4"), b"fixture").unwrap();
    f.scan().await;
    let tracks = json!([{"index":0,"kind":"video","codec":"h264","language":null,"width":1920,"height":1080,"average_frame_rate":{"numerator":60,"denominator":1},"color_transfer":"bt709"}]);
    sqlx::query("UPDATE media_files SET duration_seconds=10,tracks_json=?")
        .bind(tracks.to_string())
        .execute(&f.app.db)
        .await
        .unwrap();
    let source = f.items().await.remove(0);
    let profile = json!({"version":1,"codec":"h264","max_width":1280,"max_height":720,"video_bitrate":2000000,"audio_bitrate":128000,"frame_rate":{"numerator":30,"denominator":1}});
    let request = json!({"source_file_id":source["file_id"],"source_revision":source["revision"],"recipe":"video_profile","backend":"software","video_profile":profile,"idempotency_key":"profile-one"});
    assert_eq!(
        f.json(
            "POST",
            "/api/v1/processing-jobs",
            Some(request.clone()),
            false
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, job) = f
        .json(
            "POST",
            "/api/v1/processing-jobs",
            Some(request.clone()),
            true,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(job["video_profile"], profile);
    assert_eq!(
        f.json(
            "POST",
            "/api/v1/processing-jobs",
            Some(request.clone()),
            true
        )
        .await
        .1["id"],
        job["id"]
    );
    let mut changed = request.clone();
    changed["video_profile"]["max_height"] = json!(360);
    assert_eq!(
        f.json(
            "POST",
            "/api/v1/processing-jobs",
            Some(changed.clone()),
            true
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    changed["idempotency_key"] = json!("profile-two");
    let other = f
        .json("POST", "/api/v1/processing-jobs", Some(changed), true)
        .await
        .1;
    assert_ne!(other["id"], job["id"]);
    let path = format!(
        "/api/v1/profiles/default/items/{}/playback-plan",
        source["id"].as_str().unwrap()
    );
    let plan_request = json!({"mode":"convert","recipe":"video_profile","video_profile":profile});
    let plan = f
        .json("POST", &path, Some(plan_request.clone()), false)
        .await
        .1;
    assert_eq!(plan["preparation"]["job_id"], job["id"]);
    assert_eq!(plan["preparation"]["video_profile"], profile);
    let mut invalid = request.clone();
    invalid["idempotency_key"] = json!("invalid");
    invalid["video_profile"]["frame_rate"]["denominator"] = json!(0);
    assert_eq!(
        f.json("POST", "/api/v1/processing-jobs", Some(invalid), true)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut invalid = request.clone();
    invalid["recipe"] = json!("h264720p");
    assert_eq!(
        f.json("POST", "/api/v1/processing-jobs", Some(invalid), true)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut invalid = request.clone();
    invalid.as_object_mut().unwrap().remove("video_profile");
    assert_eq!(
        f.json("POST", "/api/v1/processing-jobs", Some(invalid), true)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut invalid = plan_request.clone();
    invalid["video_profile"]["codec"] = json!("arbitrary-command");
    assert_eq!(
        f.json("POST", &path, Some(invalid), false).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // Nearest-frame CFR cannot represent a clip shorter than half a frame.
    sqlx::query("UPDATE media_files SET duration_seconds=0.2")
        .execute(&f.app.db)
        .await
        .unwrap();
    let mut short = request.clone();
    short["idempotency_key"] = json!("too-short");
    short["video_profile"]["frame_rate"] = json!({"numerator":1,"denominator":1});
    assert_eq!(
        f.json("POST", "/api/v1/processing-jobs", Some(short), true)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    sqlx::query("UPDATE media_files SET duration_seconds=10")
        .execute(&f.app.db)
        .await
        .unwrap();
    // Old track JSON remains readable, but cannot silently satisfy a profile admission.
    sqlx::query("UPDATE media_files SET tracks_json='[]'")
        .execute(&f.app.db)
        .await
        .unwrap();
    let mut fresh = request;
    fresh["idempotency_key"] = json!("unprobed");
    assert_eq!(
        f.json("POST", "/api/v1/processing-jobs", Some(fresh), true)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn registered_renditions_expose_probed_metadata_not_recipe_claims() {
    let f = Fixture::new().await;
    std::fs::write(f.root.join("source.mp4"), b"source").unwrap();
    std::fs::write(f.root.join("output.mp4"), b"output").unwrap();
    f.scan().await;
    let items = f.items().await;
    let source = &items[0];
    let output = &items[1];
    let tracks = json!([{"index":0,"kind":"video","codec":"hevc","language":null,"width":640,"height":360,"average_frame_rate":{"numerator":24000,"denominator":1001}}]);
    sqlx::query("UPDATE media_files SET tracks_json=?,duration_seconds=4 WHERE id=?")
        .bind(tracks.to_string())
        .bind(output["file_id"].as_str().unwrap())
        .execute(&f.app.db)
        .await
        .unwrap();
    let path = format!(
        "/api/v1/items/{}/renditions/catabolic/test",
        source["id"].as_str().unwrap()
    );
    let (status,choices)=f.json("PUT",&path,Some(json!({"file_id":output["file_id"],"file_revision":output["revision"],"source_file_id":source["file_id"],"source_revision":source["revision"],"label":"External","recipe":{"claimed_width":3840}})),true).await;
    assert_eq!(status, StatusCode::OK);
    let variant = &choices["renditions"][0];
    assert_eq!(variant["tracks"][0]["width"], 640);
    assert_eq!(
        variant["tracks"][0]["average_frame_rate"]["denominator"],
        1001
    );
    assert_eq!(variant["average_bitrate"], 12);
    assert_eq!(variant["bytes"], 6);
    assert_eq!(variant["duration_seconds"], 4.0);
}
