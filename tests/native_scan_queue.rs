//! Exact native scan queue integration using the actual shipped playscale
//! executable as closed companion. This is an internal worker qualification;
//! it does not bypass or claim HTTP scan allowlist authorization.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use playscale::{App, api, db};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
const TOKEN: &str = "native-queue-test-operator-0123456789abcdef";
fn production_artifact() -> catabolic_runtime::helper::TrustedClosedObservationArtifact {
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_playscale"));
    catabolic_runtime::helper::TrustedClosedObservationArtifact::after_packaging_verification(
        binary,
        &catabolic_core::sha256(std::fs::read(binary).unwrap()),
        catabolic_runtime::helper::ClosedObservationProtocol::NoChildWithExactJoinV2,
    )
    .unwrap()
}
struct Fixture {
    dir: tempfile::TempDir,
    app: App,
    mapping: motion_catalog_backend::sources::SourceMapping,
    library: String,
    observed: Option<Value>,
}
impl Fixture {
    async fn new() -> Self {
        Self::new_observed(false).await
    }
    async fn new_observed(observe_first: bool) -> Self {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("media");
        std::fs::create_dir(&media).unwrap();
        // A real one-second PCM original, not fabricated ffprobe facts.
        let mut wav = Vec::new();
        let bytes = 16000u32;
        wav.extend(b"RIFF");
        wav.extend((bytes + 36).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(8000u32.to_le_bytes());
        wav.extend(16000u32.to_le_bytes());
        wav.extend(2u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(bytes.to_le_bytes());
        wav.resize(44 + bytes as usize, 0);
        std::fs::write(media.join("original.wav"), wav).unwrap();
        let db = db::connect(&dir.path().join("motion.sqlite3"))
            .await
            .unwrap();
        let catalog = dir.path().join("native.sqlite3");
        motion_catalog_backend::initialize(catalog.clone())
            .await
            .unwrap();
        let backend = motion_catalog_backend::NativeCatalog::bind(
            db.clone(),
            catalog.clone(),
            motion_catalog_backend::inspect(catalog.clone())
                .await
                .unwrap()
                .identity,
        )
        .await
        .unwrap();
        let mapping = backend
            .register_native_source("queue-source", "motion:unassigned", "Audio", media, vec![])
            .await
            .unwrap();
        backend
            .freeze_motion_source("queue-empty", 0)
            .await
            .unwrap();
        backend.capture_motion_import("queue-empty").await.unwrap();
        backend.stage_import("queue-empty").await.unwrap();
        backend.prepare_motion_cutover("queue-empty").await.unwrap();
        backend
            .activate_motion_cutover(std::time::Duration::from_secs(30))
            .await
            .unwrap();
        drop(backend);
        let observed = if observe_first {
            // Native observation precedes the server owner; never open a second
            // writer alongside the retained runtime.
            let observer = motion_catalog_backend::NativeCatalog::bind(
                db.clone(),
                catalog.clone(),
                mapping.catalog.clone(),
            )
            .await
            .unwrap();
            observer
                .scan_source(
                    "queue-source",
                    production_artifact(),
                    motion_catalog_backend::sources::ScanCancellation::default(),
                    std::time::Duration::from_secs(120),
                )
                .await
                .unwrap();
            observer
                .drain(std::time::Duration::from_secs(30))
                .await
                .unwrap();
            drop(observer);
            let store = catabolic_store::Store::open(&catalog, true, false).unwrap();
            let snapshot = catabolic_store::catalog_import_typed::capture_source_occurrences(
                &store,
                "default",
                &mapping.native_source,
            )
            .unwrap();
            let observations = snapshot["occurrences"].as_array().unwrap();
            assert_eq!(observations.len(), 1);
            Some(observations[0].clone())
        } else {
            None
        };
        let runtime = Arc::new(
            playscale::native_catalog::Runtime::open(db.clone(), catalog, "default".into())
                .await
                .unwrap(),
        );
        // This path is Cargo's production binary from this package, whose main
        // unconditionally dispatches the closed observer before Tokio/parser setup.
        let artifact = production_artifact();
        runtime.register_observer(artifact).await.unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let app = App {
            health: Arc::new(playscale::operations::Health::new(false)),
            native_catalog: Some(runtime),
            db,
            admin_token: Arc::new(TOKEN.into()),
            origin: Arc::new("http://127.0.0.1:8787".into()),
            authority: Arc::new("127.0.0.1:8787".into()),
            ffprobe: Arc::new("ffprobe".into()),
            jobs: Arc::new(Mutex::new(())),
            streams: Arc::new(Semaphore::new(2)),
            event_streams: Arc::new(Semaphore::new(4)),
            storage: Arc::new(playscale::storage::Runtime::new(state, Default::default())),
            processing: Arc::new(playscale::processing::Runtime::new(
                dir.path().join("cache"),
                Default::default(),
            )),
            access: Arc::new(playscale::v2::Runtime::new(
                playscale_core::access::AccessMode::Restricted,
                playscale::v2::auth::random_key(),
            )),
        };
        let response=api::router(app.clone(),None).oneshot(Request::builder().method("POST").uri("/api/v2/libraries").header("host","127.0.0.1:8787").header("authorization",format!("Bearer {TOKEN}")).header("idempotency-key","queue-library-create-0001").header("content-type","application/json").body(Body::from(json!({"name":"Audio","kind":"mixed","language":"en","source_ids":["queue-source"]}).to_string())).unwrap()).await.unwrap();
        let status = response.status();
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(status, StatusCode::CREATED, "{body}");
        Self {
            dir,
            app,
            mapping,
            library: body["id"].as_str().unwrap().into(),
            observed,
        }
    }
    async fn admit(&self) -> String {
        let mut tx = self.app.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let (catalog,incarnation,digest,query,query_revision,query_digest,library_revision):(String,String,String,String,i64,String,i64)=sqlx::query_as("SELECT catalog_id,incarnation,source_digest,query_id,query_revision,query_digest,library_revision FROM catabolic_server_scopes WHERE library_id=? AND profile='default'").bind(&self.library).fetch_one(&mut *tx).await.unwrap();
        // Scope facts originate in the real native-library create adapter above.
        let proof = json!({"contract_version":1,"library_id":self.library,"library_revision":library_revision,"catalog_id":catalog,"incarnation":incarnation,"source_digest":digest,"query_id":query,"query_revision":query_revision,"query_digest":query_digest,"accepted_principal":playscale_core::access::Principal::operator()});
        let view = playscale::native_scans::admit_in(
            &mut tx,
            &self.library,
            "operator",
            proof,
            vec![self.mapping.clone()],
            true,
            true,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        view["id"].as_str().unwrap().into()
    }
    async fn view(&self, id: &str) -> Value {
        let mut tx = self.app.db.begin().await.unwrap();
        playscale::native_scans::get_in(&mut tx, id).await.unwrap()
    }
    async fn wait_complete(&self, id: &str) {
        tokio::time::timeout(std::time::Duration::from_secs(120), async {
            loop {
                let view = self.view(id).await;
                if view["status"] == "complete" {
                    return;
                }
                assert!(
                    view["status"] != "failed" && view["status"] != "cancelled",
                    "{view}"
                );
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("native queue did not complete");
    }
    async fn read(&self, path: &str) -> Value {
        let response = api::router(self.app.clone(), None)
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("host", "127.0.0.1:8787")
                    .header("authorization", format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(status, StatusCode::OK, "GET {path}: {body}");
        body
    }
    async fn close(self) {
        self.app
            .native_catalog
            .as_ref()
            .unwrap()
            .shutdown()
            .await
            .unwrap();
    }
}
#[tokio::test]
async fn accepted_scan_observes_probes_creates_and_retries_without_new_identity() {
    let f = Fixture::new().await;
    let first = f.admit().await;
    let stop = CancellationToken::new();
    let worker = tokio::spawn(playscale::native_scans::worker(f.app.clone(), stop.clone()));
    f.wait_complete(&first).await;
    let before:Vec<(String,String)>=sqlx::query_as("SELECT operation_id,ids_json FROM catabolic_discovery_intents WHERE state='committed' ORDER BY operation_id").fetch_all(&f.app.db).await.unwrap();
    assert_eq!(before.len(), 1);
    // A different real writer may retain custody longer than the pool's five
    // second busy timeout. An idle no-effect claim must not kill the worker.
    let mut held = f.app.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    assert!(
        !worker.is_finished(),
        "idle claim contention stopped native worker"
    );
    let pending: i64 =
        sqlx::query_scalar("SELECT count(*) FROM catabolic_scan_demands WHERE phase='running'")
            .fetch_one(&mut *held)
            .await
            .unwrap();
    assert_eq!(pending, 0);
    held.commit().await.unwrap();

    let files: Vec<(String, i64)> = sqlx::query_as("SELECT id,available FROM media_files")
        .fetch_all(&f.app.db)
        .await
        .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].1, 1);
    let ready: i64 = sqlx::query_scalar("SELECT count(*) FROM media_versions")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(ready, 1);
    let ids: Value = serde_json::from_str(&before[0].1).unwrap();
    let work = ids["work"].as_str().unwrap();
    eprintln!("queue stage: first discovered work GET");
    let item = f.read(&format!("/api/v2/catalog/items/{work}")).await;
    assert_eq!(item["library_ids"], json!([f.library]));
    assert_eq!(item["availability"], "available");
    assert_eq!(
        item["kind"], "other",
        "generic WAV audio is not a music recording"
    );
    assert_eq!(item["default_timeline_id"], ids["timeline"]);
    {
        let store =
            catabolic_store::Store::open(&f.dir.path().join("native.sqlite3"), false, false)
                .unwrap();
        let native_kind: String = store
            .db
            .query_row("SELECT kind FROM items WHERE id=?", [&work], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(native_kind, "custom:motion_audio");
        let measured: (String, i64, i64, Option<String>) = store.db.query_row(
            "SELECT r.state,r.coverage_start_us,r.coverage_end_us,r.equivalence_evidence_id FROM catalog_versions v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision WHERE v.id=?",
            [ids["version"].as_str().unwrap()],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        ).unwrap();
        assert_eq!(
            measured,
            ("ready".into(), 0, 1_000_000, None),
            "actual seekable PCM probe must qualify only its own measured original, without equivalence"
        );
    }
    eprintln!("queue stage: first discovered work browse");
    let browse = f.read("/api/v2/catalog/items").await;
    assert!(
        browse["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == work)
    );
    let second = f.admit().await;
    f.wait_complete(&second).await;
    let after:Vec<(String,String)>=sqlx::query_as("SELECT operation_id,ids_json FROM catabolic_discovery_intents WHERE state='committed' ORDER BY operation_id").fetch_all(&f.app.db).await.unwrap();
    assert_eq!(before, after);
    // Removal updates availability, never reallocates historical identities.
    std::fs::remove_file(f.dir.path().join("media/original.wav")).unwrap();
    let third = f.admit().await;
    f.wait_complete(&third).await;
    let unavailable: i64 = sqlx::query_scalar("SELECT available FROM media_files WHERE id=?")
        .bind(&files[0].0)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(unavailable, 0);
    eprintln!("queue stage: removed work GET");
    let unavailable_item = f.read(&format!("/api/v2/catalog/items/{work}")).await;
    assert_eq!(unavailable_item["availability"], "unavailable");
    assert_eq!(unavailable_item["default_timeline_id"], ids["timeline"]);
    eprintln!("queue stage: removed work browse");
    let unavailable_browse = f.read("/api/v2/catalog/items").await;
    assert!(
        unavailable_browse["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == work && item["availability"] == "unavailable")
    );
    let retained:Vec<(String,String)>=sqlx::query_as("SELECT operation_id,ids_json FROM catabolic_discovery_intents WHERE state='committed' ORDER BY operation_id").fetch_all(&f.app.db).await.unwrap();
    assert_eq!(before, retained);
    stop.cancel();
    worker.await.unwrap().unwrap();
    f.close().await;
}
#[tokio::test]
async fn durable_cancel_before_worker_survives_restart_without_native_discovery() {
    let f = Fixture::new().await;
    let id = f.admit().await;
    {
        let mut tx = f.app.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        assert_eq!(
            playscale::native_scans::cancel_in(&mut tx, &id)
                .await
                .unwrap()["status"],
            "cancelled"
        );
        tx.commit().await.unwrap();
    }
    for _ in 0..2 {
        let stop = CancellationToken::new();
        let worker = tokio::spawn(playscale::native_scans::worker(f.app.clone(), stop.clone()));
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        stop.cancel();
        worker.await.unwrap().unwrap();
        assert_eq!(f.view(&id).await["status"], "cancelled");
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_discovery_intents")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert!(f.dir.path().join("media/original.wav").is_file());
    f.close().await;
}

#[tokio::test]
async fn cancellation_custody_inhibits_claim_without_fabricating_cancel() {
    let f = Fixture::new().await;
    let id = f.admit().await;
    let first = playscale::native_scans::signal_cancel(&f.app, &id).unwrap();
    let second = playscale::native_scans::signal_cancel(&f.app, &id).unwrap();
    first.wait_cleanup().await;
    let stop = CancellationToken::new();
    let worker = tokio::spawn(playscale::native_scans::worker(f.app.clone(), stop.clone()));
    drop(first);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let state: (String, String, i64) = sqlx::query_as(
        "SELECT phase,status,attempt FROM catabolic_scan_demands WHERE request_id=?",
    )
    .bind(&id)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(state, ("queued".into(), "pending".into(), 0));
    // Caller abort releases only the last inhibition. Pending authorized work may
    // resume; it was never reported durably cancelled.
    drop(second);
    f.wait_complete(&id).await;
    stop.cancel();
    worker.await.unwrap().unwrap();
    f.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_cancellation_waits_exact_cleanup_and_inhibits_reclaim_until_ack() {
    let f = Fixture::new().await;
    for directory in 0..100 {
        let folder = f.dir.path().join(format!("media/folder-{directory}"));
        std::fs::create_dir(&folder).unwrap();
        for file in 0..50 {
            std::fs::write(folder.join(format!("file-{file}.txt")), b"fixture").unwrap();
        }
    }
    let id = f.admit().await;
    let stop = CancellationToken::new();
    let worker = tokio::spawn(playscale::native_scans::worker(f.app.clone(), stop.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let path = f.dir.path().join("native.sqlite3");
            let active = tokio::task::spawn_blocking(move || {
                let store = catabolic_store::Store::open(&path, false, false).unwrap();
                !catabolic_store::native_worker_inventory::inspect(&store)
                    .unwrap()
                    .admits_new_work()
            })
            .await
            .unwrap();
            if active {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("actual native worker ownership not observed");
    let custody = playscale::native_scans::signal_cancel(&f.app, &id).unwrap();
    let started = std::time::Instant::now();
    tokio::time::timeout(std::time::Duration::from_secs(120), custody.wait_cleanup())
        .await
        .expect("actual native cleanup did not finish");
    eprintln!(
        "native queue cancellation cleanup elapsed {:?}",
        started.elapsed()
    );
    let state: (String, String, i64) = sqlx::query_as(
        "SELECT phase,status,attempt FROM catabolic_scan_demands WHERE request_id=?",
    )
    .bind(&id)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(state, ("queued".into(), "pending".into(), 1));
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let attempts: i64 =
        sqlx::query_scalar("SELECT attempt FROM catabolic_scan_demands WHERE request_id=?")
            .bind(&id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(
        attempts, 1,
        "cleanup alone must not release reclaim inhibition"
    );
    // A real handler reauthorizes here before this acknowledgment transaction.
    let mut tx = f.app.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
    assert_eq!(
        playscale::native_scans::cancel_in(&mut tx, &id)
            .await
            .unwrap()["status"],
        "cancelled"
    );
    tx.commit().await.unwrap();
    drop(custody);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(f.view(&id).await["status"], "cancelled");
    let effects: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM catabolic_discovery_intents WHERE state='committed'",
    )
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(effects, 0);
    stop.cancel();
    worker.await.unwrap().unwrap();
    f.close().await;
}

#[tokio::test]
async fn real_native_discovery_commit_with_lost_motion_ack_recovers_receipt_only() {
    let mut f = Fixture::new_observed(true).await;
    let observed = f.observed.clone().unwrap();
    let intent = playscale::native_discovery::prepare(&f.app.db, f.mapping.clone(), observed)
        .await
        .unwrap();
    let intent = playscale::native_discovery::probe(&f.app, intent, CancellationToken::new())
        .await
        .unwrap();
    // Force a real stale-generation decision, not a fabricated no-effect grant.
    // The successor must keep these allocated entity IDs and the exact payload.
    let original_command = catabolic_core::catalog_command::CommandEnvelope::new(
        1,
        catabolic_core::catalog_command::CommandKey::new(
            &intent.source.catalog.catalog_id,
            "motion.catalog.operator.v1",
            &intent.operation_id,
        )
        .unwrap(),
        &intent.source.catalog.incarnation,
        1,
        &playscale::native_discovery::discovery_payload(&intent, "motion.catalog.operator.v1")
            .unwrap()
            .to_string(),
    )
    .unwrap();
    sqlx::query("UPDATE catabolic_discovery_intents SET command_json=? WHERE operation_id=? AND state='probed'")
        .bind(original_command.canonical_payload()).bind(&intent.operation_id).execute(&f.app.db).await.unwrap();
    let original_ids = intent.ids.clone();
    let operation = intent.operation_id.clone();
    let pending = playscale::native_discovery::dispatch_for_acknowledgment(&f.app, intent)
        .await
        .unwrap();
    // This is the real production boundary: native committed, while the owned
    // Motion writer transaction has not acknowledged receipt/mapping/baseline.
    let state: String =
        sqlx::query_scalar("SELECT state FROM catabolic_discovery_intents WHERE operation_id=?")
            .bind(&operation)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(state, "probed");
    let mut read = f.app.db.begin().await.unwrap();
    let (ordinal, latest) =
        motion_catalog_backend::command_attempts::latest_in(&mut read, &original_command)
            .await
            .unwrap();
    assert_eq!(ordinal, 1);
    assert_ne!(latest.key(), original_command.key());
    assert_eq!(latest.payload(), original_command.payload());
    read.rollback().await.unwrap();

    let catalog = f.dir.path().join("native.sqlite3");
    {
        let store = catabolic_store::Store::open(&catalog, false, false).unwrap();
        let native_count: i64 = store
            .db
            .query_row(
                "SELECT count(*) FROM items WHERE id=?",
                [&original_ids.work],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            native_count, 1,
            "native effect must really commit before losing ACK"
        );
    }
    drop(pending); // actual host transaction rollback, not SQL proof tampering
    f.app
        .native_catalog
        .as_ref()
        .unwrap()
        .shutdown()
        .await
        .unwrap();
    let runtime = Arc::new(
        playscale::native_catalog::Runtime::open(f.app.db.clone(), catalog, "default".into())
            .await
            .unwrap(),
    );
    f.app.native_catalog = Some(runtime);
    let (state, ids, receipt): (String, String, String) = sqlx::query_as(
        "SELECT state,ids_json,receipt_json FROM catabolic_discovery_intents WHERE operation_id=?",
    )
    .bind(&operation)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(state, "committed");
    assert_eq!(
        serde_json::from_str::<playscale::native_discovery::DiscoveryIds>(&ids).unwrap(),
        original_ids
    );
    let receipt: Value = serde_json::from_str(&receipt).unwrap();
    assert_eq!(receipt["status"], "replay");
    let item = f
        .read(&format!("/api/v2/catalog/items/{}", original_ids.work))
        .await;
    assert_eq!(item["availability"], "available");
    assert_eq!(
        item["kind"], "other",
        "generic WAV audio is not a music recording"
    );
    assert_eq!(item["default_timeline_id"], original_ids.timeline);
    f.close().await;
}

#[tokio::test]
async fn startup_never_dispatches_discovery_intent_without_native_receipt() {
    let mut f = Fixture::new_observed(true).await;
    let intent = playscale::native_discovery::prepare(
        &f.app.db,
        f.mapping.clone(),
        f.observed.clone().unwrap(),
    )
    .await
    .unwrap();
    let intent = playscale::native_discovery::probe(&f.app, intent, CancellationToken::new())
        .await
        .unwrap();
    let command = catabolic_core::catalog_command::CommandEnvelope::new(
        1,
        catabolic_core::catalog_command::CommandKey::new(
            &intent.source.catalog.catalog_id,
            "motion.catalog.operator.v1",
            &intent.operation_id,
        )
        .unwrap(),
        &intent.source.catalog.incarnation,
        1,
        &playscale::native_discovery::discovery_payload(&intent, "motion.catalog.operator.v1")
            .unwrap()
            .to_string(),
    )
    .unwrap();
    // Persist a genuine prepared intent with no native dispatch. Stale expected
    // generation is deliberate: startup must not refresh or execute it either.
    sqlx::query("UPDATE catabolic_discovery_intents SET command_json=? WHERE operation_id=? AND state='probed'").bind(command.canonical_payload()).bind(&intent.operation_id).execute(&f.app.db).await.unwrap();
    f.app
        .native_catalog
        .as_ref()
        .unwrap()
        .shutdown()
        .await
        .unwrap();
    let catalog = f.dir.path().join("native.sqlite3");
    f.app.native_catalog = Some(Arc::new(
        playscale::native_catalog::Runtime::open(
            f.app.db.clone(),
            catalog.clone(),
            "default".into(),
        )
        .await
        .unwrap(),
    ));
    let (state,saved,receipt):(String,String,Option<String>) = sqlx::query_as("SELECT state,command_json,receipt_json FROM catabolic_discovery_intents WHERE operation_id=?").bind(&intent.operation_id).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(state, "probed");
    assert_eq!(saved, command.canonical_payload());
    assert!(receipt.is_none());
    let store = catabolic_store::Store::open(&catalog, false, false).unwrap();
    let native_count: i64 = store
        .db
        .query_row(
            "SELECT count(*) FROM items WHERE id=?",
            [&intent.ids.work],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(native_count, 0);
    drop(store);
    f.close().await;
}

#[tokio::test]
async fn withdrawn_source_rejects_retained_probe_and_new_command_without_native_effect() {
    let f = Fixture::new_observed(true).await;
    let intent = playscale::native_discovery::prepare(
        &f.app.db,
        f.mapping.clone(),
        f.observed.clone().unwrap(),
    )
    .await
    .unwrap();
    let intent = playscale::native_discovery::probe(&f.app, intent, CancellationToken::new())
        .await
        .unwrap();
    async fn request(
        f: &Fixture,
        method: &str,
        path: &str,
        value: Option<Value>,
        etag: &str,
    ) -> (StatusCode, Value) {
        let response = api::router(f.app.clone(), None)
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", "127.0.0.1:8787")
                    .header("authorization", format!("Bearer {TOKEN}"))
                    .header("if-match", etag)
                    .header("content-type", "application/json")
                    .body(Body::from(value.map(|v| v.to_string()).unwrap_or_default()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).unwrap()
            },
        )
    }
    let detached = request(
        &f,
        "PUT",
        &format!("/api/v2/libraries/{}", f.library),
        Some(json!({"name":"Audio","kind":"mixed","language":"en","source_ids":[]})),
        "\"r-1\"",
    )
    .await;
    assert_eq!(detached.0, StatusCode::OK, "{}", detached.1);
    let catalog = f.dir.path().join("native.sqlite3");
    let native_generation = || {
        let store = catabolic_store::Store::open(&catalog, false, false).unwrap();
        store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
    };
    let before = native_generation();
    let withdrawn = request(
        &f,
        "DELETE",
        "/api/v2/sources/queue-source",
        None,
        "\"r-1\"",
    )
    .await;
    assert_eq!(withdrawn.0, StatusCode::OK, "{}", withdrawn.1);
    assert_eq!(withdrawn.1["availability"], "unavailable");
    assert_eq!(withdrawn.1["revision"], "2");
    assert!(
        playscale::native_discovery::probe(&f.app, intent.clone(), CancellationToken::new())
            .await
            .is_err()
    );
    assert!(
        playscale::native_discovery::dispatch_for_acknowledgment(&f.app, intent.clone())
            .await
            .is_err()
    );
    assert!(
        playscale::native_discovery::prepare(
            &f.app.db,
            f.mapping.clone(),
            f.observed.clone().unwrap()
        )
        .await
        .is_err()
    );
    let saved:(String,Option<String>,Option<String>)=sqlx::query_as("SELECT state,command_json,receipt_json FROM catabolic_discovery_intents WHERE operation_id=?")
        .bind(&intent.operation_id).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(
        saved,
        ("probed".into(), None, None),
        "withdrawn intent must not be replaced or acquire an undispatched command"
    );
    assert_eq!(before, native_generation());
    let store = catabolic_store::Store::open(&catalog, false, false).unwrap();
    let created: i64 = store
        .db
        .query_row(
            "SELECT count(*) FROM items WHERE id=?",
            [&intent.ids.work],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(created, 0);
    drop(store);
    f.close().await;
}
