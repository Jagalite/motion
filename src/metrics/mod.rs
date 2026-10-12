//! Operational observations only: none of these measurements authorizes work,
//! changes a job transition, or participates in delivery/publication decisions.
pub mod history;
pub mod process;

use axum::{
    body::{Body, Bytes, HttpBody},
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use serde::Serialize;
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
};
use tokio::sync::Semaphore;

pub struct Runtime {
    requests_non_error_total: AtomicU64,
    request_errors_total: AtomicU64,
    requests_in_flight: AtomicU64,
    process_reads: Arc<Semaphore>,
    pub history: tokio::sync::OnceCell<sqlx::SqlitePool>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime {
    pub fn new() -> Self {
        Self {
            requests_non_error_total: AtomicU64::new(0),
            request_errors_total: AtomicU64::new(0),
            requests_in_flight: AtomicU64::new(0),
            process_reads: Arc::new(Semaphore::new(2)),
            history: tokio::sync::OnceCell::new(),
        }
    }

    pub fn http_snapshot(&self) -> HttpSnapshot {
        // Derive the total from the same error observation, so concurrent
        // completions cannot produce an error count greater than the total.
        let non_errors = self.requests_non_error_total.load(Ordering::Relaxed);
        let errors = self.request_errors_total.load(Ordering::Relaxed);
        HttpSnapshot {
            requests_total: non_errors.saturating_add(errors),
            request_errors_total: errors,
            requests_in_flight: self.requests_in_flight.load(Ordering::Relaxed),
        }
    }

    /// Native process reads execute off the async executor. Keep the permit in
    /// the blocking closure so a cancelled caller cannot queue unbounded work.
    pub async fn snapshot(&self) -> Result<ProcessMetrics, &'static str> {
        let permit = self
            .process_reads
            .clone()
            .try_acquire_owned()
            .map_err(|_| "process_metrics_busy")?;
        let system = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            process::snapshot()
        })
        .await
        .map_err(|_| "process_metrics_unavailable")?
        .map_err(|_| "process_metrics_unavailable")?;
        let http = self.http_snapshot();
        Ok(ProcessMetrics {
            system,
            requests_total: http.requests_total.to_string(),
            request_errors_total: http.request_errors_total.to_string(),
            requests_in_flight: http.requests_in_flight.to_string(),
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HttpSnapshot {
    pub requests_total: u64,
    pub request_errors_total: u64,
    pub requests_in_flight: u64,
}

#[derive(Debug, Serialize)]
pub struct ProcessMetrics {
    #[serde(flatten)]
    pub system: process::Snapshot,
    pub requests_total: String,
    pub request_errors_total: String,
    pub requests_in_flight: String,
}

struct RequestGuard {
    runtime: Arc<Runtime>,
    response_error: Option<bool>,
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        if let Some(error) = self.response_error {
            if error {
                self.runtime
                    .request_errors_total
                    .fetch_add(1, Ordering::Relaxed);
            } else {
                self.runtime
                    .requests_non_error_total
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        self.runtime
            .requests_in_flight
            .fetch_sub(1, Ordering::Relaxed);
    }
}

/// Count external HTTP requests once at the composed router boundary. Internal
/// presentation-to-v2 calls do not pass this layer. In-flight includes response
/// streaming; completion includes a response body ending or being dropped.
/// Cancellation before a response only releases in-flight, since no status exists.
pub async fn track(State(runtime): State<Arc<Runtime>>, request: Request, next: Next) -> Response {
    runtime.requests_in_flight.fetch_add(1, Ordering::Relaxed);
    let mut guard = RequestGuard {
        runtime,
        response_error: None,
    };
    let response = next.run(request).await;
    guard.response_error = Some(response.status().as_u16() >= 500);
    response.map(|body| {
        if body.is_end_stream() {
            drop(guard);
            body
        } else {
            Body::new(TrackedBody {
                body,
                guard: Some(guard),
            })
        }
    })
}

struct TrackedBody {
    body: Body,
    guard: Option<RequestGuard>,
}

impl HttpBody for TrackedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let result = Pin::new(&mut self.body).poll_frame(cx);
        if matches!(result, Poll::Ready(None | Some(Err(_)))) || self.body.is_end_stream() {
            self.guard.take();
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
    fn size_hint(&self) -> http_body::SizeHint {
        self.body.size_hint()
    }
}
