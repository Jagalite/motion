mod native_bootstrap;
mod service_shutdown;
use anyhow::Context;
use clap::Parser;
use playscale::{App, db, scan};
use std::{io::Write, sync::Arc};
use tokio::sync::{Mutex, Semaphore};
use tokio_util::sync::CancellationToken;

use playscale::config::Args;

fn token(path: &std::path::Path) -> anyhow::Result<String> {
    if path.exists() {
        let value = std::fs::read_to_string(path)?;
        anyhow::ensure!(value.trim().len() >= 32, "invalid admin token file");
        return Ok(value.trim().into());
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    let value = format!("{}{}", playscale::new_id(), playscale::new_id());
    file.write_all(value.as_bytes())?;
    file.sync_all()?;
    Ok(value)
}

fn main() -> anyhow::Result<()> {
    if let Some(code) = motion_catalog_backend::observer::dispatch_builtin_invocation() {
        std::process::exit(code);
    }
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new("--internal-ffmpeg-supervisor"))
    {
        std::process::exit(playscale::processing::supervise_encoder(
            std::env::args_os().skip(2),
        )?);
    }
    execute(run())
}

fn execute(work: impl std::future::Future<Output = anyhow::Result<()>>) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(work);
    // A stalled filesystem call cannot be cancelled. Do not let Tokio's blocking
    // pool keep a stopped service alive indefinitely after HTTP/DB shutdown.
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    result
}

async fn run() -> anyhow::Result<()> {
    let cli = Args::parse();
    if let Some(fd) = cli.ready_fd {
        anyhow::ensure!(
            fd > 2 && Some(fd) != cli.bootstrap_fd,
            "readiness requires a distinct private descriptor"
        );
        anyhow::ensure!(
            cli.bootstrap_fd.is_some(),
            "native readiness requires protected bootstrap"
        );
    }
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let defaults =
        playscale::config::packaged_defaults(&std::env::current_exe()?, home.as_deref())?;
    let args = cli.resolve_with_defaults(defaults)?;
    if cli.check_config {
        println!("{}", serde_json::to_string_pretty(&args)?);
        return Ok(());
    }
    anyhow::ensure!(
        args.listen.ip().is_loopback(),
        "Playscale must bind to loopback; expose it through Tailscale Serve"
    );
    if cli.native_schema_upgrade.is_some() {
        anyhow::ensure!(
            args.data_dir.is_dir(),
            "offline native upgrade requires an existing data directory"
        );
    }
    tokio::fs::create_dir_all(&args.data_dir).await?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(args.data_dir.join("server.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock)
        .map_err(|_| anyhow::anyhow!("another server owns this data directory"))?;
    playscale::native_service::require_complete_pair_install(
        &args.data_dir,
        &args
            .catalog_path
            .clone()
            .unwrap_or_else(|| args.data_dir.join("catabolic.sqlite3")),
        &lock,
    )?;
    if let Some(operation) = cli.native_schema_upgrade.as_deref() {
        anyhow::ensure!(
            cli.confirm_native_upgrade_quiescence,
            "explicit offline native upgrade confirmation is required"
        );
        let motion = args.data_dir.join("playscale.sqlite3");
        let native = args
            .catalog_path
            .clone()
            .unwrap_or_else(|| args.data_dir.join("catabolic.sqlite3"));
        anyhow::ensure!(
            motion.is_file() && native.is_file(),
            "offline upgrade never initializes missing catalogs"
        );
        // Only the reviewed sealed Motion control migrations run here. No
        // bootstrap, listener, retained native session or background task starts.
        let control = db::connect_catalog_control(&motion).await?;
        control.close().await;
        let operation = operation.to_owned();
        let result = tokio::task::spawn_blocking(move || {
            let _server_lock = lock;
            let quiescence = catabolic_store::migration::NativeUpgradeQuiescence {
                admission_stopped: true,
                readers_and_workers_stopped: true,
                exclusive_operational_ownership: true,
            };
            match operation.as_str() {
                "prepare" => {
                    motion_catalog_backend::native_upgrade::prepare(&motion, &native, quiescence)
                }
                "execute" => {
                    motion_catalog_backend::native_upgrade::execute(&motion, &native, quiescence)
                }
                _ => anyhow::bail!("unknown offline native upgrade operation"),
            }
        })
        .await??;
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    let log = playscale::storage::RollingLog::new(
        args.data_dir.join("logs"),
        args.storage.log_bytes,
        args.storage.log_files,
    )?;
    let (writer, _log_guard) = tracing_appender::non_blocking(log);
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(writer)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "playscale=info,tower_http=info".into()),
        )
        .init();
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    let ingress = match &args.api.trusted_ingress {
        Some(settings) => Some(playscale::v2::ingress_listener(&settings.socket)?),
        None => None,
    };
    let address = listener.local_addr()?;
    let origin = playscale::config::canonical_origin(
        &args
            .public_origin
            .unwrap_or_else(|| format!("http://{address}")),
    )?;
    let uri: axum::http::Uri = origin.parse()?;
    let authority = uri
        .authority()
        .expect("validated origin")
        .as_str()
        .to_owned();
    let motion_path = args.data_dir.join("playscale.sqlite3");
    // Native is the default owner. Existing legacy databases require explicit
    // offline migration; configuration removal can never restart legacy writers.
    let path = args
        .catalog_path
        .clone()
        .unwrap_or_else(|| args.data_dir.join("catabolic.sqlite3"));
    anyhow::ensure!(
        args.libraries.is_empty(),
        "native catalog sources must be registered through native source APIs; legacy --library startup scanning is disabled"
    );
    let fresh_intent = native_bootstrap::prepare(&motion_path, &path).await?;
    let db = if fresh_intent {
        db::connect_native_bootstrap(&motion_path).await?
    } else {
        db::connect_catalog_control(&motion_path).await?
    };
    if fresh_intent {
        bootstrap_empty_native(db.clone(), path.clone()).await?;
    } else {
        let native_bound: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM catabolic_binding)")
                .fetch_one(&db)
                .await?;
        anyhow::ensure!(
            native_bound,
            "existing Motion data requires explicit offline Catabolic migration; legacy fallback is forbidden"
        );
        anyhow::ensure!(
            path.try_exists()?,
            "native catalog is missing; explicit native catalog configuration is required and legacy fallback is forbidden"
        );
    }
    let native =
        playscale::native_catalog::Runtime::open(db.clone(), path, args.catalog_profile.clone())
            .await?;
    // Only this executable owns the embedded helper dispatcher.
    native
        .register_observer(motion_catalog_backend::observer::packaged_current_executable()?)
        .await?;
    native.require_serving_routes().await?;
    let native_catalog = Some(Arc::new(native));
    let token_path = args.data_dir.join("admin-token");
    let assets = args
        .demuxe_dir
        .join("web/generated/player/index.js")
        .is_file()
        .then_some(args.demuxe_dir.clone());
    if cli.ready_fd.is_some() {
        playscale::desktop::verify_player_assets(&args.demuxe_dir)?;
    }
    let app = App {
        health: Arc::new(playscale::operations::Health::new(assets.is_some())),
        db,
        native_catalog,
        admin_token: Arc::new(token(&token_path)?),
        origin: Arc::new(origin.clone()),
        authority: Arc::new(authority),
        ffprobe: Arc::new(args.ffprobe),
        jobs: Arc::new(Mutex::new(())),
        streams: Arc::new(Semaphore::new(16)),
        event_streams: Arc::new(Semaphore::new(32)),
        storage: Arc::new(playscale::storage::Runtime::new(
            tokio::fs::canonicalize(&args.data_dir).await?,
            args.storage,
        )),
        processing: Arc::new(playscale::processing::Runtime::new(
            tokio::fs::canonicalize(&args.data_dir)
                .await?
                .join("generated"),
            args.processing,
        )),
        access: Arc::new(
            playscale::v2::Runtime::new(
                args.access_mode,
                playscale::v2::auth::load_or_create_key(&args.data_dir.join("credential-key"))?,
            )
            .with_settings(args.api.clone()),
        ),
    };
    if let Some(fd) = cli.bootstrap_fd {
        app.access.set_bootstrap(&read_bootstrap(fd)?)?;
    }
    // Native receipt recovery and projection ACK have already completed in
    // Runtime::open. Adopt actual encoder witnesses before serving or claiming
    // new work; native recovery never invokes legacy catalog publication.
    playscale::processing::recover(&app).await?;
    playscale::delivery::recover(&app).await?;
    if assets.is_none() {
        tracing::warn!(
            "Demuxe assets absent; run scripts/install_demuxe.py before browser playback"
        );
    }
    let router = playscale::api::router(app.clone(), assets.clone());
    // Install the shutdown handlers before anything serves. Until a handler is
    // registered, SIGINT/SIGTERM still have their default action and would kill
    // a server that is already answering requests, skipping graceful shutdown.
    let shutdown_requested = shutdown_signal()?;
    let shutdown = CancellationToken::new();
    if app.native_catalog.is_some() {
        // Library-owned construction launches only the known real native roles;
        // main cannot substitute caller-created successful task proofs.
        let mut service = playscale::native_service::NativeService::start(
            app.clone(),
            assets,
            listener,
            ingress,
            lock,
            shutdown.clone(),
        )?;
        announce_started(&app, &cli, &origin, address, &token_path).await?;
        let outcome: anyhow::Result<()> = tokio::select! {
            result = shutdown_requested => result,
            result = service.wait_for_exit() => result,
        };
        if let Err(error) = &outcome {
            tracing::error!(error=%format!("{error:#}"), "native service owner stopped; beginning owned shutdown");
        }
        // This is normal shutdown only. No backup is automatically requested.
        let stopped = service.stop().await?;
        stopped.close().await?;
        return outcome;
    }
    use service_shutdown::ownership::{Registry, Role};
    let mut owner_registry = Registry::new(app.native_catalog.clone());
    // Observability failures never fail the job supervisor or HTTP server.
    let metrics_app = app.clone();
    let metrics_stop = shutdown.clone();
    let mut metrics = owner_registry.register(
        Role::Metrics,
        tokio::spawn(async move {
            playscale::metrics::history::worker(metrics_app, metrics_stop).await
        }),
    )?;
    app.health
        .worker_running
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let worker_app = app.clone();
    let worker_stop = shutdown.clone();
    let mut worker = owner_registry.register(
        Role::Worker,
        tokio::spawn(async move {
            let result = if worker_app.native_catalog.is_some() {
                // Legacy workers publish catalog rows, refresh FTS, or mutate scan
                // state. They must not run against the sealed compatibility source.
                // Motion retains delivery/auth and durable accepted-demand ownership.
                // This worker uses only the retained native observation/publication
                // pipeline; it never starts the legacy scanner or configure writer.
                let (scan, processing, previews, delivery, mail) = tokio::join!(
                    service_shutdown::cancel_on_error(worker_stop.clone(), async {
                        playscale::native_scans::worker(worker_app.clone(), worker_stop.clone())
                            .await
                            .context("native scan coordinator")
                    }),
                    service_shutdown::cancel_on_error(worker_stop.clone(), async {
                        playscale::processing::worker(worker_app.clone(), worker_stop.clone())
                            .await
                            .context("processing coordinator")
                    }),
                    service_shutdown::cancel_on_error(worker_stop.clone(), async {
                        playscale::v2::previews::worker(worker_app.clone(), worker_stop.clone())
                            .await
                            .context("native preview coordinator")
                    }),
                    service_shutdown::cancel_on_error(worker_stop.clone(), async {
                        playscale::delivery::worker(worker_app.clone(), worker_stop.clone())
                            .await
                            .context("delivery coordinator")
                    }),
                    service_shutdown::cancel_on_error(worker_stop.clone(), async {
                        playscale::v2::signin::mail_worker(worker_app.clone(), worker_stop.clone())
                            .await
                            .context("sign-in mail coordinator")
                    })
                );
                scan.and(processing).and(previews).and(delivery).and(mail)
            } else {
                tokio::try_join!(
                    scan::worker(worker_app.clone(), worker_stop.clone()),
                    playscale::processing::worker(worker_app.clone(), worker_stop.clone()),
                    playscale::v2::previews::worker(worker_app.clone(), worker_stop.clone()),
                    playscale::delivery::worker(worker_app.clone(), worker_stop.clone()),
                    playscale::maintenance::worker(worker_app.clone(), worker_stop.clone()),
                    playscale::storage::worker(worker_app.clone(), worker_stop.clone()),
                    playscale::watch::worker(
                        worker_app.clone(),
                        worker_stop.clone(),
                        Default::default()
                    ),
                    playscale::v2::signin::mail_worker(worker_app.clone(), worker_stop.clone()),
                    playscale::scan::configure(worker_app.clone(), args.libraries, worker_stop)
                )
                .map(|_| ())
            };
            worker_app
                .health
                .worker_running
                .store(false, std::sync::atomic::Ordering::Relaxed);
            result
        }),
    )?;
    let ingress_task = if let Some(socket) = ingress {
        let router = router
            .clone()
            .layer(axum::Extension(playscale::v2::auth::TrustedIngress));
        let stop = shutdown.clone();
        Some(tokio::spawn(async move {
            axum::serve(socket, router)
                .with_graceful_shutdown(stop.cancelled_owned())
                .await
        }))
    } else {
        None
    };
    let mut ingress_task = owner_registry.ingress(ingress_task)?;
    let stop_http = shutdown.clone();
    let mut http = owner_registry.register(
        Role::Http,
        tokio::spawn(async move {
            // Peer addresses classify a playback client's network for monitoring
            // (local, private or remote); they are not stored or exposed.
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(stop_http.cancelled_owned())
            .await
        }),
    )?;
    let required_owners = owner_registry.seal()?;
    announce_started(&app, &cli, &origin, address, &token_path).await?;
    let outcome: anyhow::Result<()> = tokio::select! {
        result = shutdown_requested => result,
        result = worker.wait() => {
            let terminal=result;
            let reason=terminal.as_ref().err().cloned().unwrap_or_else(||"worker stopped unexpectedly".into());
            Err(anyhow::anyhow!(reason))
        },
        result = http.wait() => {
            let terminal=result;
            let reason=terminal.as_ref().err().cloned().unwrap_or_else(||"HTTP server stopped unexpectedly".into());
            Err(anyhow::anyhow!(reason))
        },
    };
    if let Err(error) = &outcome {
        tracing::error!(error = %format!("{error:#}"), "server owner stopped; beginning retained shutdown");
    }
    app.health
        .shutting_down
        .store(true, std::sync::atomic::Ordering::Relaxed);
    shutdown.cancel();
    let grace = std::time::Duration::from_secs(if app.native_catalog.is_some() { 120 } else { 5 });
    // Attempt all registered owners even if one fails. Each proof is emitted
    // only by its owned handle's actual successful join, never is_finished.
    let (worker_join, http_join, metrics_join, ingress_join) = tokio::join!(
        worker.join(grace),
        http.join(std::time::Duration::from_secs(5)),
        metrics.join(std::time::Duration::from_secs(2)),
        async {
            match ingress_task.as_mut() {
                Some(task) => task.join(std::time::Duration::from_secs(5)).await.map(Some),
                None => Ok(None),
            }
        }
    );
    let mut joined = Vec::new();
    let mut failure = None;
    for result in [worker_join, http_join, metrics_join] {
        match result {
            Ok(proof) => joined.push(proof),
            Err(error) => {
                tracing::warn!(%error, "service owner did not stop cleanly");
                failure = Some(error);
            }
        }
    }
    match ingress_join {
        Ok(Some(proof)) => joined.push(proof),
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(%error, "trusted ingress did not stop cleanly");
            failure = Some(error);
        }
    }
    let joined_service = if failure.is_none() {
        Some(required_owners.finish(joined)?)
    } else {
        None
    };
    if let Some(native) = &app.native_catalog {
        if let Some(error) = failure {
            return Err(error);
        }
        anyhow::ensure!(
            joined_service
                .as_ref()
                .is_some_and(|proof| proof.belongs_to(native)),
            "native startup owner proof mismatch"
        );
    }
    // Motion owns renderer/supervisor cleanup independently of Catabolic's
    // observation owner. Neither an elapsed timeout nor a returned worker is
    // proof that an unjoined encoder or detached blocking copy has stopped.
    let _native_io_custody = if app.native_catalog.is_some() {
        Some(native_shutdown_custody(&app.processing)?)
    } else {
        None
    };
    if let Some(native) = &app.native_catalog {
        if let Err(error) = native.shutdown().await {
            tracing::error!(%error, "native catalog did not shut down cleanly");
            app.db.close().await;
            return Err(error);
        }
    }
    app.db.close().await;
    drop(lock);
    outcome
}

/// Bootstrap requires the durable fresh-install intent created while both
/// destinations were absent. Replay uses the same exact audited cutover intent.
async fn bootstrap_empty_native(
    db: sqlx::SqlitePool,
    path: std::path::PathBuf,
) -> anyhow::Result<()> {
    let identity = motion_catalog_backend::inspect(path.clone())
        .await?
        .identity;
    const IMPORT: &str = "motion.fresh.native.v1";
    let imports: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT import_id,input_digest,snapshot_json FROM catabolic_import_snapshots",
    )
    .fetch_all(&db)
    .await?;
    anyhow::ensure!(
        imports.len() <= 1,
        "fresh bootstrap refuses unrelated import snapshots"
    );
    if let Some((id, digest, json)) = imports.first() {
        let snapshot: motion_catalog_backend::migration::Snapshot = serde_json::from_str(json)?;
        anyhow::ensure!(
            id == IMPORT
                && snapshot.manifest.import_id == IMPORT
                && snapshot.manifest.contract_version == 3
                && snapshot.batches.is_empty()
                && snapshot.manifest.batches.is_empty()
                && snapshot.validate()? == *digest,
            "fresh bootstrap requires its exact retained empty import"
        );
    } else {
        let catalog_rows: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM items)+(SELECT count(*) FROM editions)+(SELECT count(*) FROM media_files)+(SELECT count(*) FROM libraries)+(SELECT count(*) FROM catalog_libraries)")
            .fetch_one(&db).await?;
        anyhow::ensure!(
            catalog_rows == 0,
            "fresh bootstrap refuses nonempty source data; use explicit migration"
        );
    }
    let backend = motion_catalog_backend::NativeCatalog::bind(db.clone(), path, identity).await?;
    let recorded: Option<(String, String)> =
        sqlx::query_as("SELECT import_id,phase FROM catabolic_cutover WHERE singleton=1")
            .fetch_optional(&db)
            .await?;
    anyhow::ensure!(
        recorded.as_ref().is_none_or(|(id, _)| id == IMPORT),
        "fresh bootstrap cannot resume another migration identity"
    );
    let phase = recorded.map(|(_, phase)| phase);
    if phase.as_deref() == Some("activated") {
        backend.reconcile_motion_cutover().await?;
        return Ok(());
    }
    if phase.as_deref() == Some("validated") {
        backend
            .activate_motion_cutover(std::time::Duration::from_secs(30))
            .await?;
        return Ok(());
    }
    if motion_catalog_backend::source_freeze::admission(&db)
        .await?
        .is_none_or(|state| state.active_freeze().is_none())
    {
        backend.freeze_motion_source(IMPORT, 0).await?;
    }
    backend.capture_motion_import(IMPORT).await?;
    backend.stage_import(IMPORT).await?;
    backend.prepare_motion_cutover(IMPORT).await?;
    backend
        .activate_motion_cutover(std::time::Duration::from_secs(30))
        .await?;
    Ok(())
}

/// Read the bootstrap secret from an inherited descriptor and close it.
#[cfg(unix)]
fn read_bootstrap(fd: i32) -> anyhow::Result<String> {
    use std::{io::Read, os::fd::FromRawFd};
    anyhow::ensure!(
        fd > 2,
        "bootstrap descriptor must not be stdin/stdout/stderr"
    );
    // SAFETY: the launcher passes this descriptor to us for exclusive use;
    // it is consumed and closed here exactly once.
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let mut secret = String::new();
    Read::by_ref(&mut file)
        .take(256)
        .read_to_string(&mut secret)?;
    Ok(secret.trim().to_owned())
}

#[cfg(not(unix))]
fn read_bootstrap(_fd: i32) -> anyhow::Result<String> {
    anyhow::bail!("bootstrap descriptors are supported on Unix only")
}

/// Independent Motion custody gate. A successful guard is retained through
/// native shutdown; refusal must return before polling the native clean drain.
async fn announce_started(
    app: &App,
    cli: &Args,
    origin: &str,
    address: std::net::SocketAddr,
    token_path: &std::path::Path,
) -> anyhow::Result<()> {
    if let Some(fd) = cli.ready_fd {
        let (server_id, _) = playscale::v2::system::server_identity(&app)
            .await
            .map_err(|_| anyhow::anyhow!("cannot read server identity"))?;
        playscale::desktop::publish(
            fd,
            &playscale::desktop::Ready {
                protocol: 1,
                server_id,
                server_epoch: app.access.server_epoch.clone(),
                origin: origin.to_owned(),
                version: env!("CARGO_PKG_VERSION"),
                contract_digest: playscale::v2::system::contract_digest(),
            },
        )?;
    }
    tracing::info!(%address, %origin, admin_token_file=%token_path.display(), "Playscale listening");
    println!(
        "Motion is running at {origin}\nAdmin token file: {}\nPress Ctrl+C to stop.",
        token_path.display()
    );
    if app.access.mode == playscale_core::access::AccessMode::TrustedHousehold {
        // Explicitly chosen; say what it means every start.
        tracing::warn!(%origin, loopback = address.ip().is_loopback(), "Household access mode: requests without a credential browse and play as the household viewer");
        println!(
            "Household mode: anyone who can reach {origin} can browse, play and save progress without signing in.\nAdministration still needs the admin token file or a signed-in administrator.{}",
            if address.ip().is_loopback() {
                ""
            } else {
                "\nWARNING: this server listens beyond this computer. Use household mode only on a network you trust."
            }
        );
    }
    if cli.open_browser {
        #[cfg(target_os = "macos")]
        if let Err(error) = std::process::Command::new("/usr/bin/open")
            .arg(&origin)
            .spawn()
        {
            tracing::warn!(%error, "Could not open browser");
        }
    }
    Ok(())
}

fn native_shutdown_custody(
    processing: &playscale::processing::Runtime,
) -> anyhow::Result<tokio::sync::OwnedSemaphorePermit> {
    for (name, state) in [
        ("execution", processing.execution.snapshot()),
        ("hardware", processing.hardware.snapshot()),
    ] {
        anyhow::ensure!(
            state.used == 0
                && state.running == 0
                && state.terminating == 0
                && state.stuck.is_empty()
                && state.waiting == 0,
            "Motion {name} ownership remains unresolved; no clean native shutdown acknowledged"
        );
    }
    let permit=processing.io.clone().try_acquire_owned().map_err(|_| {
        anyhow::anyhow!("Motion blocking I/O ownership remains unresolved; no clean native shutdown acknowledged")
    })?;
    // The checks above are diagnostic only. These core transitions linearize
    // terminal refusal of new reservations under each actual ledger mutex.
    processing.execution.close_admission_if_idle()?;
    processing.hardware.close_admission_if_idle()?;
    Ok(permit)
}

/// Register for SIGINT and SIGTERM (Ctrl+C on Windows) now; the returned
/// future completes on either. Registration happens in this call, not on
/// first poll.
fn shutdown_signal() -> anyhow::Result<impl std::future::Future<Output = anyhow::Result<()>>> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut terminate = signal(SignalKind::terminate())?;
        Ok(async move {
            tokio::select! { _ = interrupt.recv() => {}, _ = terminate.recv() => {} }
            Ok(())
        })
    }
    #[cfg(windows)]
    {
        let mut ctrl_c = tokio::signal::windows::ctrl_c()?;
        Ok(async move {
            ctrl_c.recv().await;
            Ok(())
        })
    }
    #[cfg(not(any(unix, windows)))]
    Ok(async { Ok(tokio::signal::ctrl_c().await?) })
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn fresh_native_bootstrap_reopens_without_legacy_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let native = dir.path().join("catalog.sqlite3");
        motion_catalog_backend::initialize(native.clone())
            .await
            .unwrap();
        let db = playscale::db::connect(&dir.path().join("motion.sqlite3"))
            .await
            .unwrap();
        super::bootstrap_empty_native(db.clone(), native.clone())
            .await
            .unwrap();
        // A retained fresh intent replays acknowledgment, not a second import.
        super::bootstrap_empty_native(db.clone(), native.clone())
            .await
            .unwrap();
        let mut admission =
            motion_catalog_backend::cutover::open_server(db.clone(), native, "default".into())
                .await
                .unwrap();
        admission
            .admission
            .admitted_host()
            .unwrap()
            .close()
            .unwrap();
        assert!(
            sqlx::query("INSERT INTO items(id,title,kind) VALUES('old','Old','video')")
                .execute(&db)
                .await
                .is_err()
        );
        sqlx::query("UPDATE profiles SET name='Native profile' WHERE id='default'")
            .execute(&db)
            .await
            .unwrap();
    }
    #[test]
    fn blocked_io_does_not_prevent_runtime_shutdown() {
        let (started, wait_started) = std::sync::mpsc::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let before = std::time::Instant::now();
        super::execute(async move {
            tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                let _ = blocked.recv();
            });
            tokio::task::spawn_blocking(move || wait_started.recv()).await??;
            Ok(())
        })
        .unwrap();
        release.send(()).unwrap();
        assert!(before.elapsed() < std::time::Duration::from_secs(5));
    }

    /// A signal delivered after `shutdown_signal()` returns but before its
    /// future is first polled must request shutdown, not kill the process
    /// (SIGINT's default action). The server serves in exactly that window.
    #[cfg(unix)]
    #[test]
    fn shutdown_signals_are_registered_before_first_poll() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for signal in [libc::SIGINT, libc::SIGTERM] {
            let requested = {
                let _context = runtime.enter(); // signal() needs the runtime's driver
                super::shutdown_signal().unwrap()
            };
            // SAFETY: raise only queues a signal for this process.
            assert_eq!(unsafe { libc::raise(signal) }, 0);
            runtime
                .block_on(async {
                    tokio::time::timeout(std::time::Duration::from_secs(5), requested).await
                })
                .expect("shutdown not requested")
                .unwrap();
        }
    }
}

#[cfg(test)]
mod native_shutdown_tests {
    use super::native_shutdown_custody;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // The callback is only a test observation of reaching the next statement,
    // never a substitute native cleanup witness or a synthetic process exit.
    fn attempt_drain(
        processing: &playscale::processing::Runtime,
        calls: &AtomicUsize,
    ) -> anyhow::Result<()> {
        let _custody = native_shutdown_custody(processing)?;
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    #[tokio::test]
    async fn outstanding_motion_io_prevents_native_clean_drain() {
        let dir = tempfile::tempdir().unwrap();
        let processing =
            playscale::processing::Runtime::new(dir.path().join("generated"), Default::default());
        let calls = AtomicUsize::new(0);
        let live_copy = processing.io.clone().acquire_owned().await.unwrap();
        assert!(
            attempt_drain(&processing, &calls)
                .unwrap_err()
                .to_string()
                .contains("I/O ownership")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            processing.io.available_permits(),
            0,
            "refusal must not release someone else's I/O"
        );
        drop(live_copy);
        attempt_drain(&processing, &calls).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn outstanding_execution_or_hardware_owner_prevents_native_clean_drain() {
        let dir = tempfile::tempdir().unwrap();
        let processing =
            playscale::processing::Runtime::new(dir.path().join("generated"), Default::default());
        let calls = AtomicUsize::new(0);
        for (label, coordinator) in [
            ("execution", &processing.execution),
            ("hardware", &processing.hardware),
        ] {
            let owner = coordinator
                .reserve(
                    format!("shutdown-test-{label}"),
                    playscale_core::work::Class::Preparation,
                    1,
                )
                .await
                .unwrap();
            assert!(
                attempt_drain(&processing, &calls)
                    .unwrap_err()
                    .to_string()
                    .contains(label)
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert!(
                coordinator.snapshot().used > 0,
                "refusal must not release an actual owner"
            );
            drop(owner); // This reservation has not launched a child.
            assert_eq!(coordinator.snapshot().used, 0);
        }
        attempt_drain(&processing, &calls).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
