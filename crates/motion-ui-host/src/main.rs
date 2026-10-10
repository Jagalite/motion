use clap::Parser;
use std::{
    io::{Read, Write},
    path::PathBuf,
    sync::Arc,
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    cache_dir: PathBuf,
    #[arg(long)]
    demuxe_dir: PathBuf,
    #[arg(long)]
    bootstrap_fd: i32,
    #[arg(long)]
    ready_fd: i32,
    #[arg(long)]
    lifetime_fd: i32,
}

#[cfg(unix)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use std::os::fd::FromRawFd;
    let args = Args::parse();
    anyhow::ensure!(
        [args.bootstrap_fd, args.ready_fd, args.lifetime_fd]
            .into_iter()
            .all(|fd| fd > 2)
            && args.bootstrap_fd != args.ready_fd
            && args.bootstrap_fd != args.lifetime_fd
            && args.ready_fd != args.lifetime_fd,
        "private distinct descriptors required"
    );
    let mut credential = String::new();
    // The host transfers ownership of these three inherited descriptors once.
    unsafe { std::fs::File::from_raw_fd(args.bootstrap_fd) }
        .take(129)
        .read_to_string(&mut credential)?;
    anyhow::ensure!(
        credential.len() == 64 && credential.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid bootstrap capability"
    );
    let cache = Arc::new(motion_ui_host::Cache::open(&args.cache_dir)?);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let router = motion_ui_host::Host::new(cache, origin.clone(), credential)
        .with_player_assets(&args.demuxe_dir)?
        .router();
    let mut ready = unsafe { std::fs::File::from_raw_fd(args.ready_fd) };
    serde_json::to_writer(
        &mut ready,
        &serde_json::json!({"protocol":1,"kind":"offline-presentation","origin":origin,"epoch":uuid::Uuid::new_v4().to_string()}),
    )?;
    ready.write_all(b"\n")?;
    ready.flush()?;
    drop(ready);
    let lifetime = unsafe { std::fs::File::from_raw_fd(args.lifetime_fd) };
    // Parent loss is a crash boundary: no media transfer or stalled cache read
    // may keep an orphan helper alive. Acknowledged events were fsynced before
    // their response; unacknowledged writes recover as the old or new whole log.
    std::thread::spawn(move || {
        let mut byte = [0u8];
        let mut pipe = lifetime;
        let _ = pipe.read(&mut byte);
        std::process::exit(0);
    });
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

#[cfg(not(unix))]
fn main() {
    eprintln!("This build requires Unix inherited-pipe transport");
    std::process::exit(1);
}
