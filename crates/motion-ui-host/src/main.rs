use clap::{Parser, Subcommand};
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

/// One-shot cache writer used by the desktop download agent. Each command
/// takes the cache's exclusive lock and prints one JSON result.
#[derive(Parser)]
#[command(name = "motion-ui-host cache")]
struct CacheArgs {
    #[arg(long)]
    cache_dir: PathBuf,
    /// The root's CacheScope as JSON; a root never changes scope.
    #[arg(long)]
    scope: String,
    #[command(subcommand)]
    command: CacheCommand,
}

#[derive(Subcommand)]
enum CacheCommand {
    /// Manifest, byte usage and the event log's next sequence.
    Status,
    /// Whether a transfer of `incoming` bytes fits the quota and headroom.
    Admit {
        #[arg(long)]
        incoming: u64,
        #[arg(long)]
        quota: u64,
        #[arg(long)]
        headroom: u64,
    },
    /// Verify `staging/<download_id>.part` against the entry read from stdin
    /// and publish it.
    Publish,
    Remove {
        #[arg(long)]
        download_id: String,
    },
    Events,
    Acknowledge {
        #[arg(long)]
        through: u64,
    },
    Rebase {
        #[arg(long)]
        download_id: String,
        #[arg(long)]
        viewing_revision: String,
        #[arg(long)]
        manual_epoch: String,
        #[arg(long)]
        resume_position_ms: Option<u64>,
    },
}

fn cache_command(args: CacheArgs) -> anyhow::Result<serde_json::Value> {
    use motion_ui_host::store::Store;
    use serde_json::json;
    let scope = serde_json::from_str(&args.scope)?;
    let mut store = Store::open(&args.cache_dir, &scope)?;
    Ok(match args.command {
        CacheCommand::Status => {
            let events = store.events()?;
            json!({"manifest":store.manifest(),"usage":store.usage()?.to_string(),
                "pending_events":events.events.len(),"next_sequence":events.next_sequence().map(|n|n.to_string())})
        }
        CacheCommand::Admit {
            incoming,
            quota,
            headroom,
        } => json!({"admitted": store.admits_transfer(incoming, quota, headroom)?}),
        CacheCommand::Publish => {
            let mut input = String::new();
            std::io::stdin()
                .take(64 * 1024)
                .read_to_string(&mut input)?;
            json!({"publication": store.publish(serde_json::from_str(&input)?)?})
        }
        CacheCommand::Remove { download_id } => json!({"removed": store.remove(&download_id)?}),
        CacheCommand::Events => serde_json::to_value(store.events()?)?,
        CacheCommand::Acknowledge { through } => serde_json::to_value(store.acknowledge(through)?)?,
        CacheCommand::Rebase {
            download_id,
            viewing_revision,
            manual_epoch,
            resume_position_ms,
        } => {
            json!({"rebased": store.rebase(&download_id, &viewing_revision, &manual_epoch, resume_position_ms)?})
        }
    })
}

#[cfg(unix)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use std::os::fd::FromRawFd;
    if std::env::args_os().nth(1).is_some_and(|a| a == "cache") {
        let args = CacheArgs::parse_from(std::env::args_os().skip(1));
        return match cache_command(args) {
            Ok(value) => {
                println!("{value}");
                Ok(())
            }
            Err(error) => {
                println!("{}", serde_json::json!({"error": error.to_string()}));
                std::process::exit(1);
            }
        };
    }
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
