//! Private generated-output observation. This is not a public source registry.
//! The caller owns the Motion writer and retained HostSession through native
//! observation and receipt-backed publication, including cancellation cleanup.
use crate::{native_discovery, scan};
use anyhow::{Context, ensure};
use catabolic_runtime::host::HostSession;
use motion_catalog_backend::sources::{ScanCancellation, SourceMapping};
use serde_json::{Value, json};
use sqlx::SqliteConnection;
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

pub(crate) const SOURCE_ID: &str = motion_catalog_backend::generated_source::SOURCE_ID;

/// Constructed only by the retained owner and exact immutable journal ACK.
/// It carries no logical-library membership or public source authority.
pub(crate) struct GeneratedRoot {
    mapping: SourceMapping,
}
impl GeneratedRoot {
    pub(crate) fn mapping(&self) -> &SourceMapping {
        &self.mapping
    }
    pub(crate) fn from_retained_verified_mapping(
        host: &HostSession,
        configured_root: &Path,
        mapping: SourceMapping,
    ) -> anyhow::Result<Self> {
        ensure!(
            mapping.library_id == SOURCE_ID
                && mapping.native_source == catabolic_core::sha256(SOURCE_ID.as_bytes())
                && mapping.motion_binding_revision == 1
                && mapping.exclusions.is_empty(),
            "invalid private generated source identity"
        );
        ensure!(
            configured_root.canonicalize()? == PathBuf::from(&mapping.motion_root),
            "configured generated root differs from retained mapping"
        );
        verify_root(host, &mapping)?;
        Ok(Self { mapping })
    }
}

pub(crate) async fn mapping_in(db: &mut SqliteConnection) -> anyhow::Result<Option<SourceMapping>> {
    motion_catalog_backend::generated_source::mapping_in(db).await
}

/// Native registration precedes the immutable host ACK. The fixed source name
/// cannot adopt a different configured root on retry. Caller commits this writer
/// before relying on the mapping in a durable publication command intent.
pub(crate) async fn register_output_root(
    db: &mut SqliteConnection,
    host: &mut HostSession,
    configured_root: &Path,
) -> anyhow::Result<GeneratedRoot> {
    let canonical = configured_root.canonicalize()?;
    let public_mappings: Vec<String> =
        sqlx::query_scalar("SELECT mapping_json FROM catabolic_source_mappings")
            .fetch_all(&mut *db)
            .await?;
    for value in public_mappings {
        let public: SourceMapping = serde_json::from_str(&value)?;
        let public_root = Path::new(&public.motion_root);
        ensure!(
            !canonical.starts_with(public_root) && !public_root.starts_with(&canonical),
            "private generated root overlaps a public source root"
        );
    }
    let prepared = motion_catalog_backend::source_registry::register_with_host(
        host,
        SOURCE_ID,
        "motion:private-generated",
        "Generated media",
        configured_root.to_owned(),
        vec![],
    )?;
    let mapping = prepared.mapping().clone();
    if let Some(saved) = mapping_in(db).await? {
        ensure!(
            saved == mapping,
            "private generated source differs from immutable journal"
        );
    } else {
        sqlx::query("INSERT INTO catabolic_generated_source(id,mapping_json,created_at) VALUES (?,?,unixepoch())")
            .bind(SOURCE_ID).bind(serde_json::to_string(&mapping)?).execute(&mut *db).await?;
    }
    GeneratedRoot::from_retained_verified_mapping(host, configured_root, mapping)
}

fn verify_root(host: &HostSession, mapping: &SourceMapping) -> anyhow::Result<()> {
    motion_catalog_backend::generated_source::verify_with_host(host, mapping)
}

/// This proof owns the actual read-only completed output descriptor. It cannot
/// be deserialized from a journal or manufactured from IDs/probe JSON alone.
pub(crate) struct GeneratedOccurrenceProof {
    source: SourceMapping,
    observed: Value,
    probe: Value,
    file: Arc<File>,
}
impl GeneratedOccurrenceProof {
    pub(crate) fn source(&self) -> &SourceMapping {
        &self.source
    }
    pub(crate) fn observed(&self) -> &Value {
        &self.observed
    }
    pub(crate) fn probe(&self) -> &Value {
        &self.probe
    }
    pub(crate) fn file(&self) -> Arc<File> {
        self.file.clone()
    }
}

/// Journal evidence is not yet authority. Only this loader can construct the
/// private record; recovery must independently validate its bytes and native
/// occurrence before it obtains a retained descriptor proof.
pub(crate) struct SavedOutput {
    source: SourceMapping,
    relative: PathBuf,
    observed: Value,
    probe: Value,
}

pub(crate) async fn load_saved_output_in(
    db: &mut SqliteConnection,
    operation_id: &str,
) -> anyhow::Result<SavedOutput> {
    ensure!(
        !operation_id.is_empty() && operation_id.len() <= 256,
        "invalid generated recovery identity"
    );
    let (relative, encoded, source): (String, String, String) = sqlx::query_as(
        "SELECT output_relative,output_json,source_json FROM catabolic_generated_publications \
         WHERE operation_id=? AND command_json IS NOT NULL AND acknowledged=0",
    )
    .bind(operation_id)
    .fetch_one(&mut *db)
    .await?;
    ensure!(
        encoded.len() <= 4 * 1024 * 1024 && source.len() <= 1024 * 1024,
        "saved generated recovery evidence exceeds bound"
    );
    let output: Value = serde_json::from_str(&encoded)?;
    let original: Value = serde_json::from_str(&source)?;
    let source: SourceMapping = serde_json::from_value(output["mapping"].clone())?;
    let registered = mapping_in(db)
        .await?
        .context("saved generated source ACK missing")?;
    ensure!(
        source == registered,
        "saved generated output uses another private source mapping"
    );
    ensure!(
        original["contract_version"] == 1
            && original["admission"]["catalog_id"].as_str()
                == Some(source.catalog.catalog_id.as_str())
            && original["admission"]["incarnation"].as_str()
                == Some(source.catalog.incarnation.as_str()),
        "saved generated output belongs to another admission incarnation"
    );
    ensure!(
        relative.len() <= 1024 && !relative.contains('\\'),
        "saved generated recovery locator exceeds the committed path contract"
    );
    let relative = PathBuf::from(relative);
    ensure!(
        relative.components().count() == 1
            && matches!(
                relative.components().next(),
                Some(std::path::Component::Normal(_))
            ),
        "saved generated recovery locator must be a flat filename"
    );
    let text = relative
        .to_str()
        .context("saved generated locator must be UTF-8")?;
    ensure!(
        output["probe"]["relative_path"].as_str() == Some(text)
            && output["observed"]["path"].as_str() == Some(text)
            && output["observed"]["status"] == "present"
            && native_discovery::current_matches(&output["probe"], &output["observed"]),
        "saved generated probe, occurrence, and committed locator differ"
    );
    Ok(SavedOutput {
        source,
        relative,
        observed: output["observed"].clone(),
        probe: output["probe"].clone(),
    })
}

/// Run in the retained host's blocking worker. The caller retains its actual
/// Motion writer and processing I/O custody through this check and dispatch.
/// No new observation, output copy, logical identity, or receipt is created.
pub(crate) fn revalidate_saved_output<P: Send + 'static>(
    host: &HostSession,
    root: &GeneratedRoot,
    saved: SavedOutput,
    cancellation: &tokio_util::sync::CancellationToken,
    permit: P,
) -> anyhow::Result<GeneratedOccurrenceProof> {
    let _custody = permit;
    ensure!(
        !cancellation.is_cancelled(),
        "generated recovery validation cancelled"
    );
    ensure!(
        saved.source == root.mapping,
        "saved output differs from retained private root"
    );
    verify_root(host, &root.mapping)?;
    let file = scan::open_file(Path::new(&root.mapping.motion_root), &saved.relative)?;
    let file = verify_saved_bytes(file, &saved.probe, cancellation)?;
    let proof = GeneratedOccurrenceProof {
        source: saved.source,
        observed: saved.observed,
        probe: saved.probe,
        file,
    };
    verify_current(host, &proof)?;
    ensure!(
        !cancellation.is_cancelled(),
        "generated recovery validation cancelled after verification"
    );
    Ok(proof)
}

fn verify_saved_bytes(
    file: File,
    probe: &Value,
    cancellation: &tokio_util::sync::CancellationToken,
) -> anyhow::Result<Arc<File>> {
    let expected = probe["fingerprint"]
        .as_str()
        .context("saved output fingerprint missing")?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && scan::fingerprint(&metadata) == expected,
        "saved output full fingerprint changed before recovery"
    );
    #[cfg(unix)]
    {
        use std::os::{fd::AsRawFd, unix::fs::MetadataExt};
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        ensure!(
            flags >= 0 && flags & libc::O_ACCMODE == libc::O_RDONLY && metadata.nlink() == 1,
            "saved output must retain one read-only independent final inode"
        );
    }
    let size = probe["bytes"]
        .as_u64()
        .filter(|n| *n > 0)
        .context("saved output positive size missing")?;
    let sha = probe["revision"]
        .as_str()
        .context("saved output SHA256 missing")?;
    ensure!(
        sha.len() == 64
            && sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "saved output SHA256 malformed"
    );
    let verified = verify_materialized(file, sha, size, cancellation)?;
    ensure!(
        verified.fingerprint == expected,
        "saved output changed during independent recovery hash"
    );
    Ok(verified.file)
}

/// `found` must be the validated probe of the FINAL no-clobber output, not its
/// staging predecessor: copying/linking may alter the full fingerprint. Only
/// the selected exact output occurrence is returned; sibling output inventory
/// does not create catalog entities or grant publication authority.
pub(crate) fn observe_output<P: Send + 'static>(
    host: &mut HostSession,
    root: &GeneratedRoot,
    relative: &Path,
    found: &scan::Found,
    cancellation: &ScanCancellation,
    timeout: Duration,
    permit: P,
) -> anyhow::Result<GeneratedOccurrenceProof> {
    ensure!(
        !cancellation.is_cancelled(),
        "generated observation cancelled"
    );
    verify_root(host, &root.mapping)?;
    let relative_text = relative
        .to_str()
        .context("generated output path must be UTF-8")?;
    ensure!(
        relative_text == found.relative,
        "completed output probe addresses another path"
    );
    let file = scan::open_file(Path::new(&root.mapping.motion_root), relative)?;
    ensure!(
        scan::fingerprint(&file.metadata()?) == found.fingerprint,
        "completed generated output changed before observation"
    );
    let report = motion_catalog_backend::sources::scan_with_host(
        host,
        &root.mapping,
        cancellation,
        timeout,
        permit,
    )?;
    ensure!(
        report["complete"] == true,
        "generated output observation coverage is incomplete"
    );
    verify_root(host, &root.mapping)?;
    let snapshot = host.source_occurrences(&root.mapping.native_source)?;
    verify_snapshot(&root.mapping, &snapshot)?;
    let candidates: Vec<_> = snapshot["occurrences"]
        .as_array()
        .context("generated native occurrences missing")?
        .iter()
        .filter(|row| row["path"].as_str() == Some(relative_text) && row["status"] == "present")
        .collect();
    ensure!(
        candidates.len() == 1,
        "completed generated output has no unique present native occurrence"
    );
    let observed = candidates[0].clone();
    let probe = json!({"title":found.title,"relative_path":found.relative,"revision":found.revision,
        "fingerprint":found.fingerprint,"bytes":found.bytes,"duration_seconds":found.duration,
        "tracks":found.tracks,"chapters":found.chapters});
    ensure!(
        native_discovery::current_matches(&probe, &observed),
        "generated probe does not match exact native observation"
    );
    let proof = GeneratedOccurrenceProof {
        source: root.mapping.clone(),
        observed,
        probe,
        file: Arc::new(file),
    };
    verify_current(host, &proof)?;
    ensure!(
        !cancellation.is_cancelled(),
        "generated observation cancelled after cleanup"
    );
    Ok(proof)
}

fn verify_snapshot(mapping: &SourceMapping, snapshot: &Value) -> anyhow::Result<()> {
    ensure!(
        snapshot["catalog_id"].as_str() == Some(&mapping.catalog.catalog_id)
            && snapshot["catalog_incarnation"].as_str() == Some(&mapping.catalog.incarnation)
            && snapshot["profile"].as_str() == Some(&mapping.profile)
            && snapshot["binding"] == mapping.native_binding
            && snapshot["policy"] == mapping.native_policy,
        "generated occurrence snapshot differs from private source authority"
    );
    Ok(())
}

/// Call immediately before native generated admission while retaining the same
/// host and Motion publication writer. It does not dispatch or invent a receipt.
pub(crate) fn verify_current(
    host: &HostSession,
    proof: &GeneratedOccurrenceProof,
) -> anyhow::Result<Value> {
    verify_root(host, &proof.source)?;
    ensure!(
        proof.probe["fingerprint"].as_str() == Some(&scan::fingerprint(&proof.file.metadata()?)),
        "retained generated output descriptor changed"
    );
    // Validate the current source-relative locator as well: an unchanged open
    // descriptor alone must not authorize a path replaced after observation.
    let relative = proof.observed["path"]
        .as_str()
        .context("generated occurrence path missing")?;
    let current = scan::open_file(
        &PathBuf::from(&proof.source.motion_root),
        Path::new(relative),
    )?;
    ensure!(
        proof.probe["fingerprint"].as_str() == Some(&scan::fingerprint(&current.metadata()?)),
        "generated output locator changed"
    );
    let snapshot = host.source_occurrences(&proof.source.native_source)?;
    verify_snapshot(&proof.source, &snapshot)?;
    let current = snapshot["occurrences"]
        .as_array()
        .context("generated native occurrences missing")?
        .iter()
        .find(|row| row["id"] == proof.observed["id"])
        .context("generated occurrence disappeared")?;
    ensure!(
        current == &proof.observed && native_discovery::current_matches(&proof.probe, current),
        "generated occurrence or full measured fingerprint changed"
    );
    Ok(snapshot)
}

/// These physical grant fields are derived from the retained proof, never from
/// the generated command request. The processing owner supplies only its already
/// journaled host file ID, not native physical identity or measurement claims.
pub(crate) struct GeneratedPhysicalFacts {
    pub(crate) output: catabolic_store::catalog_import_typed::PhysicalMapping,
    pub(crate) output_ctime_ns: i64,
    pub(crate) output_size: i64,
    pub(crate) output_sha256: String,
    pub(crate) output_duration_us: i64,
    pub(crate) output_binding: Value,
    pub(crate) output_file: Arc<File>,
}
impl GeneratedOccurrenceProof {
    pub(crate) fn admission_facts(
        &self,
        host: &HostSession,
        motion_file_id: &str,
    ) -> anyhow::Result<GeneratedPhysicalFacts> {
        ensure!(
            !motion_file_id.is_empty() && motion_file_id.len() <= 256,
            "invalid generated host file identity"
        );
        verify_current(host, self)?;
        let duration = self.probe["duration_seconds"]
            .as_f64()
            .context("generated output has no measured duration")?
            * 1_000_000.0;
        ensure!(
            duration.is_finite()
                && duration > 0.0
                && duration < i64::MAX as f64
                && duration.fract() == 0.0,
            "generated output duration is not positive integral microseconds"
        );
        let text = |key: &str| {
            self.observed[key]
                .as_str()
                .with_context(|| format!("generated occurrence {key} missing"))
        };
        let sha = self.probe["revision"]
            .as_str()
            .context("generated output SHA256 missing")?;
        ensure!(
            sha.len() == 64
                && sha
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid generated output SHA256"
        );
        let size = self.probe["bytes"]
            .as_i64()
            .context("generated output size missing")?;
        ensure!(
            size > 0 && self.observed["size"].as_i64() == Some(size),
            "generated output size differs from observation"
        );
        Ok(GeneratedPhysicalFacts {
            output: catabolic_store::catalog_import_typed::PhysicalMapping {
                source_occurrence_id: motion_file_id.into(),
                profile: self.source.profile.clone(),
                location: self.source.native_source.clone(),
                path: text("path")?.into(),
                exact: catabolic_core::catalog_identity::ExactFileRevision {
                    file_id: text("id")?.into(),
                    revision: text("exact_revision")?.into(),
                },
                observed_status: Some("present".into()),
            },
            output_ctime_ns: self.observed["ctime_ns"]
                .as_i64()
                .context("generated output ctime missing")?,
            output_size: size,
            output_sha256: sha.into(),
            output_duration_us: duration as i64,
            output_binding: self.source.native_binding.clone(),
            output_file: self.file.clone(),
        })
    }
}

pub(crate) struct MaterializedOutput {
    pub(crate) file: Arc<File>,
    pub(crate) fingerprint: String,
    pub(crate) sha256: String,
    pub(crate) bytes: u64,
}

/// Blocking I/O adapter; call with the existing processing I/O permit retained.
/// The final locator must already be durably journaled. Cancellation never
/// removes a possibly published final file. Orphan spool files remain outside
/// native inventory for later explicit ownership-based cleanup.
pub(crate) fn materialize_output(
    root: &GeneratedRoot,
    output_relative: &Path,
    completed: &File,
    expected_fingerprint: &str,
    expected_sha256: &str,
    cancellation: &tokio_util::sync::CancellationToken,
) -> anyhow::Result<MaterializedOutput> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Seek, SeekFrom, Write};
    ensure!(
        output_relative.components().count() == 1
            && matches!(
                output_relative.components().next(),
                Some(std::path::Component::Normal(_))
            ),
        "generated final output must be a flat filename"
    );
    ensure!(
        expected_sha256.len() == 64
            && expected_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid completed output SHA256"
    );
    ensure!(!cancellation.is_cancelled(), "generated copy cancelled");
    let metadata = completed.metadata()?;
    ensure!(
        metadata.is_file() && scan::fingerprint(&metadata) == expected_fingerprint,
        "completed staging output changed before copy"
    );
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // The descriptor is borrowed and live for this call; F_GETFL neither
        // changes it nor transfers ownership.
        let flags = unsafe { libc::fcntl(completed.as_raw_fd(), libc::F_GETFL) };
        ensure!(
            flags >= 0 && flags & libc::O_ACCMODE == libc::O_RDONLY,
            "completed output descriptor must be read-only"
        );
    }
    let root_path = Path::new(&root.mapping.motion_root);
    let final_dir = cap_std::fs::Dir::open_ambient_dir(root_path, cap_std::ambient_authority())?;
    ensure!(
        crate::db::root_identity(&final_dir.try_clone()?.into_std_file().metadata()?)
            == root.mapping.motion_root_identity,
        "generated final root changed before copy"
    );
    let expected_size = metadata.len();
    let parent = cap_std::fs::Dir::open_ambient_dir(
        root_path.parent().context("generated root has no parent")?,
        cap_std::ambient_authority(),
    )?;
    let spool_name = ".native-generated-spool";
    match parent.create_dir(spool_name) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let spool_path = root_path
        .parent()
        .context("generated root has no parent")?
        .join(spool_name);
    let spool_metadata = std::fs::symlink_metadata(&spool_path)?;
    ensure!(
        spool_metadata.is_dir() && !spool_metadata.file_type().is_symlink(),
        "private generated spool is not an owned directory"
    );
    let spool_dir = parent.open_dir(spool_name)?;
    ensure!(
        crate::db::root_identity(&spool_metadata)
            == crate::db::root_identity(&spool_dir.try_clone()?.into_std_file().metadata()?),
        "private generated spool changed while opening"
    );
    let spool_file = format!(
        "{}.partial",
        catabolic_core::sha256(output_relative.as_os_str().as_encoded_bytes())
    );
    // Every copier for this committed locator retains the same exclusive lease.
    // A process crash releases it; an active/uncertain writer makes retry refuse.
    let mut lock_options = cap_std::fs::OpenOptions::new();
    lock_options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        lock_options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let lease = spool_dir
        .open_with(format!("{spool_file}.lease"), &lock_options)?
        .into_std();
    ensure!(
        lease.metadata()?.is_file(),
        "generated lease is not a regular file"
    );
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        ensure!(
            unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "generated copy still has active ownership"
        );
    }
    #[cfg(not(unix))]
    anyhow::bail!("generated copy recovery requires an explicit supported exclusive lease");
    // The lease is held until function exit, including hash and cleanup.
    let _lease = lease;
    match scan::open_file(root_path, output_relative) {
        Ok(file) => {
            let verified = verify_materialized(file, expected_sha256, expected_size, cancellation)?;
            match scan::open_file(&spool_path, Path::new(&spool_file)) {
                Ok(spool) => {
                    ensure!(
                        spool_dir.symlink_metadata(&spool_file)?.is_file(),
                        "owned spool alias is not a regular leaf"
                    );
                    ensure!(
                        crate::db::root_identity(&spool.metadata()?)
                            == crate::db::root_identity(&verified.file.metadata()?),
                        "existing final and owned spool are different inodes; no cleanup authorized"
                    );
                    verify_materialized(spool, expected_sha256, expected_size, cancellation)?;
                    spool_dir.remove_file(&spool_file)?;
                    sync_directory(&spool_dir)?;
                }
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
                Err(error) => return Err(error),
            }
            verified.file.sync_all()?;
            sync_directory(&final_dir)?;
            ensure!(
                crate::db::root_identity(&std::fs::metadata(root_path)?)
                    == root.mapping.motion_root_identity,
                "generated final root changed during replay"
            );
            return verify_final_materialized(
                verified.file.try_clone()?,
                expected_sha256,
                expected_size,
                cancellation,
            );
        }
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
        Err(error) => return Err(error),
    }
    // No final exists and exclusive exact-locator custody is proven. Only this
    // journal-owned regular, unaliased spool leaf may be replaced after crash.
    match spool_dir.symlink_metadata(&spool_file) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "owned spool leaf is not regular"
            );
            let spool = scan::open_file(&spool_path, Path::new(&spool_file))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                ensure!(
                    spool.metadata()?.nlink() == 1,
                    "unpublished spool has unknown aliases"
                );
            }
            spool_dir.remove_file(&spool_file)?;
            sync_directory(&spool_dir)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut options = cap_std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut destination = spool_dir.open_with(&spool_file, &options)?.into_std();
    let mut source = completed.try_clone()?;
    source.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        ensure!(
            !cancellation.is_cancelled(),
            "generated copy cancelled; private spool retained"
        );
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .context("generated output length overflow")?;
        ensure!(
            bytes <= expected_size,
            "completed staging output grew during copy"
        );
        hash.update(&buffer[..read]);
        destination.write_all(&buffer[..read])?;
    }
    ensure!(
        bytes == expected_size
            && format!("{:x}", hash.finalize()) == expected_sha256
            && scan::fingerprint(&completed.metadata()?) == expected_fingerprint,
        "completed staging output bytes or full fingerprint changed during copy"
    );
    destination.sync_all()?;
    drop(destination);
    sync_directory(&spool_dir)?;
    ensure!(
        !cancellation.is_cancelled(),
        "generated copy cancelled before publish; private spool retained"
    );
    match spool_dir.hard_link(&spool_file, &final_dir, output_relative) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    sync_directory(&final_dir)?;
    let final_file = scan::open_file(root_path, output_relative)?;
    let verified = verify_materialized(final_file, expected_sha256, expected_size, cancellation)?;
    ensure!(
        crate::db::root_identity(&std::fs::metadata(root_path)?)
            == root.mapping.motion_root_identity,
        "generated final root changed during publish"
    );
    // Removing this private link changes ctime on the copied final inode. Do it
    // BEFORE recording the final fingerprint/probing, and never remove final.
    spool_dir.remove_file(&spool_file)?;
    sync_directory(&spool_dir)?;
    verify_final_materialized(
        verified.file.try_clone()?,
        expected_sha256,
        expected_size,
        cancellation,
    )
}

fn verify_materialized(
    mut file: File,
    expected_sha256: &str,
    expected_size: u64,
    cancellation: &tokio_util::sync::CancellationToken,
) -> anyhow::Result<MaterializedOutput> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Seek, SeekFrom};
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.len() == expected_size,
        "existing generated output length differs"
    );
    let fingerprint = scan::fingerprint(&before);
    file.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        ensure!(
            !cancellation.is_cancelled(),
            "generated verification cancelled; final output retained"
        );
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .context("generated output length overflow")?;
        ensure!(
            bytes <= expected_size,
            "generated output grew during verification"
        );
        hash.update(&buffer[..read]);
    }
    let sha256 = format!("{:x}", hash.finalize());
    ensure!(
        bytes == expected_size
            && sha256 == expected_sha256
            && scan::fingerprint(&file.metadata()?) == fingerprint,
        "existing generated output bytes or full fingerprint differs"
    );
    file.seek(SeekFrom::Start(0))?;
    Ok(MaterializedOutput {
        file: Arc::new(file),
        fingerprint,
        sha256,
        bytes,
    })
}

#[cfg(all(test, unix))]
mod materialization_tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::MetadataExt;

    // This fixture tests only the filesystem copier; it is not a native
    // registration or occurrence grant and cannot construct an admission proof.
    fn root(path: &Path) -> GeneratedRoot {
        GeneratedRoot {
            mapping: SourceMapping {
                library_id: SOURCE_ID.into(),
                profile: "default".into(),
                native_source: catabolic_core::sha256(SOURCE_ID.as_bytes()),
                catalog: playscale_core::catalog_backend::Identity {
                    catalog_id: "copier-test".into(),
                    incarnation: "copier-test".into(),
                    schema_version: 39,
                },
                motion_root: path.to_str().unwrap().into(),
                motion_root_identity: crate::db::root_identity(&std::fs::metadata(path).unwrap()),
                motion_binding_revision: 1,
                native_binding: Value::Null,
                exclusions: vec![],
                native_policy: Value::Null,
            },
        }
    }
    #[test]
    fn final_output_is_independent_no_clobber_and_exact_replay() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("final");
        std::fs::create_dir(&final_path).unwrap();
        let root = root(&final_path);
        let staging = dir.path().join("encoder-output");
        let bytes = b"completed independently validated output";
        std::fs::write(&staging, bytes).unwrap();
        let input = File::open(&staging).unwrap();
        let stamp = scan::fingerprint(&input.metadata().unwrap());
        let digest = format!("{:x}", Sha256::digest(bytes));
        let stop = tokio_util::sync::CancellationToken::new();
        let output = materialize_output(
            &root,
            Path::new("job-output.mp4"),
            &input,
            &stamp,
            &digest,
            &stop,
        )
        .unwrap();
        assert_ne!(
            input.metadata().unwrap().ino(),
            output.file.metadata().unwrap().ino(),
            "must never link encoder-owned inode"
        );
        assert_eq!(output.sha256, digest);
        assert_eq!(output.bytes, bytes.len() as u64);
        assert_eq!(
            output.fingerprint,
            scan::fingerprint(&output.file.metadata().unwrap())
        );
        let again = materialize_output(
            &root,
            Path::new("job-output.mp4"),
            &input,
            &stamp,
            &digest,
            &stop,
        )
        .unwrap();
        assert_eq!(
            again.file.metadata().unwrap().ino(),
            output.file.metadata().unwrap().ino()
        );
        assert_eq!(again.fingerprint, output.fingerprint);
        std::fs::write(&staging, b"late encoder write").unwrap();
        assert_eq!(
            std::fs::read(final_path.join("job-output.mp4")).unwrap(),
            bytes
        );
        assert!(
            materialize_output(
                &root,
                Path::new("job-output.mp4"),
                &input,
                &stamp,
                &digest,
                &stop
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(final_path.join("job-output.mp4")).unwrap(),
            bytes
        );
    }
    #[test]
    fn conflicting_final_and_cancelled_copy_never_remove_final() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("final");
        std::fs::create_dir(&final_path).unwrap();
        let root = root(&final_path);
        let staging = dir.path().join("encoder-output");
        std::fs::write(&staging, b"expected").unwrap();
        std::fs::write(final_path.join("output.mp4"), b"conflict").unwrap();
        let input = File::open(staging).unwrap();
        let stamp = scan::fingerprint(&input.metadata().unwrap());
        let digest = format!("{:x}", Sha256::digest(b"expected"));
        let stop = tokio_util::sync::CancellationToken::new();
        assert!(
            materialize_output(
                &root,
                Path::new("output.mp4"),
                &input,
                &stamp,
                &digest,
                &stop
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(final_path.join("output.mp4")).unwrap(),
            b"conflict"
        );
        stop.cancel();
        assert!(
            materialize_output(
                &root,
                Path::new("output.mp4"),
                &input,
                &stamp,
                &digest,
                &stop
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(final_path.join("output.mp4")).unwrap(),
            b"conflict"
        );
        assert!(
            materialize_output(
                &root,
                Path::new("../escape"),
                &input,
                &stamp,
                &digest,
                &stop
            )
            .is_err()
        );
    }

    #[test]
    fn exact_owned_spool_alias_is_reconciled_but_unknown_alias_is_retained() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("final");
        std::fs::create_dir(&final_path).unwrap();
        let root = root(&final_path);
        let staging = dir.path().join("encoder-output");
        std::fs::write(&staging, b"expected").unwrap();
        let input = File::open(staging).unwrap();
        let stamp = scan::fingerprint(&input.metadata().unwrap());
        let digest = format!("{:x}", Sha256::digest(b"expected"));
        let stop = tokio_util::sync::CancellationToken::new();
        let relative = Path::new("output.mp4");
        let first = materialize_output(&root, relative, &input, &stamp, &digest, &stop).unwrap();
        let spool = dir.path().join(".native-generated-spool").join(format!(
            "{}.partial",
            catabolic_core::sha256(relative.as_os_str().as_encoded_bytes())
        ));
        // Actual same-inode crash-after-link state, no manufactured receipt.
        std::fs::hard_link(final_path.join(relative), &spool).unwrap();
        assert_eq!(first.file.metadata().unwrap().nlink(), 2);
        let replay = materialize_output(&root, relative, &input, &stamp, &digest, &stop).unwrap();
        assert!(!spool.exists());
        assert_eq!(replay.file.metadata().unwrap().nlink(), 1);
        assert_eq!(
            replay.file.metadata().unwrap().ino(),
            first.file.metadata().unwrap().ino()
        );
        assert_eq!(
            replay.fingerprint,
            scan::fingerprint(&replay.file.metadata().unwrap())
        );
        let unknown = dir.path().join("unknown-alias");
        std::fs::hard_link(final_path.join(relative), &unknown).unwrap();
        assert!(materialize_output(&root, relative, &input, &stamp, &digest, &stop).is_err());
        assert!(unknown.exists());
        assert!(final_path.join(relative).exists());
    }
    #[test]
    fn active_spool_owner_blocks_retry_and_partial_recovers_only_after_release() {
        use std::os::fd::AsRawFd;
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("final");
        std::fs::create_dir(&final_path).unwrap();
        let root = root(&final_path);
        let staging = dir.path().join("encoder-output");
        std::fs::write(&staging, b"expected").unwrap();
        let input = File::open(staging).unwrap();
        let stamp = scan::fingerprint(&input.metadata().unwrap());
        let digest = format!("{:x}", Sha256::digest(b"expected"));
        let stop = tokio_util::sync::CancellationToken::new();
        let relative = Path::new("output.mp4");
        let spool_dir = dir.path().join(".native-generated-spool");
        std::fs::create_dir(&spool_dir).unwrap();
        let leaf = format!(
            "{}.partial",
            catabolic_core::sha256(relative.as_os_str().as_encoded_bytes())
        );
        let lease = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(spool_dir.join(format!("{leaf}.lease")))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        std::fs::write(spool_dir.join(&leaf), b"partial").unwrap();
        assert!(materialize_output(&root, relative, &input, &stamp, &digest, &stop).is_err());
        assert_eq!(std::fs::read(spool_dir.join(&leaf)).unwrap(), b"partial");
        assert!(!final_path.join(relative).exists());
        drop(lease);
        let recovered =
            materialize_output(&root, relative, &input, &stamp, &digest, &stop).unwrap();
        assert_eq!(recovered.sha256, digest);
        assert_eq!(recovered.file.metadata().unwrap().nlink(), 1);
        assert!(!spool_dir.join(&leaf).exists());
    }
}

#[cfg(all(test, unix))]
mod registration_tests {
    use super::*;
    #[tokio::test]
    async fn private_source_replays_exact_native_binding_without_public_registry() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("final");
        let other = dir.path().join("other");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&other).unwrap();
        let catalog = dir.path().join("native.sqlite3");
        motion_catalog_backend::initialize(catalog.clone())
            .await
            .unwrap();
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        let mut db = pool.acquire().await.unwrap();
        sqlx::raw_sql("CREATE TABLE catabolic_generated_source(id TEXT PRIMARY KEY,mapping_json TEXT NOT NULL,created_at INTEGER NOT NULL);CREATE TABLE catabolic_source_mappings(library_id TEXT PRIMARY KEY,mapping_json TEXT NOT NULL);")
            .execute(&mut *db).await.unwrap();
        let open = || {
            HostSession::open_authorized(
                &catalog,
                "default",
                true,
                catabolic_runtime::host::CONTRACT_VERSION,
                catabolic_core::catalog_host_lease::HostAuthorization::new(
                    "motion.generated.test".into(),
                    "r1".into(),
                )
                .unwrap(),
            )
            .unwrap()
        };
        let mut host = open();
        let first = register_output_root(&mut db, &mut host, &root)
            .await
            .unwrap();
        let mapping = first.mapping().clone();
        let created: i64 = sqlx::query_scalar("SELECT created_at FROM catabolic_generated_source")
            .fetch_one(&mut *db)
            .await
            .unwrap();
        let repeated = register_output_root(&mut db, &mut host, &root)
            .await
            .unwrap();
        assert_eq!(repeated.mapping(), &mapping);
        let public: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_source_mappings")
            .fetch_one(&mut *db)
            .await
            .unwrap();
        assert_eq!(public, 0);
        host.close().unwrap();
        let mut host = open();
        let retained = mapping_in(&mut db).await.unwrap().unwrap();
        GeneratedRoot::from_retained_verified_mapping(&host, &root, retained.clone()).unwrap();
        assert!(
            GeneratedRoot::from_retained_verified_mapping(&host, &other, retained.clone()).is_err()
        );
        let mut wrong = retained;
        wrong.native_binding["inode"] = json!(0);
        assert!(GeneratedRoot::from_retained_verified_mapping(&host, &root, wrong).is_err());
        assert!(
            register_output_root(&mut db, &mut host, &other)
                .await
                .is_err()
        );
        assert_eq!(mapping_in(&mut db).await.unwrap().unwrap(), mapping);
        let after: i64 = sqlx::query_scalar("SELECT created_at FROM catabolic_generated_source")
            .fetch_one(&mut *db)
            .await
            .unwrap();
        assert_eq!(after, created);
        host.close().unwrap();
    }
}

fn verify_final_materialized(
    file: File,
    digest: &str,
    size: u64,
    stop: &tokio_util::sync::CancellationToken,
) -> anyhow::Result<MaterializedOutput> {
    let output = verify_materialized(file, digest, size, stop)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            output.file.metadata()?.nlink() == 1,
            "generated final output has unknown aliases"
        );
    }
    Ok(output)
}

fn sync_directory(directory: &cap_std::fs::Dir) -> anyhow::Result<()> {
    // cap-std's directory capability may be an O_PATH descriptor on Linux.
    // fstat works on that handle, but fsync returns EBADF. Open only "."
    // relative to the retained capability to obtain a readable directory FD.
    let file = directory.open(".")?.into_std();
    ensure!(file.metadata()?.is_dir(), "directory sync target changed");
    file.sync_all()?;
    Ok(())
}

#[cfg(all(test, unix))]
mod saved_output_validation_tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn probe(path: &Path) -> Value {
        let bytes = std::fs::read(path).unwrap();
        json!({"revision":catabolic_core::sha256(&bytes),
            "bytes":bytes.len(),"fingerprint":scan::fingerprint(&std::fs::metadata(path).unwrap())})
    }

    #[test]
    fn recovery_rehashes_readonly_final_bytes_and_rejects_invented_digest_or_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output.mp4");
        std::fs::write(&path, b"retained completed output").unwrap();
        let saved = probe(&path);
        let stop = tokio_util::sync::CancellationToken::new();
        let held = verify_saved_bytes(File::open(&path).unwrap(), &saved, &stop).unwrap();
        assert_eq!(held.metadata().unwrap().nlink(), 1);
        let mut wrong = saved.clone();
        wrong["revision"] = json!("0".repeat(64));
        assert!(verify_saved_bytes(File::open(&path).unwrap(), &wrong, &stop).is_err());
        wrong = saved.clone();
        wrong["bytes"] = json!(1);
        assert!(verify_saved_bytes(File::open(&path).unwrap(), &wrong, &stop).is_err());
        let writable = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(verify_saved_bytes(writable, &saved, &stop).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"retained completed output");
    }

    #[test]
    fn recovery_rejects_ctime_only_change_even_when_content_hash_and_mtime_match() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output.mp4");
        std::fs::write(&path, b"same final bytes").unwrap();
        let before = std::fs::metadata(&path).unwrap();
        let saved = probe(&path);
        std::thread::sleep(Duration::from_millis(2));
        // A full rewrite restores both bytes and mtime. The ctime proof must
        // still invalidate the immutable saved measurement.
        std::fs::write(&path, b"same final bytes").unwrap();
        File::open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(before.modified().unwrap()))
            .unwrap();
        let after = std::fs::metadata(&path).unwrap();
        assert_eq!(after.modified().unwrap(), before.modified().unwrap());
        assert_eq!(after.len(), before.len());
        assert_ne!(
            scan::fingerprint(&after),
            saved["fingerprint"].as_str().unwrap()
        );
        assert!(
            verify_saved_bytes(
                File::open(&path).unwrap(),
                &saved,
                &tokio_util::sync::CancellationToken::new()
            )
            .is_err()
        );
    }

    #[test]
    fn recovery_cancellation_and_alias_refusal_never_remove_retained_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output.mp4");
        std::fs::write(&path, b"retained on refusal").unwrap();
        let saved = probe(&path);
        let stop = tokio_util::sync::CancellationToken::new();
        stop.cancel();
        assert!(verify_saved_bytes(File::open(&path).unwrap(), &saved, &stop).is_err());
        let alias = dir.path().join("unknown-alias.mp4");
        std::fs::hard_link(&path, &alias).unwrap();
        // Even a current fingerprint does not excuse a live alias.
        let current = probe(&path);
        assert!(
            verify_saved_bytes(
                File::open(&path).unwrap(),
                &current,
                &tokio_util::sync::CancellationToken::new()
            )
            .is_err()
        );
        assert_eq!(std::fs::metadata(&path).unwrap().nlink(), 2);
        assert!(alias.exists() && path.exists());
    }
}
