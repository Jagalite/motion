//! Actual retained native owner regressions. These complement, not replace,
//! the four-response HLS fixture with real yielded/cloned/sliced media bytes.
use super::*;
use crate as playscale_app;
use catabolic_core::catalog_host_lease::{HostAuthorization, LeaseError};
use catabolic_runtime::host::Lifecycle;
#[path = "../../tests/support/native_application_fixture.rs"]
mod native_application_fixture;
use native_application_fixture::{Fixture, observer_binary};

async fn fixture() -> Fixture {
    let mut fixture = Fixture::new().await;
    fixture.cutover().await;
    fixture
}
async fn gate(runtime: &Runtime) -> HostAuthorization {
    runtime
        .with_host(|host| Ok(host.authorization()?))
        .await
        .unwrap()
}
async fn revoked(gate: &HostAuthorization) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match gate.acquire(gate.revision()) {
                Err(LeaseError::Revoked) => break,
                Ok(lease) => drop(lease),
                Err(error) => panic!("unexpected native authorization failure: {error:?}"),
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("terminal close must revoke actual shared authorization");
}
async fn lifecycle(runtime: &Runtime) -> Lifecycle {
    let owner = runtime.admission.clone();
    tokio::time::timeout(
        Duration::from_secs(1),
        tokio::task::spawn_blocking(move || {
            let mut held = owner.lock().unwrap();
            assert!(held.terminal);
            held.server.admission.admitted_host().unwrap().lifecycle()
        }),
    )
    .await
    .expect("native mutex must remain available while genuine leases drain")
    .unwrap()
}
async fn closed(runtime: &Runtime) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while lifecycle(runtime).await != Lifecycle::Closed {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("actual blocking close owner must finish after its last lease drops");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_shutdown_timeout_retains_real_owner_for_exact_retry() {
    let mut f = fixture().await;
    let runtime = f.app.native_catalog.as_ref().unwrap().clone();
    let gate = gate(&runtime).await;
    let lease = gate.acquire(gate.revision()).unwrap();
    let error = tokio::time::timeout(Duration::from_secs(7), runtime.shutdown())
        .await
        .expect("ordinary shutdown must return its bounded refusal")
        .expect_err("a live actual native lease must prevent successful close");
    assert_eq!(
        error.to_string(),
        "native authorization did not drain: Timeout"
    );
    revoked(&gate).await;
    assert!(runtime.with_host(|_| Ok(())).await.is_err());
    assert_eq!(lifecycle(&runtime).await, Lifecycle::Draining);
    // The real backend remains held, but no application writer is retained.
    tokio::time::timeout(Duration::from_secs(1), crate::db::begin_write(&f.app.db))
        .await
        .unwrap()
        .unwrap()
        .rollback()
        .await
        .unwrap();
    drop(lease);
    tokio::time::timeout(Duration::from_secs(2), runtime.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lifecycle(&runtime).await, Lifecycle::Closed);
    assert!(matches!(
        gate.acquire(gate.revision()),
        Err(LeaseError::Revoked)
    ));
    f.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_shutdown_waiters_do_not_cancel_actual_close_custody() {
    for terminal_export_owner in [false, true] {
        let mut f = fixture().await;
        let runtime = f.app.native_catalog.as_ref().unwrap().clone();
        let gate = gate(&runtime).await;
        let lease = gate.acquire(gate.revision()).unwrap();
        let terminal = if terminal_export_owner {
            Some(runtime.close_admission_for_stopped_service().await.unwrap())
        } else {
            None
        };
        let worker = runtime.clone();
        let close = tokio::spawn(async move {
            match terminal {
                Some(terminal) => terminal.close().await,
                None => worker.shutdown().await,
            }
        });
        revoked(&gate).await;
        assert_eq!(lifecycle(&runtime).await, Lifecycle::Draining);
        assert!(!close.is_finished());
        close.abort();
        assert!(close.await.unwrap_err().is_cancelled());
        assert_eq!(lifecycle(&runtime).await, Lifecycle::Draining);
        assert!(runtime.with_host(|_| Ok(())).await.is_err());
        drop(lease);
        // No replacement close is dispatched to make this assertion pass.
        closed(&runtime).await;
        assert!(matches!(
            gate.acquire(gate.revision()),
            Err(LeaseError::Revoked)
        ));
        f.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_export_admission_remains_nonrevoking_until_actual_close() {
    let mut f = fixture().await;
    let runtime = f.app.native_catalog.as_ref().unwrap().clone();
    let gate = gate(&runtime).await;
    let mut terminal = runtime.close_admission_for_stopped_service().await.unwrap();
    assert!(runtime.with_host(|_| Ok(())).await.is_err());
    drop(gate.acquire(gate.revision()).unwrap());
    let destination = f._dir.path().join("export");
    std::fs::create_dir(&destination).unwrap();
    terminal
        .with_export(
            destination.clone(),
            Arc::new(AtomicBool::new(false)),
            |image, _| {
                assert!(image.bytes() > 0);
                assert_eq!(image.sha256().len(), 64);
                Ok(())
            },
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read_dir(destination).unwrap().count(), 0);
    let lease = gate.acquire(gate.revision()).unwrap();
    let mut close = tokio::spawn(terminal.close());
    revoked(&gate).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut close)
            .await
            .is_err()
    );
    assert_eq!(lifecycle(&runtime).await, Lifecycle::Draining);
    drop(lease);
    tokio::time::timeout(Duration::from_secs(2), close)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(lifecycle(&runtime).await, Lifecycle::Closed);
    f.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_shutdown_reaps_actual_scan_after_shared_gate_revocation() {
    let mut f = fixture().await;
    let runtime = f.app.native_catalog.as_ref().unwrap().clone();
    let gate = gate(&runtime).await;
    let binary = observer_binary();
    let helper =
        catabolic_runtime::helper::TrustedClosedObservationArtifact::after_packaging_verification(
            &binary,
            &catabolic_core::sha256(std::fs::read(&binary).unwrap()),
            catabolic_runtime::helper::ClosedObservationProtocol::NoChildWithExactJoinV2,
        )
        .unwrap();
    let source = f._dir.path().join("terminal-scan-source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("one.txt"), b"actual terminal scan").unwrap();
    let handle = runtime
        .with_host(move |host| {
            host.register_source("terminal-scan", &source, &[])?;
            host.register_closed_observation_helper(&helper)?;
            let handle = host.start_scan("terminal-scan", ())?;
            assert_eq!(host.active_scan(), Some(handle.clone()));
            // No poll/wait has run. The real Child and native lease remain owned,
            // even if the short helper happens to have already exited in the OS.
            Ok(handle)
        })
        .await
        .unwrap();
    let first_close = tokio::time::timeout(Duration::from_secs(7), runtime.shutdown())
        .await
        .expect("first shutdown must return within its existing bounded wait");
    if let Err(error) = first_close {
        // Runtime shutdown awaits admitted shared work; unlike service stop,
        // it does not cancel a consumer's scan demand. The documented host
        // drain contract permits exactly this pending refusal, not success or
        // arbitrary errors. No production bound is extended by this fixture.
        assert_eq!(error.to_string(), "native workers did not drain");
        revoked(&gate).await;
        assert_eq!(lifecycle(&runtime).await, Lifecycle::Draining);
        assert!(runtime.with_host(|_| Ok(())).await.is_err());
        let owner = runtime.admission.clone();
        let retained_handle = handle.clone();
        tokio::task::spawn_blocking(move || {
            let mut held = owner.lock().unwrap();
            assert!(held.terminal);
            let host = held.server.admission.admitted_host().unwrap();
            assert_eq!(host.active_scan(), Some(retained_handle));
        })
        .await
        .unwrap();
        let revocation = gate.begin_revocation().unwrap();
        assert_eq!(
            revocation.wait_drained(Duration::ZERO),
            Err(LeaseError::Timeout)
        );

        let path = f._dir.path().join("native.sqlite3");
        tokio::task::spawn_blocking(move || {
            // Scheduling evidence only. This read-only connection neither
            // polls the Child nor adopts a row/PID as cleanup authority. Use
            // the existing source-observation fixture budget, not a longer
            // production close. The retained owner must still join below.
            let started = std::time::Instant::now();
            let store = catabolic_store::Store::open(&path, false, false).unwrap();
            store.db.busy_timeout(Duration::from_millis(50)).unwrap();
            let mut reservation = None;
            loop {
                assert!(
                    started.elapsed() < Duration::from_secs(30),
                    "actual retained scan did not report completion within fixture budget"
                );
                let count: i64 = store
                    .db
                    .query_row(
                        "SELECT count(*) FROM native_observation_companions",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(count, 1);
                let (nonce, state, cleanup, sealed): (String, String, Option<String>, bool) = store
                    .db
                    .query_row(
                        "SELECT reservation_nonce, state, cleanup_kind, sealed_at IS NOT NULL \
                         FROM native_observation_companions WHERE source='terminal-scan'",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                    )
                    .unwrap();
                assert_eq!(reservation.get_or_insert_with(|| nonce.clone()), &nonce);
                assert_eq!(cleanup, None);
                assert!(!sealed, "a completed row cannot seal its own child exit");
                if state == "completed" {
                    break;
                }
                assert!(matches!(state.as_str(), "reserved" | "started"));
                std::thread::sleep(Duration::from_millis(50));
            }
        })
        .await
        .unwrap();
        assert_eq!(lifecycle(&runtime).await, Lifecycle::Draining);
        assert_eq!(
            revocation.wait_drained(Duration::ZERO),
            Err(LeaseError::Timeout),
            "completed durable evidence must not release the retained Child lease"
        );
        // Same Runtime/HostSession/Child, with the same production five-second
        // bound. Only this genuine supervisor wait can permit a clean close.
        tokio::time::timeout(Duration::from_secs(7), runtime.shutdown())
            .await
            .expect("same-owner shutdown retry must remain bounded")
            .expect("pending refusal is not success: actual retry must join and close");
    }
    assert!(matches!(
        gate.acquire(gate.revision()),
        Err(LeaseError::Revoked)
    ));
    assert_eq!(lifecycle(&runtime).await, Lifecycle::Closed);
    let path = f._dir.path().join("native.sqlite3");
    tokio::task::spawn_blocking(move || {
        // Independent read-only evidence, never adopted child or cleanup authority.
        let store = catabolic_store::Store::open(&path, false, false).unwrap();
        let actual: (String, Option<String>) = store
            .db
            .query_row(
                "SELECT state, cleanup_kind FROM native_observation_companions",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            actual,
            ("quiescent".into(), Some("closed_child_exit_v1".into()))
        );
        let inventory = catabolic_store::native_worker_inventory::inspect(&store).unwrap();
        assert!(inventory.admits_new_work());
        assert_eq!(inventory.witnessed_observation, 1);
    })
    .await
    .unwrap();
    f.close().await;
}
