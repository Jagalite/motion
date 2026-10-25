//! Private exact-original text conversion. No artifact publication or public
//! selector capability is inferred from this process result.
use super::{Source, source_valid, supervised_descriptor};
use crate::{
    App, execution,
    v2::{Problem, auth::Caller, playback},
};
use axum::http::StatusCode;
use playscale_core::access::{self, Permission, Principal};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

type Writer = sqlx::Transaction<'static, sqlx::Sqlite>;

#[derive(Clone, Debug)]
pub(crate) struct Selection {
    pub profile: String,
    pub timeline: String,
    pub version: String,
    pub file: String,
    pub revision: String,
    /// Ordinal among subtitle streams, matching the existing sN contract.
    pub subtitle: u32,
}

/// The final writer remains with the caller until its independent download pin
/// and cache ACK checks finish. This is not an artifact grant by itself.
pub(crate) struct Converted {
    pub writer: Writer,
    pub principal: Principal,
    pub bytes: Vec<u8>,
    pub descriptor: Arc<std::fs::File>,
    pub lease: Arc<execution::Lease>,
    admission: Arc<dyn Send + Sync>,
}

impl Converted {
    /// The opaque source/process admission remains live in publication I/O.
    pub(crate) fn retained(&self) -> Arc<impl Send + Sync + 'static> {
        Arc::new((
            self.admission.clone(),
            self.descriptor.clone(),
            self.lease.clone(),
        ))
    }
}

/// A cache hit still needs current native track/path proof, but not a second
/// FFmpeg execution. Its owned preparation retains the writer on caller loss.
pub(crate) async fn validate_cached(
    app: App,
    caller: Caller,
    selected: Selection,
    stop: CancellationToken,
) -> Result<Converted, Problem> {
    let (mut reply, result) = oneshot::channel();
    tokio::spawn(async move {
        let stop = stop.child_token();
        let work = async {
            let lease = tokio::select! {
                _ = stop.cancelled() => return Err(unavailable()),
                lease = app.processing.execution.reserve(format!("native-subtitle-cache:{}", selected.file), playscale_core::work::Class::Interactive, 1) => Arc::new(lease.map_err(|_| unavailable())?),
            };
            let artifacts = app
                .processing
                .artifacts
                .pin([selected.file.clone()])
                .ok_or_else(unavailable)?;
            let admission = Arc::new(artifacts);
            let (tx, principal, source, _) = current(&app, &caller, &selected).await?;
            let retained = admission.clone();
            let (tx, file) = execution::prepare(
                lease.clone(),
                &stop,
                Duration::from_secs(5),
                move || {
                    let _retained = retained;
                    source_valid(&source).map(|file| (tx, file))
                },
                || {},
            )
            .await
            .ok_or_else(unavailable)?
            .map_err(Problem::internal)?
            .map_err(|_| changed())?;
            if stop.is_cancelled() {
                return Err(unavailable());
            }
            Ok(Converted {
                writer: tx,
                principal,
                bytes: Vec::new(),
                descriptor: Arc::new(file),
                lease,
                admission,
            })
        };
        tokio::pin!(work);
        let outcome = tokio::select! { result = &mut work => result, _ = reply.closed() => { stop.cancel(); work.await } };
        let _ = reply.send(outcome);
    });
    result.await.map_err(Problem::internal)?
}

fn unavailable() -> Problem {
    Problem::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "subtitles_unavailable",
        "Exact native subtitle conversion is unavailable",
    )
}
fn changed() -> Problem {
    Problem::new(
        StatusCode::CONFLICT,
        "source_revision_changed",
        "The exact subtitle source changed",
    )
}

async fn current(
    app: &App,
    caller: &Caller,
    selected: &Selection,
) -> Result<(Writer, Principal, Source, Value), Problem> {
    let mut tx = playback::begin_catalog_application(app, true).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::DownloadsManage))
        .await?;
    let (source, commitment) = facts_in(app, &mut tx, &principal, selected).await?;
    Ok((tx, principal, source, commitment))
}

/// Shared exact original/text proof for candidate pin admission and conversion.
/// Caller already owns the current native writer and fresh authorization.
pub(crate) async fn validate_selection_in(
    app: &App,
    conn: &mut sqlx::SqliteConnection,
    principal: &Principal,
    selected: &Selection,
) -> Result<(), Problem> {
    facts_in(app, conn, principal, selected).await.map(|_| ())
}

async fn facts_in(
    app: &App,
    tx: &mut sqlx::SqliteConnection,
    principal: &Principal,
    selected: &Selection,
) -> Result<(Source, Value), Problem> {
    let runtime = app.native_catalog.as_ref().ok_or_else(unavailable)?;
    runtime
        .validate_projection(tx)
        .await
        .map_err(Problem::internal)?;
    playback::usable_profile(&mut *tx, principal, &selected.profile).await?;
    playback::readable_timeline(&mut *tx, principal, &selected.timeline).await?;
    if !playback::native_version_ready(&mut *tx, &selected.version).await? {
        return Err(changed());
    }
    let facts = crate::v2::content::file_facts(&mut *tx, &selected.file)
        .await?
        .ok_or_else(changed)?;
    if facts.revision != selected.revision
        || !access::catalog_scope(principal).admits(facts.library_ids.iter())
    {
        return Err(Problem::not_found());
    }
    let exact:bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM media_versions v JOIN version_files b ON b.version_id=v.id JOIN media_files f ON f.id=b.file_id WHERE v.id=? AND v.timeline_id=? AND v.origin='original' AND b.part=1 AND b.file_id=? AND b.file_revision=? AND f.revision=b.file_revision AND f.generated=0 AND f.available=1 AND b.start_ms IS NULL AND b.end_ms IS NULL AND NOT EXISTS(SELECT 1 FROM version_files other WHERE other.version_id=v.id AND other.part<>1))")
        .bind(&selected.version).bind(&selected.timeline).bind(&selected.file).bind(&selected.revision).fetch_one(&mut *tx).await?;
    if !exact {
        return Err(changed());
    }
    let encoded:String = sqlx::query_scalar("SELECT p.mapping_json FROM catabolic_projection_files p JOIN catabolic_projection_state s ON s.singleton=1 AND s.generation=p.generation WHERE p.file_id=? AND p.available=1")
        .bind(&selected.file).fetch_one(&mut *tx).await?;
    let mapping: motion_catalog_backend::occurrence_bridge::OccurrenceMapping =
        serde_json::from_str(&encoded).map_err(Problem::internal)?;
    if mapping.motion_file["id"] != selected.file
        || mapping.motion_file["revision"] != selected.revision
        || mapping.motion_file["generated"] != 0
    {
        return Err(changed());
    }
    let raw = mapping.motion_file["tracks_json"]
        .as_str()
        .filter(|v| v.len() <= 1024 * 1024)
        .ok_or_else(unavailable)?;
    let tracks: Vec<crate::db::Track> = serde_json::from_str(raw).map_err(Problem::internal)?;
    let track = tracks
        .iter()
        .filter(|t| t.kind == "subtitle")
        .nth(selected.subtitle as usize)
        .ok_or_else(|| {
            Problem::invalid(
                "download_subtitle_unavailable",
                "The selected text subtitle is unavailable",
            )
        })?;
    if !playscale_core::delivery::sidecar_subtitle(&track.codec) {
        return Err(Problem::invalid(
            "download_subtitle_unavailable",
            "The selected subtitle is not a supported embedded text track",
        ));
    }
    let source_mapping = mapping.source.clone();
    runtime
        .with_host(move |host| {
            let observed = host.source_occurrences(&source_mapping.native_source)?;
            anyhow::ensure!(
                observed["binding"] == source_mapping.native_binding
                    && observed["policy"] == source_mapping.native_policy,
                "native subtitle source authority changed"
            );
            Ok(())
        })
        .await
        .map_err(Problem::internal)?;
    let source = Source {
        root: mapping.source.motion_root.clone().into(),
        relative: mapping.motion_file["relative_path"]
            .as_str()
            .ok_or_else(unavailable)?
            .into(),
        root_identity: mapping.source.motion_root_identity.clone(),
        fingerprint: mapping.motion_file["fingerprint"]
            .as_str()
            .ok_or_else(unavailable)?
            .into(),
    };
    Ok((
        source,
        serde_json::to_value(mapping).map_err(Problem::internal)?,
    ))
}

/// Caller loss cancels, but never drops the owner performing descriptor work.
/// The temporary public selection refusal remains outside this private owner.
pub(crate) async fn convert(
    app: App,
    caller: Caller,
    selected: Selection,
    stop: CancellationToken,
) -> Result<Converted, Problem> {
    let (mut reply, result) = oneshot::channel();
    tokio::spawn(async move {
        let cancellation = stop.child_token();
        let work = convert_owned(app, caller, selected, cancellation.clone());
        tokio::pin!(work);
        let outcome = tokio::select! {
            result = &mut work => result,
            _ = reply.closed() => { cancellation.cancel(); work.await }
        };
        let _ = reply.send(outcome);
    });
    result.await.map_err(Problem::internal)?
}

async fn convert_owned(
    app: App,
    caller: Caller,
    selected: Selection,
    stop: CancellationToken,
) -> Result<Converted, Problem> {
    let lease = tokio::select! {
        _ = stop.cancelled() => return Err(unavailable()),
        lease = app.processing.execution.reserve(format!("native-subtitle:{}:{}", selected.file, crate::new_id()), playscale_core::work::Class::Interactive, 1) => Arc::new(lease.map_err(|_| unavailable())?),
    };
    let slot = app
        .processing
        .deliveries
        .subtitle_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| unavailable())?;
    let artifacts = app
        .processing
        .artifacts
        .pin([selected.file.clone()])
        .ok_or_else(unavailable)?;
    let admission = Arc::new((slot, artifacts));
    let (tx, _, source, commitment) = current(&app, &caller, &selected).await?;
    let initial_admission = admission.clone();
    let opened = execution::prepare(
        lease.clone(),
        &stop,
        Duration::from_secs(5),
        move || {
            let _writer = tx;
            let _admission = initial_admission;
            source_valid(&source)
        },
        || {},
    )
    .await
    .ok_or_else(unavailable)?
    .map_err(Problem::internal)?
    .map_err(|_| changed())?;
    let witness_root = app
        .processing
        .root
        .join("subtitle-owners")
        .join(crate::new_id());
    let directory = witness_root.clone();
    let directory_admission = admission.clone();
    execution::prepare(
        lease.clone(),
        &stop,
        Duration::from_secs(5),
        move || {
            let _admission = directory_admission;
            std::fs::create_dir_all(directory)
        },
        || {},
    )
    .await
    .ok_or_else(unavailable)?
    .map_err(Problem::internal)?
    .map_err(Problem::internal)?;
    let converted = supervised_descriptor::run(
        supervised_descriptor::Request {
            supervisor: app.processing.supervisor.clone(),
            program: app.processing.settings.ffmpeg.clone(),
            args: vec![
                "-v".into(),
                "error".into(),
                "-nostdin".into(),
                "-i".into(),
                "/dev/fd/3".into(),
                "-map".into(),
                format!("0:s:{}", selected.subtitle).into(),
                "-c:s".into(),
                "webvtt".into(),
                "-f".into(),
                "webvtt".into(),
                "pipe:1".into(),
            ],
            input: opened,
            witness: witness_root.join(".owner"),
            max_stdout: 16 * 1024 * 1024,
            max_stderr: 64 * 1024,
            deadline: Duration::from_secs(120),
        },
        lease.clone(),
        admission.clone(),
        stop.clone(),
    )
    .await
    .map_err(Problem::internal)?;
    if stop.is_cancelled() {
        return Err(unavailable());
    }
    if !std::str::from_utf8(&converted.bytes)
        .ok()
        .is_some_and(|s| playscale_core::subtitles::parse_webvtt(s).is_some())
    {
        return Err(Problem::invalid(
            "subtitle_conversion_failed",
            "The converted subtitle is invalid",
        ));
    }
    let (tx, principal, source, latest) = current(&app, &caller, &selected).await?;
    if commitment != latest {
        return Err(changed());
    }
    // Reopen the named path under fresh writer custody as well as checking the
    // retained descriptor. An old open inode alone cannot prove current path
    // or root authority after a rename/replacement.
    let descriptor = converted.input.clone();
    let final_admission = admission.clone();
    let tx = execution::prepare(
        lease.clone(),
        &stop,
        Duration::from_secs(5),
        move || {
            let _admission = final_admission;
            let fresh = source_valid(&source)?;
            anyhow::ensure!(
                crate::scan::fingerprint(&descriptor.metadata()?) == source.fingerprint,
                "retained subtitle source changed"
            );
            drop(fresh);
            Ok::<_, anyhow::Error>(tx)
        },
        || {},
    )
    .await
    .ok_or_else(unavailable)?
    .map_err(Problem::internal)?
    .map_err(|_| changed())?;
    if stop.is_cancelled() {
        return Err(unavailable());
    }
    Ok(Converted {
        writer: tx,
        principal,
        bytes: converted.bytes,
        descriptor: converted.input,
        lease,
        admission,
    })
}
