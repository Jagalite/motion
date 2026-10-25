//! Private, actual observer + FFmpeg + supervisor qualification fixture.
//! No public native HLS capability is enabled by these tests.
use super::*;
use crate as playscale_app;
use axum::{body::Body as AxumBody, extract::FromRequestParts, http::Request};
use http_body_util::BodyExt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio_util::sync::CancellationToken;
#[path = "../../../tests/support/native_application_fixture.rs"]
mod native_application_fixture;
use native_application_fixture::{Fixture, OPERATOR, observer_binary};

async fn fixture() -> (Fixture, Caller, Claims, std::path::PathBuf, String) {
    let mut f = Fixture::new().await;
    let root = f._dir.path().join("hls-source");
    std::fs::create_dir(&root).unwrap();
    let media = root.join("original.mp4");
    let encoded = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=160x90:r=10",
            "-t",
            "9",
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
        .output()
        .unwrap();
    assert!(
        encoded.status.success(),
        "{}",
        String::from_utf8_lossy(&encoded.stderr)
    );
    let (_, token) = f
        .pair(
            &["default"],
            &["system:admin", "catalog:read", "playback:request"],
        )
        .await;
    f.pair(&["default"], &["system:admin", "catalog:read"])
        .await;
    f.cutover().await;
    let operator = format!("Bearer {OPERATOR}");
    let created = f
        .call(
            "POST",
            "/api/v2/sources",
            Some(json!({"name":"HLS source","root_path":root,"exclusions":[]})),
            &[
                ("authorization", &operator),
                ("idempotency-key", "hls-source-create"),
            ],
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
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
    crate::native_discovery::scan(
        &f.app,
        created.body["id"].as_str().unwrap(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (timeline,version,file,revision):(String,String,String,String)=sqlx::query_as("SELECT v.timeline_id,v.id,f.id,f.revision FROM media_versions v JOIN version_files b ON b.version_id=v.id JOIN media_files f ON f.id=b.file_id WHERE v.origin='original'").fetch_one(&f.app.db).await.unwrap();
    let (mut parts, _) = Request::builder()
        .uri("/api/v2/deliveries")
        .header("host", "127.0.0.1:8787")
        .header("authorization", format!("Bearer {token}"))
        .body(AxumBody::empty())
        .unwrap()
        .into_parts();
    let caller = Caller::from_request_parts(&mut parts, &f.app)
        .await
        .unwrap();
    let claims = Claims {
        principal: caller.principal.id.clone(),
        profile: "default".into(),
        timeline,
        version,
        file,
        file_revision: revision,
        route: Route::Original,
        audio: None,
        subtitle: None,
        clip: None,
        parts: vec![],
        max_bitrate_bps: None,
        adaptive: false,
        expires_at: now() + 300,
    };
    (f, caller, claims, media, token)
}
async fn ready(app: &App, id: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let value =
                serde_json::to_value(delivery::live_view(app, id).expect("delivery remains owned"))
                    .unwrap();
            if value["status"] == "ready" {
                break;
            }
            assert_ne!(value["status"], "failed", "{value}");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn actual_private_native_hls_reads_identity_fences_and_independent_sessions() {
    let (mut f, caller, claims, media, _) = fixture().await;
    let stop = CancellationToken::new();
    let worker = tokio::spawn(delivery::worker(f.app.clone(), stop.clone()));
    let token = sign(&f.app, &claims).unwrap();
    let one = admit(f.app.clone(), caller.clone(), &token, "hls-one".into(), 0)
        .await
        .unwrap();
    let two = admit(f.app.clone(), caller.clone(), &token, "hls-two".into(), 0)
        .await
        .unwrap();
    ready(&f.app, &one.id).await;
    ready(&f.app, &two.id).await;
    let init = delivery::init(State(f.app.clone()), Path((one.id.clone(), "1".into())))
        .await
        .unwrap_or_else(|error| {
            let (status, code, _) = error.parts();
            panic!("unexpected API error: {status} {code}")
        });
    assert!(
        !init
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
    let segment = delivery::segment(
        State(f.app.clone()),
        Path((two.id.clone(), "1".into(), "0.m4s".into())),
    )
    .await
    .unwrap_or_else(|error| {
        let (status, code, _) = error.parts();
        panic!("unexpected API error: {status} {code}")
    });
    assert!(
        !segment
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
    let mut tx = crate::db::begin_write(&f.app.db).await.unwrap();
    delivery::close_live(&f.app, &one.id, &mut tx)
        .await
        .unwrap_or_else(|error| {
            let (status, code, _) = error.parts();
            panic!("unexpected API error: {status} {code}")
        });
    tx.commit().await.unwrap();
    assert!(
        delivery::segment(
            State(f.app.clone()),
            Path((two.id.clone(), "1".into(), "0.m4s".into()))
        )
        .await
        .is_ok()
    );
    // An identical-length replacement is not the observed native inode.
    let bytes = std::fs::read(&media).unwrap();
    std::fs::rename(&media, media.with_extension("old")).unwrap();
    // Exact acknowledgement replay still reauthorizes the current caller, but
    // neither expired plans nor a missing native source cause another open.
    let mut expired = claims.clone();
    expired.expires_at = now() - 1;
    let replay = admit(
        f.app.clone(),
        caller.clone(),
        &sign(&f.app, &expired).unwrap(),
        "hls-two".into(),
        0,
    )
    .await
    .unwrap();
    assert_eq!(replay.id, two.id);
    std::fs::write(&media, bytes).unwrap();
    assert!(
        delivery::segment(
            State(f.app.clone()),
            Path((two.id.clone(), "1".into(), "0.m4s".into()))
        )
        .await
        .is_err()
    );
    stop.cancel();
    worker.await.unwrap().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(15), f.close())
        .await
        .unwrap();
}

async fn retained_frame_after_eof(response: Response) -> axum::body::Bytes {
    let mut body = response.into_body();
    let bytes = body
        .frame()
        .await
        .expect("native response must yield a data frame")
        .unwrap()
        .into_data()
        .expect("native response must not substitute a trailer frame");
    assert!(bytes.len() > 1);
    assert!(
        body.frame().await.is_none(),
        "bounded native response has one frame"
    );
    drop(body);
    bytes
}

async fn native_close_still_pending(
    close: &mut tokio::task::JoinHandle<anyhow::Result<()>>,
    retained: &str,
) {
    match tokio::time::timeout(std::time::Duration::from_millis(50), close).await {
        Err(_) => {}
        Ok(Ok(Ok(()))) => {
            panic!("native close completed while {retained} still retained yielded bytes")
        }
        Ok(Ok(Err(error))) => {
            panic!("native close failed while {retained} still retained yielded bytes: {error:#}")
        }
        Ok(Err(error)) => panic!("native close task failed: {error}"),
    }
}

enum FirstNativeClose {
    AbortWaiter,
    JoinTimeout,
}

#[tokio::test(flavor = "multi_thread")]
async fn native_yielded_frames_retain_custody_after_body_eof_until_last_clone_and_slice_drop() {
    retained_native_hls_frames(FirstNativeClose::AbortWaiter).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn native_yielded_frames_retain_backend_after_joined_shutdown_timeout_until_last_alias_drop()
{
    retained_native_hls_frames(FirstNativeClose::JoinTimeout).await;
}

async fn retained_native_hls_frames(first_close: FirstNativeClose) {
    let (mut f, caller, claims, _, token) = fixture().await;
    // Four actual response allocations, not four copied byte buffers. Each
    // frame and every derived alias must retain its own original admission.
    f.app.streams.add_permits(2);
    assert_eq!(f.app.streams.available_permits(), 4);
    let stop = CancellationToken::new();
    let worker = tokio::spawn(delivery::worker(f.app.clone(), stop.clone()));
    let admitted = admit(
        f.app.clone(),
        caller,
        &sign(&f.app, &claims).unwrap(),
        "hls-retained-frame-custody".into(),
        0,
    )
    .await
    .unwrap();
    ready(&f.app, &admitted.id).await;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    headers.insert(header::HOST, "127.0.0.1:8787".parse().unwrap());
    let init = retained_frame_after_eof(
        init_segment(
            State(f.app.clone()),
            Query(StreamParams { ticket: None }),
            headers.clone(),
            Path((admitted.id.clone(), "1".into(), VARIANT.into())),
        )
        .await
        .unwrap(),
    )
    .await;
    let media = retained_frame_after_eof(
        media_segment(
            State(f.app.clone()),
            Query(StreamParams { ticket: None }),
            headers.clone(),
            Path((admitted.id.clone(), "1".into(), "0.m4s".into())),
        )
        .await
        .unwrap(),
    )
    .await;
    let master = retained_frame_after_eof(
        master_playlist(
            State(f.app.clone()),
            Query(StreamParams { ticket: None }),
            headers.clone(),
            Path((admitted.id.clone(), "1".into())),
        )
        .await
        .unwrap(),
    )
    .await;
    assert!(master.starts_with(b"#EXTM3U\n"));
    let variant = retained_frame_after_eof(
        variant_playlist(
            State(f.app.clone()),
            Query(StreamParams { ticket: None }),
            headers,
            Path((admitted.id.clone(), "1".into(), VARIANT.into())),
        )
        .await
        .unwrap(),
    )
    .await;
    assert!(variant.starts_with(b"#EXTM3U\n"));
    assert_eq!(f.app.streams.available_permits(), 0);
    // Free only a stream slot: the four actual native buffer allocations must
    // remain exhausted even though all four body producers reached EOF.
    f.app.streams.add_permits(1);
    let exhausted = delivery::init(
        State(f.app.clone()),
        Path((admitted.id.clone(), "1".into())),
    )
    .await
    .err()
    .expect("yielded frames must still own native allocation capacity");
    assert_eq!(
        (exhausted.parts().0, exhausted.parts().1),
        (StatusCode::SERVICE_UNAVAILABLE, "native_hls_busy")
    );
    assert_eq!(f.app.streams.forget_permits(1), 1);
    assert_eq!(f.app.streams.available_permits(), 0);
    let mut frames: Vec<_> = [
        ("init", init),
        ("media", media),
        ("master playlist", master),
        ("variant playlist", variant),
    ]
    .into_iter()
    .map(|(name, bytes)| {
        let clone = bytes.clone();
        let slice = bytes.slice(1..);
        (name, Some(bytes), Some(clone), Some(slice))
    })
    .collect();

    stop.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(15), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(delivery::live_view(&f.app, &admitted.id).is_none());
    // The delivery lookup owner is joined, but its witness-confirmed final
    // publication task can be finishing its final destructor on another thread.
    // Observe actual coordinator release instead of equating worker return
    // with physical lease drain.
    let execution = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let snapshot = f.app.processing.execution.snapshot();
            if snapshot.used == 0
                && snapshot.running == 0
                && snapshot.terminating == 0
                && snapshot.waiting == 0
                && snapshot.stuck.is_empty()
            {
                break snapshot;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("actual execution ownership must drain independently of retained response bytes");
    assert_eq!(execution.used, 0);
    assert_eq!(execution.running, 0);
    assert_eq!(execution.terminating, 0);
    assert_eq!(execution.waiting, 0);
    assert!(execution.stuck.is_empty());
    assert_eq!(f.app.streams.available_permits(), 0);

    let native = f.app.native_catalog.take().unwrap();
    let retained_runtime = Arc::downgrade(&native);
    // The delivery worker has joined and released the session map. Retain only
    // diagnostics that cannot own Runtime, then drop the ordinary App itself.
    let streams = f.app.streams.clone();
    let execution = f.app.processing.execution.clone();
    let motion_db = f.app.db.clone();
    drop(f.app);
    let _directory = f._dir;
    let authorization = native
        .with_host(|host| Ok(host.authorization()?))
        .await
        .unwrap();
    let (started, entered) = tokio::sync::oneshot::channel();
    let mut close = tokio::spawn(async move {
        native.begin_drain();
        started.send(()).unwrap();
        native.shutdown().await
    });
    entered.await.unwrap();
    // Observe closure of the actual shared native gate, not just entry into an
    // async wrapper. This clone is not an AuthorizationLease and holds no work.
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            match authorization.acquire(authorization.revision()) {
                Err(catabolic_core::catalog_host_lease::LeaseError::Revoked) => break,
                Ok(lease) => drop(lease),
                Err(error) => panic!("unexpected native admission state: {error:?}"),
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native shutdown must close actual shared authorization admission");
    native_close_still_pending(&mut close, "all response frames").await;
    let tx = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        crate::db::begin_write(&motion_db),
    )
    .await
    .expect("retained yielded bytes must not hold the application writer")
    .unwrap();
    tx.rollback().await.unwrap();
    match first_close {
        FirstNativeClose::AbortWaiter => {
            // The blocking close still owns RuntimeAdmission, not Runtime.
            // Keep this original cancellation regression distinct from timeout.
            close.abort();
            assert!(close.await.unwrap_err().is_cancelled());
        }
        FirstNativeClose::JoinTimeout => {
            // Join the real production five-second refusal, not a timer-only
            // sleep or an aborted future. This removes BOTH the async Runtime
            // owner and the completed blocking worker's RuntimeAdmission owner
            // before testing frame-owned survival. All four actual allocations
            // and their cloned/sliced aliases remain held throughout this wait.
            let error = tokio::time::timeout(std::time::Duration::from_secs(7), close)
                .await
                .expect("actual native shutdown must return its bounded refusal")
                .expect("first native shutdown task must join normally")
                .expect_err("held native response frames must prevent successful close");
            assert_eq!(
                error.to_string(),
                "native authorization did not drain: Timeout"
            );
        }
    }
    assert_eq!(streams.available_permits(), 0);
    assert_eq!(execution.snapshot().used, 0);
    // The Weak points to the original Runtime, never a reconstructed backend.
    // In JoinTimeout no temporary shutdown worker remains to retain its host.
    let native = retained_runtime
        .upgrade()
        .expect("retained HLS frames must retain the actual backend Runtime");
    assert!(native.with_host(|_| Ok(())).await.is_err());
    close = tokio::spawn(async move { native.shutdown().await });
    native_close_still_pending(&mut close, "all frames during same-owner shutdown retry").await;
    for (_, original, _, _) in &mut frames {
        drop(original.take());
    }
    assert_eq!(streams.available_permits(), 0);
    native_close_still_pending(&mut close, "frame clones and slices").await;
    for (index, (name, _, clone, slice)) in frames.iter_mut().enumerate() {
        assert!(!clone.as_ref().unwrap().is_empty());
        assert_eq!(
            &clone.as_ref().unwrap()[1..],
            slice.as_ref().unwrap().as_ref()
        );
        drop(clone.take());
        assert_eq!(streams.available_permits(), index);
        native_close_still_pending(&mut close, name).await;
        drop(slice.take());
        assert_eq!(streams.available_permits(), index + 1);
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), close)
        .await
        .expect("native close must complete after the final yielded-byte alias drops")
        .unwrap()
        .unwrap();
    assert_eq!(streams.available_permits(), 4);
    assert_eq!(execution.snapshot().used, 0);
    assert!(matches!(
        authorization.acquire(authorization.revision()),
        Err(catabolic_core::catalog_host_lease::LeaseError::Revoked)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn private_native_hls_refuses_expiry_and_unsupported_claims_before_dispatch() {
    let (mut f, caller, mut claims, media, _) = fixture().await;
    claims.expires_at = now() - 1;
    // No native source open may precede the fresh-admission check: this source
    // is now missing, while its unchanged projected identity remains bound.
    std::fs::rename(&media, media.with_extension("offline")).unwrap();
    let expired = admit(
        f.app.clone(),
        caller.clone(),
        &sign(&f.app, &claims).unwrap(),
        "expired".into(),
        0,
    )
    .await
    .err()
    .expect("expired admission must fail before opening the missing source");
    assert_eq!(expired.code, "plan_expired");
    assert_eq!(f.app.processing.execution.snapshot().used, 0);
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM delivery_admissions")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(receipts, 0);
    claims.expires_at = now() + 300;
    claims.clip = Some(playscale_core::parts::Clip {
        start_ms: 0,
        end_ms: 1000,
    });
    assert!(
        admit(
            f.app.clone(),
            caller,
            &sign(&f.app, &claims).unwrap(),
            "edited".into(),
            0
        )
        .await
        .is_err()
    );
    f.close().await;
}

struct PreparationAuthority {
    fresh: AtomicBool,
    preparations: AtomicUsize,
    pause: AtomicBool,
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}

impl AdmissionAuthority for PreparationAuthority {
    fn reauthorize<'a>(
        &'a self,
        _: &'a mut sqlx::SqliteConnection,
        _: &'a delivery::CreateRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, ApiError>> + Send + 'a>>
    {
        Box::pin(async { Ok("preparation-owner".into()) })
    }

    fn fresh(&self) -> Result<(), ApiError> {
        self.fresh
            .load(Ordering::SeqCst)
            .then_some(())
            .ok_or_else(|| problem_to_api(&plan_problem(PlanError::Expired)))
    }

    fn prepare_new<'a>(
        &'a self,
        _: &'a mut sqlx::SqliteConnection,
        _: &'a delivery::CreateRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ApiError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.preparations.fetch_add(1, Ordering::SeqCst);
            if self.pause.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                self.resume.notified().await;
            }
            Ok(())
        })
    }
}

fn byte_request(revision: &str) -> (delivery::CreateRequest, delivery::Planned) {
    (
        delivery::CreateRequest {
            file_id: "file".into(),
            file_revision: revision.into(),
            start_ms: 0,
            audio_track: None,
            subtitle_track: None,
            operation: None,
            backend: None,
        },
        delivery::Planned {
            native_input: None,
            owner: Owner {
                principal: "preparation-owner".into(),
                profile: "default".into(),
                timeline: "timeline".into(),
                file: "file".into(),
                file_revision: revision.into(),
            },
            version: "version".into(),
            route: Route::Original,
            network: "unknown",
            sidecar: None,
            burn: None,
            clip: None,
            parts: vec![],
            max_bitrate_bps: None,
            adaptive: false,
        },
    )
}

#[tokio::test]
async fn admission_preparation_is_after_fresh_and_replay_and_cannot_outlive_expiry() {
    // Exercise the actual admission owner with an encoder-free byte route; the
    // authority's explicit gate makes expiry during preparation deterministic.
    let f = Fixture::new().await;
    f.seed_preserved_timeline("source.mp4", b"admission-ordering")
        .await;
    delivery::recover(&f.app).await.unwrap();
    let revision: String = sqlx::query_scalar("SELECT revision FROM media_files WHERE id='file'")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    let authority = Arc::new(PreparationAuthority {
        fresh: AtomicBool::new(false),
        preparations: AtomicUsize::new(0),
        pause: AtomicBool::new(false),
        entered: tokio::sync::Notify::new(),
        resume: tokio::sync::Notify::new(),
    });
    let (request, planned) = byte_request(&revision);
    let expired = delivery::admit_planned(
        f.app.clone(),
        authority.clone(),
        "expired-before-preparation".into(),
        request,
        planned,
    )
    .await
    .err()
    .expect("expired admission must not prepare");
    // This test calls shared admission directly, not PlanAuthority's retained
    // Problem adapter. Inspect its exact API error before generic v2 mapping.
    assert_eq!(
        (expired.parts().0, expired.parts().1),
        (StatusCode::CONFLICT, "plan_expired")
    );
    assert_eq!(authority.preparations.load(Ordering::SeqCst), 0);
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM delivery_admissions")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(receipts, 0);
    assert_eq!(f.app.processing.execution.snapshot().used, 0);
    assert_eq!(
        std::fs::read_dir(&f.app.processing.deliveries.root)
            .unwrap()
            .count(),
        0
    );

    authority.fresh.store(true, Ordering::SeqCst);
    authority.pause.store(true, Ordering::SeqCst);
    let (request, planned) = byte_request(&revision);
    let pending = tokio::spawn(delivery::admit_planned(
        f.app.clone(),
        authority.clone(),
        "expired-during-preparation".into(),
        request,
        planned,
    ));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        authority.entered.notified(),
    )
    .await
    .unwrap();
    authority.fresh.store(false, Ordering::SeqCst);
    authority.resume.notify_one();
    let expired = pending
        .await
        .unwrap()
        .err()
        .expect("expiry during preparation must prevent durable admission");
    // This test calls shared admission directly, not PlanAuthority's retained
    // Problem adapter. Inspect its exact API error before generic v2 mapping.
    assert_eq!(
        (expired.parts().0, expired.parts().1),
        (StatusCode::CONFLICT, "plan_expired")
    );
    assert_eq!(authority.preparations.load(Ordering::SeqCst), 1);
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM delivery_admissions")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(receipts, 0);
    assert_eq!(f.app.processing.execution.snapshot().used, 0);
    assert_eq!(
        std::fs::read_dir(&f.app.processing.deliveries.root)
            .unwrap()
            .count(),
        0
    );

    authority.fresh.store(true, Ordering::SeqCst);
    let (request, planned) = byte_request(&revision);
    let first = delivery::admit_planned(
        f.app.clone(),
        authority.clone(),
        "fresh-preparation-replay".into(),
        request,
        planned,
    )
    .await
    .unwrap_or_else(|error| {
        let (status, code, _) = error.parts();
        panic!("unexpected API error: {status} {code}")
    });
    authority.fresh.store(false, Ordering::SeqCst);
    let (request, planned) = byte_request(&revision);
    let replay = delivery::admit_planned(
        f.app.clone(),
        authority.clone(),
        "fresh-preparation-replay".into(),
        request,
        planned,
    )
    .await
    .unwrap_or_else(|error| {
        let (status, code, _) = error.parts();
        panic!("unexpected API error: {status} {code}")
    });
    assert_eq!(replay.id, first.id);
    assert_eq!(authority.preparations.load(Ordering::SeqCst), 2);
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM delivery_admissions")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(receipts, 1);
    let mut tx = crate::db::begin_write(&f.app.db).await.unwrap();
    delivery::close_live(&f.app, &first.id, &mut tx)
        .await
        .unwrap_or_else(|error| {
            let (status, code, _) = error.parts();
            panic!("unexpected API error: {status} {code}")
        });
    tx.commit().await.unwrap();
    f.app.db.close().await;
}

async fn pending_init(
    app: &App,
    id: &str,
    headers: &HeaderMap,
    params: &StreamParams,
) -> (StreamRead, Response) {
    let authority = stream_owner(app, headers, params, id, "1").await.unwrap();
    assert!(authority.native_hls);
    let response = delivery::init(State(app.clone()), Path((id.into(), "1".into())))
        .await
        .unwrap_or_else(|error| {
            let (status, code, _) = error.parts();
            panic!("unexpected API error: {status} {code}")
        });
    // The actual production output read has completed; no body has been polled
    // or released. Tests interleave revocation before the production release.
    (authority, response)
}

#[tokio::test(flavor = "multi_thread")]
async fn native_buffered_release_rechecks_actual_ticket_and_credential_after_await() {
    let (mut f, caller, claims, _, token) = fixture().await;
    let stop = CancellationToken::new();
    let worker = tokio::spawn(delivery::worker(f.app.clone(), stop.clone()));
    let admitted = admit(
        f.app.clone(),
        caller.clone(),
        &sign(&f.app, &claims).unwrap(),
        "hls-release-auth".into(),
        0,
    )
    .await
    .unwrap();
    ready(&f.app, &admitted.id).await;
    let mut headers = HeaderMap::new();
    let bearer = format!("Bearer {token}");
    headers.insert(header::AUTHORIZATION, bearer.parse().unwrap());
    headers.insert(header::HOST, "127.0.0.1:8787".parse().unwrap());
    let direct = StreamParams { ticket: None };

    for change in ["expiry", "revocation"] {
        let ticket = f
            .call(
                "POST",
                "/api/v2/content-access",
                Some(json!({
                    "purpose":"playback", "delivery_id":admitted.id,
                    "generation":"1", "file":null, "download_id":null,
                    "ttl_seconds":60
                })),
                &[
                    ("authorization", &bearer),
                    ("idempotency-key", &format!("hls-ticket-{change}")),
                ],
            )
            .await;
        assert_eq!(ticket.status, StatusCode::CREATED, "{}", ticket.body);
        let url = url::Url::parse(&format!(
            "http://127.0.0.1:8787{}",
            ticket.body["url"].as_str().unwrap()
        ))
        .unwrap();
        let params = StreamParams {
            ticket: Some(
                url.query_pairs()
                    .find(|(name, _)| name == "ticket")
                    .unwrap()
                    .1
                    .into_owned(),
            ),
        };
        let no_headers = HeaderMap::new();
        let (authority, response) = pending_init(&f.app, &admitted.id, &no_headers, &params).await;
        sqlx::query(if change == "expiry" {
            "UPDATE content_tickets SET expires_at=0 WHERE id=?"
        } else {
            "UPDATE content_tickets SET revoked=1 WHERE id=?"
        })
        .bind(ticket.body["id"].as_str().unwrap())
        .execute(&f.app.db)
        .await
        .unwrap();
        let refused = authority
            .release(&f.app, &no_headers, &params, &admitted.id, "1", response)
            .await
            .unwrap_err();
        assert_eq!(refused.code, "ticket_invalid", "{change}");
    }

    let (hash, credential) = caller.credential().unwrap();
    let (authority, response) = pending_init(&f.app, &admitted.id, &headers, &direct).await;
    sqlx::query("UPDATE credentials SET expires_at=0 WHERE token_hash=?")
        .bind(hash)
        .execute(&f.app.db)
        .await
        .unwrap();
    let refused = authority
        .release(&f.app, &headers, &direct, &admitted.id, "1", response)
        .await
        .unwrap_err();
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    sqlx::query("UPDATE credentials SET expires_at=? WHERE token_hash=?")
        .bind(credential.expires_at)
        .bind(hash)
        .execute(&f.app.db)
        .await
        .unwrap();

    // Final authorization fences response admission, not client drain. The
    // unpolled bounded body must not retain SQLite's application writer.
    let admitted_response = init_segment(
        State(f.app.clone()),
        Query(StreamParams { ticket: None }),
        headers.clone(),
        Path((admitted.id.clone(), "1".into(), VARIANT.into())),
    )
    .await
    .unwrap();
    use sqlx::Connection;
    let mut contender = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(f._dir.path().join("db.sqlite"))
            .busy_timeout(std::time::Duration::ZERO),
    )
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match sqlx::query("BEGIN IMMEDIATE").execute(&mut contender).await {
                Ok(_) => break,
                Err(error) => {
                    assert!(matches!(
                        error.as_database_error().unwrap().code().as_deref(),
                        Some("5" | "6")
                    ));
                    tokio::task::yield_now().await;
                }
            }
        }
    })
    .await
    .expect("an unpolled admitted body must not block native writers or revocation");
    sqlx::query("ROLLBACK")
        .execute(&mut contender)
        .await
        .unwrap();

    let (authority, response) = pending_init(&f.app, &admitted.id, &headers, &direct).await;
    sqlx::query("DELETE FROM credentials WHERE token_hash=?")
        .bind(hash)
        .execute(&f.app.db)
        .await
        .unwrap();
    let refused = authority
        .release(&f.app, &headers, &direct, &admitted.id, "1", response)
        .await
        .unwrap_err();
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    let refused = init_segment(
        State(f.app.clone()),
        Query(StreamParams { ticket: None }),
        headers,
        Path((admitted.id.clone(), "1".into(), VARIANT.into())),
    )
    .await
    .unwrap_err();
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert!(
        !admitted_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty(),
        "an already admitted bounded response can drain after later revocation"
    );
    stop.cancel();
    worker.await.unwrap().unwrap();
    drop(contender);
    tokio::time::timeout(std::time::Duration::from_secs(15), f.close())
        .await
        .unwrap();
}
