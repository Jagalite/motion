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
    let observation = match sidecar {
        Sidecar::Unusable => nfo::SidecarObservation::Unusable,
        Sidecar::Absent => nfo::SidecarObservation::Absent,
        Sidecar::Parsed(parsed) => nfo::SidecarObservation::Parsed {
            document: serde_json::to_string(&parsed.contribution)?,
            external_id: parsed
                .external_ids
                .first()
                .map(|(provider, value)| format!("{provider}:{value}")),
        },
    };
    let target = if matches!(observation, nfo::SidecarObservation::Parsed { .. }) {
        Some(work_facts(conn, &item).await?)
    } else {
        None
    };
    let owners: Vec<String> = if matches!(observation, nfo::SidecarObservation::Unusable) {
        Vec::new()
    } else {
        sqlx::query_scalar("SELECT item_id FROM nfo_origins WHERE file_id=? ORDER BY item_id")
            .bind(file)
            .fetch_all(&mut *conn)
            .await?
    };
    let mut owner_facts = Vec::with_capacity(owners.len());
    for owner in owners {
        owner_facts.push(work_facts(conn, &owner).await?);
    }
    let conflict: Option<String> = match &observation {
        nfo::SidecarObservation::Parsed {
            external_id: Some(id),
            ..
        } => {
            sqlx::query_scalar(
                "SELECT item_id FROM metadata_documents WHERE source='nfo' AND external_id=?",
            )
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?
        }
        _ => None,
    };
    for effect in nfo::publication(
        file,
        target.as_ref(),
        &owner_facts,
        conflict.as_deref(),
        &observation,
    )
    .map_err(|error| anyhow::anyhow!("invalid NFO publication facts: {error:?}"))?
    {
        match effect {
            nfo::PublicationEffect::DeleteContribution { item } => {
                sqlx::query("DELETE FROM metadata_documents WHERE item_id=? AND source=?")
                    .bind(item)
                    .bind(SOURCE)
                    .execute(&mut *conn)
                    .await?;
            }
            nfo::PublicationEffect::ClearOrigin { item } => {
                sqlx::query("DELETE FROM nfo_origins WHERE item_id=?")
                    .bind(item)
                    .execute(&mut *conn)
                    .await?;
            }
            nfo::PublicationEffect::PutContribution {
                item,
                document,
                external_id,
                revision,
            } => {
                sqlx::query("INSERT INTO metadata_documents VALUES (?,?,?,?,?,?) ON CONFLICT(item_id,source) DO UPDATE SET revision=excluded.revision,external_id=excluded.external_id,document_json=excluded.document_json,updated_at=excluded.updated_at")
                    .bind(item).bind(SOURCE).bind(revision).bind(external_id).bind(document).bind(crate::now()).execute(&mut *conn).await?;
            }
            nfo::PublicationEffect::SetOrigin { item, file } => {
                sqlx::query("INSERT INTO nfo_origins VALUES (?,?) ON CONFLICT(item_id) DO UPDATE SET file_id=excluded.file_id")
                    .bind(item).bind(file).execute(&mut *conn).await?;
            }
            nfo::PublicationEffect::ProjectTitle { item } => {
                crate::metadata::project_title(conn, &item).await?
            }
        }
    }
    Ok(())
}

async fn work_facts(conn: &mut SqliteConnection, item: &str) -> anyhow::Result<nfo::WorkFacts> {
    let (state, external_id, document, revision): (String, Option<String>, Option<String>, Option<i64>) = sqlx::query_as(
        "SELECT i.match_state,d.external_id,d.document_json,d.revision FROM items i LEFT JOIN metadata_documents d ON d.item_id=i.id AND d.source='nfo' WHERE i.id=?")
        .bind(item).fetch_one(&mut *conn).await?;
    Ok(nfo::WorkFacts {
        item: item.to_owned(),
        match_state: serde_json::from_value(serde_json::Value::String(state))?,
        external_id,
        document,
        revision,
    })
}

/// On-demand metadata refresh for one work without a full scan: re-observe
/// the sidecars beside its available files (outside the writer) and publish
/// them in one writer transaction. Reads happen only under a root whose
/// volume identity matches the registration (checked before and after);
/// each observation is published only if its source binding and file facts
/// are unchanged at commit. Returns how many observations were published.
pub async fn refresh_item(app: &crate::App, item: &str) -> anyhow::Result<usize> {
    let observed = observe_item(app, item).await?;
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let published = publish_refresh_in(&mut tx, item, &observed).await?;
    tx.commit().await?;
    Ok(published.len())
}

pub(crate) type RefreshRow = (String, String, String, String, i64, String);
pub(crate) async fn observe_item(
    app: &crate::App,
    item: &str,
) -> anyhow::Result<Vec<(RefreshRow, Sidecar)>> {
    let files: Vec<RefreshRow> = sqlx::query_as(
        "SELECT f.id,f.relative_path,l.root,l.root_identity,l.binding_revision,f.revision FROM media_files f JOIN editions e ON e.id=f.edition_id JOIN libraries l ON l.id=f.library_id WHERE e.item_id=? AND f.available=1 AND f.generated=0 AND l.enabled=1 ORDER BY f.id",
    )
    .bind(item)
    .fetch_all(&app.db)
    .await?;
    let observed = tokio::task::spawn_blocking(move || {
        let mut budget = SCAN_BUDGET;
        let identity = |root: &str| {
            std::fs::metadata(root)
                .ok()
                .map(|m| crate::db::root_identity(&m))
        };
        files
            .into_iter()
            .map(|row| {
                let (_, relative, root, expected, _, _) = &row;
                let before = identity(root);
                let sidecar = if playscale_core::metadata::refresh_root_matches(
                    expected,
                    before.as_deref(),
                ) {
                    let sidecar = read(Path::new(root), relative, &mut budget);
                    // A root replaced during the read proves nothing.
                    if playscale_core::metadata::refresh_root_matches(
                        expected,
                        identity(root).as_deref(),
                    ) {
                        sidecar
                    } else {
                        Sidecar::Unusable
                    }
                } else {
                    Sidecar::Unusable
                };
                (row, sidecar)
            })
            .collect::<Vec<_>>()
    })
    .await?;
    Ok(observed)
}

pub(crate) async fn publish_refresh_in(
    tx: &mut SqliteConnection,
    item: &str,
    observed: &[(RefreshRow, Sidecar)],
) -> anyhow::Result<Vec<String>> {
    let mut published = Vec::new();
    for ((file, relative, root, identity, binding, revision), sidecar) in observed {
        if matches!(sidecar, Sidecar::Unusable) {
            continue;
        }
        // The observation must still describe this file at this binding.
        let current: Option<(String, String, String, String, i64, String, bool, bool)> = sqlx::query_as(
            "SELECT e.item_id,f.relative_path,l.root,l.root_identity,l.binding_revision,f.revision,l.enabled,f.available FROM media_files f JOIN libraries l ON l.id=f.library_id JOIN editions e ON e.id=f.edition_id WHERE f.id=?",
        )
        .bind(file)
        .fetch_optional(&mut *tx)
        .await?;
        let observed_binding = playscale_core::metadata::RefreshBinding {
            work: item.to_owned(),
            file: file.clone(),
            relative: relative.clone(),
            root: root.clone(),
            root_identity: identity.clone(),
            binding_revision: *binding,
            file_revision: revision.clone(),
        };
        let current = current.map(
            |(
                work,
                relative,
                root,
                root_identity,
                binding_revision,
                file_revision,
                enabled,
                available,
            )| {
                playscale_core::metadata::CurrentRefreshBinding {
                    binding: playscale_core::metadata::RefreshBinding {
                        work,
                        file: file.clone(),
                        relative,
                        root,
                        root_identity,
                        binding_revision,
                        file_revision,
                    },
                    enabled,
                    available,
                }
            },
        );
        if !playscale_core::metadata::refresh_observation_current(
            &observed_binding,
            current.as_ref(),
            true,
        ) {
            continue;
        }
        publish(tx, file, sidecar).await?;
        published.push(file.clone());
    }
    crate::search::refresh(tx, 100).await?;
    Ok(published)
}
