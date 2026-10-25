//! Private bounded descriptor conversion, owned through real process exit.
//!
//! This is process custody, not source or publication authority. The caller
//! supplies an already authorized descriptor and keeps its exact commitments
//! for final current-state validation before publishing any returned bytes.
use crate::execution::{self, Lease, Witness};
use anyhow::{Context, Result};
use std::{ffi::OsString, fs::File, path::PathBuf, process::Stdio, sync::Arc, time::Duration};
use tokio::{io::AsyncReadExt, process::Command, sync::oneshot};
use tokio_util::sync::CancellationToken;

pub(crate) struct Request {
    pub supervisor: PathBuf,
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub input: File,
    /// Unique private owner path, retained for ordinary witness recovery.
    pub witness: PathBuf,
    pub max_stdout: usize,
    pub max_stderr: usize,
    pub deadline: Duration,
}

#[derive(Debug)]
pub(crate) struct Output {
    pub bytes: Vec<u8>,
    /// Still the same open description, for the caller's final fingerprint check.
    pub input: Arc<File>,
}

/// Dropping this waiter closes only the result receiver. The detached owner
/// observes that closure, stops the supervised group, and retains all admission
/// guards until real exit (or the existing owned quarantine) has settled.
pub(crate) async fn run<A: Send + Sync + 'static>(
    request: Request,
    lease: Arc<Lease>,
    admission: Arc<A>,
    stop: CancellationToken,
) -> Result<Output> {
    let (mut reply, result) = oneshot::channel();
    tokio::spawn(async move {
        let outcome = owned(request, lease, admission, stop, &mut reply).await;
        let _ = reply.send(outcome);
    });
    result
        .await
        .context("descriptor conversion owner ended without a result")?
}

#[cfg(not(unix))]
async fn owned<A: Send + Sync + 'static>(
    _request: Request,
    _lease: Arc<Lease>,
    _admission: Arc<A>,
    _stop: CancellationToken,
    _reply: &mut oneshot::Sender<Result<Output>>,
) -> Result<Output> {
    anyhow::bail!("descriptor conversion requires supported Unix descriptor inheritance")
}

#[cfg(unix)]
async fn owned<A: Send + Sync + 'static>(
    request: Request,
    lease: Arc<Lease>,
    admission: Arc<A>,
    stop: CancellationToken,
    reply: &mut oneshot::Sender<Result<Output>>,
) -> Result<Output> {
    anyhow::ensure!(
        request.max_stdout > 0 && request.max_stdout <= 16 * 1024 * 1024,
        "descriptor stdout bound is unsupported"
    );
    anyhow::ensure!(
        request.max_stderr > 0 && request.max_stderr <= 64 * 1024,
        "descriptor stderr bound is unsupported"
    );
    anyhow::ensure!(
        !request.deadline.is_zero() && request.deadline <= Duration::from_secs(120),
        "descriptor deadline is unsupported"
    );
    anyhow::ensure!(
        !reply.is_closed() && !stop.is_cancelled() && !lease.termination_requested().is_cancelled(),
        "descriptor conversion was cancelled before admission"
    );
    let path = request.witness.clone();
    let started_lease = lease.clone();
    let preparing_admission = admission.clone();
    let input = request.input;
    let prepared = execution::prepare(
        lease.clone(),
        &stop,
        Duration::from_secs(5),
        move || {
            let _admission = preparing_admission;
            let (witness, held) = Witness::create(&path)?;
            started_lease.started(&path)?;
            Ok::<_, std::io::Error>((witness, held, input))
        },
        || {},
    )
    .await
    .context("descriptor witness preparation has not settled")???;
    let (witness, held, input) = prepared;
    let input = Arc::new(input);
    // A cancellation during witness preparation must not start a process.
    if reply.is_closed() || stop.is_cancelled() || lease.termination_requested().is_cancelled() {
        drop(held);
        execution::confirm_exit(
            &lease,
            None,
            Some(witness),
            execution::TERMINATION_DEADLINE,
            move || drop((admission, input)),
        )
        .await;
        anyhow::bail!("descriptor conversion was cancelled before spawn");
    }
    let mut command = Command::new(&request.supervisor);
    command
        .arg("--internal-ffmpeg-supervisor")
        .arg(&request.program)
        .args(&request.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);
    // Input is fd3; the execution witness is fd4 and survives in descendants.
    execution::inherit(&mut command, &[input.as_ref(), &held.0]);
    let spawned = command.spawn();
    drop(held);
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            execution::confirm_exit(
                &lease,
                None,
                Some(witness),
                execution::TERMINATION_DEADLINE,
                move || drop((admission, input)),
            )
            .await;
            return Err(error).context("descriptor supervisor did not start");
        }
    };
    let control = child.stdin.take();
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let mut bytes = Vec::new();
    let mut stderr_bytes = 0usize;
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut status = None;
    let deadline = tokio::time::sleep(request.deadline);
    tokio::pin!(deadline);
    let outcome: Result<()> = async {
        let stdout = stdout.as_mut().context("missing descriptor stdout")?;
        let stderr = stderr.as_mut().context("missing descriptor stderr")?;
        anyhow::ensure!(control.is_some(), "missing supervisor control");
        let mut out = [0u8; 4096];
        let mut err = [0u8; 4096];
        loop {
            if status.is_some() && !stdout_open && !stderr_open { break; }
            tokio::select! {
                _ = reply.closed() => anyhow::bail!("descriptor waiter closed"),
                _ = stop.cancelled() => anyhow::bail!("descriptor conversion cancelled"),
                _ = lease.termination_requested().cancelled() => anyhow::bail!("descriptor service draining"),
                _ = &mut deadline => anyhow::bail!("descriptor conversion deadline"),
                observed = child.wait(), if status.is_none() => {
                    let observed = observed.context("descriptor supervisor wait failed")?;
                    anyhow::ensure!(observed.success(), "descriptor converter failed");
                    status = Some(observed);
                },
                read = stdout.read(&mut out), if stdout_open => {
                    let n = read.context("descriptor stdout read failed")?;
                    stdout_open = n != 0;
                    anyhow::ensure!(n <= request.max_stdout.saturating_sub(bytes.len()),
                        "descriptor stdout exceeds bound");
                    bytes.extend_from_slice(&out[..n]);
                },
                read = stderr.read(&mut err), if stderr_open => {
                    let n = read.context("descriptor stderr read failed")?;
                    stderr_open = n != 0;
                    anyhow::ensure!(n <= request.max_stderr.saturating_sub(stderr_bytes),
                        "descriptor stderr exceeds bound");
                    stderr_bytes += n;
                },
            }
        }
        Ok(())
    }.await;
    // EOF requests process-group termination. Never kill the supervisor first:
    // it owns group cleanup, while the witness also fences escaped descendants.
    drop(control);
    drop(stdout);
    drop(stderr);
    let child = status.is_none().then_some(child);
    let retained_input = input.clone();
    let confirmed = execution::confirm_exit(
        &lease,
        child,
        Some(witness),
        execution::TERMINATION_DEADLINE,
        move || drop((admission, retained_input)),
    )
    .await;
    anyhow::ensure!(
        confirmed,
        "descriptor process custody is quarantined pending actual exit"
    );
    outcome?;
    Ok(Output { bytes, input })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use playscale_core::work::{Class, Rejection};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Guard(Arc<AtomicUsize>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn request(dir: &tempfile::TempDir, script: &str) -> Request {
        let supervisor = std::env::var_os("MOTION_TEST_OBSERVER")
            .expect("official runner must supply its verified packaged playscale helper");
        let input = dir.path().join("input");
        std::fs::write(&input, b"exact descriptor bytes\n").unwrap();
        Request {
            supervisor: supervisor.into(),
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                script.into(),
                "descriptor-test".into(),
                dir.path().join("started").into_os_string(),
            ],
            input: File::open(input).unwrap(),
            witness: dir.path().join("owner"),
            max_stdout: 1024,
            max_stderr: 1024,
            deadline: Duration::from_secs(5),
        }
    }
    async fn lease(owner: &Arc<execution::Coordinator>) -> Arc<Lease> {
        Arc::new(
            owner
                .reserve("actual-descriptor-test".into(), Class::Preparation, 1)
                .await
                .unwrap(),
        )
    }
    async fn started(dir: &tempfile::TempDir) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !dir.path().join("started").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("actual supervised process did not start");
    }
    async fn idle(owner: &Arc<execution::Coordinator>) {
        tokio::time::timeout(Duration::from_secs(15), async {
            while owner.snapshot().used != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("process custody did not settle; owner remains reserved");
        assert!(owner.snapshot().stuck.is_empty());
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn actual_supervisor_reads_exact_descriptor_and_bounds_both_streams() {
        for (script, limit, expected) in [
            ("cat /dev/fd/3", 1024, None),
            ("printf 123456789", 8, Some("stdout exceeds bound")),
            ("printf 123456789 >&2", 8, Some("stderr exceeds bound")),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let owner = execution::Coordinator::new(execution::BUDGET);
            let mut req = request(&dir, script);
            req.max_stdout = limit;
            req.max_stderr = limit;
            let dropped = Arc::new(AtomicUsize::new(0));
            let result = run(
                req,
                lease(&owner).await,
                Arc::new(Guard(dropped.clone())),
                CancellationToken::new(),
            )
            .await;
            match expected {
                None => {
                    let output = result.unwrap();
                    assert_eq!(output.bytes, b"exact descriptor bytes\n");
                    assert_eq!(output.input.metadata().unwrap().len(), 23);
                }
                Some(message) => assert!(format!("{:#}", result.unwrap_err()).contains(message)),
            }
            idle(&owner).await;
            assert_eq!(dropped.load(Ordering::SeqCst), 1);
            assert!(Witness::open(&dir.path().join("owner")).unwrap().released());
            owner.close_admission_if_idle().unwrap();
        }
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn caller_abort_retains_guard_until_actual_supervisor_exit() {
        let dir = tempfile::tempdir().unwrap();
        let owner = execution::Coordinator::new(execution::BUDGET);
        let dropped = Arc::new(AtomicUsize::new(0));
        let req = request(
            &dir,
            "echo $$ > \"$1\"; trap '' TERM; while :; do sleep 1; done",
        );
        let task = tokio::spawn(run(
            req,
            lease(&owner).await,
            Arc::new(Guard(dropped.clone())),
            CancellationToken::new(),
        ));
        started(&dir).await;
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        assert!(owner.close_admission_if_idle().is_err());
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        idle(&owner).await;
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert!(Witness::open(&dir.path().join("owner")).unwrap().released());
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn service_drain_waits_for_cancelled_hung_converter_and_denies_new_admission() {
        let dir = tempfile::tempdir().unwrap();
        let owner = execution::Coordinator::new(execution::BUDGET);
        let stop = CancellationToken::new();
        let dropped = Arc::new(AtomicUsize::new(0));
        let req = request(
            &dir,
            "echo $$ > \"$1\"; trap '' TERM; while :; do sleep 1; done",
        );
        let task = tokio::spawn(run(
            req,
            lease(&owner).await,
            Arc::new(Guard(dropped.clone())),
            stop.clone(),
        ));
        started(&dir).await;
        owner.begin_drain();
        assert!(owner.close_admission_if_idle().is_err());
        assert_eq!(
            owner
                .reserve("late".into(), Class::Preparation, 1)
                .await
                .err(),
            Some(Rejection::AdmissionClosed)
        );
        // BeginDrain fences new owners; service cancellation is distinct.
        stop.cancel();
        assert!(task.await.unwrap().is_err());
        idle(&owner).await;
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        owner.close_admission_if_idle().unwrap();
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn deadline_kills_term_ignoring_converter_without_releasing_early() {
        let dir = tempfile::tempdir().unwrap();
        let owner = execution::Coordinator::new(execution::BUDGET);
        let mut req = request(
            &dir,
            "echo $$ > \"$1\"; trap '' TERM; while :; do sleep 1; done",
        );
        req.deadline = Duration::from_millis(200);
        let result = run(
            req,
            lease(&owner).await,
            Arc::new(()),
            CancellationToken::new(),
        )
        .await;
        assert!(format!("{:#}", result.unwrap_err()).contains("deadline"));
        idle(&owner).await;
        assert!(Witness::open(&dir.path().join("owner")).unwrap().released());
    }
}
