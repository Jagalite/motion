use crate::{
    App,
    db::{self, JobRow, Track},
    new_id,
};
use anyhow::Context;
use playscale_core::jobs::{Effect, Input, Phase, transition};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;

pub struct Found {
    pub relative: String,
    pub title: String,
    pub revision: String,
    pub fingerprint: String,
    pub bytes: i64,
    pub duration: Option<f64>,
    pub tracks: Vec<Track>,
    pub reused: bool,
}

pub fn fingerprint(meta: &std::fs::Metadata) -> String {
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        format!(
            "{}:{modified}:{}:{}:{}:{}",
            meta.len(),
            meta.dev(),
            meta.ino(),
            meta.ctime(),
            meta.ctime_nsec()
        )
    }
    #[cfg(not(unix))]
    {
        format!("{}:{modified}", meta.len())
    }
}

pub fn open_file(root: &Path, relative: &Path) -> anyhow::Result<std::fs::File> {
    anyhow::ensure!(
        relative.is_relative()
            && relative
                .components()
                .all(|p| matches!(p, std::path::Component::Normal(_))),
        "invalid media path"
    );
    let dir = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
    let file = dir.open(relative)?.into_std();
    anyhow::ensure!(file.metadata()?.is_file(), "not a regular file");
    Ok(file)
}

fn inventory(root: &Path, stop: &CancellationToken) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false).max_open(16) {
        anyhow::ensure!(!stop.is_cancelled(), "scan cancelled");
        let entry = entry?; // Any traversal failure aborts reconciliation; never mark an unreadable tree missing.
        if !entry.file_type().is_file() {
            continue;
        }
        let extension = entry
            .path()
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ![
            "mp4", "m4v", "mkv", "webm", "mov", "avi", "ts", "m2ts", "mp3", "m4a", "flac", "ogg",
            "wav", "opus",
        ]
        .contains(&extension.as_str())
        {
            continue;
        }
        files.push(entry.path().strip_prefix(root)?.to_path_buf());
        anyhow::ensure!(files.len() <= 100_000, "core scan limit exceeded");
    }
    files.sort();
    Ok(files)
}

pub(crate) async fn inspect(
    root: PathBuf,
    relative: PathBuf,
    ffprobe: &Path,
    stop: CancellationToken,
    permit: std::sync::Arc<tokio::sync::OwnedSemaphorePermit>,
) -> anyhow::Result<Found> {
    let rel = relative
        .to_str()
        .context("media path must be UTF-8")?
        .to_string();
    let title = relative
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled")
        .to_string();
    let hashing_permit = permit.clone();
    let (file, revision, stamp, bytes) =
        tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let _hold = hashing_permit;
            let mut file = open_file(&root, &relative)?;
            let before = fingerprint(&file.metadata()?);
            let bytes = i64::try_from(file.metadata()?.len())?;
            let mut hash = Sha256::new();
            let mut buf = [0_u8; 128 * 1024];
            loop {
                anyhow::ensure!(!stop.is_cancelled(), "scan cancelled");
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hash.update(&buf[..n]);
            }
            anyhow::ensure!(
                before == fingerprint(&file.metadata()?),
                "source changed during scan"
            );
            file.seek(SeekFrom::Start(0))?;
            Ok((file, format!("{:x}", hash.finalize()), before, bytes))
        })
        .await??;
    // A regular-file stdin keeps probing bound to the opened file, never a shell/path URL.
    let check_file = file.try_clone()?;
    let mut child = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration:stream=index,codec_type,codec_name,width,height,avg_frame_rate,bit_rate,pix_fmt,color_transfer,start_time:stream_tags=language",
            "-of",
            "json",
            "-i",
            "pipe:0",
        ])
        .stdin(file)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdout = child.stdout.take().context("missing probe output")?;
    let mut output = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        (&mut stdout)
            .take(1024 * 1024 + 1)
            .read_to_end(&mut output)
            .await?;
        anyhow::ensure!(output.len() <= 1024 * 1024, "probe output limit exceeded");
        let status = child.wait().await?;
        anyhow::ensure!(status.success(), "ffprobe could not inspect media");
        Ok::<_, anyhow::Error>(())
    })
    .await;
    // Unsupported/corrupt media remains cataloged with unknown technical metadata.
    let value: serde_json::Value = if matches!(result, Ok(Ok(()))) {
        serde_json::from_slice(&output)?
    } else {
        serde_json::json!({})
    };
    let current_stamp = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let _hold = permit;
        Ok(fingerprint(&check_file.metadata()?))
    })
    .await??;
    anyhow::ensure!(stamp == current_stamp, "source changed during probe");
    let duration = value["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n >= 0.0);
    let tracks = value["streams"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| Track {
            index: s["index"].as_i64().unwrap_or(0),
            kind: s["codec_type"].as_str().unwrap_or("unknown").into(),
            codec: s["codec_name"].as_str().unwrap_or("unknown").into(),
            language: s["tags"]["language"].as_str().map(str::to_owned),
            width: s["width"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| *n > 0),
            height: s["height"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| *n > 0),
            average_frame_rate: s["avg_frame_rate"]
                .as_str()
                .and_then(crate::encoding::FrameRate::parse),
            bitrate: s["bit_rate"]
                .as_str()
                .and_then(|v| v.parse().ok())
                .filter(|v: &u64| *v > 0),
            pixel_format: s["pix_fmt"].as_str().map(str::to_owned),
            color_transfer: s["color_transfer"].as_str().map(str::to_owned),
            start_time_seconds: s["start_time"]
                .as_str()
                .and_then(|v| v.parse().ok())
                .filter(|v: &f64| v.is_finite()),
        })
        .collect();
    Ok(Found {
        relative: rel,
        title,
        revision,
        fingerprint: stamp,
        bytes,
        duration,
        tracks,
        reused: false,
    })
}

pub async fn run_scan(app: &App, job: &JobRow, shutdown: &CancellationToken) -> anyhow::Result<()> {
    let permit = tokio::select! {_=shutdown.cancelled()=>anyhow::bail!("server stopping"),permit=app.storage.scan_io.clone().acquire_owned()=>std::sync::Arc::new(permit?)};
    let (root, identity): (String, String) =
        sqlx::query_as("SELECT root,root_identity FROM libraries WHERE id=?")
            .bind(&job.library_id)
            .fetch_one(&app.db)
            .await?;
    let path = PathBuf::from(root);
    let metadata_path = path.clone();
    let hold = permit.clone();
    let actual = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let _hold = hold;
        Ok(db::root_identity(&std::fs::metadata(metadata_path)?))
    })
    .await??;
    anyhow::ensure!(actual == identity, "library root was replaced");
    let inventory_path = path.clone();
    let stop = shutdown.child_token();
    let inventory_stop = stop.clone();
    let hold = permit.clone();
    let mut traversal = tokio::task::spawn_blocking(move || {
        let _hold = hold;
        inventory(&inventory_path, &inventory_stop)
    });
    let files = loop {
        tokio::select! {
            result = &mut traversal => break result??,
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                let current = db::get_job(&app.db, &job.id).await?;
                if shutdown.is_cancelled() || !current.state()?.executing(job.attempt.try_into()?) {
                    stop.cancel(); let _ = traversal.await; anyhow::bail!("scan interrupted");
                }
            }
        }
    };
    let mut cached = std::collections::HashMap::new();
    if app.storage.settings.incremental_scans && cfg!(unix) && !job.full_scan {
        let rows: Vec<db::ItemRow> =
            sqlx::query_as("SELECT * FROM catalog_files WHERE library_id=?")
                .bind(&job.library_id)
                .fetch_all(&app.db)
                .await?;
        for row in rows {
            cached.insert(row.relative_path.clone(), row);
        }
    }
    let mut found = Vec::new();
    for relative in files {
        let current = db::get_job(&app.db, &job.id).await?;
        if shutdown.is_cancelled() || !current.state()?.executing(job.attempt.try_into()?) {
            stop.cancel();
            anyhow::bail!("scan interrupted");
        }
        if let Some(row) = relative.to_str().and_then(|p| cached.remove(p)) {
            let root = path.clone();
            let rel = relative.clone();
            let hold = permit.clone();
            let reused = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Found>> {
                let _hold = hold;
                let file = open_file(&root, &rel)?;
                let stamp = fingerprint(&file.metadata()?);
                if stamp != row.fingerprint {
                    return Ok(None);
                }
                Ok(Some(Found {
                    relative: row.relative_path,
                    title: row.title,
                    revision: row.revision,
                    fingerprint: stamp,
                    bytes: row.bytes,
                    duration: row.duration_seconds,
                    tracks: serde_json::from_str(&row.tracks_json)?,
                    reused: true,
                }))
            })
            .await??;
            if let Some(file) = reused {
                found.push(file);
                continue;
            }
        }
        let inspect = inspect(
            path.clone(),
            relative,
            &app.ffprobe,
            stop.clone(),
            permit.clone(),
        );
        tokio::pin!(inspect);
        loop {
            tokio::select! {
                result = &mut inspect => { found.push(result?); break; }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    let current = db::get_job(&app.db, &job.id).await?;
                    if shutdown.is_cancelled() || !current.state()?.executing(job.attempt.try_into()?) {
                        stop.cancel(); anyhow::bail!("scan interrupted");
                    }
                }
            }
        }
    }
    anyhow::ensure!(!shutdown.is_cancelled(), "server stopping");
    let hold = permit.clone();
    let final_identity = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let _hold = hold;
        Ok(db::root_identity(&std::fs::metadata(path)?))
    })
    .await??;
    let complete_root = Some((identity, final_identity));
    finish(app, job, Some(found), complete_root).await
}

async fn finish(
    app: &App,
    job: &JobRow,
    found: Option<Vec<Found>>,
    complete_root: Option<(String, String)>,
) -> anyhow::Result<()> {
    let _guard = app.jobs.lock().await;
    let current = db::get_job(&app.db, &job.id).await?;
    let (root, enabled): (String, bool) =
        sqlx::query_as("SELECT root_identity,enabled FROM libraries WHERE id=?")
            .bind(&job.library_id)
            .fetch_one(&app.db)
            .await?;
    let previous = current.state()?;
    let (next, effects) = playscale_core::scan::finish(
        &previous,
        &playscale_core::scan::Completion {
            attempt: job.attempt.try_into()?,
            complete_inventory_roots: found.as_ref().and(complete_root),
            current_root: root,
            library_enabled: enabled,
        },
    );
    if next == previous && effects.is_empty() {
        return Ok(());
    }
    let mut tx = crate::db::begin_write(&app.db).await?;
    if effects.iter().any(|e| matches!(e, Effect::Publish { .. })) {
        let files = found.unwrap_or_default();
        let old: Vec<(String, String, String, String)> = sqlx::query_as(
            "SELECT id,edition_id,relative_path,revision FROM media_files WHERE library_id=?",
        )
        .bind(&job.library_id)
        .fetch_all(&mut *tx)
        .await?;
        let plan = playscale_core::scan::reconcile(
            &old.iter()
                .map(|r| playscale_core::scan::Existing {
                    id: r.0.clone(),
                    edition: r.1.clone(),
                    path: r.2.clone(),
                    revision: r.3.clone(),
                })
                .collect::<Vec<_>>(),
            &files
                .iter()
                .map(|f| playscale_core::scan::Observed {
                    path: f.relative.clone(),
                    revision: f.revision.clone(),
                })
                .collect::<Vec<_>>(),
        );
        for id in &plan.unavailable {
            sqlx::query("UPDATE media_files SET available=0 WHERE id=? AND available=1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        let reused = files.iter().filter(|f| f.reused).count() as i64;
        sqlx::query("UPDATE jobs SET reused_files=?,inspected_files=? WHERE id=?")
            .bind(reused)
            .bind(files.len() as i64 - reused)
            .bind(&job.id)
            .execute(&mut *tx)
            .await?;
        for (file, identity) in files.into_iter().zip(plan.identities) {
            let (id, edition) = if let Some(identity) = identity {
                identity
            } else {
                let item = new_id();
                let edition = new_id();
                let kind = if file.tracks.iter().any(|t| t.kind == "video") {
                    "video"
                } else if file.tracks.iter().any(|t| t.kind == "audio") {
                    "audio"
                } else {
                    "video"
                };
                sqlx::query("INSERT INTO items VALUES (?,?,?)")
                    .bind(&item)
                    .bind(&file.title)
                    .bind(kind)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("INSERT INTO item_origins VALUES (?,?)")
                    .bind(&item)
                    .bind(&file.title)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("INSERT INTO editions (id,item_id,label) VALUES (?,?,'Original')")
                    .bind(&edition)
                    .bind(item)
                    .execute(&mut *tx)
                    .await?;
                (new_id(), edition)
            };
            sqlx::query("INSERT INTO media_files (id,edition_id,library_id,relative_path,revision,fingerprint,bytes,duration_seconds,tracks_json,available) VALUES (?,?,?,?,?,?,?,?,?,1) ON CONFLICT(id) DO UPDATE SET relative_path=excluded.relative_path,revision=excluded.revision,fingerprint=excluded.fingerprint,bytes=excluded.bytes,duration_seconds=excluded.duration_seconds,tracks_json=excluded.tracks_json,available=1 WHERE media_files.revision<>excluded.revision OR media_files.fingerprint<>excluded.fingerprint OR media_files.relative_path<>excluded.relative_path OR media_files.available=0 OR media_files.tracks_json<>excluded.tracks_json OR media_files.duration_seconds IS NOT excluded.duration_seconds")
                .bind(id).bind(edition).bind(&job.library_id).bind(file.relative).bind(file.revision).bind(file.fingerprint).bind(file.bytes).bind(file.duration).bind(serde_json::to_string(&file.tracks)?).execute(&mut *tx).await?;
        }
    }
    sqlx::query("UPDATE jobs SET phase=?, error=? WHERE id=?")
        .bind(db::phase_name(next.phase))
        .bind(if next.phase == Phase::Failed {
            Some("Scan failed; see server diagnostics")
        } else {
            None
        })
        .bind(&job.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn worker(app: App, shutdown: CancellationToken) -> anyhow::Result<()> {
    loop {
        if shutdown.is_cancelled() {
            return Ok(());
        }
        let job = {
            let _guard = app.jobs.lock().await;
            let row = sqlx::query_as::<_, JobRow>(
                "SELECT * FROM jobs WHERE phase='queued' ORDER BY created_at,id LIMIT 1",
            )
            .fetch_optional(&app.db)
            .await?;
            if let Some(row) = row {
                let (next, effects) = transition(&row.state()?, Input::Start);
                let runs = matches!(effects.as_slice(), [Effect::Run { .. }]);
                sqlx::query("UPDATE jobs SET phase=?,attempt=?,error=? WHERE id=?")
                    .bind(db::phase_name(next.phase))
                    .bind(next.attempt)
                    .bind((!runs).then_some("job attempt limit reached"))
                    .bind(&row.id)
                    .execute(&app.db)
                    .await?;
                if runs {
                    Some(db::get_job(&app.db, &row.id).await?)
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some(job) = job {
            if let Err(error) = run_scan(&app, &job, &shutdown).await {
                tracing::warn!(job_id=%job.id, %error, "scan did not complete");
                if shutdown.is_cancelled() {
                    return Ok(());
                } // startup recovery requeues the persisted attempt
                finish(&app, &job, None, None).await?;
            }
        } else {
            tokio::select! { _ = shutdown.cancelled() => return Ok(()), _ = tokio::time::sleep(Duration::from_millis(100)) => {} }
        }
    }
}

/// Unavailable configured roots are an operational failure, not a reason to hide
/// the catalog or prevent the HTTP server from starting.
pub async fn configure(
    app: App,
    roots: Vec<PathBuf>,
    stop: CancellationToken,
) -> anyhow::Result<()> {
    let mut failed = false;
    for root in roots {
        let register = async {
            let root = crate::administration::candidate_root(&app, root).await?;
            let library = crate::administration::register_root(&app, root, None).await?;
            let enabled: bool = sqlx::query_scalar("SELECT enabled FROM libraries WHERE id=?")
                .bind(&library.id)
                .fetch_one(&app.db)
                .await?;
            if enabled {
                db::enqueue(&app, &library.id).await?;
            }
            Ok::<_, anyhow::Error>(())
        };
        let result = tokio::select! {_=stop.cancelled()=>return Ok(()),result=register=>result};
        if let Err(error) = result {
            failed = true;
            tracing::warn!(%error,"configured library unavailable");
        }
    }
    crate::storage::record(
        &app,
        "configured_libraries",
        if failed {
            Some("configured_library_unavailable")
        } else {
            None
        },
    )
    .await?;
    Ok(())
}
