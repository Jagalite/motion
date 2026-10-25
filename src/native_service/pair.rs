//! Private stopped-service database-pair preparation and consumed publication.
//! Anonymous preparation has no visible path. The bounded Linux publisher may
//! produce a database-only artifact or retain uncertain publication custody;
//! neither path grants restore authority or includes referenced media bytes.
#[cfg(target_os = "linux")]
mod restore_input;
use super::*;
use motion_catalog_backend::motion_snapshot::FreshWriter;
use playscale_core::paired_backup::{Artifact, NativeIdentity, PairManifest};
#[cfg(target_os = "linux")]
pub(super) use restore_input::{RestoreProof, RestoreSupervisor};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::{Component, Path},
    sync::{Arc, atomic::AtomicBool},
};

const OVERHEAD: u64 = 1024 * 1024;

/// Private fields and constructor; only actual StoppedService can produce one.
/// It intentionally has no public file/manifest/export accessors. The private
/// publisher consumes this custody and rechecks lock/descriptors; its receipt
/// remains an unqualified database-only artifact, never restore authority.
struct PreparedPair {
    // Declaration order releases the writer before the stopped-service owner
    // closes its pool. On any failure all anonymous descriptors close normally.
    _writer: FreshWriter,
    native: catabolic_store::catalog_backup::VerifiedExport,
    motion: File,
    parent: File,
    parent_identity: (u64, u64),
    manifest: PairManifest,
    _stopped: StoppedService,
}
impl PreparedPair {
    fn retained_native_allocations(&self) -> anyhow::Result<u64> {
        self.native
            .retained_allocations()
            .checked_add(self._stopped.retained_restore_storage()?)
            .context("supervised pair allocation overflow")
    }
}
/// Even an uncertain publication retains the exact linked inode evidence and
/// stopped-service custody. No automatic retry or unlink is authorized.
#[cfg(target_os = "linux")]
struct PublishedPair {
    held: Option<PublishedCustody>,
}
#[cfg(target_os = "linux")]
struct PublishedCustody {
    publication: super::publication_file::Outcome,
    _pair: PreparedPair,
}
#[cfg(target_os = "linux")]
static PUBLICATION_RECOVERY: std::sync::OnceLock<std::sync::Mutex<Vec<PublishedCustody>>> =
    std::sync::OnceLock::new();
#[cfg(target_os = "linux")]
impl Drop for PublishedPair {
    fn drop(&mut self) {
        use playscale_core::paired_backup::publication::Phase;
        let Some(held) = self.held.take() else {
            return;
        };
        if matches!(
            held.publication.phase(),
            Phase::LinkAttempted | Phase::LinkedUnconfirmed | Phase::OutcomeUnknown
        ) {
            tracing::error!(evidence=?held.publication.evidence(), error=?held.publication.error(),
                "pair publication needs terminal recovery; retaining actual artifact and stopped custody");
            PUBLICATION_RECOVERY
                .get_or_init(Default::default)
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(held);
        }
        // Definitely absent or independently durable outcomes release normally.
        // Uncertain outcomes never release their lock/IO/native/descriptor bundle.
    }
}
#[cfg(target_os = "linux")]
impl PreparedPair {
    /// Internal consumed publisher. The blocking owner retains writer, native
    /// owner, IO and server lock if its asynchronous receiver is cancelled.
    async fn publish(
        self,
        path: PathBuf,
        name: String,
        flag: Arc<AtomicBool>,
    ) -> anyhow::Result<PublishedPair> {
        let mut cancellation = PairCancellation(flag.clone(), true);
        let result =
            tokio::task::spawn_blocking(move || self.publish_blocking(&path, &name, &flag))
                .await
                .context("pair publication coordinator failed")?;
        cancellation.1 = false;
        result
    }
    fn publish_blocking(
        self,
        path: &Path,
        name: &str,
        flag: &AtomicBool,
    ) -> anyhow::Result<PublishedPair> {
        self.publish_blocking_checked(path, name, flag, || Ok(()))
    }
    fn publish_blocking_checked(
        mut self,
        path: &Path,
        name: &str,
        flag: &AtomicBool,
        after_link: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<PublishedPair> {
        self._stopped.verify_lock()?;
        ensure!(
            identity(&self.parent)? == self.parent_identity
                && self.native.parent_identity()? == self.parent_identity,
            "prepared pair parent changed"
        );
        verify_file(&mut self.motion, &self.manifest.motion_database, flag)?;
        self.motion.seek(SeekFrom::Start(0))?;
        let expected = self.manifest.clone();
        let stage = super::publication_file::Sealed::create_checked(
            path,
            name,
            flag,
            Some(self.parent_identity),
            |output| {
                super::pair_container::write(
                    output,
                    &self.manifest,
                    flag,
                    |out| {
                        std::io::copy(&mut self.motion, out)?;
                        Ok(())
                    },
                    |mut out| {
                        self.native.copy_verified_to(&mut out, flag)?;
                        Ok(())
                    },
                )
            },
            |file| super::pair_container::read_expected(file, &expected, flag),
        )?;
        self._stopped.verify_lock()?;
        let publication = stage.publish(flag, || {
            after_link()?;
            self._stopped.verify_lock()
        });
        Ok(PublishedPair {
            held: Some(PublishedCustody {
                publication,
                _pair: self,
            }),
        })
    }
}

struct Images {
    writer: FreshWriter,
    native: catabolic_store::catalog_backup::VerifiedExport,
    motion: File,
    parent: File,
    parent_identity: (u64, u64),
    manifest: PairManifest,
}

struct ImageParts {
    native: catabolic_store::catalog_backup::VerifiedExport,
    motion: File,
    parent: File,
    parent_identity: (u64, u64),
    manifest: PairManifest,
}
impl ImageParts {
    fn with_writer(self, writer: FreshWriter) -> Images {
        Images {
            writer,
            native: self.native,
            motion: self.motion,
            parent: self.parent,
            parent_identity: self.parent_identity,
            manifest: self.manifest,
        }
    }
}

fn cancelled(flag: &AtomicBool) -> anyhow::Result<()> {
    ensure!(
        !flag.load(Ordering::Acquire),
        "anonymous pair preparation cancelled"
    );
    Ok(())
}
fn identity(file: &File) -> anyhow::Result<(u64, u64)> {
    let stat = file.metadata()?;
    Ok((stat.dev(), stat.ino()))
}
/// Every component is opened relative to the previous held descriptor. No
/// canonical pathname is ever used as a substitute for directory custody.
pub(super) fn open_parent(path: &Path) -> anyhow::Result<File> {
    let absolute = std::path::absolute(path)?;
    let mut parent = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")?;
    for component in absolute.components() {
        let Component::Normal(name) = component else {
            ensure!(
                matches!(component, Component::RootDir),
                "unsupported pair parent component"
            );
            continue;
        };
        use std::os::unix::ffi::OsStrExt;
        let name = CString::new(name.as_bytes())?;
        // SAFETY: held parent and NUL-terminated name live through synchronous call.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        ensure!(
            fd >= 0,
            "pair parent open failed: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: successful openat transferred this fresh descriptor to us.
        parent = unsafe { File::from_raw_fd(fd) };
    }
    Ok(parent)
}
use std::os::unix::fs::OpenOptionsExt;
fn anonymous(parent: &File) -> anyhow::Result<File> {
    #[cfg(not(target_os = "linux"))]
    anyhow::bail!("anonymous pair preparation requires Linux O_TMPFILE");
    #[cfg(target_os = "linux")]
    {
        // SAFETY: valid borrowed directory FD and static NUL-terminated path.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                c".".as_ptr(),
                libc::O_TMPFILE | libc::O_EXCL | libc::O_RDWR | libc::O_CLOEXEC,
                0o600,
            )
        };
        ensure!(
            fd >= 0,
            "anonymous pair stage unavailable: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: successful call returns a fresh, exclusively owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}
pub(super) fn verify_file(
    file: &mut File,
    expected: &Artifact,
    flag: &AtomicBool,
) -> anyhow::Result<()> {
    let stat = file.metadata()?;
    ensure!(
        stat.is_file() && stat.nlink() == 0 && stat.len() == expected.bytes,
        "anonymous pair stage identity/length changed"
    );
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut length = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        cancelled(flag)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        length = length
            .checked_add(count as u64)
            .context("pair stage length overflow")?;
        ensure!(length <= expected.bytes, "pair stage grew");
        digest.update(&buffer[..count]);
    }
    ensure!(
        length == expected.bytes && format!("{:x}", digest.finalize()) == expected.sha256,
        "pair stage digest mismatch"
    );
    cancelled(flag)
}

fn compose(
    native: catabolic_store::catalog_backup::VerifiedExport,
    writer: FreshWriter,
    profile: String,
    parent_path: &Path,
    flag: &AtomicBool,
) -> anyhow::Result<Images> {
    compose_checked(native, writer, profile, parent_path, flag, || Ok(()))
}
fn compose_checked(
    native: catabolic_store::catalog_backup::VerifiedExport,
    mut writer: FreshWriter,
    profile: String,
    parent_path: &Path,
    flag: &AtomicBool,
    after_capture: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<Images> {
    let parts = compose_borrowed(
        native,
        &mut writer,
        profile,
        parent_path,
        flag,
        after_capture,
        0,
    )?;
    Ok(parts.with_writer(writer))
}
fn compose_borrowed(
    mut native: catabolic_store::catalog_backup::VerifiedExport,
    writer: &mut FreshWriter,
    profile: String,
    parent_path: &Path,
    flag: &AtomicBool,
    after_capture: impl FnOnce() -> anyhow::Result<()>,
    service_storage: u64,
) -> anyhow::Result<ImageParts> {
    cancelled(flag)?;
    let parent_identity = native.parent_identity()?;
    let parent = open_parent(parent_path)?;
    ensure!(
        identity(&parent)? == parent_identity,
        "pair parent differs from verified native export"
    );
    let reserve = native
        .retained_allocations()
        .checked_add(OVERHEAD)
        .and_then(|n| n.checked_add(service_storage))
        .context("pair accounting overflow")?;
    let image = writer.capture_reserved_blocking(flag, reserve)?;
    let binding = image
        .binding()
        .context("pair requires verified durable Motion binding")?;
    let native_identity = NativeIdentity {
        catalog_id: native.catalog_id().to_owned(),
        incarnation: native.incarnation().to_owned(),
        generation: u64::try_from(native.generation()).context("negative native generation")?,
        schema_version: native.schema_version(),
        profile,
    };
    let manifest = PairManifest {
        contract_version: 1,
        pair_id: uuid::Uuid::new_v4().to_string(),
        native_identity: native_identity.clone(),
        motion_schema_version: image.schema_version(),
        motion_database: Artifact {
            bytes: image.len() as u64,
            sha256: image.sha256().to_owned(),
        },
        native_database: Artifact {
            bytes: native.bytes(),
            sha256: native.sha256().to_owned(),
        },
        motion_migration_ledger_sha256: image.migration_ledger_sha256().to_owned(),
        native_migration_ledger_sha256: native.migration_ledger_sha256().to_owned(),
        // Complete database inventory is stronger than a journal-only subset.
        // Pending command/attempt/recovery rows are retained without alteration.
        motion_control_journals_sha256: image.inventory_sha256().to_owned(),
        native_control_journals_sha256: native.inventory_sha256().to_owned(),
        motion_binding_sha256: binding.evidence_sha256().to_owned(),
    };
    manifest
        .validate(&native_identity, binding.effective_identity())
        .map_err(|e| anyhow::anyhow!("verified pair identity mismatch: {e:?}"))?;
    ensure!(
        serde_json::to_vec(&manifest)?.len() <= 65536,
        "pair manifest exceeds bound"
    );
    after_capture()?;
    // No stage is created until both databases and effective binding agree.
    cancelled(flag)?;
    ensure!(
        native.parent_identity()? == identity(&parent)?,
        "pair parent replaced before stage"
    );
    let mut motion = anonymous(&parent)?;
    image.copy_verified_to(&mut motion, flag)?;
    drop(image);
    motion.sync_all()?;
    verify_file(&mut motion, &manifest.motion_database, flag)?;
    // Reread actual native bytes, not just the receipt digest, after Motion copy.
    native.copy_verified_to(&mut std::io::sink(), flag)?;
    ensure!(
        native.parent_identity()? == identity(&parent)?,
        "pair parent changed during preparation"
    );
    cancelled(flag)?;
    Ok(ImageParts {
        native,
        motion,
        parent,
        parent_identity,
        manifest,
    })
}

impl StoppedService {
    /// Internal engineering seam only. Calling this does not publish a backup.
    async fn prepare_pair(
        mut self,
        parent: PathBuf,
        flag: Arc<AtomicBool>,
    ) -> anyhow::Result<PreparedPair> {
        let owners = self
            .owners
            .take()
            .context("stopped service already consumed")?;
        let mut cancellation = PairCancellation(flag.clone(), true);
        let result = tokio::spawn(async move {
            let mut custody = Retained::new(owners);
            custody.get_mut().lock.verify()?;
            let writer = match FreshWriter::begin(&custody.get_mut().app.db, &flag).await {
                Ok(writer) => writer,
                Err(error) => {
                    close_stopped(custody).await?;
                    return Err(error);
                }
            };
            let path = parent.clone();
            let images = custody
                .get_mut()
                .native
                .as_mut()
                .context("native terminal owner missing")?
                .with_pair_export(
                    parent,
                    flag,
                    writer,
                    move |native, writer, profile, flag| {
                        compose(native, writer, profile, &path, &flag)
                    },
                )
                .await;
            match images {
                Ok(images) => {
                    custody.get_mut().lock.verify()?;
                    let stopped = StoppedService {
                        #[cfg(target_os = "linux")]
                        supervised: None,
                        owners: Some(custody.complete()),
                    };
                    Ok(PreparedPair {
                        _writer: images.writer,
                        native: images.native,
                        motion: images.motion,
                        parent: images.parent,
                        parent_identity: images.parent_identity,
                        manifest: images.manifest,
                        _stopped: stopped,
                    })
                }
                Err(error) => {
                    // Explicit close must complete before ordinary custody release.
                    // Close failure quarantines the real lock/IO/App bundle.
                    close_stopped(custody).await?;
                    Err(error)
                }
            }
        })
        .await
        .context("anonymous pair coordinator failed")?;
        cancellation.1 = false;
        result
    }
}
struct PairCancellation(Arc<AtomicBool>, bool);
impl Drop for PairCancellation {
    fn drop(&mut self) {
        if self.1 {
            self.0.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn fixture() -> (
        tempfile::TempDir,
        Arc<crate::native_catalog::Runtime>,
        sqlx::SqlitePool,
    ) {
        let root = tempfile::tempdir().unwrap();
        let native = root.path().join("native.sqlite3");
        motion_catalog_backend::initialize(native.clone())
            .await
            .unwrap();
        let db = crate::db::connect_native_bootstrap(&root.path().join("motion.sqlite3"))
            .await
            .unwrap();
        let backend = motion_catalog_backend::NativeCatalog::bind(
            db.clone(),
            native.clone(),
            motion_catalog_backend::inspect(native.clone())
                .await
                .unwrap()
                .identity,
        )
        .await
        .unwrap();
        backend.freeze_motion_source("empty", 0).await.unwrap();
        backend.capture_motion_import("empty").await.unwrap();
        backend.stage_import("empty").await.unwrap();
        backend.prepare_motion_cutover("empty").await.unwrap();
        backend
            .activate_motion_cutover(Duration::from_secs(5))
            .await
            .unwrap();
        drop(backend);
        let runtime = crate::native_catalog::Runtime::open(db.clone(), native, "default".into())
            .await
            .unwrap();
        (root, Arc::new(runtime), db)
    }
    #[tokio::test]
    async fn actual_service_joins_ingress_refuses_clones_and_prepares_then_closes_pair() {
        actual_service_pair(0).await;
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_stopped_service_publishes_complete_verified_pair_create_only() {
        actual_service_pair(1).await;
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_pair_postlink_receiver_loss_retains_recovery_custody() {
        actual_service_pair(2).await;
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_pair_postlink_panic_retains_recovery_custody() {
        actual_service_pair(3).await;
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_published_pair_extracts_independently_verified_restore_input() {
        actual_service_pair(4).await;
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_backup_reopens_in_new_process_as_descriptive_input() {
        actual_service_pair(5).await;
    }
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "subprocess entrypoint requires an actual published pair"]
    fn selected_pair_new_process_reader() {
        let path = std::path::PathBuf::from(
            std::env::var_os("MOTION_SELECTED_PAIR_FIXTURE").expect("actual pair path required"),
        );
        restore_input::selected::exercise_process_fixture(&path);
        println!("selected-pair-process-evidence-complete");
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_pair_reserves_recorded_conversion_stage_and_refuses_replacement_restart() {
        actual_service_pair(6).await;
    }

    #[tokio::test]
    async fn actual_scanned_pair_converts_empty_nfo_custody_without_installing_or_mutating_source()
    {
        actual_service_pair(7).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_service_keeps_restore_custody_after_status_waiter_is_abandoned() {
        actual_service_pair_with_ingress(11, false).await;
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_service_closes_inline_pair_export_after_request_is_abandoned() {
        actual_service_pair_with_ingress(12, false).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_prepared_conversion_expiry_refuses_without97_ddl_or_native_effect() {
        actual_service_pair(8).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_converted_target_policy_denial_cannot_use_live_source_permission() {
        actual_service_pair(9).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_tcp_scanned_pair_converts_empty_nfo_and_relocates_without_install() {
        actual_service_pair_with_ingress(7, false).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_tcp_prepared_conversion_expiry_refuses_without97_effects() {
        actual_service_pair_with_ingress(8, false).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_tcp_converted_target_policy_denial_preserves_source_permission() {
        actual_service_pair_with_ingress(9, false).await;
    }

    async fn actual_service_pair(mode: u8) {
        actual_service_pair_with_ingress(mode, true).await;
    }

    // TCP-only fixtures qualify the actual supported optional-ingress=None
    // cohort. They do not replace or qualify the original Unix ingress tests.
    async fn actual_service_pair_with_ingress(mode: u8, unix_ingress: bool) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (root, runtime, db) = fixture().await;
        let state = root.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let cache = root.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        let app = App {
            health: Arc::new(crate::operations::Health::new(false)),
            db: db.clone(),
            native_catalog: Some(runtime.clone()),
            admin_token: Arc::new("pair-fixture-operator-secret-token".into()),
            origin: Arc::new("http://127.0.0.1:8787".into()),
            authority: Arc::new("127.0.0.1:8787".into()),
            ffprobe: Arc::new("ffprobe".into()),
            jobs: Arc::new(tokio::sync::Mutex::new(())),
            streams: Arc::new(tokio::sync::Semaphore::new(2)),
            event_streams: Arc::new(tokio::sync::Semaphore::new(4)),
            processing: Arc::new(crate::processing::Runtime::new(cache, Default::default())),
            storage: Arc::new(crate::storage::Runtime::new(
                state.clone(),
                Default::default(),
            )),
            access: Arc::new(crate::v2::Runtime::new(
                playscale_core::access::AccessMode::Restricted,
                crate::v2::auth::random_key(),
            )),
        };
        let conversion_fixture = if matches!(mode, 7 | 8 | 9 | 10 | 11) {
            let source = root.path().join("observed-source");
            std::fs::create_dir(&source).unwrap();
            std::fs::write(source.join("one.txt"), b"actual pair observation").unwrap();
            use tower::ServiceExt;
            let request=axum::http::Request::builder().method("POST").uri("/api/v2/sources")
                .header("host","127.0.0.1:8787")
                .header("authorization",format!("Bearer {}",app.admin_token))
                .header("idempotency-key","pair-conversion-public-source")
                .header("content-type","application/json")
                .body(axum::body::Body::from(serde_json::json!({"name":"Pair observed source","root_path":source,"exclusions":[]}).to_string())).unwrap();
            let response = crate::api::router(app.clone(), None)
                .oneshot(request)
                .await
                .unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap();
            assert_eq!(
                status,
                axum::http::StatusCode::CREATED,
                "{}",
                String::from_utf8_lossy(&bytes)
            );
            let response: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let source_id = response["id"].as_str().unwrap().to_owned();
            use catabolic_runtime::helper::{
                ClosedObservationProtocol, TrustedClosedObservationArtifact,
            };
            let binary = PathBuf::from(
                std::env::var_os("CATABOLIC_HOST_TEST_HELPER")
                    .expect("actual packaging-verified helper required"),
            );
            let grant = TrustedClosedObservationArtifact::after_packaging_verification(
                &binary,
                &catabolic_core::sha256(std::fs::read(&binary).unwrap()),
                ClosedObservationProtocol::NoChildWithExactJoinV2,
            )
            .unwrap();
            runtime.register_observer(grant).await.unwrap();
            crate::native_discovery::scan(&app, &source_id, CancellationToken::new())
                .await
                .unwrap();
            if mode == 7 {
                // Test-only evidence from the actual produced native file, not
                // a new admission or a modification of enrolled volume facts.
                use sqlx::Connection;
                let encoded: String = sqlx::query_scalar(
                    "SELECT mapping_json FROM catabolic_source_mappings WHERE library_id=?",
                )
                .bind(&source_id)
                .fetch_one(&db)
                .await
                .unwrap();
                let mapping: motion_catalog_backend::sources::SourceMapping =
                    serde_json::from_str(&encoded).unwrap();
                let mut native = sqlx::SqliteConnection::connect_with(
                    &sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(runtime.path())
                        .read_only(true),
                )
                .await
                .unwrap();
                let mut read = native.begin().await.unwrap();
                let enrollment: Vec<(String, Option<i64>, String)> = sqlx::query_as(
                    "SELECT typeof(volume_uuid),length(CAST(volume_uuid AS BLOB)),CASE WHEN length(CAST(volume_uuid AS BLOB))<=128 THEN quote(volume_uuid) ELSE '<oversized>' END FROM main.binding_volumes WHERE profile=? AND kind='source' AND owner=? LIMIT 2",
                ).bind(&mapping.profile).bind(&mapping.native_source)
                    .fetch_all(&mut *read).await.unwrap();
                let policy: Option<(String, String, i64)> = sqlx::query_as(
                    "SELECT identity_policy,CASE WHEN length(CAST(settings AS BLOB))<=4096 THEN settings ELSE '<oversized>' END,revision FROM main.source_identity_policies WHERE profile=? AND location=?",
                ).bind(&mapping.profile).bind(&mapping.native_source)
                    .fetch_optional(&mut *read).await.unwrap();
                eprintln!(
                    "actual mode7 volume enrollment={enrollment:?}; identity policy={policy:?}"
                );
                assert!(
                    enrollment.is_empty(),
                    "mode7 requires actual not-enrolled UUID evidence, observed {enrollment:?}"
                );
                let effective_uuid = if let Some((preset, settings, _)) = policy {
                    let settings: serde_json::Value = serde_json::from_str(&settings).unwrap();
                    settings.get("uuid").cloned().unwrap_or_else(|| {
                        serde_json::json!(if preset == "path" { "skip" } else { "check" })
                    })
                } else {
                    serde_json::json!("check")
                };
                assert_eq!(effective_uuid, serde_json::json!("check"));
                read.rollback().await.unwrap();
                native.close().await.unwrap();
            }
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                axum::http::header::AUTHORIZATION,
                format!("Bearer {}", app.admin_token).parse().unwrap(),
            );
            let caller = crate::v2::auth::resolve(&app, &axum::http::Method::GET, &headers)
                .await
                .unwrap();
            let candidate = root.path().join("relocated-observed-source");
            std::fs::create_dir(&candidate).unwrap();
            std::fs::copy(source.join("one.txt"), candidate.join("one.txt")).unwrap();
            Some((caller, source_id, candidate))
        } else {
            None
        };
        let lock_path = state.join("server.lock");
        let lock = File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .unwrap();
        let competing = File::options()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let ingress = if unix_ingress {
            Some(UnixListener::bind(state.join("ingress.sock")).unwrap())
        } else {
            None
        };
        let startup = super::super::startup_observation::observe(&app).unwrap();
        let original_source_fd = lock.as_raw_fd();
        let mut service = Some(
            NativeService::start(
                app.clone(),
                None,
                listener,
                ingress,
                lock,
                CancellationToken::new(),
            )
            .unwrap(),
        );
        // Actual TCP owner handles a request; stop must also join the registered
        // optional Unix ingress, metrics sampler and all seven genuine worker tasks.
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        client
            .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1:8787\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert!(response.starts_with(b"HTTP/1.1"));
        tokio::time::timeout(Duration::from_secs(10), startup.wait())
            .await
            .unwrap()
            .unwrap();
        let expiry_fixture = if mode == 8 {
            Some(crate::v2::auth::retained::actual_expiring_admin(&app).await)
        } else {
            None
        };
        let policy_fixture = if mode == 9 {
            Some((
                crate::v2::auth::retained::actual_paired_admin(&app).await,
                crate::v2::auth::retained::actual_paired_admin(&app).await,
            ))
        } else {
            None
        };
        let historical_fixture = if mode == 10 {
            let requester = crate::v2::auth::retained::actual_paired_admin(&app).await;
            let (_, reserve_token) =
                crate::v2::auth::retained::actual_paired_admin_with_bearer(&app).await;
            Some((requester, reserve_token))
        } else {
            None
        };
        let stopped = if matches!(mode, 11 | 12) {
            // Abandon a polled stop waiter. Actual service fields still own all
            // role handles and source custody; no StoppedService was returned.
            {
                use std::future::Future;
                let mut waiter = std::pin::pin!(service.as_mut().unwrap().stop_for_restore());
                std::future::poll_fn(|cx| {
                    assert!(waiter.as_mut().poll(cx).is_pending());
                    std::task::Poll::Ready(())
                })
                .await;
            }
            assert!(service.as_ref().unwrap().owners.is_some());
            assert!(fs2::FileExt::try_lock_exclusive(&competing).is_err());
            service.as_mut().unwrap().stop_for_restore().await.unwrap();
            None
        } else {
            Some(service.take().unwrap().stop().await.unwrap())
        };
        assert!(
            app.native_catalog
                .as_ref()
                .unwrap()
                .with_host(|_| Ok(()))
                .await
                .is_err()
        );
        assert!(
            app.processing
                .execution
                .reserve(
                    "after-stop".into(),
                    playscale_core::work::Class::Maintenance,
                    1
                )
                .await
                .is_err()
        );
        assert!(
            app.processing
                .hardware
                .reserve(
                    "after-stop-hw".into(),
                    playscale_core::work::Class::Preparation,
                    1
                )
                .await
                .is_err()
        );
        assert!(fs2::FileExt::try_lock_exclusive(&competing).is_err());
        assert!(app.processing.io.clone().try_acquire_owned().is_err());
        let destination = root.path().join("pair");
        std::fs::create_dir(&destination).unwrap();
        if mode == 12 {
            use std::future::Future;
            let flag = Arc::new(AtomicBool::new(false));
            let started = std::cell::Cell::new(false);
            {
                let mut request = std::pin::pin!(
                    service
                        .as_mut()
                        .unwrap()
                        .restore
                        .prepare_pair_for_abandonment_fixture(
                            destination.clone(),
                            flag.clone(),
                            &started
                        )
                );
                std::future::poll_fn(|cx| {
                    assert!(request.as_mut().poll(cx).is_pending());
                    if started.get() {
                        std::task::Poll::Ready(())
                    } else {
                        std::task::Poll::Pending
                    }
                })
                .await;
            }
            assert!(flag.load(Ordering::Acquire));
            service
                .as_ref()
                .unwrap()
                .restore
                .assert_inline_export_for_fixture(original_source_fd);
            assert!(fs2::FileExt::try_lock_exclusive(&competing).is_err());
            service.as_mut().unwrap().close_restore().await.unwrap();
            assert!(app.db.is_closed());
            assert!(app.processing.io.clone().try_acquire_owned().is_ok());
            fs2::FileExt::try_lock_exclusive(&competing).unwrap();
            fs2::FileExt::unlock(&competing).unwrap();
            assert!(
                app.native_catalog
                    .as_ref()
                    .unwrap()
                    .with_host(|_| Ok(()))
                    .await
                    .is_err()
            );
            db.close().await;
            std::fs::remove_dir_all(root).unwrap();
            return;
        }
        let prepared = if mode == 11 {
            service
                .as_mut()
                .unwrap()
                .prepare_pair_for_restore(destination.clone(), Arc::new(AtomicBool::new(false)))
                .await
                .unwrap();
            // Test-only producer bridge: production preparation returns status,
            // never a pair/custody result to an abandonable request.
            service.as_mut().unwrap().restore.take_pair_for_fixture()
        } else {
            stopped
                .unwrap()
                .prepare_pair(destination.clone(), Arc::new(AtomicBool::new(false)))
                .await
                .unwrap()
        };
        assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
        assert!(fs2::FileExt::try_lock_exclusive(&competing).is_err());
        if mode > 0 {
            #[cfg(target_os = "linux")]
            {
                let expected = prepared.manifest.clone();
                let published = if matches!(mode, 2 | 3) {
                    let path = destination.clone();
                    tokio::task::spawn_blocking(move || {
                        let flag = AtomicBool::new(false);
                        prepared.publish_blocking_checked(&path, "verified.pair", &flag, || {
                            if mode == 3 {
                                panic!("injected panic after actual visible link");
                            }
                            flag.store(true, Ordering::Release);
                            anyhow::bail!(
                                "injected directory durability failure after visible link"
                            )
                        })
                    })
                    .await
                    .unwrap()
                    .unwrap()
                } else {
                    prepared
                        .publish(
                            destination.clone(),
                            "verified.pair".into(),
                            Arc::new(AtomicBool::new(false)),
                        )
                        .await
                        .unwrap()
                };
                let result = &published.held.as_ref().unwrap().publication;
                assert_eq!(
                    result.phase(),
                    if matches!(mode, 2 | 3) {
                        playscale_core::paired_backup::publication::Phase::LinkedUnconfirmed
                    } else {
                        playscale_core::paired_backup::publication::Phase::Durable
                    }
                );
                assert_eq!(result.error().is_some(), matches!(mode, 2 | 3));
                assert!(fs2::FileExt::try_lock_exclusive(&competing).is_err());
                let mut named = File::open(destination.join("verified.pair")).unwrap();
                assert_eq!(
                    super::super::pair_container::read(&mut named, &AtomicBool::new(false))
                        .unwrap(),
                    expected
                );
                assert_eq!(
                    named.metadata().unwrap().len(),
                    result.evidence().artifact.bytes
                );
                if mode == 10 {
                    let (requester, reserve_token) = historical_fixture.unwrap();
                    historical_grant_with_current_revocation(
                        root.path(),
                        &app,
                        &competing,
                        published,
                        requester,
                        reserve_token,
                    )
                    .await;
                } else if matches!(mode, 4 | 6 | 7 | 8 | 9 | 11) {
                    let inspected = published
                        .inspect_restore_input(Arc::new(AtomicBool::new(false)))
                        .await
                        .unwrap();
                    assert_eq!(inspected.manifest(), &expected);
                    assert!(fs2::FileExt::try_lock_exclusive(&competing).is_err());
                    assert!(app.processing.io.clone().try_acquire_owned().is_err());
                    assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 1);
                    let target_dir = tempfile::tempdir().unwrap();
                    let target = inspected
                        .reserve_fresh_target(target_dir.path(), &AtomicBool::new(false))
                        .unwrap();
                    target.assert_reserved();
                    assert!(fs2::FileExt::try_lock_exclusive(&competing).is_err());
                    assert!(app.processing.io.clone().try_acquire_owned().is_err());
                    if matches!(mode, 7 | 11) {
                        let (caller, source_id, candidate) = conversion_fixture.unwrap();
                        let actor = target
                            .stage_native_conversion(&AtomicBool::new(false))
                            .unwrap()
                            .into_source_actor(caller.clone(), Arc::new(AtomicBool::new(false)))
                            .await
                            .unwrap()
                            .exercise_actual_relocation_fixture(
                                caller.clone(),
                                source_id,
                                candidate,
                            )
                            .await;
                        if mode == 7 {
                            actor
                                .exercise_current_image_fixture(caller)
                                .await
                                .exercise_converted_fixture()
                                .await;
                        } else {
                            let image = actor
                                .into_post_relocation_image(
                                    caller.clone(),
                                    Arc::new(AtomicBool::new(false)),
                                )
                                .await
                                .unwrap();
                            let supervisor = &mut service.as_mut().unwrap().restore;
                            supervisor.stage_image_for_fixture(image);
                            supervisor
                                .exercise_abandoned_waiter(
                                    caller,
                                    target_dir.path(),
                                    &competing,
                                    original_source_fd,
                                )
                                .await;
                        }
                    } else if mode == 9 {
                        let (_, source_id, candidate) = conversion_fixture.unwrap();
                        let (requester, reserve_admin) = policy_fixture.unwrap();
                        target
                            .stage_native_conversion(&AtomicBool::new(false))
                            .unwrap()
                            .into_source_actor(requester.clone(), Arc::new(AtomicBool::new(false)))
                            .await
                            .unwrap()
                            .exercise_actual_target_policy_denial(
                                requester,
                                reserve_admin,
                                source_id,
                                candidate,
                            )
                            .await;
                    } else if mode == 8 {
                        let (caller, expiry) = expiry_fixture.unwrap();
                        target
                            .stage_native_conversion(&AtomicBool::new(false))
                            .unwrap()
                            .exercise_actual_prepared_expiry(caller, expiry)
                            .await;
                    } else if mode == 6 {
                        target
                            .stage_native_conversion(&AtomicBool::new(false))
                            .unwrap()
                            .exercise_record_and_replacement();
                    } else {
                        drop(target);
                    }
                    // Dropping a reserved target never erases its recovery marker.
                    assert!(
                        target_dir
                            .path()
                            .join("paired-install.reservation.json")
                            .is_file()
                    );
                } else {
                    let exact = result.evidence().clone();
                    // Simulate the completed blocking result losing its async receiver.
                    let (send, receive) = tokio::sync::oneshot::channel();
                    drop(receive);
                    drop(send.send(published));
                    if matches!(mode, 2 | 3) {
                        assert!(fs2::FileExt::try_lock_exclusive(&competing).is_err());
                        assert!(app.processing.io.clone().try_acquire_owned().is_err());
                        let held = {
                            let mut recovery = PUBLICATION_RECOVERY.get().unwrap().lock().unwrap();
                            let index = recovery
                                .iter()
                                .position(|held| held.publication.evidence() == &exact)
                                .unwrap();
                            recovery.remove(index)
                        };
                        assert_eq!(held.publication.evidence(), &exact);
                        assert!(destination.join("verified.pair").is_file());
                        // Test-only cleanup after inspecting exact retained custody.
                        // Production has no recovery/removal or blind retry API.
                        drop(held);
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            unreachable!();
        } else {
            drop(prepared);
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if fs2::FileExt::try_lock_exclusive(&competing).is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(db.is_closed());
        assert!(app.processing.io.clone().try_acquire_owned().is_ok());
        assert!(runtime.with_host(|_| Ok(())).await.is_err());
        if mode == 5 {
            // Source service owners/pool are genuinely closed before a distinct
            // process opens the persisted artifact. No PublishedPair token crosses.
            let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "native_service::pair::tests::selected_pair_new_process_reader",
                    "--ignored",
                    "--nocapture",
                ])
                .env(
                    "MOTION_SELECTED_PAIR_FIXTURE",
                    destination.join("verified.pair"),
                )
                .kill_on_drop(true);
            let output = tokio::time::timeout(Duration::from_secs(120), command.output())
                .await
                .unwrap()
                .unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout)
                        .contains("selected-pair-process-evidence-complete"),
                "new-process pair reader failed or did not execute: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert_eq!(
            std::fs::read_dir(destination).unwrap().count(),
            usize::from(mode > 0)
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_tcp_historical_target_grant_cannot_restore_current_revoked_caller() {
        actual_service_pair_with_ingress(10, false).await;
    }

    #[cfg(target_os = "linux")]
    async fn historical_grant_with_current_revocation(
        root: &Path,
        old: &App,
        competing: &File,
        mut published: PublishedPair,
        requester: crate::v2::auth::Caller,
        reserve_token: String,
    ) {
        use tower::ServiceExt;
        let archived = root.join("pair/verified.pair");
        let original_archive = catabolic_core::sha256(std::fs::read(&archived).unwrap());
        // Fixture-only release of this verified durable publication's exact
        // owners. Join the real stopped-service close, not an elapsed-time proxy.
        let PublishedCustody { publication, _pair } = published.held.take().unwrap();
        let PreparedPair {
            _writer,
            native,
            motion,
            parent,
            _stopped,
            ..
        } = _pair;
        drop((_writer, native, motion, parent));
        _stopped.close().await.unwrap();
        assert!(old.db.is_closed());
        assert!(fs2::FileExt::try_lock_exclusive(competing).is_ok());
        assert!(old.processing.io.clone().try_acquire_owned().is_ok());
        fs2::FileExt::unlock(competing).unwrap();
        let db = crate::db::connect_catalog_control(&root.join("motion.sqlite3"))
            .await
            .unwrap();
        let native = Arc::new(
            crate::native_catalog::Runtime::open(
                db.clone(),
                root.join("native.sqlite3"),
                "default".into(),
            )
            .await
            .unwrap(),
        );
        let app = App {
            health: Arc::new(crate::operations::Health::new(false)),
            db,
            native_catalog: Some(native),
            jobs: Arc::new(tokio::sync::Mutex::new(())),
            streams: Arc::new(tokio::sync::Semaphore::new(2)),
            event_streams: Arc::new(tokio::sync::Semaphore::new(4)),
            processing: Arc::new(crate::processing::Runtime::new(
                root.join("cache"),
                Default::default(),
            )),
            storage: Arc::new(crate::storage::Runtime::new(
                root.join("state"),
                Default::default(),
            )),
            access: Arc::new(crate::v2::Runtime::new(
                playscale_core::access::AccessMode::Restricted,
                old.access.key,
            )),
            ..old.clone()
        };
        assert!(!Arc::ptr_eq(
            &app.processing.artifacts,
            &old.processing.artifacts
        ));
        assert!(!Arc::ptr_eq(
            &app.processing.execution,
            &old.processing.execution
        ));
        let lock = File::options()
            .read(true)
            .write(true)
            .open(root.join("state/server.lock"))
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let startup = super::super::startup_observation::observe(&app).unwrap();
        let service = NativeService::start(
            app.clone(),
            None,
            listener,
            None,
            lock,
            CancellationToken::new(),
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(10), startup.wait())
            .await
            .unwrap()
            .unwrap();
        let path = format!(
            "/api/v2/devices/{}",
            requester.credential().unwrap().1.device_id
        );
        let request = |method: &str, etag: Option<&str>| {
            let mut builder = axum::http::Request::builder()
                .method(method)
                .uri(&path)
                .header("host", app.authority.as_str())
                .header("authorization", format!("Bearer {reserve_token}"));
            if let Some(etag) = etag {
                builder = builder.header("if-match", etag);
            }
            builder.body(axum::body::Body::empty()).unwrap()
        };
        let response = crate::api::router(app.clone(), None)
            .oneshot(request("GET", None))
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let etag = response.headers()["etag"].to_str().unwrap().to_owned();
        drop(response);
        let response = crate::api::router(app.clone(), None)
            .oneshot(request("DELETE", Some(&etag)))
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(
            status,
            axum::http::StatusCode::NO_CONTENT,
            "{}",
            String::from_utf8_lossy(&body)
        );
        // Actual current auth is gone before acquiring the fresh source writer.
        let mut writer = app.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let denied = requester
            .reauthorize(
                &mut writer,
                Some(playscale_core::access::Permission::SystemAdmin),
            )
            .await
            .unwrap_err();
        assert_eq!(denied.status, axum::http::StatusCode::UNAUTHORIZED);
        writer.rollback().await.unwrap();
        let stopped = service.stop().await.unwrap();
        let fresh = root.join("fresh-pair");
        std::fs::create_dir(&fresh).unwrap();
        let staging = root.join("selected-staging");
        std::fs::create_dir(&staging).unwrap();
        let selected = stopped
            .prepare_pair(fresh, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap()
            .select_previous(archived.clone(), staging, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        drop(publication); // selected input now independently pins the archive.
        assert!(fs2::FileExt::try_lock_exclusive(competing).is_err());
        assert!(app.processing.io.clone().try_acquire_owned().is_err());
        let target = tempfile::tempdir().unwrap();
        selected
            .reserve_fresh_target(target.path(), &AtomicBool::new(false))
            .unwrap()
            .stage_native_conversion(&AtomicBool::new(false))
            .unwrap()
            .exercise_actual_current_revocation(requester)
            .await;
        assert_eq!(
            catabolic_core::sha256(std::fs::read(archived).unwrap()),
            original_archive
        );
        assert!(app.db.is_closed());
    }

    #[tokio::test]
    async fn real_terminal_pair_is_anonymous_verified_and_retains_motion_writer() {
        let (root, runtime, db) = fixture().await;
        let destination = root.path().join("pair");
        std::fs::create_dir(&destination).unwrap();
        let mut terminal = runtime.close_admission_for_stopped_service().await.unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let writer = FreshWriter::begin(&db, &flag).await.unwrap();
        let path = destination.clone();
        let mut images = terminal
            .with_pair_export(
                destination.clone(),
                flag,
                writer,
                move |native, writer, profile, flag| compose(native, writer, profile, &path, &flag),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
        assert_eq!(images.manifest.native_identity.profile, "default");
        assert_eq!(images.parent_identity, identity(&images.parent).unwrap());
        verify_file(
            &mut images.motion,
            &images.manifest.motion_database,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), db.begin_with("BEGIN IMMEDIATE"))
                .await
                .is_err()
        );
        images.motion.seek(SeekFrom::Start(0)).unwrap();
        images.motion.write_all(b"corrupt").unwrap();
        assert!(
            verify_file(
                &mut images.motion,
                &images.manifest.motion_database,
                &AtomicBool::new(false)
            )
            .is_err()
        );
        drop(images);
        db.begin_with("BEGIN IMMEDIATE")
            .await
            .unwrap()
            .rollback()
            .await
            .unwrap();
        assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
        terminal.close().await.unwrap();
        db.close().await;
    }
    #[tokio::test]
    async fn pair_failure_after_capture_closes_only_anonymous_owned_images() {
        let (root, runtime, db) = fixture().await;
        let destination = root.path().join("pair");
        std::fs::create_dir(&destination).unwrap();
        let mut terminal = runtime.close_admission_for_stopped_service().await.unwrap();
        for replace in [false, true] {
            let flag = Arc::new(AtomicBool::new(false));
            let writer = FreshWriter::begin(&db, &flag).await.unwrap();
            let path = destination.clone();
            let result = terminal
                .with_pair_export(
                    destination.clone(),
                    flag,
                    writer,
                    move |native, writer, profile, flag| {
                        let hook_path = path.clone();
                        let hook_flag = flag.clone();
                        compose_checked(native, writer, profile, &path, &flag, move || {
                            if replace {
                                std::fs::rename(&hook_path, hook_path.with_extension("old"))?;
                                std::fs::create_dir(&hook_path)?;
                                std::fs::write(hook_path.join("unowned"), b"preserve")?;
                            } else {
                                hook_flag.store(true, Ordering::Release);
                            }
                            Ok(())
                        })
                    },
                )
                .await;
            assert!(result.is_err());
            db.begin_with("BEGIN IMMEDIATE")
                .await
                .unwrap()
                .rollback()
                .await
                .unwrap();
            if replace {
                assert_eq!(
                    std::fs::read(destination.join("unowned")).unwrap(),
                    b"preserve"
                );
                assert_eq!(
                    std::fs::read_dir(destination.with_extension("old"))
                        .unwrap()
                        .count(),
                    0
                );
            } else {
                assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
            }
        }
        assert!(runtime.with_host(|_| Ok(())).await.is_err());
        terminal.close().await.unwrap();
        db.close().await;
    }
    #[tokio::test]
    async fn different_genuine_motion_binding_refuses_before_pair_stage() {
        let (root, runtime, db) = fixture().await;
        let other = root.path().join("other-native.sqlite3");
        motion_catalog_backend::initialize(other.clone())
            .await
            .unwrap();
        let other_db = crate::db::connect(&root.path().join("other-motion.sqlite3"))
            .await
            .unwrap();
        let other_owner = motion_catalog_backend::NativeCatalog::bind(
            other_db.clone(),
            other.clone(),
            motion_catalog_backend::inspect(other)
                .await
                .unwrap()
                .identity,
        )
        .await
        .unwrap();
        let destination = root.path().join("pair");
        std::fs::create_dir(&destination).unwrap();
        let mut terminal = runtime.close_admission_for_stopped_service().await.unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let writer = FreshWriter::begin(&other_db, &flag).await.unwrap();
        let path = destination.clone();
        assert!(
            terminal
                .with_pair_export(
                    destination.clone(),
                    flag,
                    writer,
                    move |native, writer, profile, flag| compose(
                        native, writer, profile, &path, &flag
                    )
                )
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_dir(destination).unwrap().count(), 0);
        other_db
            .begin_with("BEGIN IMMEDIATE")
            .await
            .unwrap()
            .rollback()
            .await
            .unwrap();
        drop(other_owner);
        terminal.close().await.unwrap();
        other_db.close().await;
        db.close().await;
    }
}
