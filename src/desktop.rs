//! Private inherited-pipe readiness for the native host. This is not a public
//! API or authorization token. The server retains its data-directory lock;
//! the host owns only the child handle it actually spawned.
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Ready {
    pub protocol: u8,
    pub server_id: String,
    pub server_epoch: String,
    pub origin: String,
    pub version: &'static str,
    pub contract_digest: String,
}

/// Publish only after recovery and listener/service initialization. A failed
/// write fails startup: the parent must never infer readiness from a PID.
#[cfg(unix)]
pub fn publish(fd: i32, ready: &Ready) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::fd::FromRawFd;
    anyhow::ensure!(
        fd > 2,
        "readiness descriptor must be a private inherited pipe"
    );
    // CLI ownership transfers this dedicated descriptor to this function once.
    let mut pipe = unsafe { std::fs::File::from_raw_fd(fd) };
    serde_json::to_writer(&mut pipe, ready)?;
    pipe.write_all(b"\n")?;
    pipe.flush()?;
    Ok(())
}

#[cfg(not(unix))]
pub fn publish(_: i32, _: &Ready) -> anyhow::Result<()> {
    anyhow::bail!("inherited readiness descriptors require Unix")
}

/// Qualify the installed immutable tree before advertising native readiness.
/// Path validation prevents a malformed receipt from authorizing reads outside
/// the selected package. No runtime download or package mutation occurs.
pub fn verify_player_assets(root: &std::path::Path) -> anyhow::Result<()> {
    use sha2::{Digest, Sha256};
    let receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("playscale-package.json"))?)?;
    let files = receipt["files"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("missing player asset inventory"))?;
    anyhow::ensure!(
        files.contains_key("web/generated/player/index.js"),
        "player entry absent from inventory"
    );
    let canonical = root.canonicalize()?;
    for (relative, expected) in files {
        let path = std::path::Path::new(relative);
        anyhow::ensure!(
            !path.is_absolute()
                && path
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
            "invalid player inventory path"
        );
        let path = root.join(path).canonicalize()?;
        anyhow::ensure!(
            path.starts_with(&canonical),
            "player asset leaves its package"
        );
        let digest = format!("{:x}", Sha256::digest(std::fs::read(path)?));
        anyhow::ensure!(
            expected.as_str() == Some(digest.as_str()),
            "player asset checksum mismatch: {relative}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inventory_rejects_corruption_and_paths_outside_package() {
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("web/generated/player")).unwrap();
        let entry = dir.path().join("web/generated/player/index.js");
        std::fs::write(&entry, b"player").unwrap();
        let digest = format!("{:x}", Sha256::digest(b"player"));
        let receipt = dir.path().join("playscale-package.json");
        std::fs::write(
            &receipt,
            serde_json::json!({"files":{"web/generated/player/index.js":digest}}).to_string(),
        )
        .unwrap();
        verify_player_assets(dir.path()).unwrap();
        std::fs::write(&entry, b"changed").unwrap();
        assert!(verify_player_assets(dir.path()).is_err());
        std::fs::write(&entry, b"player").unwrap();
        std::fs::write(&receipt,serde_json::json!({"files":{"web/generated/player/index.js":digest,"../secret":"unused"}}).to_string()).unwrap();
        assert!(verify_player_assets(dir.path()).is_err());
    }
}
