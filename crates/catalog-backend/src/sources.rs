//! Trusted source-owner bridge. Mapping is explicit by both IDs, never inferred
//! from a name, content digest or similar path. Native binding is never replaced.
use crate::{NativeCatalog, now, status};
use anyhow::{Context, bail};
use catabolic_runtime::{
    helper::TrustedClosedObservationArtifact,
    host::{CONTRACT_VERSION, HostSession},
};
use catabolic_store::{Store, observation};
use playscale_core::{catalog_backend::Identity, catalog_registration::SourceIdentity};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceMapping {
    pub library_id: String,
    pub profile: String,
    pub native_source: String,
    pub catalog: Identity,
    pub motion_root: String,
    pub motion_root_identity: String,
    pub motion_binding_revision: i64,
    pub native_binding: Value,
    pub exclusions: Vec<String>,
    pub native_policy: Value,
}

/// Cooperative cancellation is shared with the retained blocking owner. Dropping
/// the async scan also cancels its demand; neither action abandons cleanup.
#[derive(Clone, Default)]
pub struct ScanCancellation(Arc<AtomicBool>);
impl ScanCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
struct CancelOnDrop(ScanCancellation);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl SourceMapping {
    fn identity(&self) -> SourceIdentity {
        SourceIdentity {
            library_id: self.library_id.clone(),
            profile: self.profile.clone(),
            native_source: self.native_source.clone(),
            catalog: self.catalog.clone(),
            motion_root: self.motion_root.clone(),
            motion_root_identity: self.motion_root_identity.clone(),
            motion_binding_revision: self.motion_binding_revision,
            exclusions: self.exclusions.clone(),
        }
    }
}
// Only deliberate qualification refusals are eligible for export omission.
// SQL/JSON/native-owner/worker errors remain errors, never inferred refusals.
#[derive(Debug)]
struct QualificationRefusal(anyhow::Error);
impl std::fmt::Display for QualificationRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}
impl std::error::Error for QualificationRefusal {}
fn nonqualifying(error: anyhow::Error) -> anyhow::Error {
    QualificationRefusal(error).into()
}
fn validate_source_schema(current: &Identity, recorded: &Identity) -> anyhow::Result<()> {
    // Reject an unsupported envelope even if an earlier catalog-ID comparison
    // would otherwise report a deliberate mismatch first.
    anyhow::ensure!(
        current.schema_version == recorded.schema_version
            || (current.schema_version == 40 && recorded.schema_version == 39),
        "source catalog schema mismatch"
    );
    Ok(())
}
fn catalog_qualification_error(
    rejection: playscale_core::catalog_backend::AdmissionError,
    message: anyhow::Error,
) -> anyhow::Error {
    use playscale_core::catalog_backend::AdmissionError;
    match rejection {
        AdmissionError::CatalogChanged | AdmissionError::RestoreDetected => nonqualifying(message),
        AdmissionError::InvalidIdentity | AdmissionError::SchemaChanged => message,
    }
}
fn mapping_qualification_error(
    rejection: playscale_core::catalog_registration::Rejection,
    message: anyhow::Error,
) -> anyhow::Error {
    use playscale_core::catalog_registration::Rejection;
    match rejection {
        Rejection::MappingChanged => nonqualifying(message),
        Rejection::Catalog(error) => catalog_qualification_error(error, message),
        Rejection::Invalid => message,
    }
}
fn public_audit_error(error: anyhow::Error) -> anyhow::Error {
    match error.downcast::<QualificationRefusal>() {
        Ok(refusal) => refusal.0,
        Err(error) => error,
    }
}

fn canonical_directory(path: &Path) -> anyhow::Result<PathBuf> {
    let path = path.canonicalize()?;
    if !path.is_dir() {
        bail!("native source root must be a directory");
    }
    Ok(path)
}
pub(crate) fn check_root(binding: &Value) -> anyhow::Result<()> {
    check_root_qualified(binding).map_err(public_audit_error)
}
fn check_root_qualified(binding: &Value) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let root = binding["root"]
        .as_str()
        .context("native source root missing")?;
    let metadata = std::fs::metadata(root).map_err(|error| {
        if matches!(
            error.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
        ) {
            nonqualifying(error.into())
        } else {
            error.into()
        }
    })?;
    if !metadata.is_dir()
        || binding["device"].as_u64() != Some(metadata.dev())
        || binding["inode"].as_u64() != Some(metadata.ino())
    {
        return Err(nonqualifying(anyhow::anyhow!(
            "native source root identity changed; explicit reconciliation required"
        )));
    }
    Ok(())
}
fn validate_saved_shapes(mapping: &SourceMapping) -> anyhow::Result<()> {
    let binding = mapping
        .native_binding
        .as_object()
        .context("invalid saved native binding")?;
    anyhow::ensure!(
        binding.len() == 6
            && ["profile", "kind", "owner", "root", "device", "inode"]
                .iter()
                .all(|key| binding.contains_key(*key)),
        "invalid saved native binding fields"
    );
    anyhow::ensure!(
        binding["profile"].as_str() == Some(mapping.profile.as_str())
            && binding["kind"].as_str() == Some("source")
            && binding["owner"].as_str() == Some(mapping.native_source.as_str())
            && binding["root"]
                .as_str()
                .is_some_and(|root| Path::new(root).is_absolute())
            && binding["device"].as_i64().is_some_and(|value| value >= 0)
            && binding["inode"].as_i64().is_some_and(|value| value >= 0),
        "invalid saved native binding values"
    );
    let policy = mapping
        .native_policy
        .as_object()
        .context("invalid saved native scan policy")?;
    anyhow::ensure!(
        policy.len() == 6
            && [
                "revision",
                "exclusions",
                "extensions",
                "include_regex",
                "exclude_regex",
                "budgets"
            ]
            .iter()
            .all(|key| policy.contains_key(*key))
            && policy["revision"]
                .as_i64()
                .is_some_and(|revision| revision >= 0),
        "invalid saved native scan policy envelope"
    );
    Ok(())
}
fn validate_saved_policy(store: &Store, mapping: &SourceMapping) -> anyhow::Result<Value> {
    validate_saved_shapes(mapping)?;
    let current = observation::execute(
        store,
        &mapping.profile,
        "scan_policy",
        &json!({"source":mapping.native_source}),
    )?;
    if current != mapping.native_policy {
        observation::validate_policy_snapshot(&mapping.native_policy)?;
    }
    Ok(current)
}
pub(crate) fn inspect_mapping(store: &Store, mapping: &SourceMapping) -> anyhow::Result<()> {
    inspect_mapping_qualified(store, mapping).map_err(public_audit_error)
}
fn inspect_mapping_qualified(store: &Store, mapping: &SourceMapping) -> anyhow::Result<()> {
    playscale_core::catalog_backend::admit_retained_source_record(
        &status(store)?.identity,
        &mapping.catalog,
    )
    .map_err(|e| {
        catalog_qualification_error(e, anyhow::anyhow!("source catalog admission: {e:?}"))
    })?;
    let policy = validate_saved_policy(store, mapping)?;
    let binding = observation::binding(store, &mapping.profile, "source", &mapping.native_source)?;
    if binding != mapping.native_binding {
        return Err(nonqualifying(anyhow::anyhow!(
            "native source binding changed; explicit reconciliation required"
        )));
    }
    if policy != mapping.native_policy {
        return Err(nonqualifying(anyhow::anyhow!(
            "native source policy changed; explicit reconciliation required"
        )));
    }
    check_root_qualified(&binding)
}

impl NativeCatalog {
    /// Deterministic new native identity derived from the immutable Motion source
    /// ID, not its display name or root. Existing collisions still require exact
    /// binding admission. Logical libraries keep their separate memberships.
    pub async fn register_motion_source(&self, library_id: &str) -> anyhow::Result<SourceMapping> {
        let root: String =
            sqlx::query_scalar("SELECT root FROM libraries WHERE id=? AND managed=0")
                .bind(library_id)
                .fetch_one(&self.journal)
                .await?;
        let native_source = catabolic_core::sha256(library_id.as_bytes());
        self.register_source(library_id, "default", &native_source, PathBuf::from(root))
            .await
    }

    /// Audit every mapped source without granting physical file or activation
    /// authority. Missing unmanaged sources are an explicit error, never omitted.
    pub async fn audit_source_mappings(&self) -> anyhow::Result<Vec<SourceMapping>> {
        let ids: Vec<String> =
            sqlx::query_scalar("SELECT id FROM libraries WHERE managed=0 UNION SELECT id FROM catabolic_source_registry ORDER BY id")
                .fetch_all(&self.journal)
                .await?;
        let mut mappings = Vec::with_capacity(ids.len());
        for id in ids {
            mappings.push(self.audit_source_mapping(&id).await?);
        }
        Ok(mappings)
    }
    /// Bind an existing Motion physical library to one explicitly named native
    /// source. Roots and source binding revisions are captured under a Motion
    /// writer transaction; the native owner serializes check-and-bind atomically.
    pub async fn register_source(
        &self,
        library_id: &str,
        profile: &str,
        native_source: &str,
        root: PathBuf,
    ) -> anyhow::Result<SourceMapping> {
        if library_id.is_empty()
            || !playscale_core::catalog_registration::name(profile)
            || !playscale_core::catalog_registration::name(native_source)
        {
            bail!("invalid explicit source mapping");
        }
        let root = canonical_directory(&root)?;
        let root_text = root
            .to_str()
            .context("source root must be UTF-8")?
            .to_owned();
        let mut tx = self.journal.begin_with("BEGIN IMMEDIATE").await?;
        let (motion_root, motion_root_identity, motion_binding_revision, managed, exclusions_json): (String,String,i64,i64,String) = sqlx::query_as("SELECT root,root_identity,binding_revision,managed,exclusions_json FROM libraries WHERE id=?")
            .bind(library_id).fetch_one(&mut *tx).await?;
        if managed != 0 || canonical_directory(Path::new(&motion_root))? != root {
            bail!("source mapping must address the exact unmanaged Motion root");
        }
        use std::os::unix::fs::MetadataExt;
        let root_metadata = std::fs::metadata(&root)?;
        anyhow::ensure!(
            motion_root_identity == format!("{}:{}", root_metadata.dev(), root_metadata.ino()),
            "Motion source root identity was replaced before registration"
        );
        let exclusions =
            playscale_core::sources::normalize_exclusions(&serde_json::from_str::<Vec<String>>(
                &exclusions_json,
            )?)
            .map_err(|e| anyhow::anyhow!("source exclusions: {e:?}"))?;
        let saved: Option<String> = sqlx::query_scalar(
            "SELECT mapping_json FROM catabolic_source_mappings WHERE library_id=?",
        )
        .bind(library_id)
        .fetch_optional(&mut *tx)
        .await?;
        let mut requested = SourceMapping {
            library_id: library_id.into(),
            profile: profile.into(),
            native_source: native_source.into(),
            catalog: self.identity.clone(),
            motion_root,
            motion_root_identity,
            motion_binding_revision,
            native_binding: Value::Null,
            exclusions,
            native_policy: Value::Null,
        };
        requested
            .identity()
            .validate()
            .map_err(|e| anyhow::anyhow!("source identity: {e:?}"))?;
        if let Some(saved) = saved {
            let saved: SourceMapping = serde_json::from_str(&saved)?;
            requested.native_binding = saved.native_binding.clone();
            requested.native_policy = saved.native_policy.clone();
            saved
                .identity()
                .admit(&requested.identity())
                .map_err(|e| anyhow::anyhow!("source mapping admission: {e:?}"))?;
            let check = saved.clone();
            self.with_store(false, move |store| inspect_mapping(store, &check))
                .await?;
            tx.commit().await?;
            return Ok(saved);
        }
        // Reserve the native name in the Motion writer transaction before any
        // native mutation. Its unique index serializes competing host mappings.
        let duplicate: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM catabolic_source_mappings WHERE profile=? AND native_source=?",
        )
        .bind(profile)
        .bind(native_source)
        .fetch_one(&mut *tx)
        .await?;
        if duplicate != 0 {
            bail!("native source is already mapped to another Motion source");
        }
        let p = profile.to_owned();
        let s = native_source.to_owned();
        let exclusions = requested.exclusions.clone();
        let (native_binding, native_policy) = self.with_store(true, move |store| {
            Ok(store.transaction(|_| {
                let exists = store.db.prepare("SELECT 1 FROM bindings WHERE profile=? AND kind='source' AND owner=?")?.exists([&p,&s])?;
                let binding = if exists {
                    let existing = observation::binding(store, &p, "source", &s)?;
                    if existing["root"].as_str() != Some(&root_text) { return Err(catabolic_store::Error("refusing to replace a native source binding".into())); }
                    check_root(&existing).map_err(|e| catabolic_store::Error(e.to_string()))?;
                    existing
                } else {
                    observation::execute(store, &p, "bind", &json!({"kind":"source","owner":s,"root":root_text}))?
                };
                let before = observation::execute(store, &p, "scan_policy", &json!({"source":s}))?;
                let compatible = before["exclusions"] == json!(exclusions) && before["extensions"].is_null() && before["include_regex"] == json!([]) && before["exclude_regex"] == json!([]);
                if !compatible {
                    if before["revision"].as_i64() != Some(0) { return Err(catabolic_store::Error("refusing to replace an existing native source policy".into())); }
                    observation::execute(store, &p, "scan_policy", &json!({"source":s,"apply":true,"policy":{"exclusions":exclusions,"extensions":null,"include_regex":[],"exclude_regex":[]}}))?;
                }
                let policy = observation::execute(store, &p, "scan_policy", &json!({"source":s}))?;
                Ok((binding, policy))
            })?)
        }).await?;
        anyhow::ensure!(
            native_binding["device"].as_u64() == Some(root_metadata.dev())
                && native_binding["inode"].as_u64() == Some(root_metadata.ino()),
            "source root changed during native registration"
        );
        requested.native_binding = native_binding;
        requested.native_policy = native_policy;
        sqlx::query("INSERT INTO catabolic_source_mappings(library_id,profile,native_source,mapping_json,created_at) VALUES(?,?,?,?,?)")
            .bind(library_id).bind(profile).bind(native_source).bind(serde_json::to_string(&requested)?).bind(now()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(requested)
    }

    pub async fn source_mapping(&self, library_id: &str) -> anyhow::Result<SourceMapping> {
        let mut db = self.journal.acquire().await?;
        self.source_mapping_in(&mut db, library_id).await
    }
    pub async fn source_mapping_in(
        &self,
        db: &mut sqlx::SqliteConnection,
        library_id: &str,
    ) -> anyhow::Result<SourceMapping> {
        self.source_mapping_qualified_in(db, library_id)
            .await
            .map_err(public_audit_error)?
            .ok_or_else(|| sqlx::Error::RowNotFound.into())
    }
    async fn source_mapping_qualified_in(
        &self,
        db: &mut sqlx::SqliteConnection,
        library_id: &str,
    ) -> anyhow::Result<Option<SourceMapping>> {
        let registered: Option<String> =
            sqlx::query_scalar("SELECT descriptor_json FROM catabolic_source_registry WHERE id=?")
                .bind(library_id)
                .fetch_optional(&mut *db)
                .await?;
        if let Some(registered) = registered {
            let descriptor: crate::source_registry::NativeSourceDescriptor =
                serde_json::from_str(&registered)?;
            let raw: String = sqlx::query_scalar(
                "SELECT mapping_json FROM catabolic_source_mappings WHERE library_id=?",
            )
            .bind(library_id)
            .fetch_one(&mut *db)
            .await?;
            let mapping: SourceMapping = serde_json::from_str(&raw)?;
            validate_saved_shapes(&mapping)?;
            mapping
                .identity()
                .validate()
                .map_err(|e| anyhow::anyhow!("invalid source mapping: {e:?}"))?;
            validate_source_schema(&self.identity, &mapping.catalog)?;
            playscale_core::catalog_backend::admit_retained_source_record(
                &self.identity,
                &mapping.catalog,
            )
            .map_err(|e| {
                catalog_qualification_error(e, anyhow::anyhow!("native source catalog: {e:?}"))
            })?;
            if !(descriptor.id == mapping.library_id
                && descriptor.root == mapping.motion_root
                && descriptor.root_identity == mapping.motion_root_identity
                && descriptor.exclusions == mapping.exclusions)
            {
                return Err(nonqualifying(anyhow::anyhow!(
                    "native source registry/mapping mismatch"
                )));
            }
            return Ok(Some(mapping));
        }
        let row: Option<(String,String,String,i64,String)> = sqlx::query_as("SELECT m.mapping_json,l.root,l.root_identity,l.binding_revision,l.exclusions_json FROM catabolic_source_mappings m JOIN libraries l ON l.id=m.library_id WHERE m.library_id=?")
            .bind(library_id).fetch_optional(&mut *db).await?;
        let Some((encoded, root, identity, revision, exclusions_json)) = row else {
            return Ok(None);
        };
        let mapping: SourceMapping = serde_json::from_str(&encoded)?;
        validate_saved_shapes(&mapping)?;
        mapping
            .identity()
            .validate()
            .map_err(|e| anyhow::anyhow!("invalid source mapping: {e:?}"))?;
        validate_source_schema(&self.identity, &mapping.catalog)?;
        playscale_core::catalog_backend::admit_retained_source_record(
            &self.identity,
            &mapping.catalog,
        )
        .map_err(|e| {
            catalog_qualification_error(e, anyhow::anyhow!("source catalog mismatch: {e:?}"))
        })?;
        let mut observed = mapping.identity();
        observed.library_id = library_id.into();
        observed.motion_root = root;
        observed.motion_root_identity = identity;
        observed.motion_binding_revision = revision;
        observed.exclusions = playscale_core::sources::normalize_exclusions(
            &serde_json::from_str::<Vec<String>>(&exclusions_json)?,
        )
        .map_err(|e| anyhow::anyhow!("source exclusions: {e:?}"))?;
        mapping.identity().admit(&observed).map_err(|e| {
            mapping_qualification_error(e, anyhow::anyhow!("Motion source binding changed: {e:?}"))
        })?;
        Ok(Some(mapping))
    }

    /// Exact mapping validation, including the opened native owner and current
    /// filesystem identity. This is usable by export/reference auditing.
    pub async fn audit_source_mapping(&self, library_id: &str) -> anyhow::Result<SourceMapping> {
        let mapping = self.source_mapping(library_id).await?;
        let check = mapping.clone();
        self.with_store(false, move |store| inspect_mapping(store, &check))
            .await?;
        Ok(mapping)
    }

    /// Export qualification under the caller's retained Motion writer. Only
    /// explicit absent/stale/unavailable-source refusals omit an ID. Malformed
    /// records, SQL/storage errors and native/worker failures abort capture.
    pub(crate) async fn qualified_source_mapping_in(
        &self,
        db: &mut sqlx::SqliteConnection,
        library_id: &str,
    ) -> anyhow::Result<Option<SourceMapping>> {
        let result: anyhow::Result<Option<SourceMapping>> = async {
            let mapping = match self.source_mapping_qualified_in(db, library_id).await {
                Ok(Some(mapping)) => mapping,
                Ok(None) => return Ok(None),
                Err(error) if error.is::<QualificationRefusal>() => {
                    let raw: String = sqlx::query_scalar(
                        "SELECT mapping_json FROM catabolic_source_mappings WHERE library_id=?",
                    )
                    .bind(library_id)
                    .fetch_one(&mut *db)
                    .await?;
                    let mapping: SourceMapping = serde_json::from_str(&raw)?;
                    // Historical source/profile may not exist in this catalog.
                    // Validate producer output without consulting current binding.
                    observation::validate_policy_snapshot(&mapping.native_policy)?;
                    return Err(error);
                }
                Err(error) => return Err(error),
            };
            let check = mapping.clone();
            self.with_store(false, move |store| inspect_mapping_qualified(store, &check))
                .await?;
            Ok(Some(mapping))
        }
        .await;
        match result {
            Err(error) if error.is::<QualificationRefusal>() => Ok(None),
            other => other,
        }
    }

    /// A scan always retains its session, shared native authorization and host
    /// semaphore until exact worker cleanup. timeout requests cancellation, not
    /// permission to detach or fabricate a successful drain.
    pub async fn scan_source(
        &self,
        library_id: &str,
        helper: TrustedClosedObservationArtifact,
        cancellation: ScanCancellation,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        if timeout.is_zero() {
            bail!("scan timeout must be positive");
        }
        // A detached coordinator, not the caller's future, owns the Motion
        // writer fence. Aborting the caller signals cancellation but cannot let
        // a binding/exclusion edit commit before exact native cleanup finishes.
        let cancel_guard = CancelOnDrop(cancellation.clone());
        let backend = self.clone();
        let library_id = library_id.to_owned();
        let coordinator = tokio::spawn(async move {
            let mut fence = backend.journal.begin_with("BEGIN IMMEDIATE").await?;
            let mapping = backend.source_mapping_in(&mut fence, &library_id).await?;
            let check = mapping.clone();
            backend
                .with_store(false, move |store| inspect_mapping(store, &check))
                .await?;
            let permit = Arc::new(backend.worker.clone().acquire_owned().await?);
            let authorization = backend.authorization.clone();
            let lease = authorization
                .acquire(authorization.revision())
                .map_err(|e| anyhow::anyhow!("scan admission: {e:?}"))?;
            let path = backend.path.clone();
            let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
                let _lease = lease;
                let _permit_through_cleanup = permit.clone();
                if cancellation.is_cancelled() {
                    bail!("scan cancelled before admission");
                }
                let mut host = HostSession::open_authorized(
                    &path,
                    &mapping.profile,
                    true,
                    CONTRACT_VERSION,
                    authorization,
                )?;
                host.register_closed_observation_helper(&helper)?;
                let result = scan_with_host(&mut host, &mapping, &cancellation, timeout, permit)?;
                host.close()?;
                Ok(result)
            })
            .await?;
            // Even failed scans release this fence only after the blocking
            // owner returned (including its supervisor's Drop/join fallback).
            fence.rollback().await?;
            result
        });
        let result = coordinator.await??;
        drop(cancel_guard);
        Ok(result)
    }
}

/// Run through the server's actual retained host/gate. The native supervisor
/// owns `permit` until exact cleanup, including on error or caller cancellation.
/// Errors may leave the host draining; the caller must retain/drain it rather
/// than replacing it with a new independently authorized session.
pub fn scan_with_host<P: Send + 'static>(
    host: &mut HostSession,
    mapping: &SourceMapping,
    cancellation: &ScanCancellation,
    timeout: Duration,
    permit: P,
) -> anyhow::Result<Value> {
    use catabolic_runtime::host::ErrorCode;
    if timeout.is_zero() || cancellation.is_cancelled() {
        bail!("scan cancelled or deadline is empty");
    }
    // The native scan supervisor holds its lease through process exit. Retain an
    // additional shared gate lease through the subsequent durable cleanup seal.
    let authorization = host.authorization()?;
    let _cleanup_lease = authorization
        .acquire(authorization.revision())
        .map_err(|e| anyhow::anyhow!("source cleanup admission: {e:?}"))?;
    let identity = host.identity()?;
    let observed = Identity {
        catalog_id: identity["catalog_id"]
            .as_str()
            .context("host catalog ID missing")?
            .into(),
        incarnation: identity["catalog_incarnation"]
            .as_str()
            .context("host incarnation missing")?
            .into(),
        schema_version: identity["schema_version"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .context("host schema missing")?,
    };
    playscale_core::catalog_backend::admit_retained_source_record(&observed, &mapping.catalog)
        .map_err(|e| anyhow::anyhow!("source host admission: {e:?}"))?;
    if identity["profile"].as_str() != Some(&mapping.profile) {
        bail!("native host profile does not match source mapping");
    }
    let binding = host.source_binding(&mapping.native_source)?;
    if binding != mapping.native_binding {
        bail!("native source binding changed");
    }
    check_root(&binding)?;
    if host.source_observation_policy(&mapping.native_source)? != mapping.native_policy {
        bail!("native source policy changed");
    }
    let handle = host.start_scan(&mapping.native_source, permit)?;
    let started = Instant::now();
    let mut cancellation_acknowledged = false;
    let mut timed_out = false;
    let result = (|| -> anyhow::Result<Value> {
        let mut outcome = None;
        loop {
            timed_out |= started.elapsed() >= timeout;
            if (timed_out || cancellation.is_cancelled()) && !cancellation_acknowledged {
                cancellation_acknowledged = host.cancel_scan(&handle)?.is_some();
            }
            if let Some(result) = host.poll_scan(&handle, Duration::from_millis(50))? {
                outcome = Some(result);
            }
            // Demand terminality is not worker cleanup. Inspect through the
            // same retained native owner only after its supervisor has joined.
            if outcome.is_some() {
                match host.worker_inventory() {
                    Ok(inventory) if inventory.admits_new_work() => break,
                    Ok(_) => bail!("native worker cleanup requires reconciliation"),
                    Err(error) if error.code == ErrorCode::Busy => continue,
                    Err(error) => return Err(error.into()),
                }
            }
        }
        if timed_out {
            bail!("scan timed out; native ownership drained");
        }
        if cancellation.is_cancelled() {
            bail!("scan cancelled; native ownership drained");
        }
        outcome.context("native scan finished without a result")
    })();
    if result.is_err()
        && !host
            .worker_inventory()
            .is_ok_and(|inventory| inventory.admits_new_work())
    {
        host.begin_drain();
        // poll/drain may itself fail. Do not claim cleanup then: the retained
        // HostSession keeps its supervisor and exact native permit on return.
        while !host.drain(Duration::from_millis(50))? {}
    }
    result
}

/// Validate a supplied immutable mapping while the caller retains the Motion
/// writer fence. This uses no second pool connection or native session.
pub async fn check_mapping_in(
    db: &mut sqlx::SqliteConnection,
    mapping: &SourceMapping,
) -> anyhow::Result<()> {
    let encoded: String =
        sqlx::query_scalar("SELECT mapping_json FROM catabolic_source_mappings WHERE library_id=?")
            .bind(&mapping.library_id)
            .fetch_one(&mut *db)
            .await?;
    anyhow::ensure!(
        serde_json::from_str::<SourceMapping>(&encoded)? == *mapping,
        "registered source mapping changed"
    );
    let native: Option<String> =
        sqlx::query_scalar("SELECT descriptor_json FROM catabolic_source_registry WHERE id=?")
            .bind(&mapping.library_id)
            .fetch_optional(&mut *db)
            .await?;
    let has_imported_registry: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='catabolic_imported_source_descriptors')",
    ).fetch_one(&mut *db).await?;
    let imported = if has_imported_registry {
        crate::imported_sources::imported_sources_in(db)
            .await?
            .into_iter()
            .find(|record| record.descriptor.id == mapping.library_id)
    } else {
        None
    };
    anyhow::ensure!(
        native.is_none() || imported.is_none(),
        "source descriptor registries collide"
    );
    if let Some(record) = &imported {
        anyhow::ensure!(
            record.proof.mapping == *mapping,
            "imported source mapping changed"
        );
        let row: Option<String> = sqlx::query_scalar("SELECT json_object('id',id,'name',name,'root',root,'root_identity',root_identity,'managed',managed,'enabled',enabled,'revision',revision,'binding_revision',binding_revision,'exclusions_json',exclusions_json,'scan_barrier',scan_barrier) FROM libraries WHERE id=?")
            .bind(&mapping.library_id).fetch_optional(&mut *db).await?;
        if let Some(row) = row {
            crate::imported_sources::validate_cached_row(
                record,
                &serde_json::from_str::<Value>(&row)?,
            )?;
        } else {
            // Native binding/root/policy checks remain the caller's obligation.
            // Only a missing disposable row can use exact activated ACK proof.
            return Ok(());
        }
    }
    if let Some(native) = native {
        let descriptor: crate::source_registry::NativeSourceDescriptor =
            serde_json::from_str(&native)?;
        anyhow::ensure!(
            descriptor.id == mapping.library_id
                && descriptor.root == mapping.motion_root
                && descriptor.root_identity == mapping.motion_root_identity
                && descriptor.exclusions == mapping.exclusions,
            "native source descriptor changed"
        );
    } else {
        let (root,identity,revision,exclusions,managed):(String,String,i64,String,i64)=sqlx::query_as("SELECT root,root_identity,binding_revision,exclusions_json,managed FROM libraries WHERE id=?").bind(&mapping.library_id).fetch_one(&mut *db).await?;
        let mut observed = mapping.identity();
        observed.motion_root = root;
        observed.motion_root_identity = identity;
        observed.motion_binding_revision = revision;
        observed.exclusions = playscale_core::sources::normalize_exclusions(
            &serde_json::from_str::<Vec<String>>(&exclusions)?,
        )
        .map_err(|e| anyhow::anyhow!("source exclusions: {e:?}"))?;
        anyhow::ensure!(managed == 0, "native source changed to managed");
        mapping
            .identity()
            .admit(&observed)
            .map_err(|e| anyhow::anyhow!("source fence: {e:?}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod audit_tests;
