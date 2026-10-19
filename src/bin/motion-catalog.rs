//! Operator-only native backend preparation. Does not activate the web backend.
use clap::{Parser, Subcommand};
use playscale::catalog_backend::{self, NativeCatalog};
use std::{io::Read, path::PathBuf, time::Duration};

#[derive(Parser)]
#[command(about = "Prepare and inspect Motion's native Catabolic backend")]
struct Args {
    #[arg(long)]
    catalog: PathBuf,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Explicitly create a new private native catalog; refuses existing files.
    Init,
    /// Inspect the native catalog without modifying it.
    Inspect,
    /// Bind a Motion database to the inspected catalog identity.
    Bind {
        #[arg(long)]
        motion_db: PathBuf,
    },
    /// Record an operator item batch, then dispatch its durable intent.
    PutItems {
        #[arg(long)]
        motion_db: PathBuf,
        #[arg(long)]
        operation_id: String,
        /// JSON array of Catabolic item inputs.
        #[arg(long)]
        input: PathBuf,
    },
    /// Retain a complete bounded import snapshot before staging native effects.
    PrepareImport {
        #[arg(long)]
        motion_db: PathBuf,
        #[arg(long)]
        input: PathBuf,
    },
    /// Resume staging from the retained snapshot. Does not publish catalog data.
    StageImport {
        #[arg(long)]
        motion_db: PathBuf,
        #[arg(long)]
        import_id: String,
    },
    /// Validate staging without typed owner grants or activation authority.
    ValidateImport {
        #[arg(long)]
        motion_db: PathBuf,
        #[arg(long)]
        import_id: String,
    },
    /// Capture a consistent Motion catalog export, retaining unresolved facts.
    CaptureMotionImport {
        #[arg(long)]
        motion_db: PathBuf,
        #[arg(long)]
        import_id: String,
    },
    /// Rehearse typed metadata using the trusted Motion export provenance.
    ValidateMotionImport {
        #[arg(long)]
        motion_db: PathBuf,
        #[arg(long)]
        import_id: String,
    },
    /// Inspect durable Motion source-write admission.
    SourceState {
        #[arg(long)]
        motion_db: PathBuf,
    },
    /// Persistently freeze Motion writers for a new migration identity.
    FreezeMotionSource {
        #[arg(long)]
        motion_db: PathBuf,
        #[arg(long)]
        import_id: String,
        #[arg(long)]
        expected_revision: u64,
    },
    /// Reopen legacy writes only before the freeze has been sealed for cutover.
    AbandonMotionFreeze {
        #[arg(long)]
        motion_db: PathBuf,
        #[arg(long)]
        freeze_revision: u64,
    },
    /// Dispatch/reconcile an existing command without creating a new identity.
    Retry {
        #[arg(long)]
        motion_db: PathBuf,
        #[arg(long)]
        operation_id: String,
    },
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let path = catalog_backend::destination(&args.catalog)?;
    let result = match args.command {
        Command::Init => catalog_backend::initialize(path).await?,
        Command::Inspect => serde_json::to_value(catalog_backend::inspect(path).await?)?,
        command => {
            let motion_db = match &command {
                Command::Bind { motion_db }
                | Command::PutItems { motion_db, .. }
                | Command::Retry { motion_db, .. }
                | Command::PrepareImport { motion_db, .. }
                | Command::StageImport { motion_db, .. }
                | Command::ValidateImport { motion_db, .. }
                | Command::CaptureMotionImport { motion_db, .. }
                | Command::ValidateMotionImport { motion_db, .. }
                | Command::SourceState { motion_db }
                | Command::FreezeMotionSource { motion_db, .. }
                | Command::AbandonMotionFreeze { motion_db, .. } => motion_db,
                _ => unreachable!(),
            };
            // Never create a Motion database implicitly or mutate the source
            // catalog with SQLx migrations accidentally pointed at the same file.
            let motion_db = motion_db.canonicalize()?;
            anyhow::ensure!(
                motion_db != path.canonicalize()?,
                "the two stores must be distinct"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let a = std::fs::metadata(&motion_db)?;
                let b = std::fs::metadata(&path)?;
                anyhow::ensure!(
                    (a.dev(), a.ino()) != (b.dev(), b.ino()),
                    "the two stores must not be hard links to the same file"
                );
            }
            let db = playscale::db::connect_catalog_control(&motion_db).await?;
            let identity = catalog_backend::inspect(path.clone()).await?.identity;
            let backend = NativeCatalog::bind(db.clone(), path, identity).await?;
            let result = match command {
                Command::Bind { .. } => serde_json::to_value(backend.status().await?)?,
                Command::PutItems {
                    operation_id,
                    input,
                    ..
                } => {
                    let mut bytes = Vec::new();
                    std::fs::File::open(input)?
                        .take(1024 * 1024 + 1)
                        .read_to_end(&mut bytes)?;
                    anyhow::ensure!(bytes.len() <= 1024 * 1024, "item input exceeds 1 MiB");
                    backend
                        .prepare_items(&operation_id, serde_json::from_slice(&bytes)?)
                        .await?;
                    serde_json::to_value(backend.dispatch(&operation_id).await?)?
                }
                Command::PrepareImport { input, .. } => {
                    let mut bytes = Vec::new();
                    std::fs::File::open(input)?
                        .take(2 * 1024 * 1024 + 1)
                        .read_to_end(&mut bytes)?;
                    anyhow::ensure!(bytes.len() <= 2 * 1024 * 1024, "import input exceeds 2 MiB");
                    backend
                        .prepare_import(serde_json::from_slice(&bytes)?)
                        .await?;
                    serde_json::json!({"prepared": true, "activated": false})
                }
                Command::StageImport { import_id, .. } => {
                    serde_json::to_value(backend.stage_import(&import_id).await?)?
                }
                Command::ValidateImport { import_id, .. } => {
                    serde_json::to_value(backend.validate_import(&import_id, None).await?)?
                }
                Command::CaptureMotionImport { import_id, .. } => {
                    serde_json::to_value(backend.capture_motion_import(&import_id).await?)?
                }
                Command::ValidateMotionImport { import_id, .. } => {
                    serde_json::to_value(backend.validate_motion_import(&import_id).await?)?
                }
                Command::SourceState { .. } => serde_json::to_value(backend.source_state().await?)?,
                Command::FreezeMotionSource {
                    import_id,
                    expected_revision,
                    ..
                } => serde_json::to_value(
                    backend
                        .freeze_motion_source(&import_id, expected_revision)
                        .await?,
                )?,
                Command::AbandonMotionFreeze {
                    freeze_revision, ..
                } => serde_json::to_value(backend.abandon_motion_freeze(freeze_revision).await?)?,
                Command::Retry { operation_id, .. } => {
                    serde_json::to_value(backend.dispatch(&operation_id).await?)?
                }
                _ => unreachable!(),
            };
            backend.drain(Duration::from_secs(10)).await?;
            db.close().await;
            result
        }
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    if let Some(status) = result.get("status").and_then(|value| value.as_str()) {
        anyhow::ensure!(
            matches!(status, "committed" | "replay"),
            "catalog command requires reconciliation: {status}"
        );
    }
    Ok(())
}
