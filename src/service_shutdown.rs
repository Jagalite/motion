//! Genuine terminal joins for stopped-service custody, not an is_finished probe.
#[allow(dead_code)]
pub(crate) mod ownership;

use std::{fmt::Display, future::Future, time::Duration};
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;

pub(super) type Completion = Result<(), String>;
pub(super) fn completed<E: Display>(result: Result<Result<(), E>, JoinError>) -> Completion {
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(e) => Err(e.to_string()),
    }
}
/// An error requests sibling termination before the join group finishes. The
/// group must use join!, not try_join!, so no sibling future is dropped early.
pub(super) async fn cancel_on_error<T>(
    stop: CancellationToken,
    work: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    let result = work.await;
    if result.is_err() {
        stop.cancel();
    }
    result
}
/// A consumed completion is supplied only by the actual select!/await caller.
/// A deadline is a refusal, not a terminal event or an authorization token.
pub(super) async fn join<E: Display>(
    name: &str,
    task: &mut JoinHandle<Result<(), E>>,
    terminal: &mut Option<Completion>,
    grace: Duration,
) -> Completion {
    if terminal.is_none() {
        match tokio::time::timeout(grace, task).await {
            Ok(result) => *terminal = Some(completed(result)),
            Err(_) => {
                return Err(format!(
                    "{name} cleanup timed out; stopped-service custody unavailable"
                ));
            }
        }
    }
    terminal
        .as_ref()
        .expect("actual completion stored")
        .clone()
        .map_err(|e| format!("{name} failed: {e}"))
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    #[tokio::test]
    async fn failed_sibling_cancels_and_waits_for_actual_other_exit() {
        let stop = CancellationToken::new();
        let exited = Arc::new(AtomicBool::new(false));
        let observed = exited.clone();
        let wait = stop.clone();
        let (first, second) = tokio::join!(
            cancel_on_error(stop.clone(), async {
                Err::<(), _>(anyhow::anyhow!("failure"))
            }),
            cancel_on_error(stop.clone(), async move {
                wait.cancelled().await;
                tokio::task::yield_now().await;
                observed.store(true, Ordering::Release);
                Ok(())
            })
        );
        assert!(first.is_err());
        assert!(second.is_ok());
        assert!(exited.load(Ordering::Acquire));
    }
    #[tokio::test]
    async fn finished_is_joined_and_consumed_result_is_not_polled_twice() {
        let mut task = tokio::spawn(async { Ok::<(), anyhow::Error>(()) });
        while !task.is_finished() {
            tokio::task::yield_now().await;
        }
        let mut terminal = None;
        assert!(
            join("actual", &mut task, &mut terminal, Duration::from_secs(1))
                .await
                .is_ok()
        );
        assert!(
            join("actual", &mut task, &mut terminal, Duration::from_secs(1))
                .await
                .is_ok()
        );
    }
    #[tokio::test]
    async fn timeout_never_claims_terminal_and_later_join_remains_required() {
        let (send, receive) = tokio::sync::oneshot::channel();
        let mut task = tokio::spawn(async move {
            receive.await?;
            Ok::<(), anyhow::Error>(())
        });
        let mut terminal = None;
        assert!(
            join(
                "blocked",
                &mut task,
                &mut terminal,
                Duration::from_millis(1)
            )
            .await
            .is_err()
        );
        assert!(terminal.is_none());
        send.send(()).unwrap();
        assert!(
            join("blocked", &mut task, &mut terminal, Duration::from_secs(1))
                .await
                .is_ok()
        );
    }
    #[tokio::test]
    async fn panic_or_abort_is_not_successful_drain() {
        let mut panic = tokio::spawn(async {
            panic!("owner failed");
            #[allow(unreachable_code)]
            Ok::<(), anyhow::Error>(())
        });
        let mut result = None;
        assert!(
            join("panic", &mut panic, &mut result, Duration::from_secs(1))
                .await
                .is_err()
        );
        let mut aborted = tokio::spawn(async {
            std::future::pending::<()>().await;
            Ok::<(), anyhow::Error>(())
        });
        aborted.abort();
        assert!(
            join("aborted", &mut aborted, &mut None, Duration::from_secs(1))
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn blocking_sampler_must_actually_exit_before_join_succeeds() {
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let (entered, started) = tokio::sync::oneshot::channel();
        let mut sampler = tokio::spawn(async move {
            tokio::task::spawn_blocking(move || {
                entered.send(()).unwrap();
                wait.recv().unwrap();
            })
            .await?;
            Ok::<(), anyhow::Error>(())
        });
        started.await.unwrap();
        let mut terminal = None;
        assert!(
            join(
                "metrics",
                &mut sampler,
                &mut terminal,
                Duration::from_millis(1)
            )
            .await
            .is_err()
        );
        assert!(terminal.is_none());
        release.send(()).unwrap();
        assert!(
            join(
                "metrics",
                &mut sampler,
                &mut terminal,
                Duration::from_secs(1)
            )
            .await
            .is_ok()
        );
    }
    #[tokio::test]
    async fn ingress_request_is_retained_until_graceful_response_finishes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let handler_entered = entered.clone();
        let handler_release = release.clone();
        let router = axum::Router::new().route(
            "/",
            axum::routing::get(move || {
                let entered = handler_entered.clone();
                let release = handler_release.clone();
                async move {
                    entered.notify_one();
                    release.notified().await;
                    "finished"
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let stop = CancellationToken::new();
        let halt = stop.clone();
        let mut task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(halt.cancelled_owned())
                .await
        });
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        entered.notified().await;
        stop.cancel();
        let mut terminal = None;
        assert!(
            join(
                "ingress",
                &mut task,
                &mut terminal,
                Duration::from_millis(1)
            )
            .await
            .is_err()
        );
        release.notify_one();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert!(String::from_utf8(response).unwrap().ends_with("finished"));
        assert!(
            join("ingress", &mut task, &mut terminal, Duration::from_secs(1))
                .await
                .is_ok()
        );
    }
}
