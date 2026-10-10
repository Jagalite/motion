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
    permit: Arc<OwnedSemaphorePermit>,
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
    let row =
        sqlx::query_as::<_, ItemRow>("SELECT * FROM catalog_files WHERE id=? AND available=1")
            .bind(&id)
            .fetch_optional(&app.db)
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
            .fetch_one(&app.db)
            .await?;
    let permit = Arc::new(app.streams.clone().try_acquire_owned().map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "stream_limit",
            "Too many open streams",
        )
    })?);
    let relative = row.relative_path.clone();
    let (file, meta) = admitted_io(permit.clone(), move || {
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
    async fn cancelled_io_keeps_admission_until_blocking_operation_finishes() {
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = Arc::new(slots.clone().try_acquire_owned().unwrap());
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
        release.send(()).unwrap();
        let permit = tokio::time::timeout(std::time::Duration::from_secs(2), slots.acquire())
            .await
            .unwrap()
            .unwrap();
        drop(permit);
        assert_eq!(slots.available_permits(), 1);
    }
}
