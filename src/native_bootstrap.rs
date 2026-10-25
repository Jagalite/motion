//! Private fresh-install custody. This journal authorizes only destinations that
//! were both absent; existing installations never acquire implicit migration
//! authority. Native publication follows durable identity acknowledgment.
use anyhow::{Context, ensure};
use playscale_core::catalog_backend::Identity;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    contract: u32,
    motion: PathBuf,
    native: PathBuf,
    staging: PathBuf,
    nonce: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepared {
    nonce: String,
    identity: Identity,
    motion_identity: FileIdentity,
}
#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
struct FileIdentity {
    device: u64,
    inode: u64,
}
fn file_identity(path: &Path) -> anyhow::Result<FileIdentity> {
    ensure!(regular(path)?, "bootstrap-owned file is missing");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(path)?;
        Ok(FileIdentity {
            device: meta.dev(),
            inode: meta.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        anyhow::bail!("fresh bootstrap requires supported filesystem identity")
    }
}
fn create_empty(path: &Path) -> anyhow::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.sync_all()?;
    fs::File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}

fn regular(path: &Path) -> anyhow::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            ensure!(
                meta.is_file(),
                "bootstrap path is not a regular file: {}",
                path.display()
            );
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn durable_new(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let mut file = opts.open(&temp)?;
    file.write_all(&serde_json::to_vec(value)?)?;
    file.sync_all()?;
    drop(file);
    fs::hard_link(&temp, path)?;
    fs::remove_file(&temp)?;
    fs::File::open(path.parent().context("bootstrap parent")?)?.sync_all()?;
    Ok(())
}
fn absolute_destination(path: &Path) -> anyhow::Result<PathBuf> {
    let name = path
        .file_name()
        .context("bootstrap destination needs a filename")?;
    Ok(fs::canonicalize(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?
    .join(name))
}

/// Returns true only with a retained fresh-install ownership intent. Call under
/// the data-directory process lock, before opening either runtime for serving.
pub async fn prepare(motion: &Path, native: &Path) -> anyhow::Result<bool> {
    let motion = absolute_destination(motion)?;
    let native = absolute_destination(native)?;
    ensure!(
        motion != native,
        "Motion and native catalogs need separate destinations"
    );
    let marker = motion
        .parent()
        .unwrap()
        .join("native-bootstrap-intent.json");
    let receipt = motion
        .parent()
        .unwrap()
        .join("native-bootstrap-identity.json");
    let intent: Intent = if regular(&marker)? {
        serde_json::from_slice(&fs::read(&marker)?)?
    } else {
        if regular(&motion)? {
            return Ok(false);
        }
        ensure!(
            !regular(&native)?,
            "fresh native bootstrap refuses an existing catalog; use the operator cutover workflow"
        );
        for destination in [&motion, &native] {
            for suffix in ["-wal", "-shm", "-journal"] {
                ensure!(
                    !PathBuf::from(format!("{}{suffix}", destination.display())).exists(),
                    "orphaned SQLite sidecar requires reconciliation before fresh bootstrap"
                );
            }
        }
        ensure!(
            !regular(&receipt)?,
            "orphaned native bootstrap identity requires reconciliation"
        );
        let nonce = uuid::Uuid::new_v4().to_string();
        let intent = Intent {
            contract: 1,
            motion: motion.clone(),
            native: native.clone(),
            staging: native
                .parent()
                .unwrap()
                .join(format!(".motion-native-{nonce}.sqlite3")),
            nonce,
        };
        durable_new(&marker, &intent)?;
        intent
    };
    ensure!(
        intent.contract == 1
            && intent.motion == motion
            && intent.native == native
            && intent.staging
                == native
                    .parent()
                    .unwrap()
                    .join(format!(".motion-native-{}.sqlite3", intent.nonce))
            && uuid::Uuid::parse_str(&intent.nonce).is_ok(),
        "fresh bootstrap destination changed; explicit native catalog configuration is required and legacy fallback is forbidden"
    );
    let motion_staging = motion
        .parent()
        .unwrap()
        .join(format!(".motion-host-{}.sqlite3", intent.nonce));
    let prepared: Prepared = if regular(&receipt)? {
        serde_json::from_slice(&fs::read(&receipt)?)?
    } else {
        ensure!(
            !regular(&native)?,
            "native destination appeared before durable bootstrap identity"
        );
        ensure!(
            !regular(&motion)?,
            "Motion destination appeared before durable bootstrap identity"
        );
        if !regular(&motion_staging)? {
            create_empty(&motion_staging)?;
        }
        ensure!(
            fs::metadata(&motion_staging)?.len() == 0,
            "unacknowledged Motion staging is not empty"
        );
        if !regular(&intent.staging)? {
            motion_catalog_backend::initialize(intent.staging.clone()).await?;
        }
        let status = motion_catalog_backend::inspect(intent.staging.clone()).await?;
        ensure!(
            status.generation == 0,
            "unacknowledged bootstrap staging catalog is not pristine"
        );
        let prepared = Prepared {
            nonce: intent.nonce.clone(),
            identity: status.identity,
            motion_identity: file_identity(&motion_staging)?,
        };
        durable_new(&receipt, &prepared)?;
        prepared
    };
    ensure!(
        prepared.nonce == intent.nonce,
        "bootstrap identity belongs to another intent"
    );
    if regular(&motion)? {
        ensure!(
            file_identity(&motion)? == prepared.motion_identity,
            "Motion bootstrap database was replaced; explicit reconciliation required"
        );
    } else {
        ensure!(
            file_identity(&motion_staging)? == prepared.motion_identity,
            "Motion staging identity changed"
        );
        fs::hard_link(&motion_staging, &motion)?;
        fs::File::open(motion.parent().unwrap())?.sync_all()?;
    }
    if regular(&motion_staging)? {
        ensure!(
            file_identity(&motion_staging)? == prepared.motion_identity,
            "Motion staging identity changed"
        );
        fs::remove_file(&motion_staging)?;
        fs::File::open(motion.parent().unwrap())?.sync_all()?;
    }
    if !regular(&native)? {
        let status = motion_catalog_backend::inspect(intent.staging.clone()).await?;
        ensure!(
            status.generation == 0,
            "prepared staging catalog changed before publication"
        );
        prepared
            .identity
            .admit(&status.identity)
            .map_err(|e| anyhow::anyhow!("bootstrap identity: {e:?}"))?;
        // All native handles are closed. No-clobber publication never replaces
        // another database. A crash leaving both links is cleaned before open.
        for suffix in ["-wal", "-shm"] {
            ensure!(
                !PathBuf::from(format!("{}{suffix}", intent.staging.display())).exists(),
                "bootstrap staging has live SQLite sidecars"
            );
        }
        fs::hard_link(&intent.staging, &native)?;
        fs::File::open(native.parent().unwrap())?.sync_all()?;
    }
    if regular(&intent.staging)? {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let a = fs::metadata(&intent.staging)?;
            let b = fs::metadata(&native)?;
            ensure!(
                a.dev() == b.dev() && a.ino() == b.ino(),
                "bootstrap destination replaced before publication completed"
            );
        }
        #[cfg(not(unix))]
        {
            anyhow::bail!(
                "bootstrap duplicate-link reconciliation requires supported file identity"
            );
        }
        fs::remove_file(&intent.staging)?;
        fs::File::open(native.parent().unwrap())?.sync_all()?;
    }
    let status = motion_catalog_backend::inspect(native).await?;
    prepared
        .identity
        .admit(&status.identity)
        .map_err(|e| anyhow::anyhow!("bootstrap catalog replaced: {e:?}"))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn fresh_intent_publishes_once_and_reuses_exact_identity() {
        let dir = tempfile::tempdir().unwrap();
        let motion = dir.path().join("motion.sqlite3");
        let native = dir.path().join("catabolic.sqlite3");
        assert!(super::prepare(&motion, &native).await.unwrap());
        let first = motion_catalog_backend::inspect(native.clone())
            .await
            .unwrap();
        assert!(super::prepare(&motion, &native).await.unwrap());
        assert_eq!(
            first.identity,
            motion_catalog_backend::inspect(native.clone())
                .await
                .unwrap()
                .identity
        );
        assert!(!dir.path().read_dir().unwrap().any(|e| {
            let name = e.unwrap().file_name();
            let name = name.to_string_lossy();
            name.starts_with(".motion-native-") && name.ends_with(".sqlite3")
        }));
        assert!(
            super::prepare(&motion, &dir.path().join("different.sqlite3"))
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn interruption_before_identity_and_after_publication_resumes() {
        for published in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let motion = dir.path().join("motion.sqlite3");
            let native = dir.path().join("catabolic.sqlite3");
            let nonce = uuid::Uuid::new_v4().to_string();
            let staging = dir.path().join(format!(".motion-native-{nonce}.sqlite3"));
            let intent = super::Intent {
                contract: 1,
                motion: motion.clone(),
                native: native.clone(),
                staging: staging.clone(),
                nonce: nonce.clone(),
            };
            super::durable_new(&dir.path().join("native-bootstrap-intent.json"), &intent).unwrap();
            motion_catalog_backend::initialize(staging.clone())
                .await
                .unwrap();
            let identity = motion_catalog_backend::inspect(staging.clone())
                .await
                .unwrap()
                .identity;
            if published {
                let motion_staging = dir.path().join(format!(".motion-host-{nonce}.sqlite3"));
                super::create_empty(&motion_staging).unwrap();
                super::durable_new(
                    &dir.path().join("native-bootstrap-identity.json"),
                    &super::Prepared {
                        nonce,
                        identity: identity.clone(),
                        motion_identity: super::file_identity(&motion_staging).unwrap(),
                    },
                )
                .unwrap();
                std::fs::hard_link(&staging, &native).unwrap();
            }
            assert!(super::prepare(&motion, &native).await.unwrap());
            assert!(!staging.exists());
            assert_eq!(
                identity,
                motion_catalog_backend::inspect(native)
                    .await
                    .unwrap()
                    .identity
            );
        }
    }
    #[tokio::test]
    async fn existing_data_never_acquires_fresh_install_authority() {
        let dir = tempfile::tempdir().unwrap();
        let motion = dir.path().join("motion.sqlite3");
        let native = dir.path().join("catabolic.sqlite3");
        std::fs::write(&motion, b"existing").unwrap();
        assert!(!super::prepare(&motion, &native).await.unwrap());
        assert!(!native.exists());
        assert!(!dir.path().join("native-bootstrap-intent.json").exists());
    }
    #[tokio::test]
    async fn retained_intent_never_adopts_replaced_motion_database() {
        let dir = tempfile::tempdir().unwrap();
        let motion = dir.path().join("motion.sqlite3");
        let native = dir.path().join("native.sqlite3");
        super::prepare(&motion, &native).await.unwrap();
        std::fs::rename(&motion, dir.path().join("owned-motion.sqlite3")).unwrap();
        let db = playscale::db::connect(&motion).await.unwrap();
        sqlx::query("INSERT INTO items(id,title,kind) VALUES('foreign','Foreign','video')")
            .execute(&db)
            .await
            .unwrap();
        db.close().await;
        assert!(super::prepare(&motion, &native).await.is_err());
        let db = playscale::db::connect(&motion).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM catabolic_binding")
                .fetch_one(&db)
                .await
                .unwrap(),
            0
        );
    }
    #[tokio::test]
    async fn destination_without_ownership_and_replaced_identity_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let motion = dir.path().join("motion.sqlite3");
        let native = dir.path().join("catabolic.sqlite3");
        motion_catalog_backend::initialize(native.clone())
            .await
            .unwrap();
        assert!(super::prepare(&motion, &native).await.is_err());
        let owned = tempfile::tempdir().unwrap();
        let m = owned.path().join("motion.sqlite3");
        let n = owned.path().join("native.sqlite3");
        super::prepare(&m, &n).await.unwrap();
        std::fs::rename(&n, owned.path().join("retained-original.sqlite3")).unwrap();
        motion_catalog_backend::initialize(n.clone()).await.unwrap();
        assert!(super::prepare(&m, &n).await.is_err());
    }
}
