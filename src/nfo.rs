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
    Present(Vec<u8>),
    /// Exists but cannot be used (too large, unreadable, not a regular file):
    /// keep whatever was published before.
    Unusable,
}

/// Read `<stem>.nfo` beside `relative` through the handle-relative opener.
pub fn read(root: &Path, relative: &str) -> Sidecar {
    let sidecar = Path::new(relative).with_extension("nfo");
    let file = match crate::scan::open_file(root, &sidecar) {
        Ok(file) => file,
        Err(error) => {
            let missing = error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound);
            return if missing {
                Sidecar::Absent
            } else {
                Sidecar::Unusable
            };
        }
    };
    let mut bytes = Vec::new();
    match file.take(nfo::MAX_BYTES as u64 + 1).read_to_end(&mut bytes) {
        Ok(_) if bytes.len() <= nfo::MAX_BYTES => Sidecar::Present(bytes),
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
            // Only the file that supplied the contribution can withdraw it.
            let origin: Option<String> =
                sqlx::query_scalar("SELECT file_id FROM nfo_origins WHERE item_id=?")
                    .bind(&item)
                    .fetch_optional(&mut *conn)
                    .await?;
            if origin.as_deref() == Some(file) {
                sqlx::query("DELETE FROM metadata_documents WHERE item_id=? AND source=?")
                    .bind(&item)
                    .bind(SOURCE)
                    .execute(&mut *conn)
                    .await?;
                sqlx::query("DELETE FROM nfo_origins WHERE item_id=?")
                    .bind(&item)
                    .execute(&mut *conn)
                    .await?;
                crate::metadata::project_title(conn, &item).await?;
            }
            Ok(())
        }
        Sidecar::Present(bytes) => {
            let parsed = match nfo::parse(bytes) {
                Ok(parsed) => parsed,
                Err(error) => {
                    tracing::debug!(?error, "sidecar NFO ignored");
                    return Ok(());
                }
            };
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
