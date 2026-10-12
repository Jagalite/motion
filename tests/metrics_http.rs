//! Request-lifetime observations through the real composed HTTP router and
//! temporary SQLite. These tests control transport timing, not domain state.
use axum::{
    Router,
    body::{Body, Bytes},
    extract::Path,
    http::{Request, StatusCode},
    response::Response,
    routing::get,
};
use http_body_util::BodyExt;
use playscale::{App, api, db};
use playscale_core::access::AccessMode;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Barrier, Mutex, Notify, Semaphore};
use tower::ServiceExt;

struct Fixture {
    _dir: tempfile::TempDir,
    app: App,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let database = db::connect(&dir.path().join("db.sqlite")).await.unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let app = App {
            health: Arc::new(playscale::operations::Health::new(false)),
            db: database,
            admin_token: Arc::new("metrics-test-operator-token".into()),
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
                AccessMode::Restricted,
                playscale::v2::auth::random_key(),
            )),
        };
        Self { _dir: dir, app }
    }

    fn router(&self, presentation: Router) -> Router {
        api::router_with(self.app.clone(), None, Some(presentation))
    }

    fn counts(&self, total: u64, errors: u64, in_flight: u64) {
        let sample = self.app.health.metrics.http_snapshot();
        assert_eq!(sample.requests_total, total);
        assert_eq!(sample.request_errors_total, errors);
        assert_eq!(sample.requests_in_flight, in_flight);
    }
}

fn request(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header("host", "127.0.0.1:8787")
        .body(Body::empty())
        .unwrap()
}

fn status_router() -> Router {
    Router::new().route(
        "/status/{code}",
        get(|Path(code): Path<u16>| async move { StatusCode::from_u16(code).unwrap() }),
    )
}

#[tokio::test]
async fn response_error_threshold_is_literal_status_at_least_500() {
    let fixture = Fixture::new().await;
    let router = fixture.router(status_router());
    fixture.counts(0, 0, 0);
    for (index, code) in [499u16, 500, 599, 600].into_iter().enumerate() {
        let response = router
            .clone()
            .oneshot(request(&format!("/status/{code}")))
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), code);
        response.into_body().collect().await.unwrap();
        fixture.counts(index as u64 + 1, index as u64, 0);
    }
}

#[tokio::test]
async fn cancelling_pending_handler_releases_in_flight_without_completing_response() {
    let fixture = Fixture::new().await;
    let entered = Arc::new(Notify::new());
    let handler_entered = entered.clone();
    let presentation = Router::new().route(
        "/pending",
        get(move || {
            let entered = handler_entered.clone();
            async move {
                entered.notify_one();
                std::future::pending::<StatusCode>().await
            }
        }),
    );
    let pending = tokio::spawn(fixture.router(presentation).oneshot(request("/pending")));
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    fixture.counts(0, 0, 1);
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    fixture.counts(0, 0, 0);
}

#[tokio::test]
async fn cancelling_pending_stream_consumption_completes_known_response_once() {
    let fixture = Fixture::new().await;
    let awaiting_more = Arc::new(Notify::new());
    let stream_waiting = awaiting_more.clone();
    let presentation = Router::new().route(
        "/stream",
        get(move || {
            let awaiting_more = stream_waiting.clone();
            async move {
                let stream = async_stream::stream! {
                    yield Ok::<_, std::io::Error>(Bytes::from_static(b"first frame"));
                    awaiting_more.notify_one();
                    std::future::pending::<()>().await;
                };
                Response::builder()
                    .status(StatusCode::SERVICE_UNAVAILABLE)
                    .body(Body::from_stream(stream))
                    .unwrap()
            }
        }),
    );
    let response = fixture
        .router(presentation)
        .oneshot(request("/stream"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let mut body = response.into_body();
    let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert_eq!(first, "first frame");
    fixture.counts(0, 0, 1);
    let reader = tokio::spawn(async move { body.collect().await });
    tokio::time::timeout(Duration::from_secs(5), awaiting_more.notified())
        .await
        .unwrap();
    fixture.counts(0, 0, 1);
    reader.abort();
    assert!(reader.await.unwrap_err().is_cancelled());
    fixture.counts(1, 1, 0);
}

#[tokio::test]
async fn stream_transport_error_releases_guard_before_body_drop_without_inventing_5xx() {
    let fixture = Fixture::new().await;
    let presentation = Router::new().route(
        "/broken-stream",
        get(|| async {
            Body::from_stream(async_stream::stream! {
                yield Ok::<_, std::io::Error>(Bytes::from_static(b"first frame"));
                yield Err(std::io::Error::other("test transport failure"));
            })
        }),
    );
    let response = fixture
        .router(presentation)
        .oneshot(request("/broken-stream"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    body.frame().await.unwrap().unwrap();
    fixture.counts(0, 0, 1);
    assert!(body.frame().await.unwrap().is_err());
    fixture.counts(1, 0, 0);
    drop(body);
    fixture.counts(1, 0, 0);
}

#[tokio::test]
async fn stream_eof_completes_response_while_body_is_still_owned() {
    let fixture = Fixture::new().await;
    let presentation = Router::new().route(
        "/finite-stream",
        get(|| async {
            Body::from_stream(async_stream::stream! {
                yield Ok::<_, std::io::Error>(Bytes::from_static(b"only frame"));
            })
        }),
    );
    let response = fixture
        .router(presentation)
        .oneshot(request("/finite-stream"))
        .await
        .unwrap();
    let mut body = response.into_body();
    body.frame().await.unwrap().unwrap();
    fixture.counts(0, 0, 1);
    assert!(body.frame().await.is_none());
    fixture.counts(1, 0, 0);
    drop(body);
    fixture.counts(1, 0, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_completions_preserve_counter_subset_and_exact_final_totals() {
    const WORKERS: usize = 12;
    const REQUESTS: usize = 24;
    let fixture = Fixture::new().await;
    let router = fixture.router(status_router());
    let start = Arc::new(Barrier::new(WORKERS + 1));
    let mut workers = tokio::task::JoinSet::new();
    for _ in 0..WORKERS {
        let router = router.clone();
        let start = start.clone();
        workers.spawn(async move {
            start.wait().await;
            for number in 0..REQUESTS {
                let path = if number % 2 == 0 {
                    "/status/200"
                } else {
                    "/status/500"
                };
                router
                    .clone()
                    .oneshot(request(path))
                    .await
                    .unwrap()
                    .into_body()
                    .collect()
                    .await
                    .unwrap();
            }
        });
    }
    let stop = Arc::new(AtomicBool::new(false));
    let watching = Arc::new(Notify::new());
    let observer_stop = stop.clone();
    let observer_started = watching.clone();
    let runtime = fixture.app.health.metrics.clone();
    let observer = tokio::spawn(async move {
        let mut samples = 0usize;
        loop {
            let sample = runtime.http_snapshot();
            assert!(sample.request_errors_total <= sample.requests_total);
            samples += 1;
            if samples == 1 {
                observer_started.notify_one();
            }
            if observer_stop.load(Ordering::Acquire) {
                return samples;
            }
            tokio::task::yield_now().await;
        }
    });
    tokio::time::timeout(Duration::from_secs(5), watching.notified())
        .await
        .unwrap();
    start.wait().await;
    tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(result) = workers.join_next().await {
            result.unwrap();
        }
    })
    .await
    .unwrap();
    stop.store(true, Ordering::Release);
    assert!(observer.await.unwrap() > 0);
    fixture.counts(
        (WORKERS * REQUESTS) as u64,
        (WORKERS * REQUESTS / 2) as u64,
        0,
    );
}
