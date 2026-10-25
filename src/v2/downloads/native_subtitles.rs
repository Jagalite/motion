//! Candidate native original WebVTT cache owner. No public selector or route
//! calls this until the complete pin/cache/manifest cohort is qualified.
use super::*;
use crate::{delivery::native_subtitles as conversion, execution};
use std::{
    io::{Read, Write},
    path::Path,
};
use tokio_util::sync::CancellationToken;

fn expired() -> Problem {
    Problem::new(
        StatusCode::CONFLICT,
        "download_expired",
        "The exact download is no longer transferable",
    )
}
fn selection(row: &Row) -> Result<conversion::Selection, Problem> {
    let subtitle = row
        .subtitle_track_id
        .as_deref()
        .and_then(|s| s.strip_prefix('s'))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| {
            Problem::invalid(
                "download_subtitle_unavailable",
                "This download has no supported original text selection",
            )
        })?;
    Ok(conversion::Selection {
        profile: row.profile_id.clone(),
        timeline: row.timeline_id.clone(),
        version: row.version_id.clone(),
        file: row.file_id.clone(),
        revision: row.file_revision.clone(),
        subtitle,
    })
}
async fn pin(conn: &mut SqliteConnection, principal: &Principal, id: &str) -> Result<Row, Problem> {
    let row = owned(conn, principal, id).await?;
    let current = observe(conn, principal, &row).await?;
    if !core::transferable(core::state(row.stored(), &row.pin(), &current)) {
        return Err(expired());
    }
    Ok(row)
}
fn same(row: &Row, selected: &conversion::Selection) -> bool {
    row.profile_id == selected.profile
        && row.timeline_id == selected.timeline
        && row.version_id == selected.version
        && row.file_id == selected.file
        && row.file_revision == selected.revision
        && row.subtitle_track_id.as_deref() == Some(format!("s{}", selected.subtitle).as_str())
}
fn digest(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}
fn read_exact(path: &Path, expected: &str, size: i64) -> anyhow::Result<Option<Vec<u8>>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let before = file.metadata()?;
    anyhow::ensure!(
        before.is_file()
            && before.len() <= 16 * 1024 * 1024
            && i64::try_from(before.len()).ok() == Some(size),
        "subtitle cache size changed"
    );
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        crate::scan::fingerprint(&before) == crate::scan::fingerprint(&file.metadata()?)
            && digest(&bytes) == expected,
        "subtitle cache digest changed"
    );
    Ok(Some(bytes))
}

/// A directory is a held container, not deletion authority. The artifact
/// itself is created atomically with O_EXCL and remains open through ACK.
struct Published {
    directory: std::fs::File,
    output: std::fs::File,
    path: std::path::PathBuf,
    name: std::ffi::CString,
    fingerprint: String,
}
impl Published {
    fn matches_name(&self) -> anyhow::Result<()> {
        use std::os::{
            fd::{AsRawFd, FromRawFd},
            unix::fs::{MetadataExt, OpenOptionsExt},
        };
        let parent = self
            .path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("missing cache parent"))?;
        let current = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(parent)?;
        let (expected, actual) = (self.directory.metadata()?, current.metadata()?);
        anyhow::ensure!(
            expected.dev() == actual.dev() && expected.ino() == actual.ino(),
            "subtitle cache directory changed"
        );
        let fd = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                self.name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let named = unsafe { std::fs::File::from_raw_fd(fd) };
        let (expected, actual) = (self.output.metadata()?, named.metadata()?);
        anyhow::ensure!(
            crate::scan::fingerprint(&expected) == self.fingerprint
                && crate::scan::fingerprint(&actual) == self.fingerprint,
            "subtitle artifact bytes changed before ACK"
        );
        anyhow::ensure!(
            expected.is_file()
                && actual.is_file()
                && expected.dev() == actual.dev()
                && expected.ino() == actual.ino()
                && actual.nlink() == 1,
            "subtitle artifact name changed"
        );
        Ok(())
    }
}
fn publish_exact(
    path: &Path,
    bytes: &[u8],
    reserve: u64,
    stop: &CancellationToken,
) -> anyhow::Result<Published> {
    use std::io::{Seek, SeekFrom};
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::OpenOptionsExt,
    };
    anyhow::ensure!(!stop.is_cancelled(), "subtitle publication cancelled");
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing cache parent"))?;
    match std::fs::create_dir(parent) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(parent)?;
    let directory_path = std::path::PathBuf::from(format!("/dev/fd/{}", directory.as_raw_fd()));
    crate::storage::require_space(&directory_path, bytes.len() as u64, reserve)?;
    let name = std::ffi::CString::new(
        path.file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow::anyhow!("invalid cache name"))?,
    )?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR
                | libc::O_CREAT
                | libc::O_EXCL
                | libc::O_CLOEXEC
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut output = unsafe { std::fs::File::from_raw_fd(fd) };
    output.write_all(bytes)?;
    output.sync_all()?;
    let fingerprint = crate::scan::fingerprint(&output.metadata()?);
    output.seek(SeekFrom::Start(0))?;
    let mut actual = Vec::new();
    std::io::Read::by_ref(&mut output)
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut actual)?;
    anyhow::ensure!(
        actual == bytes,
        "subtitle output changed during publication"
    );
    directory.sync_all()?;
    let published = Published {
        directory,
        output,
        path: path.to_owned(),
        name,
        fingerprint,
    };
    published.matches_name()?;
    // Failed/cancelled publication leaves an unacknowledged file. Never
    // overwrite/delete an unknown entry to make a retry succeed.
    anyhow::ensure!(!stop.is_cancelled(), "subtitle publication cancelled");
    Ok(published)
}

/// One detached per-download owner retains cancellation/process/publication
/// custody. Result bytes have a committed exact artifact ACK, not just a file.
pub(crate) async fn ensure(
    app: App,
    caller: Caller,
    id: String,
    stop: CancellationToken,
) -> Result<Vec<u8>, Problem> {
    let (mut reply, result) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let stop = stop.child_token();
        let work = ensure_owned(app, caller, id, stop.clone());
        tokio::pin!(work);
        let outcome = tokio::select! { result=&mut work=>result,_=reply.closed()=>{stop.cancel();work.await} };
        let _ = reply.send(outcome);
    });
    result.await.map_err(Problem::internal)?
}
async fn ensure_owned(
    app: App,
    caller: Caller,
    id: String,
    stop: CancellationToken,
) -> Result<Vec<u8>, Problem> {
    caller.require(Permission::DownloadsManage)?;
    let path = subtitles_path(&app, &id);
    let held = tokio::select! { _=stop.cancelled()=>return Err(expired()), lock=subtitles_lock(&path).lock_owned()=>lock };
    let held = Arc::new(held);
    let mut tx = begin_catalog_application(&app, true).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::DownloadsManage))
        .await?;
    let row = pin(&mut tx, &principal, &id).await?;
    let selected = selection(&row)?;
    crate::delivery::native_subtitles::validate_selection_in(&app, &mut tx, &principal, &selected)
        .await?;
    let recorded: Option<(String, i64, Option<String>)> = sqlx::query_as(
        "SELECT state,size_bytes,digest FROM download_subtitle_artifacts WHERE download_id=?",
    )
    .bind(&id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    if recorded.as_ref().is_some_and(|r| r.0 != "ready") {
        return Err(Problem::invalid(
            "subtitle_conversion_failed",
            "The pinned subtitle conversion previously failed",
        ));
    }
    if let Some((_, size, expected)) = &recorded {
        let expected = expected
            .clone()
            .ok_or_else(|| Problem::internal("subtitle artifact has no digest"))?;
        let mut checked = conversion::validate_cached(
            app.clone(),
            caller.clone(),
            selected.clone(),
            stop.clone(),
        )
        .await?;
        let current = pin(&mut checked.writer, &checked.principal, &id).await?;
        if !same(&current, &selected) {
            return Err(expired());
        }
        let retained = (checked.retained(), held.clone());
        let cache = path.clone();
        let size = *size;
        let read = execution::prepare(
            checked.lease.clone(),
            &stop,
            Duration::from_secs(5),
            move || {
                let _retained = retained;
                let tx = checked.writer;
                read_exact(&cache, &expected, size).map(|bytes| (tx, bytes))
            },
            || {},
        )
        .await
        .ok_or_else(expired)?
        .map_err(Problem::internal)?
        .map_err(|_| {
            Problem::new(
                StatusCode::CONFLICT,
                "subtitle_artifact_changed",
                "The pinned subtitle artifact changed",
            )
        })?;
        let (tx, bytes) = read;
        tx.commit().await?;
        if let Some(bytes) = bytes {
            return Ok(bytes);
        }
    }
    let mut converted =
        conversion::convert(app.clone(), caller.clone(), selected.clone(), stop.clone()).await?;
    // Consume the returned writer immediately. It remains held through exact
    // pin comparison, owned file publication and the artifact ACK transaction.
    let latest = pin(&mut converted.writer, &converted.principal, &id).await?;
    if !same(&latest, &selected) {
        return Err(expired());
    }
    let bytes = converted.bytes.clone();
    let hash = digest(&bytes);
    if let Some((_, size, expected)) = &recorded {
        if *size != bytes.len() as i64 || expected.as_deref() != Some(hash.as_str()) {
            return Err(Problem::new(
                StatusCode::CONFLICT,
                "subtitle_artifact_changed",
                "Reconversion differs from the immutable artifact",
            ));
        }
    }
    let publication_source = converted.retained();
    let retained = (publication_source.clone(), held.clone());
    let output = bytes.clone();
    let destination = path.clone();
    let reserve = app.storage.settings.min_free_bytes;
    let publication_stop = stop.clone();
    let (mut tx, published) = execution::prepare(
        converted.lease.clone(),
        &stop,
        Duration::from_secs(5),
        move || {
            let _retained = retained;
            let tx = converted.writer;
            let published = publish_exact(&destination, &output, reserve, &publication_stop)?;
            Ok::<_, anyhow::Error>((tx, published))
        },
        || {},
    )
    .await
    .ok_or_else(|| {
        stop.cancel();
        expired()
    })?
    .map_err(Problem::internal)?
    .map_err(|_| {
        Problem::new(
            StatusCode::CONFLICT,
            "subtitle_artifact_changed",
            "The subtitle output cannot be published without replacing an unknown entry",
        )
    })?;
    if stop.is_cancelled() {
        return Err(expired());
    }
    let changed=sqlx::query("INSERT INTO download_subtitle_artifacts(download_id,state,size_bytes,digest) VALUES (?,'ready',?,?) ON CONFLICT(download_id) DO NOTHING")
        .bind(&id).bind(bytes.len() as i64).bind(&hash).execute(&mut *tx).await?.rows_affected();
    let actual: (String, i64, Option<String>) = sqlx::query_as(
        "SELECT state,size_bytes,digest FROM download_subtitle_artifacts WHERE download_id=?",
    )
    .bind(&id)
    .fetch_one(&mut *tx)
    .await?;
    if actual.0 != "ready"
        || actual.1 != bytes.len() as i64
        || actual.2.as_deref() != Some(hash.as_str())
    {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "subtitle_artifact_changed",
            "The pinned subtitle artifact changed",
        ));
    }
    if changed != 0 {
        sqlx::query("UPDATE downloads SET revision=revision+1,updated_at=? WHERE id=?")
            .bind(now())
            .bind(&id)
            .execute(&mut *tx)
            .await?;
    }
    published.matches_name().map_err(|_| {
        Problem::new(
            StatusCode::CONFLICT,
            "subtitle_artifact_changed",
            "The subtitle artifact changed before acknowledgment",
        )
    })?;
    tx.commit().await?;
    drop((published, publication_source));
    Ok(bytes)
}

/// Candidate manifest consumes the actual cache owner, then rechecks the exact
/// pin, source/track and immutable artifact under a new current writer. It is
/// not wired into the public manifest while text transport remains unqualified.
pub(crate) async fn manifest(
    app: App,
    caller: Caller,
    id: String,
    stop: CancellationToken,
) -> Result<Value, Problem> {
    let bytes = ensure(app.clone(), caller.clone(), id.clone(), stop.clone()).await?;
    let mut tx = begin_catalog_application(&app, false).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::DownloadsManage))
        .await?;
    let selected = selection(&pin(&mut tx, &principal, &id).await?)?;
    tx.commit().await?;
    let mut checked =
        conversion::validate_cached(app, caller, selected.clone(), stop.clone()).await?;
    let row = pin(&mut checked.writer, &checked.principal, &id).await?;
    if !same(&row, &selected) || stop.is_cancelled() {
        return Err(expired());
    }
    let artifact: (String, i64, Option<String>) = sqlx::query_as(
        "SELECT state,size_bytes,digest FROM download_subtitle_artifacts WHERE download_id=?",
    )
    .bind(&id)
    .fetch_one(&mut *checked.writer)
    .await?;
    let hash = digest(&bytes);
    if artifact.0 != "ready"
        || artifact.1 != bytes.len() as i64
        || artifact.2.as_deref() != Some(hash.as_str())
    {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "subtitle_artifact_changed",
            "The pinned subtitle artifact changed",
        ));
    }
    let mut body = native_manifest_body(&mut checked.writer, &row).await?;
    body["files"]
        .as_array_mut()
        .expect("native files")
        .push(json!({
            "asset_id":format!("{}-subtitles",row.id), "relative_name":"subtitles.vtt",
            "digest":format!("sha256:{hash}"), "size_bytes":bytes.len().to_string(),
            "media_url":format!("/api/v2/downloads/{}/subtitles?digest=sha256:{hash}",row.id),
            "role":"subtitle"
        }));
    checked.writer.commit().await?;
    Ok(body)
}

#[cfg(test)]
mod publication_tests {
    use super::*;

    #[test]
    fn existing_cache_entry_is_not_replaced_or_deleted() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("cache");
        std::fs::create_dir(&parent).unwrap();
        let path = parent.join("download.vtt");
        std::fs::write(&path, b"foreign cache entry").unwrap();
        assert!(
            publish_exact(
                &path,
                b"WEBVTT\n\nowned output\n",
                0,
                &CancellationToken::new()
            )
            .is_err()
        );
        assert_eq!(std::fs::read(path).unwrap(), b"foreign cache entry");
    }

    #[test]
    fn same_inode_mutation_loses_final_publication_fingerprint() {
        use std::os::unix::fs::MetadataExt;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache/download.vtt");
        let published = publish_exact(
            &path,
            b"WEBVTT\n\nowned output\n",
            0,
            &CancellationToken::new(),
        )
        .unwrap();
        let before = published.output.metadata().unwrap();
        let mut other = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        other.write_all(b"ALTERED").unwrap();
        other
            .set_modified(before.modified().unwrap() + std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(other.metadata().unwrap().ino(), before.ino());
        assert!(published.matches_name().is_err());
        assert!(path.exists(), "no cleanup authority follows a refusal");
    }

    #[test]
    fn same_bytes_replacement_loses_named_output_identity_without_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache/download.vtt");
        let bytes = b"WEBVTT\n\nowned output\n";
        let published = publish_exact(&path, bytes, 0, &CancellationToken::new()).unwrap();
        let retained = temp.path().join("retained-output.vtt");
        std::fs::rename(&path, &retained).unwrap();
        std::fs::write(&path, bytes).unwrap();
        assert!(published.matches_name().is_err());
        drop(published);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::read(retained).unwrap(), bytes);
    }
}
