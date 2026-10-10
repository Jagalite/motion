//! Sidecar NFO adapter. Reading happens during traversal (never inside the
//! writer transaction); parsing is `playscale_core::nfo`; publication stores a
//! bounded `nfo` contribution for the file's work.
use playscale_core::nfo;
use sqlx::SqliteConnection;
use std::{io::Read, path::Path};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sidecar {
    /// No `<stem>.nfo` next to the media file.
    Absent,
    /// Parsed during traversal; raw bytes are not retained.
    Parsed(nfo::Nfo),
    /// Exists but cannot be used (too large, unreadable, not a regular file,
    /// malformed, or over the scan's read budget): keep whatever was
    /// published before.
    Unusable,
}

/// Raw sidecar bytes one scan may read in total.
pub const SCAN_BUDGET: usize = 64 * 1024 * 1024;

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd"
))]
const O_NONBLOCK: i32 = 0x0004;
#[cfg(target_os = "linux")]
const O_NONBLOCK: i32 = 0o4000;

/// Read and parse `<stem>.nfo` beside `relative`, handle-relative to the
/// root. The open is non-blocking so a FIFO or device cannot stall the scan
/// worker; anything but a regular file is unusable.
pub fn read(root: &Path, relative: &str, budget: &mut usize) -> Sidecar {
    let sidecar = Path::new(relative).with_extension("nfo");
    if !sidecar
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)))
    {
        return Sidecar::Unusable;
    }
    let Ok(dir) = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority()) else {
        return Sidecar::Unusable;
    };
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(O_NONBLOCK);
    }
    let file = match dir.open_with(&sidecar, &options) {
        Ok(file) => file.into_std(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Sidecar::Absent,
        Err(_) => return Sidecar::Unusable,
    };
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return Sidecar::Unusable;
    }
    let allowance = (*budget).min(nfo::MAX_BYTES);
    if allowance == 0 {
        return Sidecar::Unusable;
    }
    let mut bytes = Vec::new();
    let read = file.take(allowance as u64 + 1).read_to_end(&mut bytes);
    *budget = budget.saturating_sub(bytes.len());
    match read {
        Ok(_) if bytes.len() <= allowance => match nfo::parse(&bytes) {
            Ok(parsed) => Sidecar::Parsed(parsed),
            Err(error) => {
                tracing::debug!(?error, "sidecar NFO ignored");
                Sidecar::Unusable
            }
        },
        _ => Sidecar::Unusable,
    }
}

const SOURCE: &str = "nfo";

/// Publish (or withdraw) a work's `nfo` contribution from one observed file,
/// inside the scan's writer transaction.
pub(crate) async fn publish(
    conn: &mut SqliteConnection,
    file: &str,
    sidecar: &Sidecar,
) -> anyhow::Result<()> {
    let item: String = sqlx::query_scalar(
        "SELECT e.item_id FROM media_files f JOIN editions e ON e.id=f.edition_id WHERE f.id=?",
    )
    .bind(file)
    .fetch_one(&mut *conn)
    .await?;
    match sidecar {
        Sidecar::Unusable => Ok(()),
        Sidecar::Absent => {
            // Every work this file's sidecar supplied loses that contribution,
            // each subject to its own manual pin.
            withdraw(conn, file, None).await
        }
        Sidecar::Parsed(parsed) => {
            let document = serde_json::to_string(&parsed.contribution)?;
            let external = parsed.external_ids.first().map(|(p, v)| format!("{p}:{v}"));
            if !crate::matching::provider_identity_allowed(conn, &item, SOURCE, external.as_deref())
                .await?
            {
                return Ok(());
            }
            let upsert = "INSERT INTO metadata_documents VALUES (?,?,1,?,?,?) ON CONFLICT(item_id,source) DO UPDATE SET revision=metadata_documents.revision+1,external_id=excluded.external_id,document_json=excluded.document_json,updated_at=excluded.updated_at WHERE metadata_documents.document_json IS NOT excluded.document_json OR metadata_documents.external_id IS NOT excluded.external_id";
            let result = sqlx::query(upsert)
                .bind(&item)
                .bind(SOURCE)
                .bind(&external)
                .bind(&document)
                .bind(crate::now())
                .execute(&mut *conn)
                .await;
            match result {
                // Another work already claims this NFO identity: keep the
                // values as evidence without the conflicting identity.
                Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                    sqlx::query(upsert)
                        .bind(&item)
                        .bind(SOURCE)
                        .bind(None::<String>)
                        .bind(&document)
                        .bind(crate::now())
                        .execute(&mut *conn)
                        .await?;
                }
                other => {
                    other?;
                }
            }
            // A file supplies one work: contributions it left on works it no
            // longer belongs to (after a split) are withdrawn.
            withdraw(conn, file, Some(&item)).await?;
            sqlx::query("INSERT INTO nfo_origins VALUES (?,?) ON CONFLICT(item_id) DO UPDATE SET file_id=excluded.file_id")
                .bind(&item)
                .bind(file)
                .execute(&mut *conn)
                .await?;
            crate::metadata::project_title(conn, &item).await?;
            Ok(())
        }
    }
}

/// Withdraw the `nfo` contribution from every work whose origin is `file`,
/// except `keep`, unless a manual identification pins that work's identity.
async fn withdraw(
    conn: &mut SqliteConnection,
    file: &str,
    keep: Option<&str>,
) -> anyhow::Result<()> {
    let owners: Vec<String> = sqlx::query_scalar("SELECT item_id FROM nfo_origins WHERE file_id=?")
        .bind(file)
        .fetch_all(&mut *conn)
        .await?;
    for owner in owners.into_iter().filter(|o| Some(o.as_str()) != keep) {
        if !crate::matching::provider_identity_allowed(conn, &owner, SOURCE, None).await? {
            continue;
        }
        sqlx::query("DELETE FROM metadata_documents WHERE item_id=? AND source=?")
            .bind(&owner)
            .bind(SOURCE)
            .execute(&mut *conn)
            .await?;
        sqlx::query("DELETE FROM nfo_origins WHERE item_id=?")
            .bind(&owner)
            .execute(&mut *conn)
            .await?;
        crate::metadata::project_title(conn, &owner).await?;
    }
    Ok(())
}
