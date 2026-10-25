use crate::{
    App,
    api::ApiError,
    db::ItemRow,
    scan::{fingerprint, open_file},
};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::Response,
};
use playscale_core::ranges::{RangeDecision, select_range};
use serde::Deserialize;
use std::{
    io::{Read, Seek},
    sync::Arc,
};
use tokio::sync::OwnedSemaphorePermit;

// A cancelled waiter must not release admission while its OS operation is still running.
async fn admitted_io<T: Send + 'static>(
    permit: Arc<(OwnedSemaphorePermit, Arc<crate::artifact_leases::Lease>)>,
    operation: impl FnOnce() -> T + Send + 'static,
) -> Result<T, tokio::task::JoinError> {
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation()
    })
    .await
}

#[derive(Deserialize)]
pub struct MediaQuery {
    pub revision: Option<String>,
}

fn matches_etag(value: &str, etag: &str, weak: bool) -> bool {
    value.split(',').any(|s| {
        let s = s.trim();
        s == "*"
            || (if weak {
                s.strip_prefix("W/").unwrap_or(s)
            } else {
                s
            }) == etag
    })
}

pub async fn serve(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(query): Query<MediaQuery>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if app.native_catalog.is_some()
        || sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM catabolic_binding)")
            .fetch_one(&app.db)
            .await?
    {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "native_authorization_required",
            "Native bytes require a current authorized file-open fence",
        ));
    }
    serve_inner(app, id, query, method, headers, None).await
}

/// The caller retains its authorization and native generation reservation until
/// the blocking open has established an owned descriptor. Cancellation transfers
/// custody to that blocking operation, never releases the fence early.
pub(crate) async fn serve_authorized(
    app: App,
    id: String,
    query: MediaQuery,
    method: Method,
    headers: HeaderMap,
    mut fence: sqlx::Transaction<'static, sqlx::Sqlite>,
    principal: &playscale_core::access::Principal,
) -> Result<Response, ApiError> {
    let convert = |p: crate::v2::Problem| ApiError::new(p.status, p.code, &p.detail);
    if let Some(native) = &app.native_catalog {
        native
            .validate_projection(&mut fence)
            .await
            .map_err(ApiError::internal)?;
    } else if crate::v2::playback::native_bound(&mut fence)
        .await
        .map_err(convert)?
    {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "native_owner_unavailable",
            "Native catalog ownership is not admitted",
        ));
    }
    crate::v2::playback::require_native_scope(&mut fence, principal)
        .await
        .map_err(convert)?;
    let facts = crate::v2::content::transport_file_facts(&mut fence, principal, &id)
        .await
        .map_err(convert)?;
    playscale_core::access::admit_file_bytes(principal, facts.as_ref())
        .map_err(crate::v2::Problem::from)
        .map_err(convert)?;
    serve_inner(app, id, query, method, headers, Some(fence)).await
}

async fn serve_inner(
    app: App,
    id: String,
    query: MediaQuery,
    method: Method,
    headers: HeaderMap,
    mut fence: Option<sqlx::Transaction<'static, sqlx::Sqlite>>,
) -> Result<Response, ApiError> {
    // Acquire before the available-row observation, so GC cannot retire a file
    // between that observation and the real open/read. The body and blocking
    // closures share ownership even when their async waiter is cancelled.
    let artifact = app
        .processing
        .artifacts
        .pin([id.clone()])
        .ok_or_else(ApiError::not_found)?;
    let mut ordinary;
    let conn: &mut sqlx::SqliteConnection = if let Some(tx) = fence.as_mut() {
        &mut *tx
    } else {
        ordinary = app.db.acquire().await?;
        &mut ordinary
    };
    let row =
        sqlx::query_as::<_, ItemRow>("SELECT * FROM catalog_files WHERE id=? AND available=1")
            .bind(&id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(ApiError::not_found)?;
    if query.revision.as_ref().is_some_and(|r| *r != row.revision) {
        return Err(ApiError::conflict(
            "source_revision_changed",
            "Source changed; refresh the item",
        ));
    }
    let (root, identity): (String, String) =
        sqlx::query_as("SELECT root,root_identity FROM libraries WHERE id=?")
            .bind(&row.library_id)
            .fetch_one(&mut *conn)
            .await?;
    if app.native_catalog.is_some() {
        let raw: String = sqlx::query_scalar(
            "SELECT mapping_json FROM catabolic_projection_files WHERE file_id=? AND available=1",
        )
        .bind(&id)
        .fetch_one(&mut *conn)
        .await?;
        let mapping: motion_catalog_backend::occurrence_bridge::OccurrenceMapping =
            serde_json::from_str(&raw).map_err(ApiError::internal)?;
        let source = mapping.source.clone();
        app.native_catalog
            .as_ref()
            .ok_or_else(ApiError::not_found)?
            .with_host(move |host| {
                let observed = host.source_occurrences(&source.native_source)?;
                anyhow::ensure!(
                    observed["binding"] == source.native_binding
                        && observed["policy"] == source.native_policy,
                    "native source authority changed before file open"
                );
                Ok(())
            })
            .await
            .map_err(ApiError::internal)?;
        if mapping.source.library_id != row.library_id
            || mapping.source.motion_root != root
            || mapping.source.motion_root_identity != identity
            || mapping.motion_file["id"].as_str() != Some(id.as_str())
            || mapping.motion_file["relative_path"].as_str() != Some(row.relative_path.as_str())
            || mapping.motion_file["fingerprint"].as_str() != Some(row.fingerprint.as_str())
            || mapping.motion_file["revision"].as_str() != Some(row.revision.as_str())
        {
            return Err(ApiError::not_found());
        }
    }
    let permit = Arc::new((
        app.streams.clone().try_acquire_owned().map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "stream_limit",
                "Too many open streams",
            )
        })?,
        artifact,
    ));
    let relative = row.relative_path.clone();
    let expected = row.fingerprint.clone();
    let (file, meta) = admitted_io(permit.clone(), move || {
        // This owned transaction remains in the OS task even when its waiter
        // is cancelled. Drop only after open + full fingerprint verification.
        let _fence = fence;
        let root_meta = std::fs::metadata(&root).map_err(|_| ApiError::not_found())?;
        if crate::db::root_identity(&root_meta) != identity {
            return Err(ApiError::conflict(
                "source_unavailable",
                "Library root was replaced",
            ));
        }
        let file = open_file(std::path::Path::new(&root), std::path::Path::new(&relative))
            .map_err(|_| ApiError::not_found())?;
        let meta = file.metadata().map_err(ApiError::internal)?;
        if fingerprint(&meta) != expected {
            return Err(ApiError::conflict(
                "source_revision_changed",
                "Source changed; rescan the library",
            ));
        }
        Ok((file, meta))
    })
    .await
    .map_err(ApiError::internal)??;
    if fingerprint(&meta) != row.fingerprint {
        return Err(ApiError::conflict(
            "source_revision_changed",
            "Source changed; rescan the library",
        ));
    }
    let etag = format!("\"{}\"", row.revision);
    let modified = meta.modified().map_err(ApiError::internal)?;
    let last_modified = httpdate::fmt_http_date(modified);
    let get = |name| headers.get(name).and_then(|v| v.to_str().ok());
    let older_or_equal = |date: &str| {
        httpdate::parse_http_date(date).ok().is_some_and(|date| {
            modified
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                <= date
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
        })
    };
    let mut builder = Response::builder()
        .header(header::ETAG, &etag)
        .header(header::LAST_MODIFIED, &last_modified)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "private, no-cache")
        .header(
            header::CONTENT_TYPE,
            mime_guess::from_path(&row.relative_path)
                .first_or_octet_stream()
                .as_ref(),
        );
    let failed = if let Some(value) = get(header::IF_MATCH) {
        !matches_etag(value, &etag, false)
    } else {
        get(header::IF_UNMODIFIED_SINCE)
            .is_some_and(|d| httpdate::parse_http_date(d).is_ok() && !older_or_equal(d))
    };
    if failed {
        return Ok(builder
            .status(StatusCode::PRECONDITION_FAILED)
            .body(Body::empty())
            .unwrap());
    }
    let unchanged = if let Some(value) = get(header::IF_NONE_MATCH) {
        matches_etag(value, &etag, true)
    } else {
        get(header::IF_MODIFIED_SINCE).is_some_and(older_or_equal)
    };
    if unchanged {
        return Ok(builder
            .status(StatusCode::NOT_MODIFIED)
            .body(Body::empty())
            .unwrap());
    }
    // Only strong entity tags are admitted for If-Range. Date validators conservatively fall back to full delivery.
    let use_range =
        method == Method::GET && get(header::IF_RANGE).is_none_or(|value| value == etag);
    let decision = select_range(
        if use_range { get(header::RANGE) } else { None },
        meta.len(),
    );
    let (start, length) = match decision {
        RangeDecision::Unsatisfiable => {
            return Ok(builder
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{}", meta.len()))
                .header(header::CONTENT_LENGTH, 0)
                .body(Body::empty())
                .unwrap());
        }
        RangeDecision::Full => (0, meta.len()),
        RangeDecision::Partial { start, end } => {
            builder = builder.status(StatusCode::PARTIAL_CONTENT).header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{}", meta.len()),
            );
            (start, end - start + 1)
        }
    };
    builder = builder.header(header::CONTENT_LENGTH, length);
    if method == Method::HEAD {
        return Ok(builder.body(Body::empty()).unwrap());
    }
    let body = async_stream::stream! {
        let mut file = file;
        let mut remaining = length;
        let mut position = start;
        while remaining > 0 {
            let wanted = remaining.min(64 * 1024) as usize;
            let expected = row.fingerprint.clone();
            let result = admitted_io(permit.clone(), move || -> std::io::Result<_> {
                file.seek(std::io::SeekFrom::Start(position))?;
                let mut buffer = vec![0_u8; wanted];
                let n = file.read(&mut buffer)?;
                if n == 0 {
                    return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "source truncated during response"));
                }
                if fingerprint(&file.metadata()?) != expected {
                    return Err(std::io::Error::other("source changed during response"));
                }
                buffer.truncate(n);
                Ok((file, axum::body::Bytes::from(buffer)))
            }).await;
            match result {
                Ok(Ok((next_file, bytes))) => {
                    file = next_file;
                    remaining -= bytes.len() as u64;
                    position += bytes.len() as u64;
                    yield Ok(bytes);
                }
                Ok(Err(error)) => { yield Err(error); break; }
                Err(error) => { yield Err(std::io::Error::other(error)); break; }
            }
        }
    };
    Ok(builder.body(Body::from_stream(body)).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_file_open_keeps_native_writer_fence_until_descriptor_step_finishes() {
        use sqlx::Connection;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fence.sqlite");
        let pool = crate::db::connect(&path).await.unwrap();
        let fence = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let mut contender = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .busy_timeout(std::time::Duration::ZERO),
        )
        .await
        .unwrap();
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let artifacts = Arc::new(crate::artifact_leases::Registry::default());
        let permit = Arc::new((
            slots.clone().try_acquire_owned().unwrap(),
            artifacts.pin(["file".into()]).unwrap(),
        ));
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let task = tokio::spawn(admitted_io(permit, move || {
            let _fence = fence;
            started.send(()).unwrap();
            blocked.recv().unwrap();
        }));
        ready.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let refused = sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut contender)
            .await
            .unwrap_err();
        assert_eq!(
            refused.as_database_error().unwrap().code().as_deref(),
            Some("5")
        );
        release.send(()).unwrap();
        let _permit = tokio::time::timeout(std::time::Duration::from_secs(2), slots.acquire())
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match sqlx::query("BEGIN IMMEDIATE").execute(&mut contender).await {
                    Ok(_) => break,
                    Err(e) => {
                        assert_eq!(e.as_database_error().unwrap().code().as_deref(), Some("5"));
                        tokio::task::yield_now().await;
                    }
                }
            }
        })
        .await
        .unwrap();
        sqlx::query("ROLLBACK")
            .execute(&mut contender)
            .await
            .unwrap();
        pool.close().await;
    }

    #[tokio::test]
    async fn cancelled_io_keeps_admission_until_blocking_operation_finishes() {
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let artifacts = Arc::new(crate::artifact_leases::Registry::default());
        let pin = artifacts.pin(["file".into()]).unwrap();
        let permit = Arc::new((slots.clone().try_acquire_owned().unwrap(), pin));
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let task = tokio::spawn(admitted_io(permit, move || {
            started.send(()).unwrap();
            blocked.recv().unwrap();
        }));
        ready.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(slots.available_permits(), 0);
        assert!(slots.clone().try_acquire_owned().is_err());
        assert!(artifacts.claim_cleanup("file".into()).is_none());
        release.send(()).unwrap();
        let permit = tokio::time::timeout(std::time::Duration::from_secs(2), slots.acquire())
            .await
            .unwrap()
            .unwrap();
        drop(permit);
        assert_eq!(slots.available_permits(), 1);
        // Tuple fields drop in order: the semaphore can wake this task before
        // the blocking thread finishes releasing the artifact pin. Observe
        // both releases instead of assuming they form one atomic operation.
        let _cleanup = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(cleanup) = artifacts.claim_cleanup("file".into()) {
                    break cleanup;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("completed blocking operation retained its artifact pin");
    }
}
