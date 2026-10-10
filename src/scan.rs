use crate::{
    App,
    db::{self, JobRow, Track},
    new_id,
};
use anyhow::Context;
use playscale_core::jobs::{Effect, Input, Phase, transition};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
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
    /// Sidecar NFO observed and parsed beside the file (outside the writer).
    pub nfo: crate::nfo::Sidecar,
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

/// Media files beyond this count are not inspected; the attempt reports partial
/// coverage instead of an apparently complete inventory.
pub const FILE_LIMIT: usize = 100_000;
/// Incomplete directories retained per attempt for diagnostics.
const INCOMPLETE_REPORT_LIMIT: usize = 200;

#[derive(Default)]
pub struct Inventory {
    pub files: Vec<PathBuf>,
    pub complete: BTreeSet<String>,
    pub incomplete: BTreeMap<String, &'static str>,
}
impl Inventory {
    fn coverage(&self) -> playscale_core::scan::Coverage {
        playscale_core::scan::Coverage::Listed {
            complete: self.complete.clone(),
            incomplete: self.incomplete.keys().cloned().collect(),
        }
    }
    /// A file that could not be inspected leaves its directory unproven.
    fn uninspected(&mut self, relative: &Path) {
        let file = relative.to_str().unwrap_or_default();
        let dir = playscale_core::scan::parent(file).to_owned();
        self.complete.remove(&dir);
        self.incomplete.entry(dir).or_insert("inspection_failed");
    }
}

fn key(relative: &Path) -> Option<String> {
    let parts: Option<Vec<&str>> = relative
        .components()
        .map(|c| c.as_os_str().to_str())
        .collect();
    parts.map(|p| p.join("/"))
}
fn device(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.dev()
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        0
    }
}
const MEDIA: [&str; 14] = [
    "mp4", "m4v", "mkv", "webm", "mov", "avi", "ts", "m2ts", "mp3", "m4a", "flac", "ogg", "wav",
    "opus",
];

/// Enumerate directories one complete listing at a time. Only the root listing is
/// mandatory; any other failure leaves that directory (and what it would have
/// proven) incomplete rather than aborting or implying absence. Symlinks are not
/// followed and nested mounts are boundaries: an unmounted volume's empty mount
/// point must never prove that its files disappeared.
/// Load a source's normalized exclusions.
async fn exclusions(db: &sqlx::SqlitePool, source: &str) -> anyhow::Result<Vec<String>> {
    let text: String = sqlx::query_scalar("SELECT exclusions_json FROM libraries WHERE id=?")
        .bind(source)
        .fetch_one(db)
        .await?;
    Ok(serde_json::from_str(&text)?)
}

fn inventory(
    root: &Path,
    exclusions: &[String],
    stop: &CancellationToken,
) -> anyhow::Result<Inventory> {
    let out_of_scope =
        |path: &Path| key(path).is_some_and(|k| playscale_core::sources::excluded(&k, exclusions));
    let root_device = device(&std::fs::symlink_metadata(root)?);
    let mut out = Inventory::default();
    let mut pending = vec![PathBuf::new()];
    let mut first = true;
    while let Some(relative) = pending.pop() {
        anyhow::ensure!(!stop.is_cancelled(), "scan cancelled");
        let Some(name) = key(&relative) else {
            continue; // Non-UTF-8 directories hold no catalogable paths.
        };
        let listing = std::fs::read_dir(root.join(&relative))
            .and_then(|entries| entries.collect::<Result<Vec<_>, _>>());
        let mut entries = match listing {
            Ok(entries) => entries,
            Err(error) if first => {
                return Err(error).context("source root could not be listed");
            }
            Err(_) => {
                out.incomplete.insert(name, "unreadable");
                continue;
            }
        };
        first = false;
        entries.sort_by_key(|e| e.file_name());
        let mut reason = None;
        for entry in entries {
            let child = relative.join(entry.file_name());
            // Excluded paths are outside the source's scope: neither listed,
            // inspected, nor counted as unproven.
            if out_of_scope(&child) {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                reason = Some("unreadable_entry");
                continue;
            };
            if kind.is_dir() {
                match entry.metadata() {
                    Ok(meta) if device(&meta) == root_device => pending.push(child),
                    Ok(_) => {
                        if let Some(dir) = key(&child) {
                            out.incomplete.insert(dir, "mount_boundary");
                        }
                    }
                    Err(_) => reason = Some("unreadable_entry"),
                }
                continue;
            }
            let extension = child
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !kind.is_file() || !MEDIA.contains(&extension.as_str()) {
                continue;
            }
            if key(&child).is_none() {
                reason = Some("non_utf8_path");
            } else if out.files.len() >= FILE_LIMIT {
                reason = Some("file_limit");
            } else {
                out.files.push(child);
            }
        }
        match reason {
            None => {
                out.complete.insert(name);
            }
            Some(reason) => {
                out.incomplete.insert(name, reason);
            }
        }
        if reason == Some("file_limit") {
            for dir in pending.drain(..).filter_map(|d| key(&d)) {
                out.incomplete.insert(dir, "file_limit");
            }
        }
    }
    out.files.sort();
    Ok(out)
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
        nfo: crate::nfo::Sidecar::Absent,
    })
}

pub async fn run_scan(app: &App, job: &JobRow, shutdown: &CancellationToken) -> anyhow::Result<()> {
    let permit = tokio::select! {_=shutdown.cancelled()=>anyhow::bail!("server stopping"),permit=app.storage.scan_io.clone().acquire_owned()=>std::sync::Arc::new(permit?)};
    let (root, identity): (String, String) =
        sqlx::query_as("SELECT root,root_identity FROM libraries WHERE id=?")
            .bind(&job.library_id)
            .fetch_one(&app.db)
            .await?;
    let inventory_scope = exclusions(&app.db, &job.library_id).await?;
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
        inventory(&inventory_path, &inventory_scope, &inventory_stop)
    });
    let mut inventory = loop {
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
    for relative in std::mem::take(&mut inventory.files) {
        let current = db::get_job(&app.db, &job.id).await?;
        if shutdown.is_cancelled() || !current.state()?.executing(job.attempt.try_into()?) {
            stop.cancel();
            anyhow::bail!("scan interrupted");
        }
        if let Some(row) = relative.to_str().and_then(|p| cached.remove(p)) {
            let root = path.clone();
            let rel = relative.clone();
            let hold = permit.clone();
            let reused =
                match tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Found>> {
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
                        nfo: crate::nfo::Sidecar::Absent,
                    }))
                })
                .await?
                {
                    Ok(reused) => reused,
                    Err(error) => {
                        tracing::debug!(%error, "file could not be revalidated");
                        inventory.uninspected(&relative);
                        continue;
                    }
                };
            if let Some(file) = reused {
                found.push(file);
                continue;
            }
        }
        let inspect = inspect(
            path.clone(),
            relative.clone(),
            &app.ffprobe,
            stop.clone(),
            permit.clone(),
        );
        tokio::pin!(inspect);
        loop {
            tokio::select! {
                result = &mut inspect => {
                    match result {
                        Ok(file) => found.push(file),
                        Err(error) if stop.is_cancelled() || shutdown.is_cancelled() => return Err(error),
                        // One unreadable or changing file leaves only its directory unproven.
                        Err(error) => {
                            tracing::debug!(%error, "file could not be inspected");
                            inventory.uninspected(&relative);
                        }
                    }
                    break;
                }
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
    // Sidecar NFOs are small bounded reads, done before the writer transaction.
    let sidecar_root = path.clone();
    let hold = permit.clone();
    found = tokio::task::spawn_blocking(move || {
        let _hold = hold;
        let mut budget = crate::nfo::SCAN_BUDGET;
        found
            .into_iter()
            .map(|mut f| {
                f.nfo = crate::nfo::read(&sidecar_root, &f.relative, &mut budget);
                f
            })
            .collect::<Vec<_>>()
    })
    .await?;
    let hold = permit.clone();
    let final_identity = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let _hold = hold;
        Ok(db::root_identity(&std::fs::metadata(path)?))
    })
    .await??;
    let complete_root = Some((identity, final_identity));
    finish(app, job, Some((found, inventory)), complete_root).await
}

async fn finish(
    app: &App,
    job: &JobRow,
    found: Option<(Vec<Found>, Inventory)>,
    complete_root: Option<(String, String)>,
) -> anyhow::Result<()> {
    let _guard = app.jobs.lock().await;
    let current = db::get_job(&app.db, &job.id).await?;
    let (root, enabled, binding_now, scope): (String, bool, i64, String) = sqlx::query_as(
        "SELECT root_identity,enabled,binding_revision,exclusions_json FROM libraries WHERE id=?",
    )
    .bind(&job.library_id)
    .fetch_one(&app.db)
    .await?;
    let scope: Vec<String> = serde_json::from_str(&scope)?;
    // Jobs started before migration 0017 carry no binding; they are fenced by
    // the root identity check alone.
    let binding_at_start = current.binding_revision.unwrap_or(binding_now);
    let previous = current.state()?;
    let (next, effects) = playscale_core::scan::finish(
        &previous,
        &playscale_core::scan::Completion {
            attempt: job.attempt.try_into()?,
            complete_inventory_roots: found.as_ref().and(complete_root),
            current_root: root,
            library_enabled: enabled,
            binding_at_start: u64::try_from(binding_at_start)?,
            binding_now: u64::try_from(binding_now)?,
        },
    );
    if next == previous && effects.is_empty() {
        return Ok(());
    }
    let mut tx = crate::db::begin_write(&app.db).await?;
    if effects.iter().any(|e| matches!(e, Effect::Publish { .. }))
        && let Some((files, inventory)) = found
    {
        let coverage = inventory.coverage();
        let old: Vec<(String, String, String, String)> = sqlx::query_as(
            "SELECT id,edition_id,relative_path,revision FROM media_files WHERE library_id=?",
        )
        .bind(&job.library_id)
        .fetch_all(&mut *tx)
        .await?;
        let (plan, excluded) = playscale_core::scan::reconcile_scoped(
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
            &coverage,
            &scope,
        );
        // Proven absences and newly excluded files both leave the browsable
        // catalog; only the former is recorded as observed absence.
        for id in plan.unavailable.iter().chain(&excluded) {
            sqlx::query("UPDATE media_files SET available=0 WHERE id=? AND available=1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        let reused = files.iter().filter(|f| f.reused).count() as i64;
        let outcome = match playscale_core::scan::outcome(true, inventory.incomplete.len()) {
            Some(playscale_core::scan::Outcome::Partial) => "partial",
            _ => "complete",
        };
        sqlx::query("UPDATE jobs SET reused_files=?,inspected_files=?,outcome=?,complete_directories=?,incomplete_directories=? WHERE id=?")
            .bind(reused)
            .bind(files.len() as i64 - reused)
            .bind(outcome)
            .bind(i64::try_from(inventory.complete.len())?)
            .bind(i64::try_from(inventory.incomplete.len())?)
            .bind(&job.id)
            .execute(&mut *tx)
            .await?;
        for (directory, reason) in inventory.incomplete.iter().take(INCOMPLETE_REPORT_LIMIT) {
            sqlx::query("INSERT OR REPLACE INTO scan_incomplete_directories VALUES (?,?,?)")
                .bind(&job.id)
                .bind(directory)
                .bind(reason)
                .execute(&mut *tx)
                .await?;
        }
        let mut assigned: Vec<String> = Vec::with_capacity(files.len());
        for (mut file, assignment) in files.into_iter().zip(plan.assignments) {
            let sidecar = std::mem::replace(&mut file.nfo, crate::nfo::Sidecar::Absent);
            use playscale_core::scan::Assignment;
            let mut first_version = false;
            let (id, edition) = match assignment {
                Assignment::OutOfScope => {
                    assigned.push(String::new());
                    continue;
                }
                Assignment::CopyOf { observation } => (
                    new_id(),
                    assigned
                        .get(observation)
                        .cloned()
                        .context("copy refers to a later observation")?,
                ),
                Assignment::Existing { id, edition } => (id, edition),
                // A verified copy of content one edition already holds is another
                // occurrence of that edition, not a new work.
                Assignment::Copy { edition } => (new_id(), edition),
                Assignment::New => {
                    let item = new_id();
                    let edition = new_id();
                    let kind = if file.tracks.iter().any(|t| t.kind == "video") {
                        "video"
                    } else if file.tracks.iter().any(|t| t.kind == "audio") {
                        "audio"
                    } else {
                        "video"
                    };
                    sqlx::query("INSERT INTO items (id,title,kind) VALUES (?,?,?)")
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
                    crate::curation::create_edition(&mut tx, &edition, &item, "Original").await?;
                    first_version = true;
                    (new_id(), edition)
                }
            };
            assigned.push(edition.clone());
            let file_id = id.clone();
            let edition_timeline = edition.clone();
            sqlx::query("INSERT INTO media_files (id,edition_id,library_id,relative_path,revision,fingerprint,bytes,duration_seconds,tracks_json,available) VALUES (?,?,?,?,?,?,?,?,?,1) ON CONFLICT(id) DO UPDATE SET relative_path=excluded.relative_path,revision=excluded.revision,fingerprint=excluded.fingerprint,bytes=excluded.bytes,duration_seconds=excluded.duration_seconds,tracks_json=excluded.tracks_json,available=1 WHERE media_files.revision<>excluded.revision OR media_files.fingerprint<>excluded.fingerprint OR media_files.relative_path<>excluded.relative_path OR media_files.available=0 OR media_files.tracks_json<>excluded.tracks_json OR media_files.duration_seconds IS NOT excluded.duration_seconds")
                .bind(id).bind(edition).bind(&job.library_id).bind(file.relative).bind(file.revision).bind(file.fingerprint).bind(file.bytes).bind(file.duration).bind(serde_json::to_string(&file.tracks)?).execute(&mut *tx).await?;
            if first_version {
                // Unknown equivalence is permitted only in the new, empty timeline.
                crate::curation::create_version(
                    &mut tx,
                    &edition_timeline,
                    &file_id,
                    playscale_core::identity::Origin::Original,
                    playscale_core::identity::Equivalence::Unknown,
                )
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            }
            crate::nfo::publish(&mut tx, &file_id, &sidecar).await?;
        }
    }
    crate::matching::invalidate_changed(&mut tx).await?;
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
    crate::scans::after_attempt(&mut tx, &job.id).await?;
    crate::search::refresh(&mut tx, 500).await?;
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
                // The attempt records the source binding it observes; a rebind
                // before publication fences it.
                sqlx::query("UPDATE jobs SET phase=?,attempt=?,error=?,binding_revision=(SELECT binding_revision FROM libraries WHERE id=jobs.library_id),started_barrier=(SELECT scan_barrier FROM libraries WHERE id=jobs.library_id) WHERE id=?")
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
