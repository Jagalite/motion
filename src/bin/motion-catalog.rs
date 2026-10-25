//! Operator-only native backend preparation and fail-closed cutover recovery.
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
    /// Register one exact physical Motion source using its immutable source ID.
    RegisterSource {
        #[arg(long)]
        motion_db: PathBuf,
        #[arg(long)]
        source_id: String,
    },
    /// Register a native-owned source without writing frozen legacy libraries.
    RegisterNativeSource {
        #[arg(long)]
        motion_db: PathBuf,
        #[arg(long)]
        source_id: String,
        #[arg(long)]
        logical_library_id: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        exclude: Vec<String>,
    },
    /// List immutable native-owned source descriptors.
    NativeSources {
        #[arg(long)]
        motion_db: PathBuf,
    },
    /// Verify every physical source mapping and current native binding/policy.
    AuditSources {
        #[arg(long)]
        motion_db: PathBuf,
    },
    /// Observe one registered source with this packaged executable's closed helper.
    ScanSource {
        #[arg(long)]
        motion_db: PathBuf,
        #[arg(long)]
        source_id: String,
        #[arg(long, default_value_t = 120)]
        timeout_seconds: u64,
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
    /// Persist a cutover intent only after frozen-source/reference qualification.
    PrepareMotionCutover {
        #[arg(long)]
        motion_db: PathBuf,
        #[arg(long)]
        import_id: String,
    },
    /// Activate the exact prepared cutover after closing and draining native work.
    ActivateMotionCutover {
        #[arg(long)]
        motion_db: PathBuf,
        #[arg(long, default_value_t = 30)]
        timeout_seconds: u64,
    },
    /// Reconcile a lost activation reply without executing an absent command.
    ReconcileMotionCutover {
        #[arg(long)]
        motion_db: PathBuf,
    },
    /// Capture a complete ordered set of bounded imports from one frozen source.
    CaptureMotionExportPlan {
        #[arg(long)]
        motion_db: PathBuf,
        #[arg(long)]
        root_import_id: String,
    },
    /// Persist the full qualified partition manifest before any cutover effects.
    PrepareMotionPartitionCutover {
        #[arg(long)]
        motion_db: PathBuf,
        #[arg(long)]
        root_import_id: String,
    },
    /// Resume the exact complete partition cutover; partial sets never serve.
    ActivateMotionPartitionCutover {
        #[arg(long)]
        motion_db: PathBuf,
        #[arg(long, default_value_t = 30)]
        timeout_seconds: u64,
    },
    /// Dispatch/reconcile an existing command without creating a new identity.
    Retry {
        #[arg(long)]
        motion_db: PathBuf,
        #[arg(long)]
        operation_id: String,
    },
}
fn main() -> anyhow::Result<()> {
    if let Some(code) = catalog_backend::observer::dispatch_builtin_invocation() {
        std::process::exit(code);
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run())
}
async fn run() -> anyhow::Result<()> {
    let args = Args::parse();
    let path = catalog_backend::destination(&args.catalog)?;
    let result = match args.command {
        Command::Init => catalog_backend::initialize(path).await?,
        Command::Inspect => serde_json::to_value(catalog_backend::inspect(path).await?)?,
        command => {
            let motion_db = match &command {
                Command::Bind { motion_db }
                | Command::RegisterSource { motion_db, .. }
                | Command::RegisterNativeSource { motion_db, .. }
                | Command::NativeSources { motion_db }
                | Command::AuditSources { motion_db }
                | Command::ScanSource { motion_db, .. }
                | Command::PutItems { motion_db, .. }
                | Command::Retry { motion_db, .. }
                | Command::PrepareImport { motion_db, .. }
                | Command::StageImport { motion_db, .. }
                | Command::ValidateImport { motion_db, .. }
                | Command::CaptureMotionImport { motion_db, .. }
                | Command::ValidateMotionImport { motion_db, .. }
                | Command::PrepareMotionCutover { motion_db, .. }
                | Command::ActivateMotionCutover { motion_db, .. }
                | Command::ReconcileMotionCutover { motion_db }
                | Command::CaptureMotionExportPlan { motion_db, .. }
                | Command::PrepareMotionPartitionCutover { motion_db, .. }
                | Command::ActivateMotionPartitionCutover { motion_db, .. }
                | Command::SourceState { motion_db }
                | Command::FreezeMotionSource { motion_db, .. }
                | Command::AbandonMotionFreeze { motion_db, .. } => motion_db,
                _ => unreachable!(),
            };
            // Never create a Motion database implicitly or mutate the source
            // catalog with SQLx migrations accidentally pointed at the same file.
            let motion_db = motion_db.canonicalize()?;
            anyhow::ensure!(
                std::fs::metadata(&motion_db)?.is_file(),
                "Motion database must be a regular file"
            );
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
                Command::RegisterSource { source_id, .. } => {
                    serde_json::to_value(backend.register_motion_source(&source_id).await?)?
                }
                Command::RegisterNativeSource {
                    source_id,
                    logical_library_id,
                    name,
                    root,
                    exclude,
                    ..
                } => serde_json::to_value(
                    backend
                        .register_native_source(
                            &source_id,
                            &logical_library_id,
                            &name,
                            root,
                            exclude,
                        )
                        .await?,
                )?,
                Command::NativeSources { .. } => {
                    serde_json::to_value(backend.native_sources().await?)?
                }
                Command::AuditSources { .. } => {
                    serde_json::to_value(backend.audit_source_mappings().await?)?
                }
                Command::ScanSource {
                    source_id,
                    timeout_seconds,
                    ..
                } => {
                    let artifact = catalog_backend::observer::packaged_current_executable()?;
                    backend
                        .scan_source(
                            &source_id,
                            artifact,
                            catalog_backend::sources::ScanCancellation::default(),
                            Duration::from_secs(timeout_seconds),
                        )
                        .await?
                }
                Command::PutItems {
                    operation_id,
                    input,
                    ..
                } => {
                    let mut bytes = Vec::new();
                    regular_input(&input)?
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
                    regular_input(&input)?
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
                Command::PrepareMotionCutover { import_id, .. } => {
                    serde_json::to_value(backend.prepare_motion_cutover(&import_id).await?)?
                }
                Command::ActivateMotionCutover {
                    timeout_seconds, ..
                } => serde_json::to_value(
                    backend
                        .activate_motion_cutover(Duration::from_secs(timeout_seconds))
                        .await?,
                )?,
                Command::ReconcileMotionCutover { .. } => {
                    serde_json::to_value(backend.reconcile_motion_cutover().await?)?
                }
                Command::CaptureMotionExportPlan { root_import_id, .. } => serde_json::to_value(
                    backend.capture_motion_export_plan(&root_import_id).await?,
                )?,
                Command::PrepareMotionPartitionCutover { root_import_id, .. } => {
                    serde_json::to_value(
                        backend
                            .prepare_motion_partition_cutover(&root_import_id)
                            .await?,
                    )?
                }
                Command::ActivateMotionPartitionCutover {
                    timeout_seconds, ..
                } => serde_json::to_value(
                    backend
                        .activate_motion_partition_cutover(Duration::from_secs(timeout_seconds))
                        .await?,
                )?,
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

fn regular_input(path: &std::path::Path) -> anyhow::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    anyhow::ensure!(file.metadata()?.is_file(), "input must be a regular file");
    Ok(file)
}

#[cfg(all(test, unix))]
mod input_tests {
    use super::*;
    #[test]
    fn rejects_fifo_and_symlink_without_reading() {
        use std::os::unix::ffi::OsStrExt;
        let temp = tempfile::tempdir().unwrap();
        let fifo = temp.path().join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(regular_input(&fifo).is_err());
        let regular = temp.path().join("regular");
        std::fs::write(&regular, "{}").unwrap();
        assert!(regular_input(&regular).is_ok());
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&regular, &link).unwrap();
        assert!(regular_input(&link).is_err());
    }
}

#[cfg(test)]
mod aggregate_cli_tests {
    use super::*;
    #[test]
    fn aggregate_commands_require_explicit_source_and_root() {
        let args = Args::try_parse_from([
            "motion-catalog",
            "--catalog",
            "native.sqlite3",
            "capture-motion-export-plan",
            "--motion-db",
            "motion.sqlite3",
            "--root-import-id",
            "migration",
        ])
        .unwrap();
        assert!(
            matches!(args.command,Command::CaptureMotionExportPlan{root_import_id,..} if root_import_id=="migration")
        );
        assert!(
            Args::try_parse_from([
                "motion-catalog",
                "--catalog",
                "native.sqlite3",
                "prepare-motion-partition-cutover",
                "--motion-db",
                "motion.sqlite3"
            ])
            .is_err()
        );
        let args = Args::try_parse_from([
            "motion-catalog",
            "--catalog",
            "native.sqlite3",
            "activate-motion-partition-cutover",
            "--motion-db",
            "motion.sqlite3",
        ])
        .unwrap();
        assert!(matches!(
            args.command,
            Command::ActivateMotionPartitionCutover {
                timeout_seconds: 30,
                ..
            }
        ));
    }
}
