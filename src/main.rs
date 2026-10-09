use clap::Parser;
use playscale::{App, api, db, scan};
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
    tokio::fs::create_dir_all(&args.data_dir).await?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(args.data_dir.join("server.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock)
        .map_err(|_| anyhow::anyhow!("another server owns this data directory"))?;
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
    let db = db::connect(&args.data_dir.join("playscale.sqlite3")).await?;
    db::recover(&db).await?;
    let token_path = args.data_dir.join("admin-token");
    let assets = args
        .demuxe_dir
        .join("web/generated/player/index.js")
        .is_file()
        .then_some(args.demuxe_dir.clone());
    let app = App {
        health: Arc::new(playscale::operations::Health::new(assets.is_some())),
        db,
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
    playscale::processing::recover(&app).await?;
    if assets.is_none() {
        tracing::warn!(
            "Demuxe assets absent; run scripts/install_demuxe.py before browser playback"
        );
    }
    let router = api::router(app.clone(), assets);
    let shutdown = CancellationToken::new();
    app.health
        .worker_running
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let worker_app = app.clone();
    let worker_stop = shutdown.clone();
    let mut worker = tokio::spawn(async move {
        let result = tokio::try_join!(
            scan::worker(worker_app.clone(), worker_stop.clone()),
            playscale::processing::worker(worker_app.clone(), worker_stop.clone()),
            playscale::maintenance::worker(worker_app.clone(), worker_stop.clone()),
            playscale::storage::worker(worker_app.clone(), worker_stop.clone()),
            playscale::scan::configure(worker_app.clone(), args.libraries, worker_stop)
        )
        .map(|_| ());
        worker_app
            .health
            .worker_running
            .store(false, std::sync::atomic::Ordering::Relaxed);
        result
    });
    let stop_http = shutdown.clone();
    let mut http = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(stop_http.cancelled_owned())
            .await
    });
    tracing::info!(%address, %origin, admin_token_file=%token_path.display(), "Playscale listening");
    println!(
        "Motion is running at {origin}\nAdmin token file: {}\nPress Ctrl+C to stop.",
        token_path.display()
    );
    if cli.open_browser {
        #[cfg(target_os = "macos")]
        if let Err(error) = std::process::Command::new("/usr/bin/open")
            .arg(&origin)
            .spawn()
        {
            tracing::warn!(%error, "Could not open browser");
        }
    }
    let outcome: anyhow::Result<()> = tokio::select! {
        result = shutdown_signal() => result,
        result = &mut worker => match result { Ok(Ok(())) => Err(anyhow::anyhow!("scan worker stopped unexpectedly")), Ok(Err(e)) => Err(e), Err(e) => Err(e.into()) },
        result = &mut http => match result { Ok(Ok(())) => Err(anyhow::anyhow!("HTTP server stopped unexpectedly")), Ok(Err(e)) => Err(e.into()), Err(e) => Err(e.into()) },
    };
    app.health
        .shutting_down
        .store(true, std::sync::atomic::Ordering::Relaxed);
    shutdown.cancel();
    if !worker.is_finished()
        && tokio::time::timeout(std::time::Duration::from_secs(5), &mut worker)
            .await
            .is_err()
    {
        worker.abort();
    }
    if !http.is_finished()
        && tokio::time::timeout(std::time::Duration::from_secs(5), &mut http)
            .await
            .is_err()
    {
        http.abort();
    }
    app.db.close().await;
    drop(lock);
    outcome
}

async fn shutdown_signal() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! { result = tokio::signal::ctrl_c() => { result?; }, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
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
}
