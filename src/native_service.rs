//! Actual native service owners and terminal shutdown custody.
//! No caller-supplied futures or join proofs can mint StoppedService.
mod pair;
mod pair_container;
#[cfg(target_os = "linux")]
mod pair_install;
#[cfg(target_os = "linux")]
mod publication_file;
#[cfg(test)]
pub(crate) mod startup_observation;
use crate::{App, native_catalog::terminal::TerminalOwner};
use anyhow::{Context, ensure};
use std::{fs::File, path::PathBuf, sync::atomic::Ordering, time::Duration};
use tokio::{
    net::{TcpListener, UnixListener},
    sync::{OwnedSemaphorePermit, oneshot},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

type Completion = Result<(), String>;
struct Task {
    name: &'static str,
    handle: JoinHandle<anyhow::Result<()>>,
    completion: Option<Completion>,
}
impl Task {
    fn new(name: &'static str, handle: JoinHandle<anyhow::Result<()>>) -> Self {
        Self {
            name,
            handle,
            completion: None,
        }
    }
    async fn wait(&mut self) -> Completion {
        if self.completion.is_none() {
            self.completion = Some(match (&mut self.handle).await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(format!("{}: {e:#}", self.name)),
                Err(e) => Err(format!("{} join: {e}", self.name)),
            });
        }
        self.completion.as_ref().expect("actual join").clone()
    }
    async fn deadline(&mut self, seconds: u64) -> Completion {
        tokio::time::timeout(Duration::from_secs(seconds), self.wait())
            .await
            .unwrap_or_else(|_| Err(format!("{} did not finish", self.name)))
    }
}
struct ServerLock {
    file: File,
    path: PathBuf,
}
impl ServerLock {
    fn new(file: File, path: PathBuf) -> anyhow::Result<Self> {
        let held = Self { file, path };
        held.verify()?;
        fs2::FileExt::try_lock_exclusive(&held.file)
            .context("actual server lock custody required")?;
        Ok(held)
    }
    fn verify(&self) -> anyhow::Result<()> {
        use std::os::unix::fs::MetadataExt;
        let held = self.file.metadata()?;
        let named = std::fs::symlink_metadata(&self.path)?;
        ensure!(
            held.is_file()
                && named.is_file()
                && held.dev() == named.dev()
                && held.ino() == named.ino(),
            "startup server lock identity changed"
        );
        Ok(())
    }
}
/// Read-only evidence that this actual service owner has begun terminal stop.
/// Only `launch_stop` can publish the intent; it precedes admission draining.
#[derive(Clone)]
pub(crate) struct ServiceStopIntent {
    requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
    execution: std::sync::Weak<crate::execution::Coordinator>,
    native: Option<std::sync::Weak<crate::native_catalog::Runtime>>,
}
impl ServiceStopIntent {
    fn new(
        execution: std::sync::Arc<crate::execution::Coordinator>,
        native: Option<&std::sync::Arc<crate::native_catalog::Runtime>>,
    ) -> Self {
        Self {
            requested: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            execution: std::sync::Arc::downgrade(&execution),
            native: native.map(std::sync::Arc::downgrade),
        }
    }
    pub(crate) fn requested_for_native(
        &self,
        native: &std::sync::Arc<crate::native_catalog::Runtime>,
    ) -> bool {
        self.native
            .as_ref()
            .is_some_and(|owner| std::sync::Weak::ptr_eq(owner, &std::sync::Arc::downgrade(native)))
            && self.requested.load(Ordering::Acquire)
    }
    fn request(&self) {
        self.requested.store(true, Ordering::Release);
    }
    pub(crate) fn requested_for(
        &self,
        execution: &std::sync::Arc<crate::execution::Coordinator>,
    ) -> bool {
        std::sync::Weak::ptr_eq(&self.execution, &std::sync::Arc::downgrade(execution))
            && self.requested.load(Ordering::Acquire)
    }
}

struct Owners {
    app: App,
    stop_intent: ServiceStopIntent,
    stop: CancellationToken,
    worker: Task,
    http: Task,
    metrics: Task,
    ingress: Option<Task>,
    // Future actual watch startup registers its owned Task here; every registered
    // owner is included in both bounded and terminal joins below.
    additional: Vec<Task>,
    lock: ServerLock,
    io: Option<OwnedSemaphorePermit>,
    terminal: Option<TerminalOwner>,
}
/// The constructor itself launches the known production owners for this App.
pub struct NativeService {
    owners: Option<Owners>,
    #[cfg(target_os = "linux")]
    restore: pair::RestoreSupervisor,
}
struct StoppedOwners {
    app: App,
    native: Option<TerminalOwner>,
    _io: OwnedSemaphorePermit,
    lock: ServerLock,
}
/// Non-Clone/nonserde proof. Not a backup artifact or restore permission.
pub struct StoppedService {
    owners: Option<StoppedOwners>,
    #[cfg(target_os = "linux")]
    supervised: Option<pair::RestoreProof>,
}

impl NativeService {
    pub fn start(
        app: App,
        assets: Option<PathBuf>,
        listener: TcpListener,
        ingress: Option<UnixListener>,
        server_lock: File,
        stop: CancellationToken,
    ) -> anyhow::Result<Self> {
        ensure!(
            app.native_catalog.is_some(),
            "native service requires retained native owner"
        );
        let lock = ServerLock::new(server_lock, app.storage.root.join("server.lock"))?;
        let router = crate::api::router(app.clone(), assets);
        let metrics_app = app.clone();
        let metrics_stop = stop.clone();
        let metrics = Task::new(
            "metrics",
            tokio::spawn(async move {
                crate::metrics::history::worker(metrics_app, metrics_stop).await
            }),
        );
        app.health.worker_running.store(true, Ordering::Relaxed);
        let stop_intent = ServiceStopIntent::new(
            app.processing.execution.clone(),
            app.native_catalog.as_ref(),
        );
        let preview_stop_intent = stop_intent.clone();
        let processing_stop_intent = stop_intent.clone();
        let worker_app = app.clone();
        let worker_stop = stop.clone();
        let worker = Task::new(
            "native workers",
            tokio::spawn(async move {
                let scan_app = worker_app.clone();
                let scan_stop = worker_stop.clone();
                let scan =
                    tokio::spawn(
                        async move { crate::native_scans::worker(scan_app, scan_stop).await },
                    );
                let processing_app = worker_app.clone();
                let processing_stop = worker_stop.clone();
                let processing = tokio::spawn(async move {
                    crate::processing::service_worker(
                        processing_app,
                        processing_stop,
                        processing_stop_intent,
                    )
                    .await
                });
                let previews_app = worker_app.clone();
                let previews_stop = worker_stop.clone();
                let previews = tokio::spawn(async move {
                    crate::v2::previews::service_worker(
                        previews_app,
                        previews_stop,
                        preview_stop_intent,
                    )
                    .await
                });
                let delivery_app = worker_app.clone();
                let delivery_stop = worker_stop.clone();
                let delivery = tokio::spawn(async move {
                    crate::delivery::worker(delivery_app, delivery_stop).await
                });
                let mail_app = worker_app.clone();
                let mail_stop = worker_stop.clone();
                let mail = tokio::spawn(async move {
                    crate::v2::signin::mail_worker(mail_app, mail_stop).await
                });
                // Recovery and timer admission serialize on the Motion writer:
                // recovery only resets interrupted running/cancelling demands,
                // while the timer creates queued demands. The sole scan worker
                // does not claim any demand until its recovery has completed.
                let schedule_app = worker_app.clone();
                let schedule_stop = worker_stop.clone();
                let schedule = tokio::spawn(async move {
                    crate::native_schedules::worker(schedule_app, schedule_stop).await
                });
                let history_app = worker_app.clone();
                let history_stop = worker_stop.clone();
                let history = tokio::spawn(async move {
                    crate::storage::native_history::worker(history_app, history_stop).await
                });
                let (a, b, c, d, e, f, g) = tokio::join!(
                    worker_join("native scan", scan, &worker_stop),
                    worker_join("processing", processing, &worker_stop),
                    worker_join("delivery", delivery, &worker_stop),
                    worker_join("sign-in mail", mail, &worker_stop),
                    worker_join("native schedule", schedule, &worker_stop),
                    worker_join("preview", previews, &worker_stop),
                    worker_join("application history", history, &worker_stop)
                );
                worker_app
                    .health
                    .worker_running
                    .store(false, Ordering::Relaxed);
                a.and(b).and(c).and(d).and(e).and(f).and(g)
            }),
        );
        let ingress = ingress.map(|socket| {
            let router = router
                .clone()
                .layer(axum::Extension(crate::v2::auth::TrustedIngress));
            let stop = stop.clone();
            Task::new(
                "trusted ingress",
                tokio::spawn(async move {
                    axum::serve(socket, router)
                        .with_graceful_shutdown(stop.cancelled_owned())
                        .await
                        .map_err(Into::into)
                }),
            )
        });
        let http_stop = stop.clone();
        let http = Task::new(
            "HTTP",
            tokio::spawn(async move {
                axum::serve(
                    listener,
                    router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .with_graceful_shutdown(http_stop.cancelled_owned())
                .await
                .map_err(Into::into)
            }),
        );
        Ok(Self {
            #[cfg(target_os = "linux")]
            restore: Default::default(),
            owners: Some(Owners {
                app,
                stop_intent,
                stop,
                worker,
                http,
                metrics,
                ingress,
                additional: Vec::new(),
                lock,
                io: None,
                terminal: None,
            }),
        })
    }
    /// Metrics remain observational during serving. Only worker/HTTP exit is fatal.
    pub async fn wait_for_exit(&mut self) -> anyhow::Result<()> {
        let held = self.owners.as_mut().context("service already stopping")?;
        let result = tokio::select! { r=held.worker.wait()=>r, r=held.http.wait()=>r };
        match result {
            Ok(()) => anyhow::bail!("native service owner stopped unexpectedly"),
            Err(e) => Err(anyhow::anyhow!(e)),
        }
    }
    /// Restore-only borrowed stop. No stopped proof is returned to a request.
    #[cfg(target_os = "linux")]
    async fn stop_for_restore(&mut self) -> anyhow::Result<()> {
        self.restore.stop(&mut self.owners).await
    }
    #[cfg(target_os = "linux")]
    async fn prepare_pair_for_restore(
        &mut self,
        parent: PathBuf,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> anyhow::Result<()> {
        self.restore.prepare_pair(parent, cancel).await
    }
    /// Production-compiled service custody seam only. No user-facing restore
    /// endpoint is activated, and no selected/target owner leaves this service.
    #[cfg(target_os = "linux")]
    fn begin_selected_pair_restore(
        &mut self,
        selected: PathBuf,
        staging: PathBuf,
        deadline: std::time::Instant,
    ) -> anyhow::Result<()> {
        self.restore.begin_selection(selected, staging, deadline)
    }
    #[cfg(target_os = "linux")]
    async fn wait_selected_pair_restore(&self) -> anyhow::Result<()> {
        self.restore.wait().await
    }
    /// Explicit borrowed retirement. Dropping this future begins no target
    /// attempt; original selection handles remain owned until verified joins.
    #[cfg(target_os = "linux")]
    async fn settle_selected_pair_for_target(&mut self) -> anyhow::Result<()> {
        self.restore.settle_selection_for_target().await
    }
    /// Private, synchronous service-owned fresh-target reservation only.
    /// Current SystemAdmin authentication is obtained by the retained worker.
    #[cfg(target_os = "linux")]
    fn begin_selected_target_restore(
        &mut self,
        path: PathBuf,
        caller: crate::v2::auth::Caller,
    ) -> anyhow::Result<()> {
        self.restore.begin_target(path, caller)
    }
    /// No stage begins until the original target task and timer are joined.
    #[cfg(target_os = "linux")]
    async fn settle_selected_target_for_staging(&mut self) -> anyhow::Result<()> {
        self.restore.settle_target_for_staging().await
    }
    /// Production-compiled finite stage seam only, not a public restore route.
    #[cfg(target_os = "linux")]
    fn begin_selected_staging_restore(&mut self) -> anyhow::Result<()> {
        self.restore.begin_staging()
    }
    /// Private finite recorded93→97 bridge. No native96/paired95 or install.
    #[cfg(target_os = "linux")]
    async fn settle_selected_staging_for_control97(&mut self) -> anyhow::Result<()> {
        self.restore.settle_staging_for_control97().await
    }
    #[cfg(target_os = "linux")]
    fn begin_selected_control97_restore(&mut self) -> anyhow::Result<()> {
        self.restore.begin_control97()
    }
    #[cfg(target_os = "linux")]
    async fn settle_selected_control97_for_native96(&mut self) -> anyhow::Result<()> {
        self.restore.settle_control97_for_native96().await
    }
    #[cfg(target_os = "linux")]
    fn begin_selected_native96_restore(&mut self) -> anyhow::Result<()> {
        self.restore.begin_native96()
    }
    #[cfg(target_os = "linux")]
    async fn settle_selected_native96_for_source96(&mut self) -> anyhow::Result<()> {
        self.restore.settle_native96_for_source96().await
    }
    #[cfg(target_os = "linux")]
    fn begin_selected_source96_restore(&mut self) -> anyhow::Result<()> {
        self.restore.begin_source96()
    }

    #[cfg(target_os = "linux")]
    async fn settle_selected_source96_for_relocation94(&mut self) -> anyhow::Result<()> {
        self.restore.settle_source96_for_relocation94().await
    }
    #[cfg(target_os = "linux")]
    async fn settle_selected_relocation94_for_reconcile(&mut self) -> anyhow::Result<()> {
        self.restore.settle_relocation94_for_reconcile().await
    }
    #[cfg(target_os = "linux")]
    async fn settle_selected_relocation94_for_capture_input(&mut self) -> anyhow::Result<()> {
        self.restore.settle_relocation94_for_capture_input().await
    }
    #[cfg(target_os = "linux")]
    fn begin_selected_capture_acquisition(&mut self) -> anyhow::Result<()> {
        self.restore.begin_source94_capture_acquisition()
    }
    #[cfg(target_os = "linux")]
    fn settle_selected_capture_acquisition(&mut self) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        self.restore.settle_capture_acquisition()
    }
    #[cfg(target_os = "linux")]
    fn begin_selected_native_capture(&mut self) -> anyhow::Result<()> {
        self.restore.begin_source94_native_capture()
    }
    #[cfg(target_os = "linux")]
    fn settle_selected_native_capture(&mut self) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        self.restore.settle_native_capture()
    }
    #[cfg(target_os = "linux")]
    fn begin_selected_paired_capture(&mut self) -> anyhow::Result<()> {
        self.restore.begin_source94_paired_capture()
    }
    #[cfg(target_os = "linux")]
    fn settle_selected_paired_capture(&mut self) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        self.restore.settle_paired_capture()
    }
    #[cfg(target_os = "linux")]
    fn settle_selected_capture_input_for_ingress(&mut self) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        self.restore.settle_capture_input_for_ingress()
    }
    #[cfg(target_os = "linux")]
    fn retain_selected_source94_capture_ingress(&mut self) -> anyhow::Result<()> {
        self.restore.retain_source94_capture_ingress()
    }
    #[cfg(target_os = "linux")]
    fn begin_selected_source94_capture_input(&mut self) -> anyhow::Result<()> {
        self.restore.begin_source94_capture_input()
    }
    #[cfg(target_os = "linux")]
    fn begin_selected_relocation94_restore(
        &mut self,
        request: motion_catalog_backend::SourceRelocation94Request,
    ) -> anyhow::Result<()> {
        self.restore.begin_relocation94(request)
    }
    #[cfg(target_os = "linux")]
    fn begin_selected_relocation94_to_receipt(
        &mut self,
        request: motion_catalog_backend::SourceRelocation94Request,
    ) -> anyhow::Result<()> {
        self.restore.begin_relocation94_to_receipt(request)
    }
    #[cfg(target_os = "linux")]
    fn begin_selected_reconcile94_restore(&mut self) -> anyhow::Result<()> {
        self.restore.begin_reconcile94()
    }

    #[cfg(target_os = "linux")]
    async fn close_restore(&mut self) -> anyhow::Result<()> {
        self.restore.close().await
    }

    pub async fn stop(mut self) -> anyhow::Result<StoppedService> {
        let owners = self.owners.take().context("service already stopping")?;
        launch_stop(owners)
            .await
            .context("native shutdown coordinator failed")?
    }
}
async fn worker_join(
    name: &'static str,
    handle: JoinHandle<anyhow::Result<()>>,
    stop: &CancellationToken,
) -> anyhow::Result<()> {
    let result = match handle.await {
        Ok(result) => result,
        Err(e) => Err(anyhow::anyhow!("worker join: {e}")),
    };
    if let Err(error) = &result {
        tracing::error!(worker = name, error = %format!("{error:#}"), "native worker failed; requesting shared stop");
        stop.cancel();
    }
    result
}
impl Drop for NativeService {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        if self.restore.active() {
            // Unactivated restore service-owner Drop is not asynchronous close.
            // Do not launch a new Drop task or invoke legacy global retention.
            // Explicit close and service-owner lifetime remain activation gates.
            return;
        }
        if let Some(owners) = self.owners.take() {
            drop(launch_stop(owners));
        }
    }
}
fn launch_stop(mut owners: Owners) -> oneshot::Receiver<anyhow::Result<StoppedService>> {
    owners.stop_intent.request();
    // Linearize refusal before any await/spawn, including failed cleanup. These
    // transitions preserve leases and waiters and do not certify an idle drain.
    owners.app.processing.execution.begin_drain();
    owners.app.processing.hardware.begin_drain();
    if let Some(runtime) = &owners.app.native_catalog {
        runtime.begin_drain();
    }
    owners
        .app
        .health
        .shutting_down
        .store(true, Ordering::Relaxed);
    owners.stop.cancel();
    let (send, recv) = oneshot::channel();
    let runtime = match tokio::runtime::Handle::try_current() {
        Ok(runtime) => runtime,
        Err(error) => {
            let error = anyhow::anyhow!("shutdown runtime unavailable: {error}");
            quarantine(owners, &error);
            let _ = send.send(Err(error));
            return recv;
        }
    };
    let mut custody = Retained::new(owners);
    runtime.spawn(async move {
        let owners = custody.get_mut();
        let (worker, http, metrics, ingress, additional) = tokio::join!(
            owners.worker.deadline(120),
            owners.http.deadline(5),
            owners.metrics.deadline(2),
            async {
                match owners.ingress.as_mut() {
                    Some(t) => t.deadline(5).await,
                    None => Ok(()),
                }
            },
            async {
                let mut result = Ok(());
                for task in &mut owners.additional {
                    if let Err(e) = task.deadline(120).await {
                        result = Err(e);
                    }
                }
                result
            }
        );
        let result = worker.and(http).and(metrics).and(ingress).and(additional);
        if let Err(error) = result {
            let _ = send.send(Err(anyhow::anyhow!(error)));
            // The caller has a refusal, but timeout did not end ownership. Keep
            // the server lock/App/tasks until every actual owner really exits.
            let _ = tokio::join!(
                owners.worker.wait(),
                owners.http.wait(),
                owners.metrics.wait(),
                async {
                    if let Some(t) = owners.ingress.as_mut() {
                        let _ = t.wait().await;
                    }
                },
                async {
                    for t in &mut owners.additional {
                        let _ = t.wait().await;
                    }
                }
            );
            let cleanup = async {
                owners.io = Some(
                    owners
                        .app
                        .processing
                        .io
                        .clone()
                        .try_acquire_owned()
                        .context("failed shutdown retains active Motion I/O")?,
                );
                owners.app.processing.execution.close_admission_if_idle()?;
                owners.app.processing.hardware.close_admission_if_idle()?;
                owners
                    .app
                    .native_catalog
                    .as_ref()
                    .context("retained owner missing")?
                    .shutdown()
                    .await?;
                Ok::<(), anyhow::Error>(())
            }
            .await;
            match cleanup {
                Err(error) => custody.fail(&error),
                Ok(()) => drop(custody.complete()),
            }
            return;
        }
        let stopped = finish_stop(custody).await;
        if let Err(undelivered) = send.send(stopped) {
            if let Ok(stopped) = undelivered {
                let _ = stopped.close().await;
            }
        }
    });
    recv
}
async fn finish_stop(mut custody: Retained<Owners>) -> anyhow::Result<StoppedService> {
    let owners = custody.get_mut();
    let result = async {
        owners.terminal = Some(
            owners
                .app
                .native_catalog
                .as_ref()
                .context("retained owner missing")?
                .close_admission_for_stopped_service()
                .await?,
        );
        owners.lock.verify()?;
        owners.io = Some(
            owners
                .app
                .processing
                .io
                .clone()
                .try_acquire_owned()
                .context("Motion I/O ownership remains active")?,
        );
        owners.app.processing.execution.close_admission_if_idle()?;
        owners.app.processing.hardware.close_admission_if_idle()?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    match result {
        Ok(()) => {
            let mut owners = custody.complete();
            Ok(StoppedService {
                #[cfg(target_os = "linux")]
                supervised: None,
                owners: Some(StoppedOwners {
                    app: owners.app,
                    native: owners.terminal.take(),
                    _io: owners.io.take().expect("acquired before stopped proof"),
                    lock: owners.lock,
                }),
            })
        }
        Err(error) => {
            custody.fail(&error);
            Err(error)
        }
    }
}
/// Cancellation/panic of the coordinator itself also cannot release custody.
struct Retained<T: Send + 'static>(Option<T>);
impl<T: Send + 'static> Retained<T> {
    fn new(value: T) -> Self {
        Self(Some(value))
    }
    fn get_mut(&mut self) -> &mut T {
        self.0.as_mut().expect("retained custody")
    }
    fn complete(mut self) -> T {
        self.0.take().expect("retained custody")
    }
    fn fail(mut self, error: &anyhow::Error) {
        if let Some(value) = self.0.take() {
            quarantine(value, error);
        }
    }
}
impl<T: Send + 'static> Drop for Retained<T> {
    fn drop(&mut self) {
        if let Some(value) = self.0.take() {
            quarantine(
                value,
                &anyhow::anyhow!("terminal cleanup coordinator dropped before actual completion"),
            );
        }
    }
}
/// No clean shutdown or artifact is claimed on this terminal failure. Retain
/// all unresolved custody until OS process exit rather than reopen admission or
/// release the server lock while native/physical ownership is unproven.
struct TerminalFailure {
    _reason: String,
    _custody: Box<dyn std::any::Any + Send>,
}
static TERMINAL_FAILURES: std::sync::OnceLock<std::sync::Mutex<Vec<TerminalFailure>>> =
    std::sync::OnceLock::new();
fn quarantine<T: Send + 'static>(owners: T, error: &anyhow::Error) {
    let reason = format!("{error:#}");
    tracing::error!(%reason, "terminal service custody quarantined until process exit; explicit restart recovery required");
    // Named process-lifetime state, not an anonymous forgotten allocation. It
    // has no release/retry API and cannot produce StoppedService or an artifact.
    TERMINAL_FAILURES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .push(TerminalFailure {
            _reason: reason,
            _custody: Box::new(owners),
        });
}

fn stopped_source_parents(owners: &StoppedOwners) -> anyhow::Result<(File, File)> {
    owners.lock.verify()?;
    let state = pair::open_parent(
        owners
            .lock
            .path
            .parent()
            .context("source state parent missing")?,
    )?;
    let native = owners
        .app
        .native_catalog
        .as_ref()
        .context("source native owner missing")?;
    let native = pair::open_parent(
        native
            .path()
            .parent()
            .context("source native parent missing")?,
    )?;
    Ok((state, native))
}

impl StoppedService {
    fn verify_lock(&self) -> anyhow::Result<()> {
        if let Some(owners) = &self.owners {
            return owners.lock.verify();
        }
        #[cfg(target_os = "linux")]
        if let Some(proof) = &self.supervised {
            return proof.verify_lock();
        }
        anyhow::bail!("stopped custody missing")
    }
    fn source_parents(&self) -> anyhow::Result<(File, File)> {
        if let Some(owners) = &self.owners {
            return stopped_source_parents(owners);
        }
        #[cfg(target_os = "linux")]
        if let Some(proof) = &self.supervised {
            return proof.source_parents();
        }
        anyhow::bail!("stopped source custody missing")
    }
    fn source_db(&self) -> anyhow::Result<sqlx::SqlitePool> {
        if let Some(owners) = &self.owners {
            return Ok(owners.app.db.clone());
        }
        #[cfg(target_os = "linux")]
        if let Some(proof) = &self.supervised {
            return proof.source_db();
        }
        anyhow::bail!("stopped source custody missing")
    }
    fn retained_restore_storage(&self) -> anyhow::Result<u64> {
        #[cfg(target_os = "linux")]
        if let Some(proof) = &self.supervised {
            return proof.retained_storage();
        }
        Ok(0)
    }
}

impl StoppedService {
    pub async fn close(mut self) -> anyhow::Result<()> {
        let owners = self
            .owners
            .take()
            .context("stopped service already consumed")?;
        tokio::spawn(close_stopped(Retained::new(owners)))
            .await
            .context("terminal close coordinator failed")?
    }
}
async fn close_stopped(mut custody: Retained<StoppedOwners>) -> anyhow::Result<()> {
    let owners = custody.get_mut();
    let result = match owners.native.take() {
        Some(native) => native.close().await,
        None => Err(anyhow::anyhow!("terminal owner missing")),
    };
    if let Err(error) = result {
        custody.fail(&error);
        return Err(error);
    }
    // Preserve actual startup lock and I/O permit through native and DB close.
    owners.app.db.close().await;
    drop(custody.complete());
    Ok(())
}
impl Drop for StoppedService {
    fn drop(&mut self) {
        if let Some(owners) = self.owners.take() {
            match tokio::runtime::Handle::try_current() {
                Ok(runtime) => {
                    runtime.spawn(close_stopped(Retained::new(owners)));
                }
                Err(error) => quarantine(
                    owners,
                    &anyhow::anyhow!("terminal close runtime unavailable: {error}"),
                ),
            }
        }
    }
}

#[cfg(test)]
use crate as playscale_app;
#[cfg(test)]
#[path = "../tests/support/native_application_fixture.rs"]
mod recovery_stop_fixture;

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn actual_native_service_runs_application_retention_and_joins_it() {
        let mut fixture = recovery_stop_fixture::Fixture::new().await;
        fixture.cutover().await;
        let lock = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(fixture.app.storage.root.join("server.lock"))
            .unwrap();
        fs2::FileExt::try_lock_exclusive(&lock).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let stop = CancellationToken::new();
        let service = NativeService::start(
            fixture.app.clone(),
            None,
            listener,
            None,
            lock,
            stop.clone(),
        )
        .unwrap();
        // Exercise the actual production 30-second cadence, not a test worker or
        // synthetic clock that also changes native supervisor deadlines.
        let observed = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let done: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM maintenance_state WHERE name='history' AND last_success IS NOT NULL AND error IS NULL)")
                    .fetch_one(&fixture.app.db).await?;
                if done { break; }
                anyhow::ensure!(!stop.is_cancelled(), "service stopped before actual history maintenance");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Ok::<(), anyhow::Error>(())
        }).await;
        let closed = service.stop().await;
        if let Ok(stopped) = closed {
            stopped.close().await.unwrap();
        } else {
            panic!(
                "native service retention shutdown failed: {}",
                closed.err().unwrap()
            );
        }
        observed
            .expect("actual NativeService did not run application retention")
            .unwrap();
        assert!(fixture.app.db.is_closed());
        assert!(
            fixture
                .app
                .processing
                .io
                .clone()
                .try_acquire_owned()
                .is_ok()
        );
    }

    #[tokio::test]
    async fn recovery_service_stop_intent_requires_exact_runtime_before_cancel() {
        use recovery_stop_fixture::Fixture;
        let mut current = Fixture::new().await;
        current.cutover().await;
        let mut foreign = Fixture::new().await;
        foreign.cutover().await;
        let runtime = current.app.native_catalog.as_ref().unwrap();
        let intent =
            ServiceStopIntent::new(current.app.processing.execution.clone(), Some(runtime));
        let foreign_intent = ServiceStopIntent::new(
            foreign.app.processing.execution.clone(),
            foreign.app.native_catalog.as_ref(),
        );
        foreign_intent.request();
        runtime.begin_drain();
        let stop = CancellationToken::new();
        // Real terminal gate closure is insufficient without this exact service
        // owner's published intent. Neither absence nor foreign identity grants it.
        for proof in [None, Some(&intent), Some(&foreign_intent)] {
            let error = crate::native_processing::recover_reserved_for_service(
                &current.app,
                stop.clone(),
                proof,
            )
            .await
            .unwrap_err();
            assert!(
                error
                    .downcast_ref::<crate::native_catalog::OrdinaryAdmissionDraining>()
                    .is_some()
            );
            assert!(!stop.is_cancelled());
        }
        intent.request();
        // Run the actual service entry so returning from recovery cannot start
        // ordinary processing intake before the cancellation token is published.
        crate::processing::service_worker(current.app.clone(), stop.clone(), intent)
            .await
            .unwrap();
        assert!(!stop.is_cancelled());
        let tx = current.app.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        tx.rollback().await.unwrap();
        current.close().await;
        foreign.close().await;
    }

    #[tokio::test]
    async fn preview_service_stop_intent_distinguishes_drain_before_cancel_from_other_refusals() {
        use crate::{execution::Coordinator, v2::previews::reserve_execution};
        use playscale_core::work::{Budget, Class, Rejection};
        let budget = Budget {
            units: 1,
            interactive_reserve: 0,
        };
        let coordinator = Coordinator::new(budget);
        let held = coordinator
            .reserve("actual-held-owner".into(), Class::Preparation, 1)
            .await
            .unwrap();
        let stop = CancellationToken::new();
        let intent = ServiceStopIntent::new(coordinator.clone(), None);
        // Exactly the service owner's ordering, with cancellation deliberately
        // not published. No timing window, sleep, or synthetic refusal result.
        intent.request();
        coordinator.begin_drain();
        assert!(!stop.is_cancelled());
        assert!(
            reserve_execution(&coordinator, &stop, Some(&intent), 1)
                .await
                .unwrap()
                .is_none()
        );
        assert!(!stop.is_cancelled());
        assert_eq!(coordinator.snapshot().used, 1);
        assert_eq!(coordinator.snapshot().waiting, 0);
        assert!(coordinator.close_admission_if_idle().is_err());
        drop(held);
        coordinator.close_admission_if_idle().unwrap();

        // A closed standalone coordinator is not evidence that NativeService
        // requested stop, even though its typed rejection is identical.
        let standalone = Coordinator::new(budget);
        standalone.close_admission_if_idle().unwrap();
        let unrequested = ServiceStopIntent::new(standalone.clone(), None);
        for proof in [None, Some(&unrequested), Some(&intent)] {
            assert_eq!(
                reserve_execution(&standalone, &stop, proof, 1).await.err(),
                Some(Rejection::AdmissionClosed)
            );
        }
        // Genuine service intent never hides an unrelated admission defect.
        let open = Coordinator::new(budget);
        let open_intent = ServiceStopIntent::new(open.clone(), None);
        open_intent.request();
        assert_eq!(
            reserve_execution(&open, &stop, Some(&open_intent), 2)
                .await
                .err(),
            Some(Rejection::ExceedsBudget)
        );
        assert_eq!(open.snapshot().used, 0);
        assert_eq!(open.snapshot().waiting, 0);
    }

    #[tokio::test]
    async fn worker_error_and_panic_cancel_but_wait_for_real_sibling_exit() {
        for panic in [false, true] {
            let stop = CancellationToken::new();
            let sibling_stop = stop.clone();
            let (release, wait) = oneshot::channel();
            let sibling = tokio::spawn(async move {
                sibling_stop.cancelled().await;
                wait.await?;
                Ok(())
            });
            let failure = tokio::spawn(async move {
                if panic {
                    panic!("owned worker failure");
                }
                anyhow::bail!("worker error")
            });
            let hold = stop.clone();
            let mut joined = tokio::spawn(async move {
                tokio::join!(
                    worker_join("test failure", failure, &hold),
                    worker_join("test sibling", sibling, &hold)
                )
            });
            stop.cancelled().await;
            assert!(
                tokio::time::timeout(Duration::from_millis(1), &mut joined)
                    .await
                    .is_err()
            );
            release.send(()).unwrap();
            let (first, second) = joined.await.unwrap();
            assert!(first.is_err());
            assert!(second.is_ok());
        }
    }
    #[tokio::test]
    async fn timeout_and_consumed_select_never_fabricate_terminal_completion() {
        let (release, wait) = oneshot::channel();
        let mut task = Task::new(
            "delayed",
            tokio::spawn(async move {
                wait.await?;
                Ok(())
            }),
        );
        assert!(task.deadline(0).await.is_err());
        assert!(task.completion.is_none());
        release.send(()).unwrap();
        task.wait().await.unwrap();
        task.deadline(0).await.unwrap();
    }
    #[tokio::test]
    async fn explicit_terminal_quarantine_retains_lock_and_io_while_normal_drop_releases() {
        struct Custody {
            _lock: ServerLock,
            _io: OwnedSemaphorePermit,
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.lock");
        let file = File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let held = ServerLock::new(file, path.clone()).unwrap();
        let io = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let custody = Custody {
            _lock: held,
            _io: io.clone().acquire_owned().await.unwrap(),
        };
        let competing = File::options().read(true).write(true).open(&path).unwrap();
        assert!(fs2::FileExt::try_lock_exclusive(&competing).is_err());
        drop(custody);
        fs2::FileExt::try_lock_exclusive(&competing).unwrap();
        fs2::FileExt::unlock(&competing).unwrap();
        assert_eq!(io.available_permits(), 1);
        let held = ServerLock::new(competing, path.clone()).unwrap();
        quarantine(
            Custody {
                _lock: held,
                _io: io.clone().acquire_owned().await.unwrap(),
            },
            &anyhow::anyhow!("deterministic unresolved close"),
        );
        let competing = File::options().read(true).write(true).open(path).unwrap();
        assert!(fs2::FileExt::try_lock_exclusive(&competing).is_err());
        assert_eq!(io.available_permits(), 0);
    }
    #[tokio::test]
    async fn caller_abort_does_not_release_owned_cleanup_lock_before_real_exit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.lock");
        let file = File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let held = ServerLock::new(file, path.clone()).unwrap();
        let (release, wait) = oneshot::channel();
        let (started, ready) = oneshot::channel();
        let (done, received) = oneshot::channel();
        let coordinator = tokio::spawn(async move {
            let _ = started.send(());
            wait.await.unwrap();
            drop(held);
            let _ = done.send(());
        });
        let caller = tokio::spawn(async move {
            coordinator.await.unwrap();
        });
        ready.await.unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        let competing = File::options().read(true).write(true).open(path).unwrap();
        assert!(fs2::FileExt::try_lock_exclusive(&competing).is_err());
        release.send(()).unwrap();
        received.await.unwrap();
        fs2::FileExt::try_lock_exclusive(&competing).unwrap();
    }
    #[tokio::test]
    async fn real_ingress_pending_request_must_finish_before_join() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let entered = std::sync::Arc::new(tokio::sync::Notify::new());
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
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
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let stop = CancellationToken::new();
        let halt = stop.clone();
        let mut task = Task::new(
            "actual HTTP",
            tokio::spawn(async move {
                axum::serve(listener, router)
                    .with_graceful_shutdown(halt.cancelled_owned())
                    .await
                    .map_err(Into::into)
            }),
        );
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        entered.notified().await;
        stop.cancel();
        assert!(task.deadline(0).await.is_err());
        release.notify_one();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert!(String::from_utf8(response).unwrap().ends_with("finished"));
        task.deadline(1).await.unwrap();
    }
    #[tokio::test]
    async fn aborting_cleanup_coordinator_quarantines_its_actual_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.lock");
        let file = File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let custody = Retained::new(ServerLock::new(file, path.clone()).unwrap());
        let (started, ready) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _custody = custody;
            let _ = started.send(());
            std::future::pending::<()>().await;
        });
        ready.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let competing = File::options().read(true).write(true).open(path).unwrap();
        assert!(fs2::FileExt::try_lock_exclusive(&competing).is_err());
    }
}

/// Refuse interrupted fresh-target paired installation before any DB owner opens.
#[cfg(target_os = "linux")]
pub fn require_complete_pair_install(
    data_dir: &std::path::Path,
    native: &std::path::Path,
    lock: &std::fs::File,
) -> anyhow::Result<()> {
    pair_install::startup_guard(data_dir, native, lock)
}

#[cfg(not(target_os = "linux"))]
pub fn require_complete_pair_install(
    data_dir: &std::path::Path,
    _native: &std::path::Path,
    _lock: &std::fs::File,
) -> anyhow::Result<()> {
    for leaf in [
        "paired-install.reservation.json",
        "paired-install.intent.json",
        "paired-install.motion.json",
        "paired-install.native.json",
        "paired-install.complete.json",
    ] {
        match std::fs::symlink_metadata(data_dir.join(leaf)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
            Ok(_) => {
                anyhow::bail!("paired installation requires explicit supported-platform recovery")
            }
        }
    }
    Ok(())
}
