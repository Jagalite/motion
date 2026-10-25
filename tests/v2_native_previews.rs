//! Real observed originals, production preview worker and public preview routes.
//! No generated/slice preview capability is inferred from this finite cohort.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use playscale as playscale_app;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
#[path = "support/native_application_fixture.rs"]
mod native_application_fixture;
use native_application_fixture::{Fixture, OPERATOR, observer_binary};

struct PreviewFixture {
    f: Fixture,
    timeline: String,
    device: String,
    auth: String,
    media: std::path::PathBuf,
    libraries: Vec<String>,
}
async fn fixture(paused: bool) -> PreviewFixture {
    let mut f = Fixture::new().await;
    let root = f._dir.path().join("preview-source");
    std::fs::create_dir(&root).unwrap();
    let media = root.join("original.mp4");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=160x90:r=10",
            "-t",
            "2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "10",
            "-movflags",
            "+faststart",
            "-y",
        ])
        .arg(&media)
        .status()
        .unwrap();
    assert!(status.success());
    if paused {
        use std::os::unix::fs::PermissionsExt;
        let script = f._dir.path().join("controlled-preview-ffmpeg");
        std::fs::write(&script,format!("#!/bin/sh\nprintf started > '{}'\nwhile [ ! -f '{}' ]; do sleep 0.05; done\nexec ffmpeg \"$@\"\n",f._dir.path().join("preview-started").display(),f._dir.path().join("preview-release").display())).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::sync::Arc::get_mut(&mut f.app.processing)
            .unwrap()
            .settings
            .ffmpeg = script;
    }
    let (device, token) = f
        .pair(
            &["default"],
            &["system:admin", "catalog:read", "processing:request"],
        )
        .await;
    f.pair(&["default"], &["system:admin", "catalog:read"])
        .await;
    f.cutover().await;
    f.upgrade_preview_control().await;
    let operator = format!("Bearer {OPERATOR}");
    let source = f
        .call(
            "POST",
            "/api/v2/sources",
            Some(json!({"name":"Actual previews","root_path":root,"exclusions":[]})),
            &[
                ("authorization", &operator),
                ("idempotency-key", "native-preview-source-create"),
            ],
        )
        .await;
    assert_eq!(source.status, StatusCode::CREATED, "{:?}", source.body);
    let binary = observer_binary();
    let observer =
        catabolic_runtime::helper::TrustedClosedObservationArtifact::after_packaging_verification(
            &binary,
            &catabolic_core::sha256(std::fs::read(&binary).unwrap()),
            catabolic_runtime::helper::ClosedObservationProtocol::NoChildWithExactJoinV2,
        )
        .unwrap();
    f.app
        .native_catalog
        .as_ref()
        .unwrap()
        .register_observer(observer)
        .await
        .unwrap();
    playscale::native_discovery::scan(
        &f.app,
        source.body["id"].as_str().unwrap(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let timeline: String =
        sqlx::query_scalar("SELECT timeline_id FROM media_versions WHERE origin='original'")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    let mut libraries = Vec::new();
    for (name, sources, key) in [
        (
            "Preview membership",
            vec![source.body["id"].as_str().unwrap()],
            "preview-library-primary",
        ),
        ("Other preview library", vec![], "preview-library-other"),
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
        libraries.push(library.body["id"].as_str().unwrap().to_owned());
    }
    PreviewFixture {
        f,
        timeline,
        device,
        auth: format!("Bearer {token}"),
        media,
        libraries,
    }
}
impl PreviewFixture {
    fn path(&self) -> String {
        format!("/api/v2/catalog/timelines/{}/previews", self.timeline)
    }
    async fn create(&self) -> serde_json::Value {
        let response = self
            .f
            .call(
                "POST",
                &self.path(),
                None,
                &[
                    ("authorization", &self.auth),
                    ("idempotency-key", "native-preview-create-key"),
                ],
            )
            .await;
        assert_eq!(response.status, StatusCode::ACCEPTED, "{:?}", response.body);
        response.body
    }
    async fn phase(&self, job: &str, wanted: &str) {
        tokio::time::timeout(std::time::Duration::from_secs(120), async {
            loop {
                let phase: String = sqlx::query_scalar("SELECT phase FROM preview_jobs WHERE id=?")
                    .bind(job)
                    .fetch_one(&self.f.app.db)
                    .await
                    .unwrap();
                if phase == wanted {
                    break;
                }
                assert!(
                    !["failed", "cancelled"].contains(&phase.as_str()),
                    "unexpected terminal phase {phase}"
                );
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("actual preview worker did not reach expected terminal state");
    }
    async fn paused(&self) {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !self.f._dir.path().join("preview-started").exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("actual preview descriptor process did not start");
    }
    async fn policy(&self, permissions: serde_json::Value) {
        self.policy_scope(permissions, vec![]).await;
    }
    async fn policy_scope(&self, permissions: serde_json::Value, libraries: Vec<String>) {
        let operator = format!("Bearer {OPERATOR}");
        let path = format!("/api/v2/devices/{}", self.device);
        let read = self
            .f
            .call("GET", &path, None, &[("authorization", &operator)])
            .await;
        assert_eq!(read.status, StatusCode::OK);
        let changed=self.f.call("PUT",&format!("{path}/policy"),Some(json!({"library_ids":libraries,"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":permissions})),&[("authorization",&operator),("if-match",read.headers["etag"].to_str().unwrap())]).await;
        assert_eq!(changed.status, StatusCode::OK, "{:?}", changed.body);
    }
    async fn sheet(&self, path: &str, auth: Option<&str>) -> (StatusCode, Vec<u8>) {
        let mut request = Request::builder()
            .uri(path)
            .header("host", "127.0.0.1:8787");
        if let Some(auth) = auth {
            request = request.header("authorization", auth);
        }
        let response = playscale::api::router(self.f.app.clone(), None)
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        (status, bytes)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn native_preview_public_render_manifest_sheet_replay_restart_and_revocation() {
    let mut p = fixture(false).await;
    assert_eq!(
        p.f.call("GET", &p.path(), None, &[]).await.status,
        StatusCode::UNAUTHORIZED
    );
    let initial =
        p.f.call("GET", &p.path(), None, &[("authorization", &p.auth)])
            .await;
    assert_eq!(initial.status, StatusCode::OK, "{:?}", initial.body);
    assert_eq!(initial.body["status"], "none");
    assert_eq!(
        p.f.call("HEAD", &p.path(), None, &[("authorization", &p.auth)])
            .await
            .status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let accepted = p.create().await;
    let job = accepted["job_id"].as_str().unwrap();
    let stop = CancellationToken::new();
    let worker = tokio::spawn(playscale::v2::previews::worker(
        p.f.app.clone(),
        stop.clone(),
    ));
    p.phase(job, "completed").await;
    stop.cancel();
    worker.await.unwrap().unwrap();
    let ready =
        p.f.call("GET", &p.path(), None, &[("authorization", &p.auth)])
            .await;
    assert_eq!(ready.status, StatusCode::OK, "{:?}", ready.body);
    assert_eq!(ready.body["status"], "ready");
    let url = ready.body["sheets"][0].as_str().unwrap();
    assert_eq!(p.sheet(url, None).await.0, StatusCode::UNAUTHORIZED);
    let (status, bytes) = p.sheet(url, Some(&p.auth)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert!(image::load_from_memory(&bytes).is_ok());
    p.policy_scope(
        json!(["catalog:read", "processing:request"]),
        vec![p.libraries[0].clone()],
    )
    .await;
    assert_eq!(p.sheet(url, Some(&p.auth)).await.0, StatusCode::OK);
    assert_eq!(
        p.f.call("GET", &p.path(), None, &[("authorization", &p.auth)])
            .await
            .status,
        StatusCode::OK
    );
    p.policy_scope(
        json!(["catalog:read", "processing:request"]),
        vec![p.libraries[1].clone()],
    )
    .await;
    assert_eq!(p.sheet(url, Some(&p.auth)).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        p.f.call("GET", &p.path(), None, &[("authorization", &p.auth)])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    p.policy(json!([
        "system:admin",
        "catalog:read",
        "processing:request"
    ]))
    .await;
    let replay =
        p.f.call(
            "POST",
            &p.path(),
            None,
            &[
                ("authorization", &p.auth),
                ("idempotency-key", "native-preview-create-key"),
            ],
        )
        .await;
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert_eq!(replay.body, ready.body);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM preview_jobs")
        .fetch_one(&p.f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 1);
    p.f.close().await;
    p.f.open().await;
    assert_eq!(
        p.sheet(url, Some(&p.auth)).await,
        (StatusCode::OK, bytes.clone())
    );
    let artifact: (String,i64) = sqlx::query_as("SELECT digest,size_bytes FROM preview_sheet_artifacts WHERE file_id=? AND file_revision=? AND sheet=1")
        .bind(ready.body["file_id"].as_str().unwrap()).bind(ready.body["file_revision"].as_str().unwrap()).fetch_one(&p.f.app.db).await.unwrap();
    assert_eq!(artifact.0, catabolic_core::sha256(&bytes));
    assert_eq!(artifact.1, bytes.len() as i64);
    let output =
        p.f.app
            .processing
            .root
            .parent()
            .unwrap()
            .join("previews")
            .join(ready.body["file_id"].as_str().unwrap())
            .join(ready.body["file_revision"].as_str().unwrap())
            .join("sheet-1.jpg");
    let mut replacement = bytes.clone();
    replacement[0] ^= 1;
    std::fs::write(&output, &replacement).unwrap();
    assert_eq!(
        p.sheet(url, Some(&p.auth)).await.0,
        StatusCode::NOT_FOUND,
        "same-size output replacement must not reuse ACK"
    );
    std::fs::write(&output, &bytes).unwrap();
    assert_eq!(p.sheet(url, Some(&p.auth)).await.0, StatusCode::OK);
    p.policy(json!(["catalog:read"])).await;
    assert_eq!(
        p.f.call(
            "POST",
            &p.path(),
            None,
            &[
                ("authorization", &p.auth),
                ("idempotency-key", "native-preview-revoked-key")
            ]
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    p.policy(json!([])).await;
    assert_eq!(p.sheet(url, Some(&p.auth)).await.0, StatusCode::FORBIDDEN);
    let operator = format!("Bearer {OPERATOR}");
    std::fs::write(&p.media, b"changed exact preview source").unwrap();
    assert_eq!(p.sheet(url, Some(&operator)).await.0, StatusCode::NOT_FOUND);
    p.f.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn native_preview_inflight_replacement_refuses_publication() {
    let mut p = fixture(true).await;
    let accepted = p.create().await;
    let stop = CancellationToken::new();
    let worker = tokio::spawn(playscale::v2::previews::worker(
        p.f.app.clone(),
        stop.clone(),
    ));
    p.paused().await;
    let old = p.f._dir.path().join("retained-original.mp4");
    std::fs::rename(&p.media, &old).unwrap();
    std::fs::copy(&old, &p.media).unwrap();
    std::fs::write(p.f._dir.path().join("preview-release"), b"release").unwrap();
    p.phase(accepted["job_id"].as_str().unwrap(), "failed")
        .await;
    stop.cancel();
    worker.await.unwrap().unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM previews")
        .fetch_one(&p.f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(p.f.app.processing.execution.snapshot().used, 0);
    p.f.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn native_preview_worker_abort_retains_child_until_exit_and_recovers() {
    let mut p = fixture(true).await;
    let accepted = p.create().await;
    let worker = tokio::spawn(playscale::v2::previews::worker(
        p.f.app.clone(),
        CancellationToken::new(),
    ));
    p.paused().await;
    p.f.app.processing.execution.begin_drain();
    assert!(
        p.f.app
            .processing
            .execution
            .close_admission_if_idle()
            .is_err()
    );
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while p.f.app.processing.execution.snapshot().used != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("aborted preview owner did not confirm child exit");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM previews")
        .fetch_one(&p.f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
    p.f.app
        .processing
        .execution
        .close_admission_if_idle()
        .unwrap();
    p.f.close().await;
    let mut processing = playscale::processing::Runtime::new(
        p.f.app.processing.root.clone(),
        p.f.app.processing.settings.clone(),
    );
    processing.supervisor = observer_binary();
    p.f.app.processing = std::sync::Arc::new(processing);
    p.f.open().await;
    std::fs::write(
        p.f._dir.path().join("preview-release"),
        b"release after ordinary restart",
    )
    .unwrap();
    let stop = CancellationToken::new();
    let worker = tokio::spawn(playscale::v2::previews::worker(
        p.f.app.clone(),
        stop.clone(),
    ));
    p.phase(accepted["job_id"].as_str().unwrap(), "completed")
        .await;
    stop.cancel();
    worker.await.unwrap().unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM previews")
        .fetch_one(&p.f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 1);
    p.f.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn native_preview_inflight_permission_loss_refuses_publication() {
    let mut p = fixture(true).await;
    let accepted = p.create().await;
    let stop = CancellationToken::new();
    let worker = tokio::spawn(playscale::v2::previews::worker(
        p.f.app.clone(),
        stop.clone(),
    ));
    p.paused().await;
    p.policy(json!(["catalog:read"])).await;
    std::fs::write(p.f._dir.path().join("preview-release"), b"release").unwrap();
    p.phase(accepted["job_id"].as_str().unwrap(), "failed")
        .await;
    stop.cancel();
    worker.await.unwrap().unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM previews")
        .fetch_one(&p.f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(p.f.app.processing.execution.snapshot().used, 0);
    p.f.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn native_preview_replaced_staging_is_not_published_or_deleted() {
    let mut p = fixture(true).await;
    let accepted = p.create().await;
    let stop = CancellationToken::new();
    let worker = tokio::spawn(playscale::v2::previews::worker(
        p.f.app.clone(),
        stop.clone(),
    ));
    p.paused().await;
    let job = accepted["job_id"].as_str().unwrap();
    let attempt: i64 = sqlx::query_scalar("SELECT attempt FROM preview_jobs WHERE id=?")
        .bind(job)
        .fetch_one(&p.f.app.db)
        .await
        .unwrap();
    let root = p.f.app.processing.root.parent().unwrap().join("previews");
    let staging = root.join(".staging").join(format!("{job}-{attempt}"));
    std::fs::rename(&staging, p.f._dir.path().join("retained-preview-staging")).unwrap();
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(staging.join("unrelated"), b"not owned by this attempt").unwrap();
    std::fs::write(p.f._dir.path().join("preview-release"), b"release").unwrap();
    p.phase(job, "failed").await;
    stop.cancel();
    worker.await.unwrap().unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM preview_sheet_artifacts")
        .fetch_one(&p.f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        std::fs::read(staging.join("unrelated")).unwrap(),
        b"not owned by this attempt"
    );
    assert_eq!(p.f.app.processing.execution.snapshot().used, 0);
    p.f.close().await;
}
